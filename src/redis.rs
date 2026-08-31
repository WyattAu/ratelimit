use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use redis::aio::MultiplexedConnection;
use redis::Script;

use crate::backend::RateLimitBackend;
use crate::metrics::RateLimitResult;
use crate::quota::Quota;

/// Redis-backed rate limiter using the GCRA algorithm via a Lua script.
pub struct RedisBackend {
    conn: MultiplexedConnection,
}

impl RedisBackend {
    /// Create a new `RedisBackend` from an existing `MultiplexedConnection`.
    pub fn new(conn: MultiplexedConnection) -> Self {
        Self { conn }
    }

    /// Connect to a Redis instance at `url` (e.g. `"redis://127.0.0.1/"`).
    pub async fn connect(url: &str) -> Result<Self, crate::error::RateLimitError> {
        let client = redis::Client::open(url)
            .map_err(|e| crate::error::RateLimitError::BackendError(e.to_string()))?;
        let conn = client
            .get_multiplexed_async_connection()
            .await
            .map_err(|e| crate::error::RateLimitError::BackendError(e.to_string()))?;
        Ok(Self::new(conn))
    }

    /// Connect using a `redis::Client`.
    pub async fn from_client(
        client: redis::Client,
    ) -> Result<Self, crate::error::RateLimitError> {
        let conn = client
            .get_multiplexed_async_connection()
            .await
            .map_err(|e| crate::error::RateLimitError::BackendError(e.to_string()))?;
        Ok(Self::new(conn))
    }
}

impl RedisBackend {
    /// Execute the GCRA Lua script against Redis.
    ///
    /// Returns `(allowed, remaining, retry_after_ms)`.
    async fn gcra_check(
        &self,
        key: &str,
        emission: u64,
        burst: u32,
        cost: u32,
        now_ms: u64,
        ttl_ms: u64,
    ) -> Result<(bool, u32, u64), crate::error::RateLimitError> {
        let script = Script::new(GCRA_SCRIPT);
        let mut conn = self.conn.clone();

        let result: Vec<i64> = script
            .key(key)
            .arg(emission)
            .arg(burst)
            .arg(cost)
            .arg(now_ms)
            .arg(ttl_ms)
            .invoke_async(&mut conn)
            .await
            .map_err(|e| crate::error::RateLimitError::BackendError(e.to_string()))?;

        let allowed = result[0] != 0;
        let remaining = result[1] as u32;
        let retry_after_ms = result[2] as u64;

        Ok((allowed, remaining, retry_after_ms))
    }
}

#[async_trait::async_trait]
impl RateLimitBackend for RedisBackend {
    async fn check(&self, key: &str, quota: &Quota) -> RateLimitResult {
        let emission = quota.interval().as_millis() as u64;
        let burst = quota.burst;
        let cost: u32 = 1;
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let ttl_ms = emission * burst as u64;

        match self
            .gcra_check(key, emission, burst, cost, now_ms, ttl_ms)
            .await
        {
            Ok((allowed, remaining, retry_after_ms)) => {
                let reset_at = if retry_after_ms > 0 {
                    Instant::now() + Duration::from_millis(retry_after_ms)
                } else {
                    Instant::now() + quota.interval()
                };

                RateLimitResult {
                    allowed,
                    remaining: remaining as u64,
                    reset_at,
                    limit: burst as u64,
                    retry_after: if allowed {
                        None
                    } else {
                        Some(Duration::from_millis(retry_after_ms))
                    },
                }
            }
            Err(crate::error::RateLimitError::BackendError(msg)) => {
                // On Redis errors, fail open — allow the request.
                eprintln!("redis backend error, failing open: {msg}");
                RateLimitResult {
                    allowed: true,
                    remaining: 0,
                    reset_at: Instant::now() + quota.interval(),
                    limit: quota.burst as u64,
                    retry_after: None,
                }
            }
            Err(e) => {
                tracing::warn!("unexpected rate limit error, failing open: {e}");
                RateLimitResult {
                    allowed: true,
                    remaining: 0,
                    reset_at: Instant::now() + quota.interval(),
                    limit: quota.burst as u64,
                    retry_after: None,
                }
            }
        }
    }
}

const GCRA_SCRIPT: &str = r#"
-- GCRA Rate Limiter for Redis
local key = KEYS[1]
local emission = tonumber(ARGV[1])
local burst = tonumber(ARGV[2])
local cost = tonumber(ARGV[3])
local now = tonumber(ARGV[4])
local ttl = tonumber(ARGV[5])

local tat = tonumber(redis.call('GET', key))
if tat == nil then tat = now end

local increment = emission * cost
local burst_offset = emission * burst
local new_tat = math.max(tat, now) + increment
local allow_at = new_tat - burst_offset

if now >= allow_at then
    redis.call('SET', key, new_tat, 'PX', ttl)
    local remaining = math.floor((new_tat - now) / emission)
    return {1, remaining, 0}
else
    local retry_after = math.ceil((allow_at - now) / 1000)
    return {0, 0, retry_after}
end
"#;
