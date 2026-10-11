//! The indexing pipeline: list heights with getBlocks, fetch blocks concurrently but in order,
//! normalize them into batches and commit each batch on a separate writer task.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::Result;
use futures::{TryStreamExt, stream};
use sqlx::PgPool;
use tokio::sync::{mpsc, watch};

use crate::db::{self, Cursor};
use crate::model::Block;
use crate::normalize::{Batch, Missing, Stats};
use crate::rpc::Rpc;

/// Heights per `getBlocks` call; the node accepts up to 5,001.
const GET_BLOCKS_SPAN: u64 = 5_000;
const HEAD_POLL: Duration = Duration::from_millis(500);
const NOT_FOUND_RETRY: Duration = Duration::from_millis(500);

#[derive(Clone, Copy)]
pub enum Until {
    /// Index through this height, then return.
    Height(u64),
    /// Keep following the head, `lag` blocks behind it. Heights the RPC does not list yet are
    /// waited for rather than recorded as missing, since load-balanced nodes can trail each other.
    Head { lag: u64 },
}

pub struct Options {
    /// Shown in log lines.
    pub name: &'static str,
    pub from: u64,
    pub until: Until,
    pub workers: usize,
    /// A batch is committed once it has this many blocks or is `flush_after` old.
    pub batch: usize,
    pub flush_after: Duration,
    /// Which `ingest_state` cursor each committed batch advances.
    pub cursor: Cursor,
    /// Extra getBlock attempts before a listed height is recorded as not found.
    pub not_found_retries: u32,
    pub report_every: Duration,
}

#[derive(Default)]
pub struct Summary {
    pub stats: Stats,
    /// Heights covered, including missing ones.
    pub heights: u64,
    pub missing: u64,
    pub write_time: Duration,
}

enum Step {
    Fetch(u64),
    /// All heights up to `end` have been emitted; `not_listed` were absent from getBlocks.
    SpanEnd {
        end: u64,
        not_listed: Vec<(u64, u64)>,
    },
}

enum Fetched {
    Block(u64, Option<Block>),
    SpanEnd { end: u64, not_listed: Vec<(u64, u64)> },
}

/// Ranges of `[start, end]` missing from the sorted `listed`.
fn gaps(start: u64, end: u64, listed: &[u64]) -> Vec<(u64, u64)> {
    let mut out = Vec::new();
    let mut expect = start;
    for &h in listed {
        if h > expect {
            out.push((expect, h - 1));
        }
        expect = h + 1;
    }
    if expect <= end {
        out.push((expect, end));
    }
    out
}

/// Indexes from `opts.from` until `opts.until` is reached (never, when following the head), an
/// error, or `stop`. Whatever was read before stopping is committed first.
pub async fn run(
    pool: &PgPool,
    net: i16,
    rpc: &Rpc,
    opts: &Options,
    mut stop: watch::Receiver<bool>,
) -> Result<Summary> {
    let (send, mut recv) = mpsc::channel::<Batch>(2);
    let cursor = opts.cursor;
    let writer = tokio::spawn({
        let pool = pool.clone();
        async move {
            let mut s = Summary::default();
            while let Some(b) = recv.recv().await {
                let t = Instant::now();
                db::write(&pool, net, &b, cursor).await?;
                s.write_time += t.elapsed();
                s.stats.add(&b.stats);
                s.missing += b.missing.iter().map(|m| m.to - m.from + 1).sum::<u64>();
            }
            anyhow::Ok(s)
        }
    });

    let head = Arc::new(AtomicU64::new(rpc.block_height().await?));
    let until = opts.until;
    // Each item is the steps of one getBlocks span; the state is the next height to list.
    let spans = stream::try_unfold(opts.from, {
        let (rpc, head) = (rpc.clone(), head.clone());
        move |next| {
            let (rpc, head) = (rpc.clone(), head.clone());
            async move {
                loop {
                    let end = match until {
                        Until::Height(h) => h,
                        Until::Head { lag } => head.load(Ordering::Relaxed).saturating_sub(lag),
                    };
                    if next > end {
                        if let Until::Height(_) = until {
                            return Ok(None);
                        }
                        tokio::time::sleep(HEAD_POLL).await;
                        head.store(rpc.block_height().await?, Ordering::Relaxed);
                        continue;
                    }
                    let span_end = (next + GET_BLOCKS_SPAN - 1).min(end);
                    let mut listed = rpc.blocks(next, span_end).await?;
                    listed.retain(|h| (next..=span_end).contains(h));
                    listed.sort_unstable();
                    listed.dedup();
                    let covered = match (until, listed.last()) {
                        (Until::Height(_), _) => span_end,
                        (Until::Head { .. }, Some(&last)) => last,
                        (Until::Head { .. }, None) => {
                            tokio::time::sleep(HEAD_POLL).await;
                            continue;
                        }
                    };
                    let not_listed = gaps(next, covered, &listed);
                    let steps = listed.into_iter().map(Step::Fetch).chain([Step::SpanEnd { end: covered, not_listed }]);
                    return anyhow::Ok(Some((stream::iter(steps.map(anyhow::Ok)), covered + 1)));
                }
            }
        }
    });
    let retries = opts.not_found_retries;
    let fetched = spans
        .try_flatten()
        .map_ok(|step| {
            let rpc = rpc.clone();
            async move {
                Ok(match step {
                    Step::Fetch(h) => {
                        let mut block = rpc.block(h).await?;
                        for _ in 0..retries {
                            if block.is_some() {
                                break;
                            }
                            tokio::time::sleep(NOT_FOUND_RETRY).await;
                            block = rpc.block(h).await?;
                        }
                        Fetched::Block(h, block)
                    }
                    Step::SpanEnd { end, not_listed } => Fetched::SpanEnd { end, not_listed },
                })
            }
        })
        .try_buffered(opts.workers);
    let mut fetched = std::pin::pin!(fetched);

    // Runs until the input ends, fails or `stop` fires; the writer is joined afterwards either way.
    let name = opts.name;
    let read = async {
        let started = Instant::now();
        let mut batch = Batch::default();
        let (mut last_flush, mut last_report) = (Instant::now(), Instant::now());
        let mut done = 0u64;
        loop {
            let item = tokio::select! {
                item = fetched.try_next() => item?,
                _ = stop.wait_for(|s| *s) => None,
            };
            let Some(item) = item else { break };
            // The highest height handled so far, for progress lines.
            let at = match item {
                Fetched::Block(h, Some(b)) => {
                    if let Err(e) = batch.push(&b) {
                        tracing::error!(task = name, h, "skipping block: {e:#}");
                        batch.missing.push(Missing { from: h, to: h, reason: format!("invalid: {e:#}") });
                    }
                    batch.through = Some(h);
                    done += 1;
                    h
                }
                Fetched::Block(h, None) => {
                    tracing::warn!(task = name, h, "listed by getBlocks but getBlock says not found");
                    batch.missing.push(Missing { from: h, to: h, reason: "not_found".into() });
                    batch.through = Some(h);
                    done += 1;
                    h
                }
                Fetched::SpanEnd { end, not_listed } => {
                    for (from, to) in not_listed {
                        tracing::warn!(task = name, from, to, "heights not listed by getBlocks");
                        done += to - from + 1;
                        batch.missing.push(Missing { from, to, reason: "not_listed".into() });
                    }
                    batch.through = Some(end);
                    end
                }
            };
            let full = batch.stats.blocks as usize >= opts.batch;
            if full || (batch.through.is_some() && last_flush.elapsed() >= opts.flush_after) {
                // Fails only if the writer has stopped; its error surfaces when it is joined.
                if send.send(std::mem::take(&mut batch)).await.is_err() {
                    return Ok(());
                }
                last_flush = Instant::now();
            }
            if last_report.elapsed() >= opts.report_every {
                let rate = done as f64 / started.elapsed().as_secs_f64();
                let behind = head.load(Ordering::Relaxed).saturating_sub(at);
                match until {
                    Until::Height(to) => {
                        let left = to.saturating_sub(at);
                        let eta_h = left as f64 / rate.max(f64::MIN_POSITIVE) / 3600.0;
                        tracing::info!(task = name, height = at, left, "{rate:.1} heights/s, done in ~{eta_h:.1} h");
                    }
                    Until::Head { .. } => tracing::info!(task = name, height = at, behind, "{rate:.1} heights/s"),
                }
                last_report = Instant::now();
            }
        }
        if batch.through.is_some() {
            let _ = send.send(batch).await;
        }
        anyhow::Ok(())
    };
    let read_result = read.await;
    drop(send);
    let mut summary = writer.await??;
    read_result?;
    summary.heights = summary.stats.blocks + summary.missing;
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::gaps;

    #[test]
    fn gaps_between_listed_heights() {
        assert_eq!(gaps(10, 15, &[10, 11, 12, 13, 14, 15]), vec![]);
        assert_eq!(gaps(10, 15, &[11, 12, 15]), vec![(10, 10), (13, 14)]);
        assert_eq!(gaps(10, 15, &[10, 12]), vec![(11, 11), (13, 15)]);
        assert_eq!(gaps(10, 15, &[]), vec![(10, 15)]);
    }
}
