//! Public read-only HTTP API under `/v1/{network}/…`.
//! Amounts are decimal strings in kelvin (1 RLO = 10^9 kelvin) because they exceed 2^53; times
//! are ISO 8601 UTC plus milliseconds since the epoch. Lists page with an opaque `next_cursor`.

use std::collections::{HashMap, VecDeque};
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::Result;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use futures::{Stream, StreamExt, stream};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgPool;
use sqlx::postgres::PgListener;
use tokio::sync::{broadcast, watch};
use tower_http::compression::CompressionLayer;
use tower_http::cors::{Any, CorsLayer};
use tower_http::timeout::TimeoutLayer;

use crate::conformance;
use crate::db;
use crate::decode;
use crate::model::Header;
use crate::normalize;
use crate::rpc::Rpc;

const DEFAULT_LIMIT: i64 = 25;
const MAX_LIMIT: i64 = 100;
const HEAD_REFRESH: Duration = Duration::from_secs(2);
const BALANCE_TIMEOUT: Duration = Duration::from_secs(3);
/// Indexed blocks and transactions never change, so caches may keep them for a day.
const FINAL: u32 = 86_400;
/// Lists grow at the head.
const FRESH: u32 = 5;

const OPENAPI: &str = include_str!("openapi.yaml");
const DOCS: &str = include_str!("docs.html");
const EXPLORER: &str = include_str!("web/index.html");
const MARK: &str = include_str!("../brand/kelvin-mark.svg");
const OG_IMAGE: &[u8] = include_bytes!("../brand/og.png");
/// Requests that take longer get 503; streams are unaffected once their headers are sent.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

struct Network {
    id: i16,
    rpc: Rpc,
    /// Latest head seen by a background poller, so API calls never wait on the RPC for it.
    head: AtomicU64,
    /// Newly indexed head blocks, for `/stream` subscribers.
    updates: broadcast::Sender<Arc<Update>>,
}

#[derive(Clone)]
struct AppState {
    pool: PgPool,
    networks: Arc<HashMap<String, Arc<Network>>>,
    /// Ends open streams on shutdown; graceful shutdown would otherwise wait on them forever.
    stop: watch::Receiver<bool>,
}

pub async fn serve(pool: PgPool, addr: SocketAddr, mut stop: watch::Receiver<bool>) -> Result<()> {
    let rows: Vec<(i16, String, String)> =
        sqlx::query_as("SELECT id, name, rpc_url FROM networks").fetch_all(&pool).await?;
    let mut networks = HashMap::new();
    for (id, name, url) in rows {
        let n =
            Arc::new(Network { id, rpc: Rpc::new(&url)?, head: AtomicU64::new(0), updates: broadcast::channel(256).0 });
        tokio::spawn({
            let n = n.clone();
            async move {
                loop {
                    if let Ok(h) = n.rpc.block_height().await {
                        n.head.store(h, Ordering::Relaxed);
                    }
                    tokio::time::sleep(HEAD_REFRESH).await;
                }
            }
        });
        networks.insert(name, n);
    }
    let networks = Arc::new(networks);
    tokio::spawn(relay_updates(pool.clone(), networks.clone()));
    let state = AppState { pool, networks, stop: stop.clone() };

    let app = Router::new()
        .route("/", get(explorer))
        .route("/healthz", get(healthz))
        .route(
            "/favicon.svg",
            get(|| async {
                ([(header::CONTENT_TYPE, "image/svg+xml"), (header::CACHE_CONTROL, "public, max-age=86400")], MARK)
            }),
        )
        .route(
            "/og.png",
            get(|| async {
                ([(header::CONTENT_TYPE, "image/png"), (header::CACHE_CONTROL, "public, max-age=86400")], OG_IMAGE)
            }),
        )
        .route("/docs", get(|| async { Html(DOCS) }))
        .route("/openapi.yaml", get(|| async { ([(header::CONTENT_TYPE, "application/yaml")], OPENAPI) }))
        .route("/v1/{net}/status", get(status))
        .route("/v1/{net}/blocks", get(blocks))
        .route("/v1/{net}/blocks/{height}", get(block))
        .route("/v1/{net}/tx/{signature}", get(tx))
        .route("/v1/{net}/address/{address}", get(address_summary))
        .route("/v1/{net}/address/{address}/txs", get(address_txs))
        .route("/v1/{net}/address/{address}/transfers", get(address_transfers))
        .route("/v1/{net}/programs/{program}/txs", get(program_txs))
        .route("/v1/{net}/stream", get(live))
        .route("/v1/{net}/rpc-conformance", get(rpc_conformance))
        .layer(TimeoutLayer::with_status_code(StatusCode::SERVICE_UNAVAILABLE, REQUEST_TIMEOUT))
        .layer(CorsLayer::new().allow_origin(Any).allow_methods([Method::GET]))
        .layer(CompressionLayer::new())
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "API listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let _ = stop.wait_for(|s| *s).await;
        })
        .await?;
    Ok(())
}

// ---------- errors and responses ----------

enum ApiError {
    BadRequest(String),
    NotFound(String),
    Internal(anyhow::Error),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadRequest(m) | Self::NotFound(m) => f.write_str(m),
            Self::Internal(e) => write!(f, "{e:#}"),
        }
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        Self::Internal(e.into())
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        Self::Internal(e)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            Self::BadRequest(m) => (StatusCode::BAD_REQUEST, m),
            Self::NotFound(m) => (StatusCode::NOT_FOUND, m),
            Self::Internal(e) => {
                tracing::error!("api: {e:#}");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal error".to_owned())
            }
        };
        (status, Json(json!({ "error": message }))).into_response()
    }
}

type ApiResult = Result<Response, ApiError>;

fn cached<T: Serialize>(max_age: u32, body: T) -> Response {
    let mut r = Json(body).into_response();
    let value = HeaderValue::from_str(&format!("public, max-age={max_age}")).expect("ascii header");
    r.headers_mut().insert(header::CACHE_CONTROL, value);
    r
}

#[derive(Serialize)]
struct List<T> {
    items: Vec<T>,
    /// Pass as `cursor` to get the next page; `null` on the last page.
    next_cursor: Option<String>,
}

// ---------- parameters ----------

fn network(st: &AppState, name: &str) -> Result<Arc<Network>, ApiError> {
    st.networks
        .get(name)
        .cloned()
        .ok_or_else(|| ApiError::NotFound(format!("unknown network {name:?}; use testnet or devnet")))
}

fn parse_key<const N: usize>(s: &str, what: &str) -> Result<[u8; N], ApiError> {
    let bytes = bs58::decode(s).into_vec().map_err(|_| ApiError::BadRequest(format!("{what} is not base58")))?;
    bytes.try_into().map_err(|_| ApiError::BadRequest(format!("{what} must decode to {N} bytes")))
}

async fn address_id(pool: &PgPool, key: &[u8]) -> Result<Option<i64>, ApiError> {
    let row: Option<(i64,)> =
        sqlx::query_as("SELECT id FROM addresses WHERE address = $1").bind(key).fetch_optional(pool).await?;
    Ok(row.map(|(id,)| id))
}

#[derive(Deserialize)]
struct PageQuery {
    limit: Option<i64>,
    cursor: Option<String>,
    /// `desc` (newest first, default) or `asc`.
    order: Option<String>,
}

struct Page {
    limit: i64,
    asc: bool,
    /// Position to continue after, as numbers parsed from the cursor.
    after: Vec<i64>,
}

impl Page {
    fn parse(q: &PageQuery, parts: usize) -> Result<Self, ApiError> {
        let limit = q.limit.unwrap_or(DEFAULT_LIMIT);
        if !(1..=MAX_LIMIT).contains(&limit) {
            return Err(ApiError::BadRequest(format!("limit must be between 1 and {MAX_LIMIT}")));
        }
        let asc = match q.order.as_deref() {
            None | Some("desc") => false,
            Some("asc") => true,
            Some(o) => return Err(ApiError::BadRequest(format!("order must be asc or desc, not {o:?}"))),
        };
        let after = match &q.cursor {
            None => vec![if asc { -1 } else { i64::MAX }; parts],
            Some(c) => {
                let nums: Vec<i64> =
                    c.split(':').map(str::parse).collect::<Result<_, _>>().map_err(|_| bad_cursor())?;
                if nums.len() != parts {
                    return Err(bad_cursor());
                }
                nums
            }
        };
        Ok(Self { limit, asc, after })
    }

    /// `(comparison, direction)` for keyset SQL.
    fn sql(&self) -> (&'static str, &'static str) {
        if self.asc { (">", "ASC") } else { ("<", "DESC") }
    }

    /// Cuts the extra row fetched to detect another page and returns the cursor for it.
    fn finish<T>(&self, mut rows: Vec<T>, key: impl Fn(&T) -> String) -> (Vec<T>, Option<String>) {
        if rows.len() as i64 > self.limit {
            rows.truncate(self.limit as usize);
            let next = rows.last().map(key);
            (rows, next)
        } else {
            (rows, None)
        }
    }
}

/// Cursor parts are parsed as i64; narrower key columns take them clamped, so the default
/// "from the very start" cursor (`i64::MAX` or `-1`) stays at the far end of their range.
fn as_i32(v: i64) -> i32 {
    v.clamp(i32::MIN.into(), i32::MAX.into()) as i32
}

fn as_i16(v: i64) -> i16 {
    v.clamp(i16::MIN.into(), i16::MAX.into()) as i16
}

fn bad_cursor() -> ApiError {
    ApiError::BadRequest("cursor is malformed; pass next_cursor from the previous page unchanged".into())
}

fn b58(bytes: &[u8]) -> String {
    bs58::encode(bytes).into_string()
}

/// ISO 8601 UTC for a Unix time in seconds.
fn iso8601(secs: i64) -> String {
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3_600, rem % 3_600 / 60, rem % 60)
}

// ---------- shared output types ----------

#[derive(Serialize)]
struct Role {
    signer: bool,
    writable: bool,
}

impl From<i16> for Role {
    fn from(r: i16) -> Self {
        Self { signer: r <= 1, writable: r == 0 || r == 2 }
    }
}

#[derive(Serialize)]
struct InstructionBrief {
    program: String,
    program_name: Option<&'static str>,
    /// Decoded instruction type, e.g. `transfer`; `null` when Kelvin has no decoder for it.
    kind: Option<String>,
}

#[derive(Serialize)]
struct Transfer {
    from: String,
    to: String,
    amount: String,
    /// `null` for native RLO.
    mint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    instruction: Option<i16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    inner_instruction: Option<i16>,
}

#[derive(Serialize)]
struct TxSummary {
    signature: String,
    block: i64,
    index: i32,
    time: String,
    time_ms: i64,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<Value>,
    fee: String,
    /// How the queried address appears in the transaction (address lists only).
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<Role>,
    /// Top-level instructions.
    instructions: Vec<InstructionBrief>,
    /// RLO movements; in address lists only those involving the address.
    transfers: Vec<Transfer>,
}

type TxKey = (i64, i32);

/// Summaries for `keys`, in the same order. With `focus`, transfers are limited to that address id.
async fn summaries(pool: &PgPool, net: i16, keys: &[TxKey], focus: Option<i64>) -> Result<Vec<TxSummary>, ApiError> {
    let heights: Vec<i64> = keys.iter().map(|k| k.0).collect();
    let idxs: Vec<i32> = keys.iter().map(|k| k.1).collect();

    type TxRow = (i64, i32, Vec<u8>, i64, Option<Value>, i64);
    let txs: Vec<TxRow> = sqlx::query_as(
        "SELECT t.height, t.idx, t.signature, t.fee, t.err, b.block_time_s
         FROM UNNEST($2::int8[], $3::int4[]) AS k(height, idx)
         JOIN txs t ON t.network = $1 AND t.height = k.height AND t.idx = k.idx
         JOIN blocks b ON b.network = $1 AND b.height = k.height",
    )
    .bind(net)
    .bind(&heights)
    .bind(&idxs)
    .fetch_all(pool)
    .await?;

    let ixs: Vec<(i64, i32, Option<String>, Vec<u8>)> = sqlx::query_as(
        "SELECT i.height, i.idx, i.kind, a.address
         FROM UNNEST($2::int8[], $3::int4[]) AS k(height, idx)
         JOIN instructions i ON i.network = $1 AND i.height = k.height AND i.idx = k.idx AND i.inner_idx = -1
         JOIN addresses a ON a.id = i.program
         ORDER BY i.height, i.idx, i.ix_idx",
    )
    .bind(net)
    .bind(&heights)
    .bind(&idxs)
    .fetch_all(pool)
    .await?;

    type TransferRow = (i64, i32, Vec<u8>, Vec<u8>, String, Option<Vec<u8>>);
    let transfers: Vec<TransferRow> = sqlx::query_as(
        "SELECT tr.height, tr.idx, s.address, d.address, tr.amount::text, m.address
         FROM UNNEST($2::int8[], $3::int4[]) AS k(height, idx)
         JOIN transfers tr ON tr.network = $1 AND tr.height = k.height AND tr.idx = k.idx
         JOIN addresses s ON s.id = tr.source
         JOIN addresses d ON d.id = tr.dest
         LEFT JOIN addresses m ON m.id = tr.mint
         WHERE $4::int8 IS NULL OR tr.source = $4 OR tr.dest = $4
         ORDER BY tr.height, tr.idx, tr.ix_idx, tr.inner_idx",
    )
    .bind(net)
    .bind(&heights)
    .bind(&idxs)
    .bind(focus)
    .fetch_all(pool)
    .await?;

    let mut by_key: HashMap<TxKey, TxSummary> = HashMap::with_capacity(txs.len());
    for (height, idx, sig, fee, err, time_s) in txs {
        by_key.insert(
            (height, idx),
            TxSummary {
                signature: b58(&sig),
                block: height,
                index: idx,
                time: iso8601(time_s),
                time_ms: time_s * 1_000,
                status: if err.is_some() { "failed" } else { "success" },
                error: err,
                fee: fee.to_string(),
                role: None,
                instructions: Vec::new(),
                transfers: Vec::new(),
            },
        );
    }
    for (height, idx, kind, program) in ixs {
        if let Some(t) = by_key.get_mut(&(height, idx)) {
            let program = b58(&program);
            t.instructions.push(InstructionBrief { program_name: decode::label(&program), program, kind });
        }
    }
    for (height, idx, from, to, amount, mint) in transfers {
        if let Some(t) = by_key.get_mut(&(height, idx)) {
            t.transfers.push(Transfer {
                from: b58(&from),
                to: b58(&to),
                amount,
                mint: mint.as_deref().map(b58),
                instruction: None,
                inner_instruction: None,
            });
        }
    }
    Ok(keys.iter().filter_map(|k| by_key.remove(k)).collect())
}

// ---------- handlers ----------

async fn status(State(st): State<AppState>, Path(net): Path<String>) -> ApiResult {
    let n = network(&st, &net)?;
    let head = n.head.load(Ordering::Relaxed);
    let Some(s) = db::status(&st.pool, n.id).await? else {
        return Ok(cached(FRESH, json!({ "network": net, "indexing": false, "head": head })));
    };
    let indexed_tip = s.tip_next.map(|t| t - 1);
    let until = s.tip_from.map(|t| t - 1);
    let end = s.tip_from.unwrap_or(head as i64);
    let progress = (s.backfill_next - s.backfill_from) as f64 / (end - s.backfill_from).max(1) as f64;
    Ok(cached(
        FRESH,
        json!({
            "network": net,
            "indexing": true,
            "head": head,
            "indexed_tip": indexed_tip,
            "lag_blocks": indexed_tip.map(|t| (head as i64 - t).max(0)),
            "backfill": {
                "from": s.backfill_from,
                "next": s.backfill_next,
                "until": until,
                "complete": until.is_some_and(|u| s.backfill_next > u),
                "progress": progress.min(1.0),
            },
            "oldest_block_served_by_rpc": s.retention.last().map(|r| r.1),
            "missing_blocks": s.missing_heights,
            "totals": totals(&st.pool).await?,
        }),
    ))
}

/// Row estimates from the planner's statistics: instant at any size, refreshed by autovacuum.
/// Counted over all networks; `null` until a table has been analyzed.
async fn totals(pool: &PgPool) -> Result<Value, ApiError> {
    let rows: Vec<(String, i64)> = sqlx::query_as(
        "SELECT relname::text, reltuples::int8 FROM pg_class
         WHERE relkind = 'r' AND relname IN ('blocks', 'txs', 'addresses', 'transfers')",
    )
    .fetch_all(pool)
    .await?;
    let get = |t: &str| rows.iter().find(|r| r.0 == t).map(|r| r.1).filter(|&n| n >= 0);
    Ok(json!({
        "blocks": get("blocks"),
        "transactions": get("txs"),
        "addresses": get("addresses"),
        "transfers": get("transfers"),
    }))
}

/// 200 with the follower's lag when the database answers, 503 otherwise.
async fn healthz(State(st): State<AppState>) -> Response {
    let lag: Result<Vec<(String, Option<i64>)>, _> =
        sqlx::query_as("SELECT n.name, s.tip_next FROM networks n LEFT JOIN ingest_state s ON s.network = n.id")
            .fetch_all(&st.pool)
            .await;
    match lag {
        Ok(rows) => {
            let nets: serde_json::Map<String, Value> = rows
                .into_iter()
                .filter_map(|(name, tip)| {
                    let n = st.networks.get(&name)?;
                    let head = n.head.load(Ordering::Relaxed) as i64;
                    Some((name, json!({ "lag_blocks": tip.map(|t| (head - (t - 1)).max(0)) })))
                })
                .collect();
            let mut r = Json(json!({ "ok": true, "networks": nets })).into_response();
            r.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            r
        }
        Err(e) => {
            tracing::warn!("healthz: {e:#}");
            (StatusCode::SERVICE_UNAVAILABLE, Json(json!({ "ok": false }))).into_response()
        }
    }
}

/// The explorer page, with its link-preview URLs made absolute for the host it is served on.
async fn explorer(headers: axum::http::HeaderMap) -> Html<String> {
    let host = headers.get(header::HOST).and_then(|h| h.to_str().ok()).unwrap_or("localhost");
    let local = host.starts_with("localhost") || host.starts_with("127.") || host.starts_with("[::1]");
    let origin = format!("{}://{host}", if local { "http" } else { "https" });
    Html(EXPLORER.replace("__ORIGIN__", &origin))
}

#[derive(Serialize)]
struct BlockBrief {
    height: i64,
    time: String,
    time_ms: i64,
    /// All transactions in the block, including system ones.
    transaction_count: i32,
    system_transaction_count: i32,
}

async fn blocks(State(st): State<AppState>, Path(net): Path<String>, Query(q): Query<PageQuery>) -> ApiResult {
    let n = network(&st, &net)?;
    let p = Page::parse(&q, 1)?;
    let (cmp, dir) = p.sql();
    let rows: Vec<(i64, i64, i32, i32)> = sqlx::query_as(&format!(
        "SELECT height, block_time_s, tx_count, sys_tx_count FROM blocks
         WHERE network = $1 AND height {cmp} $2 ORDER BY height {dir} LIMIT $3"
    ))
    .bind(n.id)
    .bind(p.after[0])
    .bind(p.limit + 1)
    .fetch_all(&st.pool)
    .await?;
    let (rows, next) = p.finish(rows, |r| r.0.to_string());
    let items = rows
        .into_iter()
        .map(|(height, t, tx_count, sys)| BlockBrief {
            height,
            time: iso8601(t),
            time_ms: t * 1_000,
            transaction_count: tx_count,
            system_transaction_count: sys,
        })
        .collect();
    Ok(cached(FRESH, List { items, next_cursor: next }))
}

async fn not_indexed(pool: &PgPool, net: i16, height: i64) -> Result<ApiError, ApiError> {
    let missing: Option<(String,)> = sqlx::query_as(
        "SELECT reason FROM missing_blocks WHERE network = $1 AND from_height <= $2 AND to_height >= $2",
    )
    .bind(net)
    .bind(height)
    .fetch_optional(pool)
    .await?;
    if let Some((reason,)) = missing {
        return Ok(ApiError::NotFound(format!("block {height} could not be indexed ({reason})")));
    }
    let start: Option<(i64,)> = sqlx::query_as("SELECT backfill_from FROM ingest_state WHERE network = $1")
        .bind(net)
        .fetch_optional(pool)
        .await?;
    Ok(ApiError::NotFound(match start {
        Some((from,)) if height < from => {
            format!("block {height} was pruned by the RPC before Kelvin started; the oldest available block is {from}")
        }
        _ => format!("block {height} is not indexed yet"),
    }))
}

async fn block(State(st): State<AppState>, Path((net, height)): Path<(String, i64)>) -> ApiResult {
    let n = network(&st, &net)?;
    let row: Option<(i64, i32, i32)> =
        sqlx::query_as("SELECT block_time_s, tx_count, sys_tx_count FROM blocks WHERE network = $1 AND height = $2")
            .bind(n.id)
            .bind(height)
            .fetch_optional(&st.pool)
            .await?;
    let Some((t, tx_count, sys)) = row else {
        return Err(not_indexed(&st.pool, n.id, height).await?);
    };
    let keys: Vec<(i64, i32)> =
        sqlx::query_as("SELECT height, idx FROM txs WHERE network = $1 AND height = $2 ORDER BY idx")
            .bind(n.id)
            .bind(height)
            .fetch_all(&st.pool)
            .await?;
    let txs = summaries(&st.pool, n.id, &keys, None).await?;
    Ok(cached(
        FINAL,
        json!({
            "height": height,
            "time": iso8601(t),
            "time_ms": t * 1_000,
            "transaction_count": tx_count,
            "system_transaction_count": sys,
            "transactions": txs,
        }),
    ))
}

#[derive(Serialize)]
struct Account {
    address: String,
    signer: bool,
    writable: bool,
}

#[derive(Serialize)]
struct Instruction {
    index: i16,
    /// Position among the inner instructions of `index`; absent for top-level instructions.
    #[serde(skip_serializing_if = "Option::is_none")]
    inner_index: Option<i16>,
    program: String,
    program_name: Option<&'static str>,
    kind: Option<String>,
    /// Raw instruction data, base58.
    data: String,
    accounts: Vec<String>,
}

async fn tx(State(st): State<AppState>, Path((net, signature)): Path<(String, String)>) -> ApiResult {
    let n = network(&st, &net)?;
    let sig: [u8; 64] = parse_key(&signature, "signature")?;
    type Row = (i64, i32, i64, Option<Value>, Option<i64>, Option<i64>, i16, i16, i16, i64, Vec<Vec<u8>>);
    let row: Option<Row> = sqlx::query_as(
        "SELECT t.height, t.idx, t.fee, t.err, t.cu, t.valid_from_ms, t.num_signers, t.num_ro_signed,
                t.num_ro_unsigned, b.block_time_s,
                (SELECT array_agg(a.address ORDER BY u.o)
                 FROM unnest(t.account_keys) WITH ORDINALITY AS u(id, o) JOIN addresses a ON a.id = u.id)
         FROM txs t JOIN blocks b ON b.network = t.network AND b.height = t.height
         WHERE t.network = $1 AND t.signature = $2",
    )
    .bind(n.id)
    .bind(&sig[..])
    .fetch_optional(&st.pool)
    .await?;
    let Some((height, idx, fee, err, cu, valid_from, signers, ro_signed, ro_unsigned, t, keys)) = row else {
        return Err(ApiError::NotFound("transaction is not in the index (yet)".into()));
    };
    let keys: Vec<String> = keys.iter().map(|k| b58(k)).collect();
    let header = Header {
        num_required_signatures: signers as u8,
        num_readonly_signed_accounts: ro_signed as u8,
        num_readonly_unsigned_accounts: ro_unsigned as u8,
    };
    let accounts: Vec<Account> = keys
        .iter()
        .enumerate()
        .map(|(i, k)| {
            let role = Role::from(normalize::role(i, keys.len(), &header));
            Account { address: k.clone(), signer: role.signer, writable: role.writable }
        })
        .collect();

    type IxRow = (i16, i16, Vec<u8>, Option<String>, Vec<u8>, Vec<u8>);
    let ixs: Vec<IxRow> = sqlx::query_as(
        "SELECT i.ix_idx, i.inner_idx, a.address, i.kind, i.data, i.accounts
         FROM instructions i JOIN addresses a ON a.id = i.program
         WHERE i.network = $1 AND i.height = $2 AND i.idx = $3 ORDER BY i.ix_idx, i.inner_idx",
    )
    .bind(n.id)
    .bind(height)
    .bind(idx)
    .fetch_all(&st.pool)
    .await?;
    let instructions: Vec<Instruction> = ixs
        .into_iter()
        .map(|(ix, inner, program, kind, data, accts)| {
            let program = b58(&program);
            Instruction {
                index: ix,
                inner_index: (inner >= 0).then_some(inner),
                program_name: decode::label(&program),
                program,
                kind,
                data: b58(&data),
                accounts: accts.iter().filter_map(|&a| keys.get(a as usize).cloned()).collect(),
            }
        })
        .collect();

    type TransferRow = (i16, i16, Vec<u8>, Vec<u8>, String, Option<Vec<u8>>);
    let transfers: Vec<TransferRow> = sqlx::query_as(
        "SELECT tr.ix_idx, tr.inner_idx, s.address, d.address, tr.amount::text, m.address
         FROM transfers tr JOIN addresses s ON s.id = tr.source JOIN addresses d ON d.id = tr.dest
         LEFT JOIN addresses m ON m.id = tr.mint
         WHERE tr.network = $1 AND tr.height = $2 AND tr.idx = $3 ORDER BY tr.ix_idx, tr.inner_idx",
    )
    .bind(n.id)
    .bind(height)
    .bind(idx)
    .fetch_all(&st.pool)
    .await?;
    let transfers: Vec<Transfer> = transfers
        .into_iter()
        .map(|(ix, inner, from, to, amount, mint)| Transfer {
            from: b58(&from),
            to: b58(&to),
            amount,
            mint: mint.as_deref().map(b58),
            instruction: Some(ix),
            inner_instruction: (inner >= 0).then_some(inner),
        })
        .collect();

    let logs: Option<(String,)> =
        sqlx::query_as("SELECT lines FROM logs WHERE network = $1 AND height = $2 AND idx = $3")
            .bind(n.id)
            .bind(height)
            .bind(idx)
            .fetch_optional(&st.pool)
            .await?;

    Ok(cached(
        FINAL,
        json!({
            "signature": signature,
            "block": height,
            "index": idx,
            "time": iso8601(t),
            "time_ms": t * 1_000,
            "status": if err.is_some() { "failed" } else { "success" },
            "error": err,
            "fee": fee.to_string(),
            "compute_units": cu,
            "valid_from_ms": valid_from,
            "fee_payer": keys.first(),
            "accounts": accounts,
            "instructions": instructions,
            "transfers": transfers,
            "logs": logs.map(|(l,)| l.split('\n').map(str::to_owned).collect::<Vec<_>>()),
        }),
    ))
}

async fn address_summary(State(st): State<AppState>, Path((net, address)): Path<(String, String)>) -> ApiResult {
    let n = network(&st, &net)?;
    let key: [u8; 32] = parse_key(&address, "address")?;
    let balance = match tokio::time::timeout(BALANCE_TIMEOUT, n.rpc.balance(&address)).await {
        Ok(Ok(b)) => Some(b.to_string()),
        _ => None,
    };
    let Some(id) = address_id(&st.pool, &key).await? else {
        return Ok(cached(FRESH, json!({ "address": address, "transaction_count": 0, "balance": balance })));
    };
    let (count,): (i64,) = sqlx::query_as("SELECT count(*) FROM tx_accounts WHERE network = $1 AND address = $2")
        .bind(n.id)
        .bind(id)
        .fetch_one(&st.pool)
        .await?;
    let nid = n.id;
    let edge = |dir: &'static str| {
        let pool = st.pool.clone();
        async move {
            let row: Option<(i64, i64, Vec<u8>)> = sqlx::query_as(&format!(
                "SELECT ta.height, b.block_time_s, t.signature FROM tx_accounts ta
                 JOIN txs t ON t.network = ta.network AND t.height = ta.height AND t.idx = ta.idx
                 JOIN blocks b ON b.network = ta.network AND b.height = ta.height
                 WHERE ta.network = $1 AND ta.address = $2 ORDER BY ta.height {dir}, ta.idx {dir} LIMIT 1"
            ))
            .bind(nid)
            .bind(id)
            .fetch_optional(&pool)
            .await?;
            Ok::<_, ApiError>(row.map(|(h, t, s)| json!({ "block": h, "time": iso8601(t), "signature": b58(&s) })))
        }
    };
    let (first, last) = (edge("ASC").await?, edge("DESC").await?);
    let flows: Vec<(String, i64, String)> = sqlx::query_as(
        "SELECT 'received', count(*), coalesce(sum(amount), 0)::text FROM transfers
         WHERE network = $1 AND dest = $2 AND source <> $2 AND mint IS NULL
         UNION ALL
         SELECT 'sent', count(*), coalesce(sum(amount), 0)::text FROM transfers
         WHERE network = $1 AND source = $2 AND dest <> $2 AND mint IS NULL",
    )
    .bind(n.id)
    .bind(id)
    .fetch_all(&st.pool)
    .await?;
    let flows: serde_json::Map<String, Value> =
        flows.into_iter().map(|(k, c, total)| (k, json!({ "count": c, "total": total }))).collect();
    Ok(cached(
        FRESH,
        json!({
            "address": address,
            "transaction_count": count,
            "first_seen": first,
            "last_seen": last,
            "rlo_transfers": flows,
            "balance": balance,
        }),
    ))
}

async fn address_txs(
    State(st): State<AppState>,
    Path((net, address)): Path<(String, String)>,
    Query(q): Query<PageQuery>,
) -> ApiResult {
    let n = network(&st, &net)?;
    let key: [u8; 32] = parse_key(&address, "address")?;
    let p = Page::parse(&q, 2)?;
    let Some(id) = address_id(&st.pool, &key).await? else {
        return Ok(cached(FRESH, List::<TxSummary> { items: Vec::new(), next_cursor: None }));
    };
    let (cmp, dir) = p.sql();
    let rows: Vec<(i64, i32, i16)> = sqlx::query_as(&format!(
        "SELECT height, idx, role FROM tx_accounts
         WHERE network = $1 AND address = $2 AND (height, idx) {cmp} ($3, $4)
         ORDER BY height {dir}, idx {dir} LIMIT $5"
    ))
    .bind(n.id)
    .bind(id)
    .bind(p.after[0])
    .bind(as_i32(p.after[1]))
    .bind(p.limit + 1)
    .fetch_all(&st.pool)
    .await?;
    let (rows, next) = p.finish(rows, |r| format!("{}:{}", r.0, r.1));
    let keys: Vec<TxKey> = rows.iter().map(|r| (r.0, r.1)).collect();
    let roles: HashMap<TxKey, i16> = rows.iter().map(|r| ((r.0, r.1), r.2)).collect();
    let mut items = summaries(&st.pool, n.id, &keys, Some(id)).await?;
    for t in &mut items {
        t.role = roles.get(&(t.block, t.index)).map(|&r| Role::from(r));
    }
    Ok(cached(FRESH, List { items, next_cursor: next }))
}

async fn address_transfers(
    State(st): State<AppState>,
    Path((net, address)): Path<(String, String)>,
    Query(q): Query<PageQuery>,
) -> ApiResult {
    #[derive(Serialize)]
    struct Item {
        signature: String,
        block: i64,
        index: i32,
        time: String,
        time_ms: i64,
        direction: &'static str,
        #[serde(flatten)]
        transfer: Transfer,
    }
    let n = network(&st, &net)?;
    let key: [u8; 32] = parse_key(&address, "address")?;
    let p = Page::parse(&q, 4)?;
    let Some(id) = address_id(&st.pool, &key).await? else {
        return Ok(cached(FRESH, List::<Item> { items: Vec::new(), next_cursor: None }));
    };
    let (cmp, dir) = p.sql();
    let order = format!("height {dir}, idx {dir}, ix_idx {dir}, inner_idx {dir}");
    type Row = (i64, i32, i16, i16, Vec<u8>, Vec<u8>, String, Option<Vec<u8>>, Vec<u8>, i64, bool);
    // One branch per index (transfers_source, transfers_dest); self-transfers come from the first.
    let rows: Vec<Row> = sqlx::query_as(&format!(
        "WITH page AS (
            (SELECT * FROM transfers WHERE network = $1 AND source = $2
               AND (height, idx, ix_idx, inner_idx) {cmp} ($3, $4, $5, $6) ORDER BY {order} LIMIT $7)
            UNION ALL
            (SELECT * FROM transfers WHERE network = $1 AND dest = $2 AND source <> $2
               AND (height, idx, ix_idx, inner_idx) {cmp} ($3, $4, $5, $6) ORDER BY {order} LIMIT $7)
            ORDER BY {order} LIMIT $7)
         SELECT p.height, p.idx, p.ix_idx, p.inner_idx, s.address, d.address, p.amount::text, m.address,
                t.signature, b.block_time_s, p.source = $2
         FROM page p
         JOIN addresses s ON s.id = p.source JOIN addresses d ON d.id = p.dest
         LEFT JOIN addresses m ON m.id = p.mint
         JOIN txs t ON t.network = p.network AND t.height = p.height AND t.idx = p.idx
         JOIN blocks b ON b.network = p.network AND b.height = p.height
         ORDER BY p.{order_p}",
        order_p = order.replace(", ", ", p.")
    ))
    .bind(n.id)
    .bind(id)
    .bind(p.after[0])
    .bind(as_i32(p.after[1]))
    .bind(as_i16(p.after[2]))
    .bind(as_i16(p.after[3]))
    .bind(p.limit + 1)
    .fetch_all(&st.pool)
    .await?;
    let (rows, next) = p.finish(rows, |r| format!("{}:{}:{}:{}", r.0, r.1, r.2, r.3));
    let items = rows
        .into_iter()
        .map(|(height, idx, ix, inner, from, to, amount, mint, sig, t, outgoing)| Item {
            signature: b58(&sig),
            block: height,
            index: idx,
            time: iso8601(t),
            time_ms: t * 1_000,
            direction: if outgoing { "out" } else { "in" },
            transfer: Transfer {
                from: b58(&from),
                to: b58(&to),
                amount,
                mint: mint.as_deref().map(b58),
                instruction: Some(ix),
                inner_instruction: (inner >= 0).then_some(inner),
            },
        })
        .collect();
    Ok(cached(FRESH, List { items, next_cursor: next }))
}

async fn program_txs(
    State(st): State<AppState>,
    Path((net, program)): Path<(String, String)>,
    Query(q): Query<PageQuery>,
) -> ApiResult {
    let n = network(&st, &net)?;
    let key: [u8; 32] = parse_key(&program, "program")?;
    let p = Page::parse(&q, 2)?;
    let Some(id) = address_id(&st.pool, &key).await? else {
        return Ok(cached(FRESH, List::<TxSummary> { items: Vec::new(), next_cursor: None }));
    };
    let (cmp, dir) = p.sql();
    let rows: Vec<(i64, i32)> = sqlx::query_as(&format!(
        "SELECT DISTINCT height, idx FROM instructions
         WHERE network = $1 AND program = $2 AND (height, idx) {cmp} ($3, $4)
         ORDER BY height {dir}, idx {dir} LIMIT $5"
    ))
    .bind(n.id)
    .bind(id)
    .bind(p.after[0])
    .bind(as_i32(p.after[1]))
    .bind(p.limit + 1)
    .fetch_all(&st.pool)
    .await?;
    let (keys, next) = p.finish(rows, |r| format!("{}:{}", r.0, r.1));
    let items = summaries(&st.pool, n.id, &keys, None).await?;
    Ok(cached(FRESH, List { items, next_cursor: next }))
}

// ---------- rpc conformance ----------

/// Latest result of each RPC check, with since when it has held and how often it ran.
async fn rpc_conformance(State(st): State<AppState>, Path(net): Path<String>) -> ApiResult {
    let n = network(&st, &net)?;
    type Row = (String, bool, String, String, String, i64);
    let rows: Vec<Row> = sqlx::query_as(
        r#"WITH latest AS (
               SELECT DISTINCT ON (check_id) check_id, issue, observed, ts FROM rpc_conformance
               WHERE network = $1 ORDER BY check_id, ts DESC)
           SELECT l.check_id, l.issue, l.observed,
                  to_char(l.ts AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"'),
                  to_char((SELECT min(c.ts) FROM rpc_conformance c
                           WHERE c.network = $1 AND c.check_id = l.check_id
                             AND c.ts > coalesce((SELECT max(d.ts) FROM rpc_conformance d
                                                  WHERE d.network = $1 AND d.check_id = l.check_id
                                                    AND d.issue <> l.issue), '-infinity'))
                          AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"'),
                  (SELECT count(*) FROM rpc_conformance c WHERE c.network = $1 AND c.check_id = l.check_id)
           FROM latest l"#,
    )
    .bind(n.id)
    .fetch_all(&st.pool)
    .await?;
    let by_id: HashMap<&str, &Row> = rows.iter().map(|r| (r.0.as_str(), r)).collect();
    let checks: Vec<Value> = conformance::CHECKS
        .iter()
        .filter_map(|c| {
            let (_, issue, observed, checked_at, since, runs) = by_id.get(c.id)?;
            Some(json!({
                "id": c.id,
                "title": c.title,
                "expected": c.expected,
                "quirk_present": issue,
                "observed": observed,
                "checked_at": checked_at,
                "since": since,
                "runs": runs,
            }))
        })
        .collect();
    Ok(cached(60, json!({ "network": net, "checks": checks })))
}

// ---------- live stream ----------

/// One committed head-follower batch, serialized once and shared by every subscriber.
struct Update {
    /// `BlockBrief` JSON per block.
    blocks: Vec<String>,
    /// `TxSummary` JSON per user transaction, with the transaction's account keys for filtering.
    txs: Vec<(String, Vec<Vec<u8>>)>,
}

/// Turns `kelvin_tip` notifications into `Update`s for the matching network. `PgListener`
/// reconnects on its own; batches committed while it is down are not replayed.
async fn relay_updates(pool: PgPool, networks: Arc<HashMap<String, Arc<Network>>>) {
    loop {
        let mut listener = match PgListener::connect_with(&pool).await {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!("stream listener: {e:#}");
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
        };
        if let Err(e) = listener.listen(db::TIP_CHANNEL).await {
            tracing::warn!("stream listener: {e:#}");
            tokio::time::sleep(Duration::from_secs(5)).await;
            continue;
        }
        loop {
            let n = match listener.recv().await {
                Ok(n) => n,
                Err(e) => {
                    tracing::warn!("stream listener: {e:#}");
                    break;
                }
            };
            let parts: Vec<i64> = n.payload().split(':').filter_map(|p| p.parse().ok()).collect();
            let [net, first, last] = parts[..] else { continue };
            let Some(network) = networks.values().find(|x| i64::from(x.id) == net) else { continue };
            if network.updates.receiver_count() == 0 {
                continue;
            }
            match build_update(&pool, network.id, first, last).await {
                Ok(u) => {
                    let _ = network.updates.send(Arc::new(u));
                }
                Err(e) => tracing::warn!("stream update {first}..{last}: {e:#}"),
            }
        }
    }
}

async fn build_update(pool: &PgPool, net: i16, first: i64, last: i64) -> Result<Update, ApiError> {
    let blocks: Vec<(i64, i64, i32, i32)> = sqlx::query_as(
        "SELECT height, block_time_s, tx_count, sys_tx_count FROM blocks
         WHERE network = $1 AND height BETWEEN $2 AND $3 ORDER BY height",
    )
    .bind(net)
    .bind(first)
    .bind(last)
    .fetch_all(pool)
    .await?;
    let accounts: Vec<(i64, i32, Vec<Vec<u8>>)> = sqlx::query_as(
        "SELECT t.height, t.idx,
                (SELECT array_agg(a.address) FROM unnest(t.account_keys) AS u(id) JOIN addresses a ON a.id = u.id)
         FROM txs t WHERE t.network = $1 AND t.height BETWEEN $2 AND $3 ORDER BY t.height, t.idx",
    )
    .bind(net)
    .bind(first)
    .bind(last)
    .fetch_all(pool)
    .await?;
    let keys: Vec<TxKey> = accounts.iter().map(|a| (a.0, a.1)).collect();
    let summaries = summaries(pool, net, &keys, None).await?;
    let mut by_key: HashMap<TxKey, Vec<Vec<u8>>> = accounts.into_iter().map(|(h, i, a)| ((h, i), a)).collect();
    let txs = summaries
        .into_iter()
        .map(|t| {
            let accts = by_key.remove(&(t.block, t.index)).unwrap_or_default();
            (serde_json::to_string(&t).expect("serializable"), accts)
        })
        .collect();
    let blocks = blocks
        .into_iter()
        .map(|(height, t, tx_count, sys)| {
            let b = BlockBrief {
                height,
                time: iso8601(t),
                time_ms: t * 1_000,
                transaction_count: tx_count,
                system_transaction_count: sys,
            };
            serde_json::to_string(&b).expect("serializable")
        })
        .collect();
    Ok(Update { blocks, txs })
}

#[derive(Deserialize)]
struct LiveQuery {
    /// Only transactions involving this address; block events are left out.
    address: Option<String>,
}

/// Server-sent events: `block` for each new block and `tx` for each new user transaction, as the
/// head follower indexes them.
async fn live(
    State(st): State<AppState>,
    Path(net): Path<String>,
    Query(q): Query<LiveQuery>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let n = network(&st, &net)?;
    let filter: Option<[u8; 32]> = q.address.as_deref().map(|a| parse_key(a, "address")).transpose()?;
    let rx = n.updates.subscribe();
    let events = stream::unfold((rx, VecDeque::new()), move |(mut rx, mut pending)| async move {
        loop {
            if let Some(ev) = pending.pop_front() {
                return Some((Ok(ev), (rx, pending)));
            }
            match rx.recv().await {
                Ok(update) => {
                    if filter.is_none() {
                        pending.extend(update.blocks.iter().map(|b| Event::default().event("block").data(b.as_str())));
                    }
                    let wanted = update.txs.iter().filter(|(_, accts)| match &filter {
                        Some(f) => accts.iter().any(|a| a.as_slice() == f.as_slice()),
                        None => true,
                    });
                    pending.extend(wanted.map(|(t, _)| Event::default().event("tx").data(t.as_str())));
                }
                Err(broadcast::error::RecvError::Lagged(missed)) => {
                    pending.push_back(Event::default().event("lagged").data(missed.to_string()));
                }
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    });
    let mut stop = st.stop.clone();
    let events = events.take_until(async move {
        let _ = stop.wait_for(|s| *s).await;
    });
    Ok(Sse::new(events).keep_alive(KeepAlive::new().interval(Duration::from_secs(15))))
}

#[cfg(test)]
mod tests {
    use super::iso8601;

    #[test]
    fn iso8601_formats_utc() {
        assert_eq!(iso8601(0), "1970-01-01T00:00:00Z");
        // Oldest block the testnet RPC served on 2026-10-11.
        assert_eq!(iso8601(1_789_652_533), "2026-09-17T13:42:13Z");
        assert_eq!(iso8601(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(iso8601(1_798_761_599), "2026-12-31T23:59:59Z");
    }
}
