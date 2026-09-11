use std::fmt;
use std::future::Future;
use std::net::IpAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use http::{Extensions, HeaderMap, HeaderValue, StatusCode};
use tower::Service;
use tower_layer::Layer;

use crate::RateLimiter;
use crate::backend::RateLimitBackend;
use crate::client_ip::{ClientIpConfig, MissingClientIdentity, MissingClientPolicy};
use crate::quota::Quota;

use std::fmt::Write as _;

/// Fixed-capacity scratch writer over a byte buffer.
///
/// `write!` targets here never allocate: the capacity checks use
/// `get_mut`/`get` (no panics, no `unsafe`), and overflow returns a
/// formatting error, which callers treat as "keep the empty value" —
/// unreachable for the inputs used (IP text and decimal integers).
struct SliceWriter<'a> {
    buf: &'a mut [u8],
    len: usize,
}

impl fmt::Write for SliceWriter<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let end = self.len + s.len();
        let slot = self.buf.get_mut(self.len..end).ok_or(fmt::Error)?;
        slot.copy_from_slice(s.as_bytes());
        self.len = end;
        Ok(())
    }
}

/// Stack storage for an IP-address rate-limit key: the longest IPv6 text
/// form is 45 bytes, so keying by client IP needs no heap `String`.
struct KeyBuf([u8; 45]);

impl KeyBuf {
    /// Format `ip` into the buffer; the returned `&str` borrows `self`.
    fn format_ip(&mut self, ip: IpAddr) -> &str {
        let mut writer = SliceWriter {
            buf: &mut self.0,
            len: 0,
        };
        // `Display for IpAddr` emits pure ASCII bounded by 45 bytes, so
        // the write cannot overflow this buffer.
        let _ = write!(writer, "{ip}");
        let len = writer.len;
        // Unreachable fallbacks: `len <= 45` and the contents are ASCII.
        let bytes = self.0.get(..len).unwrap_or(&[]);
        // `str::from_utf8` is stable since Rust 1.0; clippy 1.94's
        // `incompatible_msrv` misfires on it when the package MSRV is
        // 1.85, so it is suppressed here rather than project-wide.
        #[allow(clippy::incompatible_msrv)]
        str::from_utf8(bytes).unwrap_or_default()
    }
}

/// Decimal ASCII form of a `u64` in stack space (max 20 digits), for
/// building `HeaderValue`s without `to_string()` allocations.
fn u64_header_value(v: u64) -> Option<HeaderValue> {
    let mut buf = [0u8; 20];
    let len = {
        let mut writer = SliceWriter {
            buf: &mut buf,
            len: 0,
        };
        let _ = write!(writer, "{v}");
        writer.len
    };
    HeaderValue::from_bytes(buf.get(..len)?).ok()
}

/// Custom, non-IP key extractor for callers that key requests by
/// something other than client IP (API key, tenant id, …). Receives the
/// request's headers and extensions; returns the rate-limit key.
///
/// Overrides client-IP identity entirely: forwarded headers are never
/// consulted.
pub type KeyExtractor = Arc<dyn Fn(&HeaderMap, &Extensions) -> String + Send + Sync>;

/// Where the rate-limit key comes from.
#[derive(Clone)]
enum KeySource {
    /// Client-IP identity resolved through [`ClientIpConfig`] (default).
    ClientIp {
        config: ClientIpConfig,
        missing_policy: MissingClientPolicy,
    },
    /// Caller-supplied extractor (API keys, tenant ids, …).
    Custom(KeyExtractor),
}

/// Tower layer that applies rate limiting to inner services.
///
/// By default the key is the **client's socket address**; forwarded
/// headers are only believed for peers listed in
/// [`ClientIpConfig::trusted_proxies`] (see
/// [`RateLimitLayer::with_client_ip`]). The default is secure by
/// default: an unconfigured layer ignores `X-Forwarded-For` entirely.
///
/// For axum, serve the router with
/// `.into_make_service_with_connect_info::<SocketAddr>()` so the peer
/// address is available; without it the
/// [`MissingClientPolicy`] applies (fail-closed `503` by default).
#[derive(Clone)]
pub struct RateLimitLayer<B: RateLimitBackend> {
    limiter: RateLimiter<B>,
    // `Arc` so the per-request clone (into the service future) is a
    // refcount bump — `ClientIpConfig`'s trusted-proxy list would
    // otherwise be copied on every request.
    key_source: Arc<KeySource>,
}

impl<B: RateLimitBackend> RateLimitLayer<B> {
    /// Create a layer that applies `quota` through the given backend.
    ///
    /// Identity defaults to the client's socket address with an empty
    /// trusted-proxy list (forwarded headers ignored) and
    /// [`MissingClientPolicy::Reject`] when the peer is unknown.
    pub fn new(quota: Quota, backend: B) -> Self {
        Self {
            limiter: RateLimiter::new(quota, backend),
            key_source: Arc::new(KeySource::ClientIp {
                config: ClientIpConfig::default(),
                missing_policy: MissingClientPolicy::default(),
            }),
        }
    }

    /// Trust `X-Forwarded-For` (or a [`ClientIpConfig::trusted_header`]
    /// override) only for the given proxy networks, using the
    /// right-to-left hop walk described there. REQ-THROTTLE-100/101.
    pub fn with_client_ip(mut self, config: ClientIpConfig) -> Self {
        let missing_policy = match self.key_source.as_ref() {
            KeySource::ClientIp { missing_policy, .. } => missing_policy.clone(),
            KeySource::Custom(_) => MissingClientPolicy::default(),
        };
        self.key_source = Arc::new(KeySource::ClientIp {
            config,
            missing_policy,
        });
        self
    }

    /// Set the policy for requests whose client identity cannot be
    /// resolved (no `ConnectInfo` extension). REQ-THROTTLE-103.
    ///
    /// Only meaningful for client-IP identity.
    pub fn with_missing_client_policy(mut self, policy: MissingClientPolicy) -> Self {
        // Clone the inner source if the `Arc` is shared (the layer was
        // cloned after construction) so builder mutation never leaks
        // into the sibling layer.
        let mut source = match Arc::try_unwrap(self.key_source) {
            Ok(source) => source,
            Err(shared) => (*shared).clone(),
        };
        if let KeySource::ClientIp { missing_policy, .. } = &mut source {
            *missing_policy = policy;
        }
        self.key_source = Arc::new(source);
        self
    }

    /// Key requests by a custom extractor instead of client IP
    /// (API key, tenant id, …).
    pub fn with_key_extractor(mut self, extractor: KeyExtractor) -> Self {
        self.key_source = Arc::new(KeySource::Custom(extractor));
        self
    }
}

impl<S, B> Layer<S> for RateLimitLayer<B>
where
    B: RateLimitBackend + Clone,
{
    type Service = RateLimitService<S, B>;

    fn layer(&self, inner: S) -> Self::Service {
        RateLimitService {
            inner,
            limiter: self.limiter.clone(),
            key_source: self.key_source.clone(),
        }
    }
}

/// Tower service that rate-limits requests by client identity.
///
/// Identity comes from the layer's key source: client IP (secure
/// forwarded-header handling, default) or a custom extractor.
#[derive(Clone)]
pub struct RateLimitService<S, B: RateLimitBackend> {
    inner: S,
    limiter: RateLimiter<B>,
    key_source: Arc<KeySource>,
}

impl<S, ReqBody, B> Service<http::Request<ReqBody>> for RateLimitService<S, B>
where
    S: Service<http::Request<ReqBody>, Response = http::Response<ReqBody>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    B: RateLimitBackend + Clone,
    // The 429/503 short-circuits must synthesize a response body of the
    // inner service's body type, so callers must use a `Default` body;
    // `Send` is required because the request is moved into the future.
    ReqBody: Default + Send + 'static,
{
    type Response = http::Response<ReqBody>;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: http::Request<ReqBody>) -> Self::Future {
        let key_source = Arc::clone(&self.key_source);
        let limiter = self.limiter.clone();
        let mut inner = self.inner.clone();

        Box::pin(async move {
            // Key scratch space lives in this future's stack frame: the
            // client-IP key is formatted into `key_buf` (no heap), and
            // only a custom extractor (or the opt-in fallback key,
            // borrowed from the `Arc`'d key source) touches heap storage.
            let mut key_buf = KeyBuf([0u8; 45]);
            let custom_key: Option<String> = match key_source.as_ref() {
                KeySource::Custom(extract) => Some(extract(req.headers(), req.extensions())),
                _ => None,
            };

            let key: &str = match key_source.as_ref() {
                KeySource::ClientIp {
                    config,
                    missing_policy,
                } => {
                    let peer = crate::client_ip::peer_ip_from_extensions(req.extensions());
                    match crate::client_ip::resolve_client_identity(req.headers(), peer, config) {
                        Ok(resolved) => key_buf.format_ip(resolved.ip),
                        Err(MissingClientIdentity) => match missing_policy {
                            // Fail closed: identity unresolvable → 503.
                            // REQ-THROTTLE-103.
                            MissingClientPolicy::Reject => {
                                let mut response = http::Response::new(ReqBody::default());
                                *response.status_mut() = StatusCode::SERVICE_UNAVAILABLE;
                                return Ok(response);
                            }
                            MissingClientPolicy::FallbackKey(key) => key.as_ref(),
                        },
                    }
                }
                KeySource::Custom(_) => custom_key.as_deref().unwrap_or_default(),
            };

            let result = limiter.check(key).await;

            if !result.allowed {
                let mut response = http::Response::new(ReqBody::default());
                *response.status_mut() = StatusCode::TOO_MANY_REQUESTS;
                response.headers_mut().insert(
                    http::header::HeaderName::from_static("retry-after"),
                    http::header::HeaderValue::from_static("1"),
                );
                return Ok(response);
            }

            let mut response = inner.call(req).await?;
            let headers = response.headers_mut();
            insert_rate_limit_headers(headers, &result);
            Ok(response)
        })
    }
}

fn insert_rate_limit_headers(headers: &mut HeaderMap, result: &crate::metrics::RateLimitResult) {
    // Values are formatted into a stack buffer and validated once by
    // `HeaderValue::from_bytes` — no `to_string().parse()` round-trips
    // (each of those costs two allocations per header).
    if let Some(val) = u64_header_value(result.limit) {
        headers.insert("X-RateLimit-Limit", val);
    }
    if let Some(val) = u64_header_value(result.remaining) {
        headers.insert("X-RateLimit-Remaining", val);
    }
    let reset_secs = result
        .reset_at
        .checked_duration_since(std::time::Instant::now())
        .unwrap_or_default()
        .as_secs();
    if let Some(val) = u64_header_value(reset_secs) {
        headers.insert("X-RateLimit-Reset", val);
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
    use crate::metrics::RateLimitResult;
    use std::convert::Infallible;
    use std::sync::Arc;
    use std::time::Instant;

    /// Backend that always allows with a full quota budget.
    #[derive(Clone)]
    struct AlwaysAllowBackend;

    impl RateLimitBackend for AlwaysAllowBackend {
        async fn check(&self, _key: &str, quota: &Quota) -> RateLimitResult {
            RateLimitResult {
                allowed: true,
                remaining: u64::from(quota.burst) - 1,
                reset_at: Instant::now() + quota.interval(),
                limit: u64::from(quota.burst),
                retry_after: None,
            }
        }
    }

    /// Inner service whose `poll_ready` never becomes ready, so readiness
    /// delegation is observable.
    #[derive(Clone)]
    struct PendingService;

    impl Service<http::Request<()>> for PendingService {
        type Response = http::Response<()>;
        type Error = Infallible;
        type Future = std::future::Ready<Result<Self::Response, Self::Error>>;

        fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Pending
        }

        fn call(&mut self, _req: http::Request<()>) -> Self::Future {
            std::future::ready(Ok(http::Response::new(())))
        }
    }

    fn layer_service() -> RateLimitService<PendingService, AlwaysAllowBackend> {
        RateLimitLayer::new(Quota::per_second(10), AlwaysAllowBackend)
            .with_key_extractor(Arc::new(|_, _| "test-key".to_string()))
            .layer(PendingService)
    }

    #[test]
    fn poll_ready_delegates_to_inner_service() {
        let mut svc = layer_service();
        let cx = &mut Context::from_waker(std::task::Waker::noop());
        assert!(
            matches!(svc.poll_ready(cx), Poll::Pending),
            "readiness must come from the inner service"
        );
    }

    #[tokio::test]
    async fn inserts_rate_limit_headers_on_allowed_response() {
        let mut svc = layer_service();
        let response = svc
            .call(http::Request::builder().body(()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let headers = response.headers();
        assert_eq!(headers.get("X-RateLimit-Limit").unwrap(), "10");
        assert_eq!(headers.get("X-RateLimit-Remaining").unwrap(), "9");
        assert!(headers.contains_key("X-RateLimit-Reset"));
    }
}
