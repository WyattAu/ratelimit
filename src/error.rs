/// Errors returned by the rate limiter.
#[derive(Debug, thiserror::Error)]
pub enum RateLimitError {
    /// The request was rejected — rate limit exceeded.
    #[error("rate limit exceeded")]
    RateLimited,

    /// Backend-specific error (e.g. Redis connection failure).
    #[error("rate limit backend error: {0}")]
    BackendError(String),
}
