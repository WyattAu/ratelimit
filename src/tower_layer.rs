use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use http::{HeaderMap, StatusCode};
use tower_layer::Layer;
use tower_service::Service;

use crate::backend::RateLimitBackend;
use crate::quota::Quota;
use crate::RateLimiter;

/// Tower layer that applies rate limiting to inner services.
#[derive(Clone)]
pub struct RateLimitLayer<B: RateLimitBackend> {
    limiter: RateLimiter<B>,
}

impl<B: RateLimitBackend> RateLimitLayer<B> {
    pub fn new(quota: Quota, backend: B) -> Self {
        Self {
            limiter: RateLimiter::new(quota, backend),
        }
    }
}

impl<S, B: RateLimitBackend> Layer<S> for RateLimitLayer<B> {
    type Service = RateLimitService<S, B>;

    fn layer(&self, inner: S) -> Self::Service {
        RateLimitService {
            inner,
            limiter: self.limiter.clone(),
        }
    }
}

/// Tower service that rate-limits requests by extracting a key from the
/// request (default: remote IP via `X-Forwarded-For` or connecting addr).
#[derive(Clone)]
pub struct RateLimitService<S, B: RateLimitBackend> {
    inner: S,
    limiter: RateLimiter<B>,
}

impl<S, ReqBody, B> Service<http::Request<ReqBody>> for RateLimitService<S, B>
where
    S: Service<http::Request<ReqBody>, Response = http::Response<ReqBody>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    B: RateLimitBackend,
{
    type Response = http::Response<ReqBody>;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: http::Request<ReqBody>) -> Self::Future {
        let key = extract_key(&req);
        let mut limiter = self.limiter.clone();
        let mut inner = self.inner.clone();

        Box::pin(async move {
            let result = limiter.check(&key).await;

            if !result.allowed {
                let response = http::Response::builder()
                    .status(StatusCode::TOO_MANY_REQUESTS)
                    .header("Retry-After", "1")
                    .body(Default::default())
                    .unwrap_or_else(|_| {
                        http::Response::builder()
                            .status(StatusCode::TOO_MANY_REQUESTS)
                            .body(Default::default())
                            .expect("valid response")
                    });
                return Ok(response);
            }

            let mut response = inner.call(req).await?;
            let headers = response.headers_mut();
            insert_rate_limit_headers(headers, &result);
            Ok(response)
        })
    }
}

fn extract_key<B>(req: &http::Request<B>) -> String {
    req.headers()
        .get("X-Forwarded-For")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .unwrap_or("unknown")
        .trim()
        .to_string()
}

fn insert_rate_limit_headers(headers: &mut HeaderMap, result: &crate::metrics::RateLimitResult) {
    if let Ok(val) = result.remaining.to_string().parse() {
        headers.insert("X-RateLimit-Remaining", val);
    }
    let reset_secs = result
        .reset_at
        .checked_duration_since(std::time::Instant::now())
        .unwrap_or_default()
        .as_secs();
    if let Ok(val) = reset_secs.to_string().parse() {
        headers.insert("X-RateLimit-Reset", val);
    }
}
