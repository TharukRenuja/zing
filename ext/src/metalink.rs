use quick_xml::events::Event;
use quick_xml::Reader;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HashAlgorithm {
    Md5,
    Sha1,
    Sha256,
    Sha512,
}

impl HashAlgorithm {
    pub fn from_metalink_str(s: &str) -> Option<Self> {
        match s.to_lowercase().replace('-', "").as_str() {
            "md5" => Some(Self::Md5),
            "sha1" => Some(Self::Sha1),
            "sha256" => Some(Self::Sha256),
            "sha512" => Some(Self::Sha512),
            _ => None,
        }
    }

    pub fn digest_size(&self) -> usize {
        match self {
            Self::Md5 => 16,
            Self::Sha1 => 20,
            Self::Sha256 => 32,
            Self::Sha512 => 64,
        }
    }

    pub fn to_hash_kind(&self) -> crate::checksum::HashKind {
        match self {
            Self::Md5 => crate::checksum::HashKind::Md5,
            Self::Sha1 => crate::checksum::HashKind::Sha1,
            Self::Sha256 => crate::checksum::HashKind::Sha256,
            Self::Sha512 => crate::checksum::HashKind::Sha512,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ChunkHashes {
    pub algorithm: HashAlgorithm,
    pub piece_length: u64,
    pub hashes: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct MetalinkFile {
    pub filename: Option<String>,
    pub size: Option<u64>,
    pub urls: Vec<String>,
    pub checksums: Vec<(String, String)>,
    pub chunk_hashes: Option<ChunkHashes>,
}

pub fn parse_metalink_str(input: &str) -> Result<Vec<MetalinkFile>, String> {
    let repaired = repair_xml(input);
    let mut reader = Reader::from_str(&repaired);
    reader.config_mut().trim_text(true);

    let mut files = Vec::new();
    let mut stack: Vec<String> = Vec::new();
    let mut current_file: Option<MetalinkFile> = None;
    let mut current_hash_type: Option<String> = None;
    let mut in_pieces = false;
    let mut pieces_algorithm: Option<HashAlgorithm> = None;
    let mut pieces_length: u64 = 0;
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                stack.push(name.clone());

                match name.as_str() {
                    "file" => {
                        let filename = e
                            .attributes()
                            .filter_map(|a| a.ok())
                            .find(|a| a.key.as_ref() == b"name")
                            .and_then(|a| String::from_utf8(a.value.to_vec()).ok());
                        current_file = Some(MetalinkFile {
                            filename,
                            ..Default::default()
                        });
                        current_hash_type = None;
                        in_pieces = false;
                        pieces_algorithm = None;
                        pieces_length = 0;
                    }
                    "hash" if !in_pieces => {
                        current_hash_type = e
                            .attributes()
                            .filter_map(|a| a.ok())
                            .find(|a| a.key.as_ref() == b"type")
                            .and_then(|a| String::from_utf8(a.value.to_vec()).ok());
                    }
                    "pieces" => {
                        in_pieces = true;
                        let mut alg = None;
                        let mut len = 0u64;
                        for attr in e.attributes().filter_map(|a| a.ok()) {
                            match attr.key.as_ref() {
                                b"type" => {
                                    if let Ok(v) = String::from_utf8(attr.value.to_vec()) {
                                        alg = HashAlgorithm::from_metalink_str(&v);
                                    }
                                }
                                b"length" => {
                                    if let Ok(v) = String::from_utf8(attr.value.to_vec()) {
                                        len = v.parse::<u64>().unwrap_or(0);
                                    }
                                }
                                _ => {}
                            }
                        }
                        pieces_algorithm = alg;
                        pieces_length = len;
                    }
                    _ => {}
                }
            }
            Ok(Event::End(ref e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                match name.as_str() {
                    "file" => {
                        if let Some(f) = current_file.take() {
                            files.push(f);
                        }
                    }
                    "pieces" => {
                        in_pieces = false;
                    }
                    _ => {}
                }
                stack.pop();
            }
            Ok(Event::Text(ref e)) => {
                let text = e.unescape().unwrap_or_default();
                let trimmed = text.trim();
                if trimmed.is_empty() {
                    buf.clear();
                    continue;
                }

                let parent = stack.last().map(|s| s.as_str());
                match parent {
                    Some("size") => {
                        if let Some(ref mut file) = current_file {
                            file.size = trimmed.parse::<u64>().ok();
                        }
                    }
                    Some("url") => {
                        if let Some(ref mut file) = current_file {
                            file.urls.push(trimmed.to_string());
                        }
                    }
                    Some("hash") if in_pieces => {
                        if let Some(ref mut file) = current_file {
                            if let Some(ref alg) = pieces_algorithm {
                                let hashes = file.chunk_hashes.get_or_insert_with(|| ChunkHashes {
                                    algorithm: *alg,
                                    piece_length: pieces_length,
                                    hashes: Vec::new(),
                                });
                                hashes.hashes.push(trimmed.to_string());
                            }
                        }
                    }
                    Some("hash") => {
                        if let Some(ref mut file) = current_file {
                            if let Some(ref htype) = current_hash_type {
                                file.checksums.push((htype.clone(), trimmed.to_string()));
                            }
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(format!("XML parse error: {e}")),
            _ => {}
        }
        buf.clear();
    }

    Ok(files)
}

fn repair_xml(input: &str) -> String {
    let mut result = String::with_capacity(input.len() + 64);
    let mut in_comment = false;
    let mut chars = input.char_indices().peekable();

    while let Some((_, ch)) = chars.next() {
        if ch == '<' && chars.peek().map(|(_, c)| *c) == Some('!') {
            let saved = chars.clone();
            chars.next();
            if chars.peek().map(|(_, c)| *c) == Some('-') {
                chars.next();
                if chars.peek().map(|(_, c)| *c) == Some('-') {
                    in_comment = true;
                    result.push_str("<!--");
                    continue;
                }
            }
            result.push_str("<!");
            chars = saved;
            continue;
        }
        if in_comment {
            result.push(ch);
            if ch == '-' && chars.peek().map(|(_, c)| *c) == Some('-') {
                chars.next();
                result.push('-');
                if chars.peek().map(|(_, c)| *c) == Some('>') {
                    chars.next();
                    result.push('>');
                    in_comment = false;
                }
            }
            continue;
        }
        if ch == '&'
            && chars.peek().map(|(_, c)| *c) != Some('a')
            && chars.peek().map(|(_, c)| *c) != Some('l')
            && chars.peek().map(|(_, c)| *c) != Some('g')
            && chars.peek().map(|(_, c)| *c) != Some('q')
            && chars.peek().map(|(_, c)| *c) != Some('t')
        {
            result.push_str("&amp;");
            continue;
        }
        result.push(ch);
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_metalink() {
        let result = parse_metalink_str(
            r#"<?xml version="1.0"?><metalink xmlns="urn:ietf:params:xml:ns:metalink"></metalink>"#,
        );
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    #[test]
    fn test_single_file() {
        let xml = r#"<?xml version="1.0"?>
<metalink xmlns="urn:ietf:params:xml:ns:metalink">
  <file name="test.iso">
    <size>1048576</size>
    <url>https://mirror1.example.com/test.iso</url>
    <url>https://mirror2.example.com/test.iso</url>
    <hash type="sha-256">abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890</hash>
  </file>
</metalink>"#;
        let files = parse_metalink_str(xml).unwrap();
        assert_eq!(files.len(), 1);
        let f = &files[0];
        assert_eq!(f.filename.as_deref(), Some("test.iso"));
        assert_eq!(f.size, Some(1048576));
        assert_eq!(f.urls.len(), 2);
        assert_eq!(f.urls[0], "https://mirror1.example.com/test.iso");
        assert_eq!(f.urls[1], "https://mirror2.example.com/test.iso");
        assert_eq!(f.checksums.len(), 1);
        assert_eq!(f.checksums[0].0, "sha-256");
    }

    #[test]
    fn test_multiple_files() {
        let xml = r#"<?xml version="1.0"?>
<metalink xmlns="urn:ietf:params:xml:ns:metalink">
  <file name="a.iso">
    <size>100</size>
    <url>http://a.example.com/a.iso</url>
  </file>
  <file name="b.iso">
    <size>200</size>
    <url>http://b.example.com/b.iso</url>
  </file>
</metalink>"#;
        let files = parse_metalink_str(xml).unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].filename.as_deref(), Some("a.iso"));
        assert_eq!(files[1].filename.as_deref(), Some("b.iso"));
    }

    #[test]
    fn test_no_urls() {
        let xml = r#"<?xml version="1.0"?>
<metalink xmlns="urn:ietf:params:xml:ns:metalink">
  <file name="orphan.iso">
    <size>512</size>
  </file>
</metalink>"#;
        let files = parse_metalink_str(xml).unwrap();
        assert_eq!(files.len(), 1);
        assert!(files[0].urls.is_empty());
        assert_eq!(files[0].size, Some(512));
    }

    #[test]
    fn test_parse_file() {
        use std::io::Write;
        let xml = br#"<?xml version="1.0"?>
<metalink xmlns="urn:ietf:params:xml:ns:metalink">
  <file name="disk.img">
    <size>999</size>
    <url>http://dl.example.com/disk.img</url>
    <hash type="md5">d41d8cd98f00b204e9800998ecf8427e</hash>
  </file>
</metalink>"#;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.meta4");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(xml).unwrap();
        drop(f);

        let content = std::fs::read_to_string(&path).unwrap();
        let files = parse_metalink_str(&content).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].filename.as_deref(), Some("disk.img"));
    }

    #[test]
    fn test_chunk_hashes() {
        let xml = r#"<?xml version="1.0"?>
<metalink xmlns="urn:ietf:params:xml:ns:metalink">
  <file name="chunked.iso">
    <size>196608</size>
    <pieces type="sha-256" length="65536">
      <hash>aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa</hash>
      <hash>bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb</hash>
      <hash>cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc</hash>
    </pieces>
    <url>https://example.com/chunked.iso</url>
  </file>
</metalink>"#;
        let files = parse_metalink_str(xml).unwrap();
        assert_eq!(files.len(), 1);
        let f = &files[0];
        let ch = f.chunk_hashes.as_ref().expect("chunk_hashes");
        assert_eq!(ch.piece_length, 65536);
        assert_eq!(ch.hashes.len(), 3);
        assert_eq!(ch.hashes[0], "a".repeat(64));
        assert_eq!(ch.hashes[1], "b".repeat(64));
        assert_eq!(ch.hashes[2], "c".repeat(64));
        assert!(matches!(ch.algorithm, HashAlgorithm::Sha256));
    }

    #[test]
    fn test_chunk_hashes_and_file_hash() {
        let xml = r#"<?xml version="1.0"?>
<metalink xmlns="urn:ietf:params:xml:ns:metalink">
  <file name="dual.iso">
    <size>65536</size>
    <pieces type="sha-256" length="65536">
      <hash>dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd</hash>
    </pieces>
    <hash type="sha-1">eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee</hash>
    <url>https://example.com/dual.iso</url>
  </file>
</metalink>"#;
        let files = parse_metalink_str(xml).unwrap();
        assert_eq!(files.len(), 1);
        let f = &files[0];
        assert!(f.chunk_hashes.is_some());
        assert_eq!(f.checksums.len(), 1);
        assert_eq!(f.checksums[0].0, "sha-1");
    }
}
