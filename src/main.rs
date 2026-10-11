//! Kelvin: indexer and history API for Rialo.

mod api;
mod conformance;
mod db;
mod decode;
mod indexer;
mod ingest;
mod model;
mod normalize;
mod redecode;
mod rpc;

use std::io::IsTerminal;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use anyhow::{Result, ensure};
use clap::{Parser, Subcommand};
use sqlx::PgPool;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::watch;
use tracing_subscriber::EnvFilter;

use crate::db::Cursor;
use crate::ingest::{Options, Summary, Until};
use crate::rpc::Rpc;

#[derive(Parser)]
#[command(version, about = "Indexer and history API for Rialo")]
struct Cli {
    #[arg(long, env = "DATABASE_URL", hide_env_values = true)]
    database_url: String,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Apply pending database migrations.
    Migrate,
    /// Follow the head and backfill everything below it, starting from the oldest block the RPC
    /// still serves. Resumes from the saved cursors; stop with Ctrl-C or SIGTERM.
    Run {
        #[arg(long, default_value = "testnet")]
        network: String,
        /// Concurrent getBlock requests for the backfill.
        #[arg(long, default_value_t = 32)]
        backfill_workers: usize,
        /// Concurrent getBlock requests for the head follower; the chain makes about 7 blocks/s.
        #[arg(long, default_value_t = 8)]
        follow_workers: usize,
        /// Only follow the head.
        #[arg(long)]
        no_backfill: bool,
        /// Also serve the HTTP API on this address, e.g. 127.0.0.1:8080.
        #[arg(long)]
        api: Option<SocketAddr>,
    },
    /// Serve the HTTP API only.
    Serve {
        #[arg(long, default_value = "127.0.0.1:8080")]
        addr: SocketAddr,
    },
    /// Index every block in [from, to] once and report throughput and size figures.
    /// Does not move the backfill cursor.
    Range {
        #[arg(long, default_value = "testnet")]
        network: String,
        #[arg(long)]
        from: u64,
        #[arg(long)]
        to: u64,
        #[arg(long, default_value_t = 16)]
        workers: usize,
        #[arg(long, default_value_t = 500)]
        batch: usize,
    },
    /// Backfill progress, missing blocks and the RPC's retention window.
    Status {
        #[arg(long, default_value = "testnet")]
        network: String,
    },
    /// Row counts and on-disk size per table.
    Sizes,
    /// Fetch heights recorded as not found or not listed again (the indexer does this hourly).
    RetryGaps {
        #[arg(long, default_value = "testnet")]
        network: String,
    },
    /// Run the RPC conformance checks once and print the results (the indexer runs them hourly).
    Conformance {
        #[arg(long, default_value = "testnet")]
        network: String,
    },
    /// Decode stored instructions that have no kind yet, after a decoder was added.
    Redecode {
        #[arg(long, default_value = "testnet")]
        network: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .init();
    let cli = Cli::parse();
    let pool = db::connect(&cli.database_url).await?;
    db::migrate(&pool).await?;
    match cli.cmd {
        Cmd::Migrate => Ok(()),
        Cmd::Run { network, backfill_workers, follow_workers, no_backfill, api } => {
            let (net, url) = db::network(&pool, &network).await?;
            let rpc = Rpc::new(&url)?;
            let cfg = indexer::Config { backfill: !no_backfill, backfill_workers, follow_workers };
            let stop = stop_signal();
            let serve = async {
                match api {
                    Some(addr) => api::serve(pool.clone(), addr, stop.clone()).await,
                    None => Ok(()),
                }
            };
            tokio::try_join!(indexer::run(&pool, net, &rpc, &cfg, stop.clone()), serve)?;
            Ok(())
        }
        Cmd::Serve { addr } => api::serve(pool, addr, stop_signal()).await,
        Cmd::Range { network, from, to, workers, batch } => {
            ensure!(from <= to, "--from must not exceed --to");
            let (net, url) = db::network(&pool, &network).await?;
            let rpc = Rpc::new(&url)?;
            let started = Instant::now();
            let opts = Options {
                name: "range",
                from,
                until: Until::Height(to),
                workers,
                batch,
                flush_after: Duration::from_secs(5),
                cursor: Cursor::None,
                not_found_retries: 2,
                report_every: Duration::from_secs(10),
            };
            let summary = ingest::run(&pool, net, &rpc, &opts, stop_signal()).await?;
            println!("range         {from}..={to} ({network})");
            print_summary(&summary, started.elapsed(), rpc.bytes_read());
            print_sizes(&pool).await
        }
        Cmd::Status { network } => status(&pool, &network).await,
        Cmd::Sizes => print_sizes(&pool).await,
        Cmd::RetryGaps { network } => {
            let (net, url) = db::network(&pool, &network).await?;
            indexer::retry_gaps(&pool, net, &Rpc::new(&url)?).await
        }
        Cmd::Conformance { network } => {
            let (net, url) = db::network(&pool, &network).await?;
            for (id, issue, observed) in conformance::run(&pool, net, &Rpc::new(&url)?).await? {
                println!("{:<28} {:<7} {observed}", id, if issue { "QUIRK" } else { "ok" });
            }
            Ok(())
        }
        Cmd::Redecode { network } => {
            let (net, _) = db::network(&pool, &network).await?;
            let o = redecode::run(&pool, net).await?;
            println!(
                "scanned {} undecoded instructions · decoded {} · new transfers {}",
                num(o.scanned),
                num(o.decoded),
                num(o.transfers)
            );
            Ok(())
        }
    }
}

/// Flips to `true` on the first Ctrl-C or SIGTERM; a second one exits immediately.
fn stop_signal() -> watch::Receiver<bool> {
    let (tx, rx) = watch::channel(false);
    tokio::spawn(async move {
        let mut term = signal(SignalKind::terminate()).expect("installing SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
        tracing::info!("stopping after the blocks already fetched are written (signal again to quit now)");
        let _ = tx.send(true);
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
        std::process::exit(130);
    });
    rx
}

async fn status(pool: &PgPool, network: &str) -> Result<()> {
    let (net, url) = db::network(pool, network).await?;
    let Some(s) = db::status(pool, net).await? else {
        println!("{network}: not started");
        return Ok(());
    };
    let head = Rpc::new(&url)?.block_height().await?;
    println!("{network} (head {})", num(head));
    match (s.tip_from, s.tip_next) {
        (Some(from), Some(next)) => {
            println!(
                "  follow     next {} · {} behind head · started at {} · updated {} UTC",
                num(next as u64),
                num(head.saturating_sub(next as u64)),
                num(from as u64),
                s.tip_updated.as_deref().unwrap_or("-")
            );
        }
        _ => println!("  follow     not started"),
    }
    let end = s.tip_from.unwrap_or(head as i64);
    let total = (end - s.backfill_from).max(1) as f64;
    let done = (s.backfill_next - s.backfill_from).clamp(0, end - s.backfill_from);
    println!(
        "  backfill   next {} · {:.2}% of {}..{} · {} to go · updated {} UTC",
        num(s.backfill_next as u64),
        100.0 * done as f64 / total,
        num(s.backfill_from as u64),
        num(end as u64),
        num((end - s.backfill_next).max(0) as u64),
        s.updated
    );
    println!("  missing    {} heights in {} ranges", num(s.missing_heights as u64), s.missing_ranges);
    if let (Some(first), Some(last)) = (s.retention.first(), s.retention.last()) {
        println!("  retention  {} samples; oldest served block:", s.retention.len());
        println!("             {} UTC  {}  (head {})", first.0, num(first.1 as u64), num(first.2 as u64));
        if s.retention.len() > 1 {
            println!("             {} UTC  {}  (head {})", last.0, num(last.1 as u64), num(last.2 as u64));
            let verdict = if last.1 > first.1 { "moving: pruning is a sliding window" } else { "not moving so far" };
            println!("             oldest moved by {} → {verdict}", num((last.1 - first.1).max(0) as u64));
        }
    }
    for (ts, kind, detail) in &s.events {
        println!("  event      {ts} UTC  {kind}: {detail}");
    }
    Ok(())
}

fn print_summary(s: &Summary, elapsed: Duration, rpc_bytes: u64) {
    let secs = elapsed.as_secs_f64();
    let t = &s.stats;
    println!("heights       {} · {} indexed · {} missing", num(s.heights), num(t.blocks), num(s.missing));
    println!(
        "elapsed       {secs:.1} s · {:.1} heights/s · DB writes {:.1} s (overlapped)",
        s.heights as f64 / secs,
        s.write_time.as_secs_f64()
    );
    println!(
        "txs           user {} (failed {}) · system {} · {:.2} user tx/block",
        num(t.user_txs),
        num(t.failed_txs),
        num(t.sys_txs),
        t.user_txs as f64 / t.blocks.max(1) as f64
    );
    println!(
        "instructions  {} · decoded {:.1}% · transfers {}",
        num(t.ixs),
        100.0 * t.ixs_decoded as f64 / t.ixs.max(1) as f64,
        num(t.transfers)
    );
    let mut undecoded: Vec<_> = t.undecoded.iter().collect();
    undecoded.sort_by(|a, b| b.1.cmp(a.1));
    for (program, n) in undecoded.iter().take(10) {
        println!("  undecoded   {:>8}  {}", num(**n), decode::label(program).unwrap_or(program));
    }
    println!("RPC payload   {} · {} B/block", mb(rpc_bytes as i64), num(rpc_bytes / t.blocks.max(1)));
}

async fn print_sizes(pool: &PgPool) -> Result<()> {
    let tables = db::sizes(pool).await?;
    let blocks = tables.iter().find(|t| t.name == "blocks").map_or(0, |t| t.rows).max(1);
    let total: i64 = tables.iter().map(|t| t.total_bytes).sum();
    println!("\n{:<14} {:>12} {:>11} {:>11} {:>12}", "table", "rows", "total", "indexes", "B/block");
    for t in &tables {
        println!(
            "{:<14} {:>12} {:>11} {:>11} {:>12.1}",
            t.name,
            num(t.rows as u64),
            mb(t.total_bytes),
            mb(t.index_bytes),
            t.total_bytes as f64 / blocks as f64
        );
    }
    let per_block = total as f64 / blocks as f64;
    println!("{:<14} {:>12} {:>11} {:>11} {:>12.1}", "all", "", mb(total), "", per_block);
    println!("≈ {:.2} GB per 1M blocks", per_block * 1e6 / 1e9);
    Ok(())
}

fn num(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn mb(bytes: i64) -> String {
    format!("{:.1} MB", bytes as f64 / 1e6)
}
