//! Postgres access. A batch is written with one `INSERT … SELECT FROM UNNEST(…)` per table inside
//! a single transaction; `ON CONFLICT DO NOTHING` makes re-indexing a range harmless.

use std::collections::HashMap;

use anyhow::{Context, Result, ensure};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

use crate::normalize::{Addr, Batch};

pub async fn connect(url: &str) -> Result<PgPool> {
    PgPoolOptions::new().max_connections(8).connect(url).await.context("connecting to Postgres")
}

pub async fn migrate(pool: &PgPool) -> Result<()> {
    sqlx::migrate!().run(pool).await.context("running migrations")
}

/// `(id, rpc_url)` of a network by name.
pub async fn network(pool: &PgPool, name: &str) -> Result<(i16, String)> {
    sqlx::query_as("SELECT id, rpc_url FROM networks WHERE name = $1")
        .bind(name)
        .fetch_optional(pool)
        .await?
        .with_context(|| format!("unknown network {name:?}"))
}

/// Ids for `keys`, in the same order, creating the missing ones. Runs outside the batch transaction
/// so concurrent writers hold the unique-index locks only briefly, and inserts in key order so
/// they never wait on each other in a cycle.
async fn address_ids(pool: &PgPool, keys: &[[u8; 32]]) -> Result<Vec<i64>> {
    let keys: Vec<&[u8]> = keys.iter().map(|k| k.as_slice()).collect();
    sqlx::query(
        "INSERT INTO addresses (address)
         SELECT k FROM UNNEST($1::bytea[]) k
         WHERE NOT EXISTS (SELECT 1 FROM addresses a WHERE a.address = k)
         ORDER BY k
         ON CONFLICT DO NOTHING",
    )
    .bind(&keys)
    .execute(pool)
    .await
    .context("inserting addresses")?;
    let rows: Vec<(i64, Vec<u8>)> = sqlx::query_as("SELECT id, address FROM addresses WHERE address = ANY($1)")
        .bind(&keys)
        .fetch_all(pool)
        .await
        .context("looking up address ids")?;
    let by_key: HashMap<Vec<u8>, i64> = rows.into_iter().map(|(id, k)| (k, id)).collect();
    keys.iter().map(|k| by_key.get(*k).copied().context("address missing right after insert")).collect()
}

pub async fn write(pool: &PgPool, net: i16, b: &Batch, cursor: Cursor) -> Result<()> {
    let ids = address_ids(pool, &b.addresses).await?;
    ensure!(ids.len() == b.addresses.len());
    let id = |v: &[Addr]| -> Vec<i64> { v.iter().map(|&a| ids[a as usize]).collect() };
    // `{1,2,3}` literals, cast to BIGINT[] per row in SQL (UNNEST can't yield arrays from a 2-D array).
    let id_arrays = |v: &[Vec<Addr>]| -> Vec<String> {
        v.iter()
            .map(|keys| {
                let ids: Vec<String> = keys.iter().map(|&a| ids[a as usize].to_string()).collect();
                format!("{{{}}}", ids.join(","))
            })
            .collect()
    };

    let mut tx = pool.begin().await?;

    sqlx::query(
        "INSERT INTO blocks (network, height, block_time_s, tx_count, sys_tx_count, fingerprint)
         SELECT $1, * FROM UNNEST($2::int8[], $3::int8[], $4::int4[], $5::int4[], $6::int8[])
         ON CONFLICT DO NOTHING",
    )
    .bind(net)
    .bind(&b.blocks.height)
    .bind(&b.blocks.time_s)
    .bind(&b.blocks.tx_count)
    .bind(&b.blocks.sys_tx_count)
    .bind(&b.blocks.fingerprint)
    .execute(&mut *tx)
    .await
    .context("inserting blocks")?;

    let t = &b.txs;
    sqlx::query(
        "INSERT INTO txs (network, height, idx, signature, fee, err, cu, valid_from_ms, fee_payer,
                          num_signers, num_ro_signed, num_ro_unsigned, account_keys)
         SELECT $1, h, i, s, f, e::jsonb, c, v, p, ns, nrs, nru, k::int8[]
         FROM UNNEST($2::int8[], $3::int4[], $4::bytea[], $5::int8[], $6::text[], $7::int8[], $8::int8[],
                     $9::int8[], $10::int2[], $11::int2[], $12::int2[], $13::text[])
              AS u(h, i, s, f, e, c, v, p, ns, nrs, nru, k)
         ON CONFLICT DO NOTHING",
    )
    .bind(net)
    .bind(&t.height)
    .bind(&t.idx)
    .bind(&t.signature)
    .bind(&t.fee)
    .bind(&t.err)
    .bind(&t.cu)
    .bind(&t.valid_from_ms)
    .bind(id(&t.fee_payer))
    .bind(&t.num_signers)
    .bind(&t.num_ro_signed)
    .bind(&t.num_ro_unsigned)
    .bind(id_arrays(&t.account_keys))
    .execute(&mut *tx)
    .await
    .context("inserting txs")?;

    let a = &b.accounts;
    sqlx::query(
        "INSERT INTO tx_accounts (network, address, height, idx, role)
         SELECT $1, * FROM UNNEST($2::int8[], $3::int8[], $4::int4[], $5::int2[])
         ON CONFLICT DO NOTHING",
    )
    .bind(net)
    .bind(id(&a.address))
    .bind(&a.height)
    .bind(&a.idx)
    .bind(&a.role)
    .execute(&mut *tx)
    .await
    .context("inserting tx_accounts")?;

    let i = &b.ixs;
    sqlx::query(
        "INSERT INTO instructions (network, height, idx, ix_idx, inner_idx, program, kind, data, accounts)
         SELECT $1, * FROM UNNEST($2::int8[], $3::int4[], $4::int2[], $5::int2[], $6::int8[], $7::text[],
                                  $8::bytea[], $9::bytea[])
         ON CONFLICT DO NOTHING",
    )
    .bind(net)
    .bind(&i.height)
    .bind(&i.idx)
    .bind(&i.ix_idx)
    .bind(&i.inner_idx)
    .bind(id(&i.program))
    .bind(&i.kind)
    .bind(&i.data)
    .bind(&i.accounts)
    .execute(&mut *tx)
    .await
    .context("inserting instructions")?;

    let t = &b.transfers;
    sqlx::query(
        "INSERT INTO transfers (network, height, idx, ix_idx, inner_idx, source, dest, mint, amount)
         SELECT $1, h, i, x, n, s, d, m, a::numeric
         FROM UNNEST($2::int8[], $3::int4[], $4::int2[], $5::int2[], $6::int8[], $7::int8[], $8::int8[], $9::text[])
              AS u(h, i, x, n, s, d, m, a)
         ON CONFLICT DO NOTHING",
    )
    .bind(net)
    .bind(&t.height)
    .bind(&t.idx)
    .bind(&t.ix_idx)
    .bind(&t.inner_idx)
    .bind(id(&t.source))
    .bind(id(&t.dest))
    .bind(t.mint.iter().map(|m| m.map(|a| ids[a as usize])).collect::<Vec<Option<i64>>>())
    .bind(&t.amount)
    .execute(&mut *tx)
    .await
    .context("inserting transfers")?;

    let l = &b.logs;
    sqlx::query(
        "INSERT INTO logs (network, height, idx, lines)
         SELECT $1, * FROM UNNEST($2::int8[], $3::int4[], $4::text[])
         ON CONFLICT DO NOTHING",
    )
    .bind(net)
    .bind(&l.height)
    .bind(&l.idx)
    .bind(&l.lines)
    .execute(&mut *tx)
    .await
    .context("inserting logs")?;

    for m in &b.missing {
        sqlx::query(
            "INSERT INTO missing_blocks (network, from_height, to_height, reason) VALUES ($1, $2, $3, $4)
             ON CONFLICT (network, from_height) DO UPDATE
             SET to_height = EXCLUDED.to_height, reason = EXCLUDED.reason, seen_at = now()",
        )
        .bind(net)
        .bind(m.from as i64)
        .bind(m.to as i64)
        .bind(&m.reason)
        .execute(&mut *tx)
        .await
        .context("inserting missing_blocks")?;
    }

    let advance = match cursor {
        Cursor::None => None,
        Cursor::Backfill => Some("UPDATE ingest_state SET backfill_next = $2, updated_at = now() WHERE network = $1"),
        Cursor::Tip => Some("UPDATE ingest_state SET tip_next = $2, tip_updated_at = now() WHERE network = $1"),
    };
    if let (Some(sql), Some(through)) = (advance, b.through) {
        sqlx::query(sql).bind(net).bind(through as i64 + 1).execute(&mut *tx).await.context("advancing cursor")?;
    }

    // New blocks at the head are announced to API processes (live stream); delivered on commit.
    if let (Cursor::Tip, Some(first), Some(last)) = (cursor, b.blocks.height.first(), b.blocks.height.last()) {
        sqlx::query("SELECT pg_notify($1, $2)")
            .bind(TIP_CHANNEL)
            .bind(format!("{net}:{first}:{last}"))
            .execute(&mut *tx)
            .await
            .context("notifying")?;
    }

    tx.commit().await?;
    Ok(())
}

/// Postgres channel carrying `network:first_height:last_height` for each batch the head follower commits.
pub const TIP_CHANNEL: &str = "kelvin_tip";

/// Which `ingest_state` cursor a committed batch advances.
#[derive(Clone, Copy)]
pub enum Cursor {
    None,
    Backfill,
    Tip,
}

/// Next heights to index. The backfill covers `backfill_from..tip_from`; the head follower
/// covers `tip_from..` once it has started.
pub struct Cursors {
    pub backfill_next: u64,
    pub tip_from: Option<u64>,
    pub tip_next: Option<u64>,
}

pub async fn cursors(pool: &PgPool, net: i16) -> Result<Option<Cursors>> {
    let row: Option<(i64, Option<i64>, Option<i64>)> =
        sqlx::query_as("SELECT backfill_next, tip_from, tip_next FROM ingest_state WHERE network = $1")
            .bind(net)
            .fetch_optional(pool)
            .await?;
    Ok(row.map(|(b, tf, tn)| Cursors {
        backfill_next: b as u64,
        tip_from: tf.map(|h| h as u64),
        tip_next: tn.map(|h| h as u64),
    }))
}

pub async fn start_backfill(pool: &PgPool, net: i16, from: u64) -> Result<()> {
    sqlx::query("INSERT INTO ingest_state (network, backfill_from, backfill_next) VALUES ($1, $2, $2)")
        .bind(net)
        .bind(from as i64)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn start_tip(pool: &PgPool, net: i16, from: u64) -> Result<()> {
    sqlx::query("UPDATE ingest_state SET tip_from = $2, tip_next = $2, tip_updated_at = now() WHERE network = $1")
        .bind(net)
        .bind(from as i64)
        .execute(pool)
        .await?;
    Ok(())
}

/// Moves the cursor past heights pruned before the backfill reached them, recording them as missing.
pub async fn skip_pruned(pool: &PgPool, net: i16, from: u64, to: u64) -> Result<()> {
    let mut tx = pool.begin().await?;
    sqlx::query(
        "INSERT INTO missing_blocks (network, from_height, to_height, reason) VALUES ($1, $2, $3, 'pruned')
         ON CONFLICT (network, from_height) DO UPDATE SET to_height = EXCLUDED.to_height, reason = 'pruned', seen_at = now()",
    )
    .bind(net)
    .bind(from as i64)
    .bind(to as i64)
    .execute(&mut *tx)
    .await?;
    sqlx::query("UPDATE ingest_state SET backfill_next = $2, updated_at = now() WHERE network = $1")
        .bind(net)
        .bind(to as i64 + 1)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

pub async fn record_retention(pool: &PgPool, net: i16, oldest: u64, head: u64) -> Result<()> {
    sqlx::query("INSERT INTO retention (network, oldest, head) VALUES ($1, $2, $3)")
        .bind(net)
        .bind(oldest as i64)
        .bind(head as i64)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn record_event(pool: &PgPool, net: i16, kind: &str, detail: &str) -> Result<()> {
    sqlx::query("INSERT INTO events (network, kind, detail) VALUES ($1, $2, $3)")
        .bind(net)
        .bind(kind)
        .bind(detail)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn fingerprint(pool: &PgPool, net: i16, height: u64) -> Result<Option<i64>> {
    let row: Option<(i64,)> = sqlx::query_as("SELECT fingerprint FROM blocks WHERE network = $1 AND height = $2")
        .bind(net)
        .bind(height as i64)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(|(f,)| f))
}

pub struct Status {
    pub backfill_from: i64,
    pub backfill_next: i64,
    pub updated: String,
    pub tip_from: Option<i64>,
    pub tip_next: Option<i64>,
    pub tip_updated: Option<String>,
    pub missing_ranges: i64,
    pub missing_heights: i64,
    /// `(when, oldest, head)`, oldest sample first.
    pub retention: Vec<(String, i64, i64)>,
    /// `(when, kind, detail)`, newest first.
    pub events: Vec<(String, String, String)>,
}

pub async fn status(pool: &PgPool, net: i16) -> Result<Option<Status>> {
    type Row = (i64, i64, String, Option<i64>, Option<i64>, Option<String>);
    let Some((backfill_from, backfill_next, updated, tip_from, tip_next, tip_updated)): Option<Row> = sqlx::query_as(
        "SELECT backfill_from, backfill_next, to_char(updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD HH24:MI:SS'),
                tip_from, tip_next, to_char(tip_updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD HH24:MI:SS')
         FROM ingest_state WHERE network = $1",
    )
    .bind(net)
    .fetch_optional(pool)
    .await?
    else {
        return Ok(None);
    };
    let (missing_ranges, missing_heights): (i64, i64) = sqlx::query_as(
        "SELECT count(*), coalesce(sum(to_height - from_height + 1), 0)::int8 FROM missing_blocks WHERE network = $1",
    )
    .bind(net)
    .fetch_one(pool)
    .await?;
    let retention = sqlx::query_as(
        "SELECT to_char(ts AT TIME ZONE 'UTC', 'YYYY-MM-DD HH24:MI'), oldest, head FROM retention
         WHERE network = $1 ORDER BY ts",
    )
    .bind(net)
    .fetch_all(pool)
    .await?;
    let events = sqlx::query_as(
        "SELECT to_char(ts AT TIME ZONE 'UTC', 'YYYY-MM-DD HH24:MI'), kind, detail FROM events
         WHERE network = $1 ORDER BY ts DESC LIMIT 5",
    )
    .bind(net)
    .fetch_all(pool)
    .await?;
    Ok(Some(Status {
        backfill_from,
        backfill_next,
        updated,
        tip_from,
        tip_next,
        tip_updated,
        missing_ranges,
        missing_heights,
        retention,
        events,
    }))
}

pub struct TableSize {
    pub name: String,
    pub rows: i64,
    pub total_bytes: i64,
    pub index_bytes: i64,
}

/// Exact row counts and on-disk sizes (heap + TOAST + indexes) of the indexer's tables.
pub async fn sizes(pool: &PgPool) -> Result<Vec<TableSize>> {
    let tables: Vec<(String, i64, i64)> = sqlx::query_as(
        "SELECT c.relname::text, pg_total_relation_size(c.oid), pg_indexes_size(c.oid)
         FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
         WHERE n.nspname = 'public' AND c.relkind = 'r' AND c.relname <> '_sqlx_migrations'
         ORDER BY 2 DESC",
    )
    .fetch_all(pool)
    .await?;
    let mut out = Vec::with_capacity(tables.len());
    for (name, total_bytes, index_bytes) in tables {
        // Table names come from pg_class, not user input.
        let (rows,): (i64,) = sqlx::query_as(&format!("SELECT count(*) FROM {name}")).fetch_one(pool).await?;
        out.push(TableSize { name, rows, total_bytes, index_bytes });
    }
    Ok(out)
}
