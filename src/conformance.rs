//! Hourly checks of how the Rialo RPC behaves where it is known to fall short. Each check records
//! whether the quirk is present (`issue`) and what was observed, so the API can show the current
//! state and since when it has held, and a change in the RPC shows up as a flip.

use anyhow::{Context, Result};
use serde_json::{Value, json};
use sqlx::PgPool;

use crate::rpc::Rpc;

pub struct Check {
    pub id: &'static str,
    pub title: &'static str,
    /// What a well-behaved RPC does.
    pub expected: &'static str,
}

pub const CHECKS: &[Check] = &[
    Check {
        id: "signatures_cap",
        title: "Address history beyond 20 entries",
        expected: "getSignaturesForAddress with limit 1000 returns more than 20 signatures for a busy address",
    },
    Check {
        id: "signatures_limit",
        title: "limit parameter",
        expected: "getSignaturesForAddress with limit 5 returns 5 signatures",
    },
    Check {
        id: "signatures_before",
        title: "Paging with before",
        expected: "getSignaturesForAddress with before = the last signature returns older signatures",
    },
    Check {
        id: "transaction_block_time",
        title: "Block time on transactions",
        expected: "getTransaction returns the block time",
    },
    Check {
        id: "time_units",
        title: "One time unit",
        expected: "getBlock and getSignaturesForAddress report a block's time in the same unit",
    },
    Check {
        id: "balance_changes",
        title: "Balance changes in transaction meta",
        expected: "getBlock transaction meta includes preBalances and postBalances",
    },
    Check { id: "history_retention", title: "Old blocks", expected: "getBlock serves every block since genesis" },
    Check {
        id: "transaction_count_monotonic",
        title: "Consistent transaction count",
        expected: "getTransactionCount never decreases across five calls in a row",
    },
    Check {
        id: "epoch_length",
        title: "Epoch length",
        expected: "getEpochInfo reports a finite number of slots per epoch",
    },
    Check {
        id: "rex_created_at",
        title: "REX request timestamps",
        expected: "getRexRequests reports createdAt as a plausible date",
    },
    Check { id: "cors", title: "Browser access (CORS)", expected: "responses carry Access-Control-Allow-Origin" },
    Check { id: "version", title: "RPC version", expected: "informational" },
];

/// Runs every check, stores the results and returns them as `(id, issue, observed)`. A check that
/// cannot run (RPC error, no suitable address yet) is logged and left out.
pub async fn run(pool: &PgPool, net: i16, rpc: &Rpc) -> Result<Vec<(&'static str, bool, String)>> {
    let mut results = Vec::new();
    let mut record = |id: &'static str, r: Result<(bool, String)>| match r {
        Ok((issue, observed)) => results.push((id, issue, observed)),
        Err(e) => tracing::warn!(check = id, "skipped: {e:#}"),
    };

    match busy_address(pool, net).await {
        Ok(Some((address, indexed))) => {
            let sigs = |extra: Value| {
                let mut p = json!({ "address": address });
                p.as_object_mut().expect("object").extend(extra.as_object().expect("object").clone());
                rpc.raw("getSignaturesForAddress", json!([p]))
            };
            match sigs(json!({ "limit": 1000 })).await.and_then(|v| items(&v)) {
                Ok(page1) => {
                    let n1 = page1.len();
                    record(
                        "signatures_cap",
                        Ok((
                            n1 <= 20 && indexed > 20,
                            format!(
                                "limit 1000 returned {n1} for {address}; Kelvin indexed {indexed} from it in the last 50,000 blocks alone"
                            ),
                        )),
                    );
                    let five = sigs(json!({ "limit": 5 })).await.and_then(|v| items(&v)).map(|p| p.len());
                    record("signatures_limit", five.map(|n| (n != 5, format!("limit 5 returned {n}"))));
                    if let (Some(first), Some(last)) = (page1.first(), page1.last()) {
                        let before = sigs(json!({ "limit": 1000, "before": last.signature })).await;
                        record(
                            "signatures_before",
                            before.and_then(|v| items(&v)).map(|page2| {
                                let max2 = page2.iter().map(|i| i.height).max();
                                // Honoured, page 2 continues strictly below page 1: no repeats, nothing newer.
                                let repeats = page2.iter().any(|i| page1.iter().any(|p| p.signature == i.signature));
                                let issue = repeats || max2.is_none_or(|m| m > last.height);
                                let seen = match max2 {
                                    Some(m) => format!("newest {m}"),
                                    None => "nothing".into(),
                                };
                                (issue, format!("page 1 reached block {}; page 2 started at {seen}", last.height))
                            }),
                        );
                        record("transaction_block_time", transaction_block_time(rpc, &first.signature).await);
                        record("time_units", time_units(rpc, first).await);
                    }
                }
                Err(e) => tracing::warn!("signature checks skipped: {e:#}"),
            }
        }
        Ok(None) => tracing::info!("signature checks skipped: no address with 25+ recent transactions indexed yet"),
        Err(e) => tracing::warn!("signature checks skipped: {e:#}"),
    }

    record("balance_changes", balance_changes(rpc).await);
    record("history_retention", history_retention(pool, net).await);
    record("transaction_count_monotonic", transaction_count(rpc).await);
    record("epoch_length", epoch_length(rpc).await);
    record("rex_created_at", rex_created_at(pool, net, rpc).await);
    record(
        "cors",
        rpc.cors_allow_origin().await.map(|h| match h {
            Some(v) => (false, format!("Access-Control-Allow-Origin: {v}")),
            None => (true, "no Access-Control-Allow-Origin header".into()),
        }),
    );
    record("version", version(rpc).await);

    for (id, issue, observed) in &results {
        sqlx::query("INSERT INTO rpc_conformance (network, check_id, issue, observed) VALUES ($1, $2, $3, $4)")
            .bind(net)
            .bind(id)
            .bind(issue)
            .bind(observed)
            .execute(pool)
            .await?;
    }
    Ok(results)
}

struct Item {
    signature: String,
    height: u64,
    /// As the RPC reports it (milliseconds, unlike getBlock).
    time: Option<i64>,
}

fn items(v: &Value) -> Result<Vec<Item>> {
    let list = v.get("value").and_then(Value::as_array).context("getSignaturesForAddress: no value array")?;
    list.iter()
        .map(|i| {
            Ok(Item {
                signature: i["signature"].as_str().context("signature")?.to_owned(),
                height: i["blockHeight"].as_u64().context("blockHeight")?,
                time: i["blockTime"].as_i64(),
            })
        })
        .collect()
}

/// An address that signed at least 25 transactions in the last ~50,000 indexed blocks, with its
/// count in Kelvin's index.
async fn busy_address(pool: &PgPool, net: i16) -> Result<Option<(String, i64)>> {
    let row: Option<(Vec<u8>, i64)> = sqlx::query_as(
        "SELECT a.address, count(*) FROM tx_accounts ta JOIN addresses a ON a.id = ta.address
         WHERE ta.network = $1 AND ta.role = 0
           AND ta.height >= (SELECT coalesce(tip_next, backfill_next) - 50000 FROM ingest_state WHERE network = $1)
         GROUP BY a.address HAVING count(*) >= 25 ORDER BY count(*) DESC LIMIT 1",
    )
    .bind(net)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(a, n)| (bs58::encode(a).into_string(), n)))
}

async fn transaction_block_time(rpc: &Rpc, signature: &str) -> Result<(bool, String)> {
    let v = rpc.raw("getTransaction", json!([{ "signature": signature }])).await?;
    let t = v.get("block_time").or_else(|| v.get("blockTime")).cloned().unwrap_or(Value::Null);
    Ok((t.is_null(), format!("block_time: {t}")))
}

async fn time_units(rpc: &Rpc, item: &Item) -> Result<(bool, String)> {
    let listed = item.time.context("no blockTime in getSignaturesForAddress")?;
    let block = rpc.block(item.height).await?.context("block not found")?;
    let ratio = listed as f64 / block.block_time.max(1) as f64;
    Ok((
        !(0.5..2.0).contains(&ratio),
        format!("block {}: getBlock {} vs getSignaturesForAddress {}", item.height, block.block_time, listed),
    ))
}

async fn balance_changes(rpc: &Rpc) -> Result<(bool, String)> {
    let head = rpc.block_height().await?;
    for h in (head.saturating_sub(40)..head.saturating_sub(5)).rev() {
        let v = rpc.raw("getBlock", json!([{ "blockHeight": h }])).await?;
        if let Some(meta) = v["value"]["transactions"].get(0).and_then(|t| t.get("meta")).and_then(Value::as_object) {
            let present = meta.contains_key("preBalances") && meta.contains_key("postBalances");
            let keys: Vec<&str> = meta.keys().map(String::as_str).collect();
            return Ok((!present, format!("block {h} meta fields: {}", keys.join(", "))));
        }
    }
    anyhow::bail!("no transaction found in recent blocks")
}

async fn history_retention(pool: &PgPool, net: i16) -> Result<(bool, String)> {
    let (oldest,): (i64,) = sqlx::query_as("SELECT oldest FROM retention WHERE network = $1 ORDER BY ts DESC LIMIT 1")
        .bind(net)
        .fetch_optional(pool)
        .await?
        .context("no retention sample yet")?;
    Ok((oldest > 1, format!("oldest block served: {oldest}")))
}

async fn transaction_count(rpc: &Rpc) -> Result<(bool, String)> {
    let mut seen = Vec::new();
    for _ in 0..5 {
        let v = rpc.raw("getTransactionCount", json!([])).await?;
        seen.push(v["value"].as_u64().or_else(|| v.as_u64()).context("getTransactionCount: no number")?);
    }
    let backwards = seen.windows(2).any(|w| w[1] < w[0]);
    let list: Vec<String> = seen.iter().map(u64::to_string).collect();
    Ok((backwards, list.join(", ")))
}

async fn epoch_length(rpc: &Rpc) -> Result<(bool, String)> {
    let v = rpc.raw("getEpochInfo", json!([])).await?;
    let slots = v["slotsInEpoch"].as_u64().context("getEpochInfo: no slotsInEpoch")?;
    Ok((slots == u64::MAX, format!("epoch {}, slotsInEpoch {slots}", v["epoch"])))
}

/// Uses recent oracle-registry callers, since those create REX requests: the first of the five
/// most frequent that has any.
async fn rex_created_at(pool: &PgPool, net: i16, rpc: &Rpc) -> Result<(bool, String)> {
    let registry = bs58::decode(crate::decode::ORACLE_REGISTRY).into_vec()?;
    let callers: Vec<(Vec<u8>,)> = sqlx::query_as(
        "SELECT a.address FROM instructions i
         JOIN txs t ON t.network = i.network AND t.height = i.height AND t.idx = i.idx
         JOIN addresses a ON a.id = t.fee_payer
         WHERE i.network = $1 AND i.program = (SELECT id FROM addresses WHERE address = $2) AND i.inner_idx = -1
           AND i.height >= (SELECT coalesce(tip_next, backfill_next) - 50000 FROM ingest_state WHERE network = $1)
         GROUP BY a.address ORDER BY count(*) DESC LIMIT 5",
    )
    .bind(net)
    .bind(&registry)
    .fetch_all(pool)
    .await?;
    for (creator,) in callers {
        let creator = bs58::encode(creator).into_string();
        let v = rpc.raw("getRexRequests", json!([{ "creator": creator }])).await?;
        let Some(first) = v["rex_requests"].get(0) else { continue };
        let created = first["createdAt"].as_str().unwrap_or_default().to_owned();
        // "+58609-02-20 11:51:57.000 UTC": a year far beyond today suggests milliseconds read as seconds.
        let year: i64 = created.trim_start_matches('+').split('-').next().and_then(|y| y.parse().ok()).unwrap_or(0);
        return Ok((!(2020..=2100).contains(&year), format!("createdAt {created} for {creator}")));
    }
    anyhow::bail!("no recent oracle-registry caller with REX requests")
}

async fn version(rpc: &Rpc) -> Result<(bool, String)> {
    let commit = rpc.raw("getVersion", json!([])).await?;
    let ctx = rpc.raw("getTransactionCount", json!([])).await?;
    let api = ctx["context"]["api_version"].as_str().unwrap_or("unknown").to_owned();
    Ok((false, format!("{api} ({})", commit.as_str().unwrap_or("?").get(..8).unwrap_or("?"))))
}
