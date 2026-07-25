use sha2::Digest;
use std::io::Read;
use std::path::Path;

#[derive(Debug, PartialEq)]
pub enum HashKind {
    Md5,
    Sha1,
    Sha256,
    Sha512,
}

impl HashKind {
    pub fn hex_len(&self) -> usize {
        match self {
            HashKind::Md5 => 32,
            HashKind::Sha1 => 40,
            HashKind::Sha256 => 64,
            HashKind::Sha512 => 128,
        }
    }

    pub fn from_hex(hex: &str) -> Option<Self> {
        match hex.len() {
            32 => Some(HashKind::Md5),
            40 => Some(HashKind::Sha1),
            64 => Some(HashKind::Sha256),
            128 => Some(HashKind::Sha512),
            _ => None,
        }
    }
}

pub fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hex_encode() {
        assert_eq!(hex_encode(&[0x00, 0xFF]), "00ff");
        assert_eq!(hex_encode(&[0xDE, 0xAD, 0xBE, 0xEF]), "deadbeef");
    }

    #[test]
    fn test_hash_kind_from_hex() {
        assert_eq!(HashKind::from_hex("d41d8cd98f00b204e9800998ecf8427e"), Some(HashKind::Md5));
        assert_eq!(HashKind::from_hex("a9993e364706816aba3e25717850c26c9cd0d89d"), Some(HashKind::Sha1));
        assert_eq!(HashKind::from_hex("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"), Some(HashKind::Sha256));
        assert_eq!(HashKind::from_hex(&format!("a{:0>127}", 0)), Some(HashKind::Sha512));
        assert_eq!(HashKind::from_hex("invalid"), None);
    }

    #[test]
    fn test_hash_empty_string() {
        let dir = std::path::PathBuf::from(std::env::temp_dir()).join("zing_test_checksum");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("empty");
        std::fs::write(&path, b"").unwrap();

        assert!(hash_file(&path, &HashKind::Md5).unwrap() == "d41d8cd98f00b204e9800998ecf8427e");
        assert!(hash_file(&path, &HashKind::Sha256).unwrap() == "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_hash_known_content() {
        let dir = std::path::PathBuf::from(std::env::temp_dir()).join("zing_test_checksum2");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("test.txt");
        std::fs::write(&path, b"Hello, World!").unwrap();

        let md5 = hash_file(&path, &HashKind::Md5).unwrap();
        assert_eq!(md5, "65a8e27d8879283831b664bd8b7f0ad4");

        let sha1 = hash_file(&path, &HashKind::Sha1).unwrap();
        assert_eq!(sha1, "0a0a9f2a6772942557ab5355d76af442f8f65e01");

        let sha256 = hash_file(&path, &HashKind::Sha256).unwrap();
        assert_eq!(sha256, "dffd6021bb2bd5b0af676290809ec3a53191dd81c7f70a4b28688a362182986f");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_verify_file() {
        let dir = std::path::PathBuf::from(std::env::temp_dir()).join("zing_test_verify");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("verify.txt");
        std::fs::write(&path, b"test data").unwrap();

        assert!(verify_file(&path, "eb733a00c0c9d336e65691a37ab54293").unwrap());
        assert!(!verify_file(&path, "00000000000000000000000000000000").unwrap());

        let _ = std::fs::remove_dir_all(&dir);
    }
}

pub fn verify_file(path: &Path, expected_hex: &str) -> Result<bool, String> {
    let kind = HashKind::from_hex(expected_hex)
        .ok_or_else(|| format!("unknown hash length: {}", expected_hex.len()))?;

    let computed = hash_file(path, &kind)?;
    Ok(computed.eq_ignore_ascii_case(expected_hex))
}

pub fn hash_file(path: &Path, kind: &HashKind) -> Result<String, String> {
    let mut file = std::fs::File::open(path)
        .map_err(|e| format!("failed to open {}: {e}", path.display()))?;

    let mut buf = [0u8; 65536];
    let computed: String = match kind {
        HashKind::Md5 => {
            let mut hasher = md5::Md5::new();
            loop {
                let n = file.read(&mut buf).map_err(|e| format!("read error: {e}"))?;
                if n == 0 { break; }
                hasher.update(&buf[..n]);
            }
            hex_encode(&hasher.finalize())
        }
        HashKind::Sha1 => {
            let mut hasher = sha1::Sha1::new();
            loop {
                let n = file.read(&mut buf).map_err(|e| format!("read error: {e}"))?;
                if n == 0 { break; }
                hasher.update(&buf[..n]);
            }
            hex_encode(&hasher.finalize())
        }
        HashKind::Sha256 => {
            let mut hasher = sha2::Sha256::new();
            loop {
                let n = file.read(&mut buf).map_err(|e| format!("read error: {e}"))?;
                if n == 0 { break; }
                hasher.update(&buf[..n]);
            }
            hex_encode(&hasher.finalize())
        }
        HashKind::Sha512 => {
            let mut hasher = sha2::Sha512::new();
            loop {
                let n = file.read(&mut buf).map_err(|e| format!("read error: {e}"))?;
                if n == 0 { break; }
                hasher.update(&buf[..n]);
            }
            hex_encode(&hasher.finalize())
        }
    };
    Ok(computed)
}
