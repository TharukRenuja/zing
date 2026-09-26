//! HTTP request method handling.
//!
//! zing originally spoke GET only. This module models the request method as
//! data so the engine can decide what is safe to do with it:
//!
//! * Range/segmented downloads are only attempted for methods whose Range
//!   semantics match GET's (RFC 9110 §14, plus QUERY per RFC 10008 §2.8).
//! * Only idempotent methods are eligible for automatic retry and mirror
//!   failover, so a non-idempotent request is never silently submitted twice.

use std::fmt;
use std::sync::Arc;

/// A validated HTTP method token (RFC 9110 §5.6.2 `token`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct HttpMethod(String);

impl HttpMethod {
    pub const GET: &'static str = "GET";
    pub const HEAD: &'static str = "HEAD";
    pub const POST: &'static str = "POST";
    pub const PUT: &'static str = "PUT";
    pub const PATCH: &'static str = "PATCH";
    pub const DELETE: &'static str = "DELETE";
    pub const OPTIONS: &'static str = "OPTIONS";
    pub const QUERY: &'static str = "QUERY";

    /// Parse and validate a method token. Case is preserved for non-standard
    /// methods, but comparisons are case-insensitive.
    pub fn parse(method: &str) -> anyhow::Result<Self> {
        let trimmed = method.trim();
        anyhow::ensure!(!trimmed.is_empty(), "HTTP method cannot be empty");
        anyhow::ensure!(
            trimmed.len() <= 32,
            "HTTP method is too long (max 32 characters)"
        );
        anyhow::ensure!(
            trimmed.bytes().all(is_token_byte),
            "Invalid HTTP method '{method}': methods must be RFC 9110 tokens"
        );
        Ok(Self(trimmed.to_string()))
    }

    /// The canonical `GET` method.
    pub fn get() -> Self {
        Self(Self::GET.to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn eq_ignore_case(&self, other: &str) -> bool {
        self.0.eq_ignore_ascii_case(other)
    }

    pub fn is_get(&self) -> bool {
        self.eq_ignore_case(Self::GET)
    }

    pub fn is_head(&self) -> bool {
        self.eq_ignore_case(Self::HEAD)
    }

    /// Whether `Range` requests for this method behave as they do for GET, and
    /// the response body is a plain byte range of a stable resource.
    ///
    /// QUERY qualifies: RFC 10008 §2.8 defines its Range semantics as identical
    /// to GET. Everything else is single-connection, because we cannot assume
    /// a repeated non-GET request addresses the same byte range of the same
    /// resource.
    pub fn supports_ranges(&self) -> bool {
        self.is_get() || self.is_head() || self.eq_ignore_case(Self::QUERY)
    }

    /// Whether the method is safe per RFC 9110 §9.2.1 (no intended state
    /// change). QUERY is safe per RFC 10008 §2.
    pub fn is_safe(&self) -> bool {
        self.is_get()
            || self.is_head()
            || self.eq_ignore_case(Self::OPTIONS)
            || self.eq_ignore_case(Self::QUERY)
    }

    /// Whether repeating the request is equivalent to issuing it once
    /// (RFC 9110 §9.2.2). Only these may be auto-retried or failed over to a
    /// mirror, otherwise a retry could duplicate a side effect.
    pub fn is_idempotent(&self) -> bool {
        self.is_safe()
            || self.eq_ignore_case(Self::PUT)
            || self.eq_ignore_case(Self::DELETE)
            || self.eq_ignore_case("TRACE")
    }

    /// Whether the engine may probe this resource with a separate request.
    ///
    /// Probing is only safe for GET and HEAD. For anything else a probe would
    /// be a second execution of the request — which for a DELETE or PUT is a
    /// real side effect — so those methods skip probing entirely.
    pub fn allows_probe(&self) -> bool {
        self.is_get() || self.is_head()
    }
}

impl Default for HttpMethod {
    fn default() -> Self {
        Self::get()
    }
}

impl fmt::Display for HttpMethod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::str::FromStr for HttpMethod {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

/// RFC 9110 §5.6.2 `tchar`.
fn is_token_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric()
        || matches!(
            b,
            b'!' | b'#'
                | b'$'
                | b'%'
                | b'&'
                | b'\''
                | b'*'
                | b'+'
                | b'-'
                | b'.'
                | b'^'
                | b'_'
                | b'`'
                | b'|'
                | b'~'
        )
}

/// A request body held in memory so it can be replayed across requests.
///
/// zing streams every request body rather than buffering to disk; bodies are
/// user-supplied API payloads and upload files, not multi-gigabyte media.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestBody(Arc<Vec<u8>>);

impl RequestBody {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(Arc::new(bytes))
    }

    pub fn bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Everything needed to describe the outgoing request beyond the URL.
#[derive(Debug, Clone, Default)]
pub struct RequestSpec {
    pub method: HttpMethod,
    /// Request body, replayed on every request this task issues.
    pub body: Option<RequestBody>,
    /// Content-Type for the body. Sent only when a body is present.
    pub content_type: Option<String>,
}

impl RequestSpec {
    pub fn get() -> Self {
        Self {
            method: HttpMethod::get(),
            body: None,
            content_type: None,
        }
    }

    /// Build a spec for `method` with an optional inline body.
    pub fn with_body(
        method: HttpMethod,
        body: Option<Vec<u8>>,
        content_type: Option<String>,
    ) -> Self {
        Self {
            method,
            body: body.map(RequestBody::new),
            content_type,
        }
    }

    pub fn has_body(&self) -> bool {
        self.body.is_some()
    }

    /// Whether this request may be segmented, probed, retried and failed over
    /// to a mirror.
    ///
    /// A body disables all of it: we would have to replay the body on every
    /// range request, and a resumed download would re-submit it.
    pub fn supports_segmentation(&self) -> bool {
        self.method.supports_ranges() && !self.has_body()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_standard_and_custom_methods() {
        for m in [
            "GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS", "QUERY",
        ] {
            assert_eq!(HttpMethod::parse(m).unwrap().as_str(), m);
        }
        // Not a fixed allowlist: any valid token works.
        assert_eq!(HttpMethod::parse("PROPFIND").unwrap().as_str(), "PROPFIND");
    }

    #[test]
    fn rejects_invalid_tokens() {
        assert!(HttpMethod::parse("").is_err());
        assert!(HttpMethod::parse("   ").is_err());
        assert!(HttpMethod::parse("GET POST").is_err());
        assert!(HttpMethod::parse("GET\r\nX: y").is_err());
        assert!(HttpMethod::parse(&"A".repeat(33)).is_err());
    }

    #[test]
    fn classification_matches_rfc_semantics() {
        let g = HttpMethod::parse("GET").unwrap();
        assert!(g.is_get() && g.is_safe() && g.is_idempotent() && g.allows_probe());

        let head = HttpMethod::parse("HEAD").unwrap();
        assert!(head.supports_ranges() && head.allows_probe());

        // RFC 10008: QUERY is safe, idempotent, and Range-compatible.
        let q = HttpMethod::parse("QUERY").unwrap();
        assert!(q.supports_ranges() && q.is_safe() && q.is_idempotent());
        assert!(!q.allows_probe(), "QUERY must not be probed");

        // PUT/DELETE are idempotent but not safe.
        for m in ["PUT", "DELETE"] {
            let x = HttpMethod::parse(m).unwrap();
            assert!(x.is_idempotent(), "{m} should be idempotent");
            assert!(!x.is_safe(), "{m} should not be safe");
            assert!(!x.supports_ranges(), "{m} should not be range-capable");
            assert!(!x.allows_probe(), "{m} must not be probed");
        }

        // POST/PATCH are neither safe nor idempotent: never retried.
        for m in ["POST", "PATCH"] {
            let x = HttpMethod::parse(m).unwrap();
            assert!(!x.is_idempotent(), "{m} must not be auto-retried");
            assert!(!x.is_safe() && !x.supports_ranges() && !x.allows_probe());
        }
    }

    #[test]
    fn method_comparison_is_case_insensitive() {
        let m = HttpMethod::parse("get").unwrap();
        assert!(m.is_get());
        assert!(m.supports_ranges() && m.is_idempotent());
    }

    #[test]
    fn body_disables_segmentation() {
        let spec = RequestSpec::with_body(
            HttpMethod::get(),
            Some(b"q=1".to_vec()),
            Some("application/x-www-form-urlencoded".into()),
        );
        assert!(spec.has_body());
        assert!(!spec.supports_segmentation());
    }

    #[test]
    fn default_spec_is_plain_get() {
        let spec = RequestSpec::default();
        assert!(spec.method.is_get());
        assert!(!spec.has_body());
        assert!(spec.supports_segmentation());
    }
}
