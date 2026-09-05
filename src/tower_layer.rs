use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use http::{Extensions, HeaderMap, StatusCode};
use tower::Service;
use tower_layer::Layer;

use crate::RateLimiter;
use crate::backend::RateLimitBackend;
use crate::client_ip::{ClientIpConfig, MissingClientIdentity, MissingClientPolicy};
use crate::quota::Quota;

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
    key_source: KeySource,
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
            key_source: KeySource::ClientIp {
                config: ClientIpConfig::default(),
                missing_policy: MissingClientPolicy::default(),
            },
        }
    }

    /// Trust `X-Forwarded-For` (or a [`ClientIpConfig::trusted_header`]
    /// override) only for the given proxy networks, using the
    /// right-to-left hop walk described there. REQ-THROTTLE-100/101.
    pub fn with_client_ip(mut self, config: ClientIpConfig) -> Self {
        let missing_policy = match &self.key_source {
            KeySource::ClientIp { missing_policy, .. } => missing_policy.clone(),
            KeySource::Custom(_) => MissingClientPolicy::default(),
        };
        self.key_source = KeySource::ClientIp {
            config,
            missing_policy,
        };
        self
    }

    /// Set the policy for requests whose client identity cannot be
    /// resolved (no `ConnectInfo` extension). REQ-THROTTLE-103.
    ///
    /// Only meaningful for client-IP identity.
    pub fn with_missing_client_policy(mut self, policy: MissingClientPolicy) -> Self {
        if let KeySource::ClientIp { missing_policy, .. } = &mut self.key_source {
            *missing_policy = policy;
        }
        self
    }

    /// Key requests by a custom extractor instead of client IP
    /// (API key, tenant id, …).
    pub fn with_key_extractor(mut self, extractor: KeyExtractor) -> Self {
        self.key_source = KeySource::Custom(extractor);
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
    key_source: KeySource,
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
        let key_source = self.key_source.clone();
        let limiter = self.limiter.clone();
        let mut inner = self.inner.clone();

        Box::pin(async move {
            let key = match &key_source {
                KeySource::ClientIp {
                    config,
                    missing_policy,
                } => {
                    let peer = crate::client_ip::peer_ip_from_extensions(req.extensions());
                    match crate::client_ip::resolve_client_identity(req.headers(), peer, config) {
                        Ok(resolved) => resolved.ip.to_string(),
                        Err(MissingClientIdentity) => match missing_policy {
                            // Fail closed: identity unresolvable → 503.
                            // REQ-THROTTLE-103.
                            MissingClientPolicy::Reject => {
                                let mut response = http::Response::new(ReqBody::default());
                                *response.status_mut() = StatusCode::SERVICE_UNAVAILABLE;
                                return Ok(response);
                            }
                            MissingClientPolicy::FallbackKey(key) => key.to_string(),
                        },
                    }
                }
                KeySource::Custom(extract) => extract(req.headers(), req.extensions()),
            };

            let result = limiter.check(&key).await;

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
    if let Ok(val) = result.limit.to_string().parse() {
        headers.insert("X-RateLimit-Limit", val);
    }
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
