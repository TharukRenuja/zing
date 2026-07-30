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

#[cfg(test)]
mod tests {
    use super::*;

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
