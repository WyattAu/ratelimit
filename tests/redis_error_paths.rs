// Redis backend tests that need no live server: every construction and
// connection error path. The GCRA execution paths are covered by CI with a
// Redis service container (see COVERAGE-NOTES.md for the exception record).
#![cfg(feature = "redis")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use throttle_kit::RedisBackend;

#[tokio::test]
async fn connect_rejects_malformed_url() {
    // `Client::open` fails before any I/O is attempted.
    assert!(RedisBackend::connect("not-a-redis-url").await.is_err());
}

#[tokio::test]
async fn connect_fails_when_no_server_is_listening() {
    // Loopback port 1: connection refused immediately, no server needed.
    assert!(RedisBackend::connect("redis://127.0.0.1:1/").await.is_err());
}

#[tokio::test]
async fn from_client_fails_when_no_server_is_listening() {
    let client = redis::Client::open("redis://127.0.0.1:1/").unwrap();
    assert!(RedisBackend::from_client(client).await.is_err());
}
