//! Long-running indexing for one network: a head follower that keeps the index current, a
//! backfill that fills in everything from the oldest block the RPC serves up to where the
//! follower started, and periodic checks for pruning and network resets.

use std::future::Future;
use std::time::Duration;

use anyhow::{Context, Result};
use sqlx::PgPool;
use tokio::sync::watch;

use crate::conformance;
use crate::db::{self, Cursor};
use crate::ingest::{self, Options, Until};
use crate::normalize::{self, Batch};
use crate::rpc::Rpc;

/// Blocks are final as soon as the RPC serves them (79 of 79 head blocks were unchanged when
/// re-fetched 20 s later on 2026-10-11); the lag only absorbs nodes that trail the head slightly.
const FOLLOW_LAG: u64 = 2;
const RETRY_AFTER: Duration = Duration::from_secs(30);
const RETENTION_SAMPLE_EVERY: Duration = Duration::from_secs(3600);
const RESET_CHECK_EVERY: Duration = Duration::from_secs(600);
const CONFORMANCE_EVERY: Duration = Duration::from_secs(3600);
const GAP_RETRY_EVERY: Duration = Duration::from_secs(3600);
/// Heights re-fetched per gap retry, so a long outage's backlog is worked off over several runs.
const GAP_RETRY_MAX: i64 = 2_000;
/// The reset check compares the stored block this far below the follower's cursor with the RPC.
const RESET_CHECK_DEPTH: u64 = 1_000;

pub struct Config {
    pub backfill: bool,
    pub backfill_workers: usize,
    pub follow_workers: usize,
}

pub async fn run(pool: &PgPool, net: i16, rpc: &Rpc, cfg: &Config, stop: watch::Receiver<bool>) -> Result<()> {
    let head = rpc.block_height().await?;
    let oldest = rpc.oldest_block(head).await?;
    db::record_retention(pool, net, oldest, head).await?;
    tracing::info!(oldest, head, "RPC serves {} blocks", head - oldest + 1);

    if db::cursors(pool, net).await?.is_none() {
        db::start_backfill(pool, net, oldest).await?;
    }
    let c = db::cursors(pool, net).await?.context("ingest_state row missing")?;
    let tip_from = match c.tip_from {
        Some(t) => t,
        None => {
            // An older backfill may already have passed head - lag while following the head itself.
            let t = head.saturating_sub(FOLLOW_LAG).max(c.backfill_next);
            db::start_tip(pool, net, t).await?;
            tracing::info!(tip_from = t, "head follower starts here; the backfill stops below it");
            t
        }
    };
    if c.backfill_next < oldest.min(tip_from) {
        tracing::warn!(from = c.backfill_next, to = oldest - 1, "pruned before the backfill reached them");
        db::skip_pruned(pool, net, c.backfill_next, oldest - 1).await?;
    }

    spawn_every(RETENTION_SAMPLE_EVERY, RETENTION_SAMPLE_EVERY, "retention sample", {
        let (pool, rpc) = (pool.clone(), rpc.clone());
        move || {
            let (pool, rpc) = (pool.clone(), rpc.clone());
            async move {
                let head = rpc.block_height().await?;
                let oldest = rpc.oldest_block(head).await?;
                db::record_retention(&pool, net, oldest, head).await?;
                tracing::info!(oldest, head, "retention sample");
                Ok(())
            }
        }
    });
    spawn_every(Duration::from_secs(120), CONFORMANCE_EVERY, "rpc conformance", {
        let (pool, rpc) = (pool.clone(), rpc.clone());
        move || {
            let (pool, rpc) = (pool.clone(), rpc.clone());
            async move {
                let results = conformance::run(&pool, net, &rpc).await?;
                let quirks = results.iter().filter(|r| r.1).count();
                tracing::info!(checks = results.len(), quirks, "rpc conformance");
                Ok(())
            }
        }
    });
    spawn_every(Duration::from_secs(600), GAP_RETRY_EVERY, "gap retry", {
        let (pool, rpc) = (pool.clone(), rpc.clone());
        move || {
            let (pool, rpc) = (pool.clone(), rpc.clone());
            async move { retry_gaps(&pool, net, &rpc).await }
        }
    });
    spawn_every(RESET_CHECK_EVERY, RESET_CHECK_EVERY, "reset check", {
        let (pool, rpc) = (pool.clone(), rpc.clone());
        move || {
            let (pool, rpc) = (pool.clone(), rpc.clone());
            async move { check_reset(&pool, net, &rpc).await }
        }
    });

    let follow = supervise("follow", stop.clone(), {
        let (pool, rpc, stop) = (pool.clone(), rpc.clone(), stop.clone());
        let workers = cfg.follow_workers;
        move || {
            let (pool, rpc, stop) = (pool.clone(), rpc.clone(), stop.clone());
            async move {
                let from = db::cursors(&pool, net).await?.and_then(|c| c.tip_next).context("tip cursor missing")?;
                let opts = Options {
                    name: "follow",
                    from,
                    until: Until::Head { lag: FOLLOW_LAG },
                    workers,
                    batch: 100,
                    flush_after: Duration::from_secs(1),
                    cursor: Cursor::Tip,
                    not_found_retries: 10,
                    report_every: Duration::from_secs(60),
                };
                tracing::info!(task = "follow", from, "following the head");
                ingest::run(&pool, net, &rpc, &opts, stop).await?;
                Ok(())
            }
        }
    });

    let backfill = async {
        if !cfg.backfill {
            return Ok(());
        }
        supervise("backfill", stop.clone(), {
            let (pool, rpc, stop) = (pool.clone(), rpc.clone(), stop.clone());
            let workers = cfg.backfill_workers;
            move || {
                let (pool, rpc, stop) = (pool.clone(), rpc.clone(), stop.clone());
                async move {
                    let from = db::cursors(&pool, net).await?.context("ingest_state row missing")?.backfill_next;
                    if from >= tip_from {
                        tracing::info!(task = "backfill", "complete: everything below the head follower is indexed");
                        return Ok(());
                    }
                    let opts = Options {
                        name: "backfill",
                        from,
                        until: Until::Height(tip_from - 1),
                        workers,
                        batch: 500,
                        flush_after: Duration::from_secs(5),
                        cursor: Cursor::Backfill,
                        not_found_retries: 2,
                        report_every: Duration::from_secs(10),
                    };
                    tracing::info!(task = "backfill", from, to = tip_from - 1, workers, "backfilling");
                    let s = ingest::run(&pool, net, &rpc, &opts, stop.clone()).await?;
                    if !*stop.borrow() {
                        tracing::info!(task = "backfill", heights = s.heights, missing = s.missing, "complete");
                        db::record_event(&pool, net, "backfill_complete", &format!("through height {}", tip_from - 1))
                            .await?;
                    }
                    Ok(())
                }
            }
        })
        .await
    };

    tokio::try_join!(follow, backfill)?;
    Ok(())
}

/// Runs `task` until it returns `Ok` or `stop` fires, restarting it `RETRY_AFTER` after each error.
/// Tasks resume from the cursors saved in the database, so a restart repeats no work.
async fn supervise<F, Fut>(name: &'static str, mut stop: watch::Receiver<bool>, mut task: F) -> Result<()>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<()>>,
{
    loop {
        match task().await {
            Ok(()) => return Ok(()),
            Err(e) => {
                tracing::error!(task = name, "{e:#}; restarting from the saved cursor in {} s", RETRY_AFTER.as_secs());
                tokio::select! {
                    _ = tokio::time::sleep(RETRY_AFTER) => {}
                    _ = stop.wait_for(|s| *s) => return Ok(()),
                }
            }
        }
    }
}

/// Runs `job` every `period` in the background, logging failures; it lives as long as the process.
fn spawn_every<F, Fut>(first_after: Duration, period: Duration, name: &'static str, mut job: F)
where
    F: FnMut() -> Fut + Send + 'static,
    Fut: Future<Output = Result<()>> + Send,
{
    tokio::spawn(async move {
        tokio::time::sleep(first_after).await;
        loop {
            if let Err(e) = job().await {
                tracing::warn!("{name} failed: {e:#}");
            }
            tokio::time::sleep(period).await;
        }
    });
}

/// Fetches heights recorded as `not_found` or `not_listed` again: load-balanced nodes trail each
/// other, so these are often transient. A fully recovered range leaves `missing_blocks`.
pub async fn retry_gaps(pool: &PgPool, net: i16, rpc: &Rpc) -> Result<()> {
    let ranges: Vec<(i64, i64)> = sqlx::query_as(
        "SELECT from_height, to_height FROM missing_blocks
         WHERE network = $1 AND reason IN ('not_found', 'not_listed') ORDER BY from_height LIMIT 100",
    )
    .bind(net)
    .fetch_all(pool)
    .await?;
    let mut budget = GAP_RETRY_MAX;
    for (from, to) in ranges {
        let n = to - from + 1;
        if n > budget {
            break;
        }
        budget -= n;
        let mut batch = Batch::default();
        let mut found = 0;
        for h in from..=to {
            if let Some(b) = rpc.block(h as u64).await?
                && batch.push(&b).is_ok()
            {
                found += 1;
            }
        }
        if found > 0 {
            db::write(pool, net, &batch, Cursor::None).await?;
        }
        let sql = if found == n {
            "DELETE FROM missing_blocks WHERE network = $1 AND from_height = $2"
        } else {
            "UPDATE missing_blocks SET seen_at = now() WHERE network = $1 AND from_height = $2"
        };
        sqlx::query(sql).bind(net).bind(from).execute(pool).await?;
        tracing::info!(from, to, recovered = found, "gap retry");
    }
    Ok(())
}

/// A reset shows up as the head moving backwards, or as a stored block that the RPC now serves
/// differently (or not at all). Either is recorded as a `reset_suspected` event.
async fn check_reset(pool: &PgPool, net: i16, rpc: &Rpc) -> Result<()> {
    let Some(c) = db::cursors(pool, net).await? else { return Ok(()) };
    let (Some(tip_from), Some(tip_next)) = (c.tip_from, c.tip_next) else { return Ok(()) };

    let head = rpc.block_height().await?;
    let mut detail = None;
    if head + RESET_CHECK_DEPTH < tip_next {
        detail = Some(format!("head {head} is below the follower cursor {tip_next}"));
    } else {
        let h = tip_next.saturating_sub(RESET_CHECK_DEPTH);
        if h >= tip_from
            && let Some(stored) = db::fingerprint(pool, net, h).await?
        {
            let served = rpc.block(h).await?.map(|b| normalize::fingerprint(&b)).transpose()?;
            if served != Some(stored) {
                let served = served.map_or("no block".to_owned(), |f| format!("{f:016x}"));
                detail = Some(format!("height {h}: stored fingerprint {stored:016x}, RPC serves {served}"));
            }
        }
    }
    if let Some(detail) = detail {
        tracing::error!("possible network reset: {detail}");
        db::record_event(pool, net, "reset_suspected", &detail).await?;
    }
    Ok(())
}
