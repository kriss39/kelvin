//! Re-runs the decoders over stored instructions that have no kind yet. Instruction data and
//! accounts are kept raw, so a program that gains a decoder is filled in from the database
//! (kinds and transfers) without fetching any block again.

use anyhow::Result;
use sqlx::PgPool;

use crate::decode::{self, DECODABLE};

const CHUNK: i64 = 5_000;

pub struct Outcome {
    pub scanned: u64,
    pub decoded: u64,
    pub transfers: u64,
}

pub async fn run(pool: &PgPool, net: i16) -> Result<Outcome> {
    let keys: Vec<Vec<u8>> = DECODABLE.iter().map(|p| bs58::decode(p).into_vec()).collect::<Result<_, _>>()?;
    let programs: Vec<(i64, Vec<u8>)> =
        sqlx::query_as("SELECT id, address FROM addresses WHERE address = ANY($1)").bind(&keys).fetch_all(pool).await?;
    let ids: Vec<i64> = programs.iter().map(|p| p.0).collect();
    let name = |id: i64| programs.iter().find(|p| p.0 == id).map(|p| bs58::encode(&p.1).into_string());

    let mut out = Outcome { scanned: 0, decoded: 0, transfers: 0 };
    let mut after: (i64, i32, i16, i16) = (-1, -1, -1, -2);
    loop {
        type Row = (i64, i32, i16, i16, i64, Vec<u8>, Vec<u8>, bool, Vec<i64>);
        let rows: Vec<Row> = sqlx::query_as(
            "SELECT i.height, i.idx, i.ix_idx, i.inner_idx, i.program, i.data, i.accounts, t.err IS NULL, t.account_keys
             FROM instructions i JOIN txs t ON t.network = i.network AND t.height = i.height AND t.idx = i.idx
             WHERE i.network = $1 AND i.kind IS NULL AND i.program = ANY($2)
               AND (i.height, i.idx, i.ix_idx, i.inner_idx) > ($3, $4, $5, $6)
             ORDER BY i.height, i.idx, i.ix_idx, i.inner_idx
             LIMIT $7",
        )
        .bind(net)
        .bind(&ids)
        .bind(after.0)
        .bind(after.1)
        .bind(after.2)
        .bind(after.3)
        .bind(CHUNK)
        .fetch_all(pool)
        .await?;
        let Some(last) = rows.last() else { break };
        after = (last.0, last.1, last.2, last.3);
        out.scanned += rows.len() as u64;

        // Columns for the kind update and for new transfers.
        let (mut kh, mut ki, mut kx, mut kn, mut kinds) = (vec![], vec![], vec![], vec![], vec![]);
        let (mut th, mut ti, mut tx, mut tn, mut ts, mut td, mut tm, mut ta) =
            (vec![], vec![], vec![], vec![], vec![], vec![], vec![], vec![]);
        for (height, idx, ix, inner, program, data, accounts, ok, account_keys) in &rows {
            let Some(d) = name(*program).and_then(|p| decode::decode(&p, data)) else { continue };
            kh.push(*height);
            ki.push(*idx);
            kx.push(*ix);
            kn.push(*inner);
            kinds.push(d.kind);
            let Some(t) = d.transfer.filter(|_| *ok) else { continue };
            let key = |pos: usize| accounts.get(pos).and_then(|&a| account_keys.get(a as usize).copied());
            let (Some(source), Some(dest)) = (key(t.source), key(t.dest)) else { continue };
            let mint = match t.mint {
                Some(pos) => match key(pos) {
                    Some(m) => Some(m),
                    None => continue,
                },
                None => None,
            };
            th.push(*height);
            ti.push(*idx);
            tx.push(*ix);
            tn.push(*inner);
            ts.push(source);
            td.push(dest);
            tm.push(mint);
            ta.push(t.amount.to_string());
        }

        let mut db = pool.begin().await?;
        sqlx::query(
            "UPDATE instructions i SET kind = u.k
             FROM UNNEST($2::int8[], $3::int4[], $4::int2[], $5::int2[], $6::text[]) AS u(h, i, x, n, k)
             WHERE i.network = $1 AND i.height = u.h AND i.idx = u.i AND i.ix_idx = u.x AND i.inner_idx = u.n",
        )
        .bind(net)
        .bind(&kh)
        .bind(&ki)
        .bind(&kx)
        .bind(&kn)
        .bind(&kinds)
        .execute(&mut *db)
        .await?;
        let inserted = sqlx::query(
            "INSERT INTO transfers (network, height, idx, ix_idx, inner_idx, source, dest, mint, amount)
             SELECT $1, h, i, x, n, s, d, m, a::numeric
             FROM UNNEST($2::int8[], $3::int4[], $4::int2[], $5::int2[], $6::int8[], $7::int8[], $8::int8[], $9::text[])
                  AS u(h, i, x, n, s, d, m, a)
             ON CONFLICT DO NOTHING",
        )
        .bind(net)
        .bind(&th)
        .bind(&ti)
        .bind(&tx)
        .bind(&tn)
        .bind(&ts)
        .bind(&td)
        .bind(&tm)
        .bind(&ta)
        .execute(&mut *db)
        .await?
        .rows_affected();
        db.commit().await?;
        out.decoded += kinds.len() as u64;
        out.transfers += inserted;
        tracing::info!(
            height = after.0,
            scanned = out.scanned,
            decoded = out.decoded,
            transfers = out.transfers,
            "redecoding"
        );
    }
    Ok(out)
}
