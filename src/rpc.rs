//! Rialo JSON-RPC client. Each method declares its own parameter shape: most take a single
//! object (`[{"blockHeight": n}]`), `getBlocks` takes positional `[start, end]`.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};

use crate::model::Block;

/// "Block not found for height: N" — returned for pruned and future heights.
const BLOCK_NOT_FOUND: i64 = -32001;
const MAX_ATTEMPTS: u32 = 6;

#[derive(Clone)]
pub struct Rpc {
    http: reqwest::Client,
    url: String,
    /// Response body bytes received, shared by clones.
    bytes: Arc<AtomicU64>,
}

#[derive(Debug, Deserialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

#[derive(Deserialize)]
struct Envelope<T> {
    result: Option<T>,
    error: Option<RpcError>,
}

#[derive(Deserialize)]
struct WithContext<T> {
    value: T,
}

impl Rpc {
    pub fn new(url: &str) -> Result<Self> {
        let http = reqwest::Client::builder()
            .pool_max_idle_per_host(128)
            .tcp_keepalive(Duration::from_secs(30))
            .timeout(Duration::from_secs(30))
            .build()?;
        Ok(Self { http, url: url.to_owned(), bytes: Arc::default() })
    }

    pub fn bytes_read(&self) -> u64 {
        self.bytes.load(Ordering::Relaxed)
    }

    /// One JSON-RPC call. Transport errors, 429 and 5xx are retried with exponential backoff;
    /// a JSON-RPC error object is returned to the caller as `Err(RpcError)`.
    async fn call<T: DeserializeOwned>(&self, method: &str, params: Value) -> Result<Result<T, RpcError>> {
        let body = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
        let mut delay = Duration::from_millis(200);
        for attempt in 1..=MAX_ATTEMPTS {
            let res = self.http.post(&self.url).json(&body).send().await;
            let retryable = match res {
                Ok(resp) => {
                    let status = resp.status();
                    if status.as_u16() == 429 || status.is_server_error() {
                        format!("HTTP {status}")
                    } else {
                        // Rialo answers JSON-RPC errors with 4xx plus a JSON body, so parse regardless of status.
                        match resp.bytes().await {
                            Err(e) => e.to_string(),
                            Ok(raw) => {
                                self.bytes.fetch_add(raw.len() as u64, Ordering::Relaxed);
                                let env: Envelope<T> = serde_json::from_slice(&raw)
                                    .with_context(|| format!("{method}: decoding response (HTTP {status})"))?;
                                return match (env.result, env.error) {
                                    (Some(r), _) => Ok(Ok(r)),
                                    (None, Some(e)) => Ok(Err(e)),
                                    (None, None) => bail!("{method}: response has neither result nor error"),
                                };
                            }
                        }
                    }
                }
                Err(e) => e.to_string(),
            };
            if attempt == MAX_ATTEMPTS {
                bail!("{method}: giving up after {attempt} attempts: {retryable}");
            }
            tracing::debug!(method, attempt, %retryable, "retrying");
            tokio::time::sleep(delay).await;
            delay = (delay * 2).min(Duration::from_secs(10));
        }
        unreachable!()
    }

    async fn call_ok<T: DeserializeOwned>(&self, method: &str, params: Value) -> Result<T> {
        self.call(method, params).await?.map_err(|e| anyhow::anyhow!("{method}: RPC error {}: {}", e.code, e.message))
    }

    pub async fn block_height(&self) -> Result<u64> {
        self.call_ok("getBlockHeight", json!([])).await
    }

    /// Heights of available blocks in `[start, end]`. The node accepts ranges of up to 5,001.
    pub async fn blocks(&self, start: u64, end: u64) -> Result<Vec<u64>> {
        self.call_ok("getBlocks", json!([start, end])).await
    }

    /// `None` when the node has no block at `height` (pruned or not produced yet).
    pub async fn block(&self, height: u64) -> Result<Option<Block>> {
        match self.call::<WithContext<Block>>("getBlock", json!([{ "blockHeight": height }])).await? {
            Ok(b) => Ok(Some(b.value)),
            Err(e) if e.code == BLOCK_NOT_FOUND => Ok(None),
            Err(e) => bail!("getBlock {height}: RPC error {}: {}", e.code, e.message),
        }
    }

    /// Any method, with the result as raw JSON; for checks on the RPC's behaviour.
    pub async fn raw(&self, method: &str, params: Value) -> Result<Value> {
        self.call_ok(method, params).await
    }

    /// Whether a browser on another origin may read responses: the `Access-Control-Allow-Origin`
    /// header for a request carrying `Origin`, if any.
    pub async fn cors_allow_origin(&self) -> Result<Option<String>> {
        let body = json!({ "jsonrpc": "2.0", "id": 1, "method": "getVersion", "params": [] });
        let resp = self.http.post(&self.url).header("Origin", "https://example.com").json(&body).send().await?;
        Ok(resp.headers().get("access-control-allow-origin").and_then(|v| v.to_str().ok()).map(str::to_owned))
    }

    /// Native balance in kelvin.
    pub async fn balance(&self, address: &str) -> Result<u64> {
        let r: WithContext<u64> = self.call_ok("getBalance", json!([{ "address": address }])).await?;
        Ok(r.value)
    }

    /// Lowest height the node still serves. Rialo has no getFirstAvailableBlock, so this binary
    /// searches with getBlock, assuming every height from the oldest one up to `head` is available.
    pub async fn oldest_block(&self, head: u64) -> Result<u64> {
        let (mut lo, mut hi) = (0, head);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if self.block(mid).await?.is_some() {
                hi = mid;
            } else {
                lo = mid + 1;
            }
        }
        Ok(hi)
    }
}
