use std::path::Path;

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

    fn cmd_name(&self) -> &'static str {
        match self {
            HashKind::Md5 => "md5sum",
            HashKind::Sha1 => "sha1sum",
            HashKind::Sha256 => "sha256sum",
            HashKind::Sha512 => "sha512sum",
        }
    }
}

pub fn verify_file(path: &Path, expected_hex: &str) -> Result<bool, String> {
    let kind = HashKind::from_hex(expected_hex)
        .ok_or_else(|| format!("unknown hash length: {}", expected_hex.len()))?;

    let output = std::process::Command::new(kind.cmd_name())
        .arg(path)
        .output()
        .map_err(|e| format!("failed to run {}: {e}", kind.cmd_name()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("{} failed: {stderr}", kind.cmd_name()));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let computed = stdout.split_whitespace().next().unwrap_or("");
    Ok(computed.eq_ignore_ascii_case(expected_hex))
}
