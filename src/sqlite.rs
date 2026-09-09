use rusqlite::{Connection, params};
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::backend::RateLimitBackend;
use crate::metrics::RateLimitResult;
use crate::quota::Quota;

/// SQLite-backed GCRA rate limiter.
///
/// Stores per-key state in a SQLite database. Each key tracks its last
/// allowed timestamp and burst capacity, enabling durable rate limiting
/// across process restarts.
pub struct SqliteBackend {
    conn: Mutex<Connection>,
}

impl SqliteBackend {
    /// Open (or create) a SQLite database at `path` for rate-limit state.
    pub fn new(path: impl AsRef<Path>) -> Result<Self, rusqlite::Error> {
        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    /// Create an in-memory SQLite database for rate-limit state.
    pub fn in_memory() -> Result<Self, rusqlite::Error> {
        let conn = Connection::open_in_memory()?;
        Self::init(conn)
    }

    fn init(conn: Connection) -> Result<Self, rusqlite::Error> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS rate_limits (
                key TEXT PRIMARY KEY,
                last_emission_ms INTEGER NOT NULL,
                remaining INTEGER NOT NULL
            );",
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }
}

#[async_trait::async_trait]
impl RateLimitBackend for SqliteBackend {
    async fn check(&self, key: &str, quota: &Quota) -> RateLimitResult {
        // A poisoned mutex only means a peer panicked mid-check; the
        // SQLite connection itself remains valid, so recover the guard
        // instead of panicking (or deadlocking) on every later request.
        let conn = match self.conn.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        let interval = quota.interval();
        let burst = quota.burst;
        let interval_ms = interval.as_millis() as u64;

        // A pre-epoch system clock is a platform misconfiguration; treat
        // it as 0 rather than panicking inside a request path.
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        // Read existing entry
        let existing: Option<(u64, u32)> = conn
            .query_row(
                "SELECT last_emission_ms, remaining FROM rate_limits WHERE key = ?1",
                params![key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .ok();

        let (last_ms, remaining) = existing.unwrap_or((now, burst));

        // GCRA: tokens replenished since last check
        let elapsed = now.saturating_sub(last_ms);
        let replenished = (elapsed / interval_ms) as u32;
        let new_remaining = if replenished > 0 {
            (remaining + replenished).min(burst)
        } else {
            remaining
        };

        let allowed = new_remaining > 0;
        let final_remaining = if allowed {
            new_remaining - 1
        } else {
            new_remaining
        };

        // Update last emission time
        let new_last_ms = if allowed { now } else { last_ms };

        let _ = conn.execute(
            "INSERT OR REPLACE INTO rate_limits (key, last_emission_ms, remaining) VALUES (?1, ?2, ?3)",
            params![key, new_last_ms, final_remaining],
        );

        let reset_at = if allowed {
            Instant::now() + interval
        } else {
            // How long until a token becomes available
            let wait_ms = interval_ms.saturating_sub(elapsed % interval_ms);
            Instant::now() + Duration::from_millis(wait_ms)
        };

        RateLimitResult {
            allowed,
            remaining: final_remaining as u64,
            reset_at,
            limit: burst as u64,
            retry_after: if allowed {
                None
            } else {
                let elapsed = now.saturating_sub(last_ms);
                let wait_ms = interval_ms.saturating_sub(elapsed % interval_ms);
                Some(Duration::from_millis(wait_ms))
            },
        }
    }
}

// Tests exercise failure paths and invariants directly; unwrap/expect,
// slicing, and panicking asserts are acceptable here — violations
// surface as test failures, not production panics.
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]
#[cfg(test)]
mod tests {
    use super::*;

    fn test_backend() -> SqliteBackend {
        SqliteBackend::in_memory().unwrap()
    }

    #[tokio::test]
    async fn first_request_allowed() {
        let backend = test_backend();
        let quota = Quota::per_second(10);
        let result = backend.check("user-1", &quota).await;
        assert!(result.allowed);
        assert_eq!(result.limit, 10);
    }

    #[tokio::test]
    async fn burst_exhaustion() {
        let backend = test_backend();
        let quota = Quota::per_second(2);

        let r1 = backend.check("user-1", &quota).await;
        assert!(r1.allowed);
        let r2 = backend.check("user-1", &quota).await;
        assert!(r2.allowed);
        let r3 = backend.check("user-1", &quota).await;
        assert!(!r3.allowed);
        assert!(r3.retry_after.is_some());
    }

    #[tokio::test]
    async fn key_isolation() {
        let backend = test_backend();
        let quota = Quota::per_second(1);

        let r1 = backend.check("a", &quota).await;
        assert!(r1.allowed);
        let r2 = backend.check("a", &quota).await;
        assert!(!r2.allowed);

        // Different key should be unaffected
        let r3 = backend.check("b", &quota).await;
        assert!(r3.allowed);
    }

    #[tokio::test]
    async fn remaining_decrements() {
        let backend = test_backend();
        let quota = Quota::per_second(5);

        let r = backend.check("u", &quota).await;
        assert_eq!(r.remaining, 4);
        let r = backend.check("u", &quota).await;
        assert_eq!(r.remaining, 3);
    }

    #[tokio::test]
    async fn reset_at_is_in_the_future_when_allowed() {
        let backend = test_backend();
        let r = backend.check("reset-ok", &Quota::per_second(2)).await;
        assert!(r.allowed);
        assert!(
            r.reset_at > Instant::now(),
            "reset_at must be in the future for an allowed request"
        );
    }

    #[tokio::test]
    async fn denial_timing_is_inside_the_current_interval() {
        let backend = test_backend();
        let quota = Quota::per_second(2); // 500 ms interval, burst 2

        assert!(backend.check("denied", &quota).await.allowed);
        assert!(backend.check("denied", &quota).await.allowed);

        // Partial-window gap: some, but not all, of the interval elapses
        // before the next request, so it is denied with a fraction of the
        // interval left to wait.
        tokio::time::sleep(Duration::from_millis(250)).await;

        let before = Instant::now();
        let r3 = backend.check("denied", &quota).await;
        assert!(!r3.allowed);
        let after = Instant::now();
        assert!(
            r3.reset_at > after,
            "reset_at must be strictly in the future for a denied request"
        );
        assert!(
            r3.reset_at < before + quota.interval(),
            "reset_at must fall within the current interval, not a full one out"
        );
        let retry = r3.retry_after.expect("denied result carries retry_after");
        assert!(retry > Duration::ZERO);
        assert!(
            retry < quota.interval(),
            "retry_after must reflect the elapsed fraction of the interval"
        );
    }

    #[tokio::test]
    async fn tokens_replenish_after_elapsed_intervals() {
        let backend = test_backend();
        let quota = Quota::per_second(10); // 100 ms interval, burst 10

        // Rapid requests exhaust the burst to exactly zero.
        for i in 1..=10u64 {
            let r = backend.check("replenish", &quota).await;
            assert!(r.allowed);
            assert_eq!(r.remaining, 10 - i);
        }
        assert!(!backend.check("replenish", &quota).await.allowed);

        // After one full interval the token bucket must refill: a backend
        // that ignores elapsed time stays denied.
        tokio::time::sleep(Duration::from_millis(110)).await;
        let r = backend.check("replenish", &quota).await;
        assert!(r.allowed, "token must replenish after one interval");
        assert!(r.remaining <= 2, "at most one interval of tokens visible");
    }

    #[test]
    fn file_backend_creates_db() {
        let dir = std::env::temp_dir().join("throttle-kit-test-sqlite");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.db");
        let _ = std::fs::remove_file(&path);

        let _backend = SqliteBackend::new(&path).unwrap();
        assert!(path.exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sync_check_via_rate_limiter() {
        use crate::{Quota, RateLimiter};
        let backend = test_backend();
        let limiter = RateLimiter::new(Quota::per_second(10), backend);
        let r = limiter.check_sync("user-1");
        assert!(r.allowed);
    }

    #[test]
    fn init_fails_on_readonly_database() {
        // The bootstrap CREATE TABLE cannot run on a read-only connection,
        // so `init` must surface the error instead of half-initialising.
        let conn = rusqlite::Connection::open_with_flags(
            ":memory:",
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        assert!(SqliteBackend::init(conn).is_err());
    }
}
