pub fn from_url(url: &str) -> String {
    // Find the last path segment after the host
    let after_host = if let Some(pos) = url.find("://") {
        let after_scheme = &url[pos + 3..];
        after_scheme.find('/').map(|p| &after_scheme[p + 1..]).unwrap_or("")
    } else {
        url
    };

    let segment = after_host
        .split('/')
        .filter(|s| !s.is_empty())
        .last()
        .map(|s| s.split('?').next().unwrap_or(s))
        .unwrap_or("download");

    if segment.is_empty() || segment.contains("://") {
        "download".to_string()
    } else {
        url_decode(segment)
    }
}

pub fn from_content_disposition(cd: &str) -> Option<String> {
    for part in cd.split(';') {
        let part = part.trim();
        if let Some(name) = part.strip_prefix("filename=") {
            let name = name.trim_matches('"').trim_matches('\'');
            if !name.is_empty() {
                return Some(name.to_string());
            }
        }
        if let Some(name) = part.strip_prefix("filename*=UTF-8''") {
            let name = url_decode(name.trim_matches('"'));
            if !name.is_empty() {
                return Some(name);
            }
        }
    }
    None
}

fn url_decode(s: &str) -> String {
    let mut bytes = Vec::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '%' {
            let hex: String = chars.by_ref().take(2).collect();
            if let Ok(byte) = u8::from_str_radix(&hex, 16) {
                bytes.push(byte);
            } else {
                bytes.push(b'%');
                bytes.extend_from_slice(hex.as_bytes());
            }
        } else if c.len_utf8() == 1 {
            bytes.push(c as u8);
        } else {
            let mut buf = [0u8; 4];
            let n = c.encode_utf8(&mut buf).len();
            bytes.extend_from_slice(&buf[..n]);
        }
    }
    match String::from_utf8(bytes) {
        Ok(s) => s,
        Err(e) => String::from_utf8_lossy(e.as_bytes()).to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_from_url_basic() {
        assert_eq!(from_url("https://example.com/file.zip"), "file.zip");
    }

    #[test]
    fn test_from_url_with_query() {
        assert_eq!(from_url("https://example.com/file.zip?download=1"), "file.zip");
    }

    #[test]
    fn test_from_url_no_path() {
        assert_eq!(from_url("https://example.com/"), "download");
    }

    #[test]
    fn test_content_disposition() {
        assert_eq!(
            from_content_disposition(r#"attachment; filename="myfile.zip""#),
            Some("myfile.zip".to_string())
        );
    }

    #[test]
    fn test_content_disposition_utf8() {
        assert_eq!(
            from_content_disposition("attachment; filename*=UTF-8''%E4%B8%AD%E6%96%87.txt"),
            Some("中文.txt".to_string())
        );
    }

    #[test]
    fn test_url_decode() {
        assert_eq!(url_decode("%E4%B8%AD%E6%96%87.txt"), "中文.txt");
        assert_eq!(url_decode("hello%20world"), "hello world");
    }
}
