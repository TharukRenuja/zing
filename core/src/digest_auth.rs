use std::collections::HashMap;

/// Parse a WWW-Authenticate header value into a map of parameters.
fn parse_auth_params(header: &str) -> HashMap<String, String> {
    let mut params = HashMap::new();
    // Skip the scheme prefix (e.g. "Digest ")
    let body = header.trim();
    let body = match body.find(' ') {
        Some(pos) => &body[pos + 1..],
        None => return params,
    };

    let mut i = 0;
    let bytes = body.as_bytes();
    while i < bytes.len() {
        // Skip whitespace and commas
        while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b',') {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        // Read key
        let key_start = i;
        while i < bytes.len() && bytes[i] != b'=' {
            i += 1;
        }
        let key = body[key_start..i].trim().to_lowercase();
        i += 1; // skip '='
                // Read value (either quoted or unquoted)
        if i < bytes.len() && bytes[i] == b'"' {
            i += 1; // skip opening quote
            let val_start = i;
            while i < bytes.len() && bytes[i] != b'"' {
                i += 1;
            }
            params.insert(key, body[val_start..i].to_string());
            i += 1; // skip closing quote
        } else {
            let val_start = i;
            while i < bytes.len() && bytes[i] != b',' && bytes[i] != b' ' {
                i += 1;
            }
            params.insert(key, body[val_start..i].to_string());
        }
    }
    params
}

/// Compute an MD5 hex digest.
fn md5_hex(data: &str) -> String {
    use md5::{Digest, Md5};
    let mut hasher = Md5::new();
    hasher.update(data.as_bytes());
    let result = hasher.finalize();
    format!("{:x}", result)
}

/// Generate a client nonce (cnonce).
fn cnonce() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    // Use the last 8 hex digits of the timestamp as a simple cnonce
    format!("{:016x}", nanos)
}

/// Compute a Digest Authorization header value from a WWW-Authenticate challenge.
pub fn compute_digest_auth(
    challenge_header: &str,
    username: &str,
    password: &str,
    method: &str,
    uri: &str,
) -> Option<String> {
    let params = parse_auth_params(challenge_header);
    let realm = params.get("realm")?;
    let nonce = params.get("nonce")?;
    let algorithm = params.get("algorithm").map(|s| s.as_str()).unwrap_or("MD5");
    let qop = params.get("qop").map(|s| s.as_str()).unwrap_or("");
    let opaque = params.get("opaque").map(|s| s.as_str()).unwrap_or("");

    let ha1 = md5_hex(&format!("{username}:{realm}:{password}"));

    let ha2 = md5_hex(&format!("{method}:{uri}"));

    let cn = cnonce();
    let nc = "00000001";

    let response = if qop.contains("auth") || qop.contains("auth-int") {
        md5_hex(&format!("{ha1}:{nonce}:{nc}:{cn}:{qop}:{ha2}"))
    } else {
        md5_hex(&format!("{ha1}:{nonce}:{ha2}"))
    };

    let mut auth = format!(
        r#"Digest username="{username}", realm="{realm}", nonce="{nonce}", uri="{uri}", response="{response}""#
    );

    if !opaque.is_empty() {
        auth.push_str(&format!(", opaque=\"{opaque}\""));
    }
    if !algorithm.eq_ignore_ascii_case("MD5") {
        auth.push_str(&format!(", algorithm={algorithm}"));
    }
    if qop.contains("auth") {
        auth.push_str(&format!(", qop={qop}, nc={nc}, cnonce=\"{cn}\""));
    }

    Some(auth)
}

/// The request-target a Digest response must be computed over.
///
/// RFC 7616 hashes the request-target, i.e. `/path?query`, not the absolute
/// URL. Handing `compute_digest_auth` a full URL yields a response the server
/// will never match, which is what the ranged path used to do.
pub fn request_uri(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(parsed) => {
            let mut target = parsed.path().to_string();
            if let Some(query) = parsed.query() {
                target.push('?');
                target.push_str(query);
            }
            if target.is_empty() {
                "/".to_string()
            } else {
                target
            }
        }
        Err(_) => url.to_string(),
    }
}

/// Whether to answer a Digest challenge, and with what credentials.
///
/// Grouping the flag and the credentials lets every request site -- probe,
/// streaming, stdout, and the ranged path -- retry a 401 the same way. Only the
/// ranged path did this before, and since a 401 carries no Content-Length an
/// authenticated resource always looked like an unknown size, which routed it
/// to streaming and left digest auth unreachable.
#[derive(Clone, Default)]
pub struct DigestAuth {
    pub enabled: bool,
    pub username: String,
    pub password: String,
}

impl DigestAuth {
    /// Build the `Authorization` value answering a `WWW-Authenticate` challenge.
    ///
    /// Returns `None` when digest is off, when the challenge is not Digest, or
    /// when it lacks the parameters a response cannot be computed from.
    pub fn header_for(&self, challenge: &str, method: &str, uri: &str) -> Option<String> {
        if !self.enabled {
            return None;
        }
        if !challenge
            .trim_start()
            .to_ascii_lowercase()
            .starts_with("digest")
        {
            return None;
        }
        compute_digest_auth(challenge, &self.username, &self.password, method, uri)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_request_uri_strips_scheme_and_authority() {
        // This is the bug: the ranged path hashed the whole URL, so the
        // response never matched what a server computes.
        assert_eq!(request_uri("http://127.0.0.1:8099/auth"), "/auth");
        assert_eq!(request_uri("https://example.com/a/b.txt"), "/a/b.txt");
        assert_eq!(request_uri("http://example.com/p?q=1&r=2"), "/p?q=1&r=2");
        assert_eq!(request_uri("http://example.com"), "/");
    }

    #[test]
    fn test_header_for_requires_digest_to_be_enabled() {
        let challenge = r#"Digest realm="r", nonce="n""#;
        let off = DigestAuth {
            enabled: false,
            username: "u".into(),
            password: "p".into(),
        };
        assert_eq!(off.header_for(challenge, "GET", "/x"), None);

        let on = DigestAuth {
            enabled: true,
            username: "u".into(),
            password: "p".into(),
        };
        assert!(on.header_for(challenge, "GET", "/x").is_some());
    }

    #[test]
    fn test_header_for_ignores_a_non_digest_challenge() {
        // Basic and Bearer challenges must not be answered with a Digest header.
        let auth = DigestAuth {
            enabled: true,
            username: "u".into(),
            password: "p".into(),
        };
        assert_eq!(auth.header_for(r#"Basic realm="r""#, "GET", "/x"), None);
        assert_eq!(auth.header_for("Bearer abc123", "GET", "/x"), None);
    }

    #[test]
    fn test_header_for_uses_the_request_target_given() {
        let auth = DigestAuth {
            enabled: true,
            username: "tester".into(),
            password: "s3cret".into(),
        };
        let header = auth
            .header_for(
                r#"Digest realm="zing-test", qop="auth", nonce="testnonce""#,
                "GET",
                "/auth",
            )
            .expect("header");
        assert!(header.starts_with("Digest "), "{header}");
        assert!(header.contains(r#"uri="/auth""#), "{header}");
        assert!(header.contains("qop=auth"), "{header}");
    }

    #[test]
    fn test_parse_auth_params_simple() {
        let header = r#"Digest realm="testrealm@host.com", nonce="dcd98b7102dd2f0e8b11d0f600bfb0c093", opaque="5ccc069c403ebaf9f0171e9517f40e41""#;
        let params = parse_auth_params(header);
        assert_eq!(params.get("realm").unwrap(), "testrealm@host.com");
        assert_eq!(
            params.get("nonce").unwrap(),
            "dcd98b7102dd2f0e8b11d0f600bfb0c093"
        );
        assert_eq!(
            params.get("opaque").unwrap(),
            "5ccc069c403ebaf9f0171e9517f40e41"
        );
    }

    #[test]
    fn test_compute_digest_rfc_example() {
        // RFC 2617 example
        let challenge = r#"Digest realm="testrealm@host.com", nonce="dcd98b7102dd2f0e8b11d0f600bfb0c093", opaque="5ccc069c403ebaf9f0171e9517f40e41", qop=auth"#;
        let result = compute_digest_auth(
            challenge,
            "Mufasa",
            "Circle Of Life",
            "GET",
            "/dir/index.html",
        );
        assert!(result.is_some());
        let auth = result.unwrap();
        // Verify it contains expected fields
        assert!(auth.contains(r#"username="Mufasa""#));
        assert!(auth.contains(r#"realm="testrealm@host.com""#));
        assert!(auth.contains(r#"response=""#));
        assert!(auth.contains("qop=auth"));
        assert!(auth.contains("nc=00000001"));
    }

    #[test]
    fn test_md5_hex() {
        let result = md5_hex("hello");
        assert_eq!(result, "5d41402abc4b2a76b9719d911017c592");
    }
}
