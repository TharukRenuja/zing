use quick_xml::events::Event;
use quick_xml::Reader;

#[derive(Debug, Clone, Default)]
pub struct MetalinkFile {
    pub filename: Option<String>,
    pub size: Option<u64>,
    pub urls: Vec<String>,
    pub checksums: Vec<(String, String)>,
}

pub fn parse_metalink_str(input: &str) -> Result<Vec<MetalinkFile>, String> {
    let repaired = repair_xml(input);
    let mut reader = Reader::from_str(&repaired);
    reader.config_mut().trim_text(true);

    let mut files = Vec::new();
    let mut stack: Vec<String> = Vec::new();
    let mut current_file: Option<MetalinkFile> = None;
    let mut current_hash_type: Option<String> = None;
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
                    }
                    "hash" => {
                        current_hash_type = e
                            .attributes()
                            .filter_map(|a| a.ok())
                            .find(|a| a.key.as_ref() == b"type")
                            .and_then(|a| String::from_utf8(a.value.to_vec()).ok());
                    }
                    _ => {}
                }
            }
            Ok(Event::End(ref e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if name == "file" {
                    if let Some(f) = current_file.take() {
                        files.push(f);
                    }
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
}
