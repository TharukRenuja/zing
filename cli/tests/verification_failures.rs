//! Regressions for the three verification bugs found by running the binary.
//!
//! Each of these was invisible to the unit tests, because the unit tests cover
//! the helper functions while the bugs lived in the wiring that decides whether
//! those helpers run at all. They are end-to-end for that reason: a real
//! process, a real socket, a real exit code.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

struct Run {
    code: i32,
    #[allow(dead_code)]
    stdout: String,
    stderr: String,
}

fn zing(args: &[&str]) -> Run {
    let out = Command::new(env!("CARGO_BIN_EXE_zing"))
        // --standalone keeps the test off any daemon the developer happens to
        // be running, which would otherwise answer with a different version.
        .args(["--standalone"])
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("zing runs");
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

/// Read one request head. Returns the method exactly as sent plus the head
/// lowercased for lookups: a Digest response hashes the method
/// case-sensitively, so folding it to lowercase here would compute a different
/// HA2 than the client did and reject valid credentials.
fn read_head(reader: &mut BufReader<std::net::TcpStream>) -> Option<(String, String)> {
    let mut raw = String::new();
    let mut method = None;
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => return None,
            Ok(_) => {}
        }
        if method.is_none() {
            method = line.split_whitespace().next().map(|s| s.to_string());
        }
        let blank = line == "\r\n" || line == "\n";
        raw.push_str(&line);
        if blank {
            return Some((method.unwrap_or_default(), raw.to_ascii_lowercase()));
        }
    }
}

fn stop_flag() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(false))
}

/// Serve `body` at every path, for as long as `stop` is false.
fn serve_bytes(body: &'static [u8], stop: Arc<AtomicBool>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            let Ok(stream) = stream else { continue };
            let Ok(clone) = stream.try_clone() else {
                continue;
            };
            let mut writer = stream;
            // Read the request before replying: closing mid-write resets the
            // connection and the client then reports a send error.
            let mut reader = BufReader::new(clone);
            let _ = read_head(&mut reader);
            let _ = writer.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .as_bytes(),
            );
            let _ = writer.write_all(body);
            let _ = writer.flush();
        }
    });
    port
}

const REALM: &str = "zing-test";
const NONCE: &str = "testnonce";
const PASSWORD: &str = "s3cret";

fn md5_hex(data: &str) -> String {
    use md5::{Digest, Md5};
    let mut h = Md5::new();
    h.update(data.as_bytes());
    format!("{:x}", h.finalize())
}

/// Verify an RFC 7616 `qop=auth` response, so wrong credentials are rejected
/// instead of waved through by a server that accepts any header.
fn digest_response_ok(head: &str, method: &str) -> bool {
    let field = |name: &str| -> Option<String> {
        let at = head.find(&format!("{name}="))?;
        let rest = &head[at + name.len() + 1..];
        if let Some(stripped) = rest.strip_prefix('"') {
            let end = stripped.find('"')?;
            Some(stripped[..end].to_string())
        } else {
            let end = rest.find([',', ' ', '\r']).unwrap_or(rest.len());
            Some(rest[..end].to_string())
        }
    };
    let (Some(response), Some(uri), Some(nc), Some(cnonce), Some(qop)) = (
        field("response"),
        field("uri"),
        field("nc"),
        field("cnonce"),
        field("qop"),
    ) else {
        return false;
    };
    if !qop.contains("auth") {
        return false;
    }
    let ha1 = md5_hex(&format!("tester:{REALM}:{PASSWORD}"));
    let ha2 = md5_hex(&format!("{method}:{uri}"));
    md5_hex(&format!("{ha1}:{NONCE}:{nc}:{cnonce}:auth:{ha2}")) == response
}

/// Answer every unauthenticated request with a Digest challenge and every
/// correctly authenticated one with `body`, mirroring the server shape that
/// made the bug unreachable: a 401 carries no Content-Length, so the client
/// learns the size only if it is willing to authenticate.
fn serve_digest(body: &'static [u8], stop: Arc<AtomicBool>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            let Ok(stream) = stream else { continue };
            let Ok(clone) = stream.try_clone() else {
                continue;
            };
            let mut writer = stream;
            let mut reader = BufReader::new(clone);
            let Some((method, head)) = read_head(&mut reader) else {
                continue;
            };

            let authorized =
                head.contains("authorization: digest") && digest_response_ok(&head, &method);

            if authorized {
                let _ = writer.write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .as_bytes(),
                );
                let _ = writer.write_all(body);
            } else {
                let _ = writer.write_all(
                    b"HTTP/1.1 401 Unauthorized\r\n\
                      WWW-Authenticate: Digest realm=\"zing-test\", qop=\"auth\", \
                      nonce=\"testnonce\", opaque=\"op1\"\r\n\
                      Content-Length: 12\r\nConnection: close\r\n\r\n\
                      unauthorized",
                );
            }
            let _ = writer.flush();
        }
    });
    port
}

// --- Bug 1: a checksum mismatch used to exit 0 -----------------------------

#[test]
fn a_correct_checksum_exits_zero() {
    let stop = stop_flag();
    let port = serve_bytes(b"hello zing merge verification\n", stop.clone());
    let dir = tempfile::tempdir().expect("tempdir");
    let r = zing(&[
        "-q",
        "-W",
        dir.path().to_str().unwrap(),
        "-c",
        "ef2cb542331c75100ec1a2301c3a592d",
        &format!("http://127.0.0.1:{port}/payload"),
    ]);
    stop.store(true, Ordering::Relaxed);
    assert_eq!(r.code, 0, "a verified download must exit 0: {}", r.stderr);
}

#[test]
fn a_checksum_mismatch_exits_one() {
    // This is the bug: the summary printed "Checksum: MISMATCH" and the process
    // still exited 0, so `&&`, Make and CI all treated a corrupt file as done.
    let stop = stop_flag();
    let port = serve_bytes(b"hello zing merge verification\n", stop.clone());
    let dir = tempfile::tempdir().expect("tempdir");
    let r = zing(&[
        "-q",
        "-W",
        dir.path().to_str().unwrap(),
        "-c",
        "00000000000000000000000000000000",
        &format!("http://127.0.0.1:{port}/payload"),
    ]);
    stop.store(true, Ordering::Relaxed);
    assert_eq!(
        r.code, 1,
        "a corrupt download must not exit 0: stderr={:?}",
        r.stderr
    );
    let reported = r.stderr.replace('\x1b', "");
    assert!(
        reported.contains("MISMATCH"),
        "the mismatch must still be reported: {reported:?}"
    );
}

// --- Bug 2: digest auth was unreachable ------------------------------------

#[test]
fn digest_auth_answers_a_challenge_and_downloads_the_body() {
    // This is the bug: only the ranged path handled a 401, and a 401 has no
    // Content-Length, so the task always fell to a streaming path that never
    // sent credentials. Every request came back 401 whatever the password.
    let stop = stop_flag();
    let port = serve_digest(b"digest auth ok", stop.clone());
    let dir = tempfile::tempdir().expect("tempdir");
    let r = zing(&[
        "-q",
        "-W",
        dir.path().to_str().unwrap(),
        "--digest",
        "-u",
        "tester:s3cret",
        &format!("http://127.0.0.1:{port}/auth"),
    ]);
    stop.store(true, Ordering::Relaxed);
    assert_eq!(r.code, 0, "digest auth must succeed: {}", r.stderr);
    let body = std::fs::read(dir.path().join("auth")).expect("body written");
    assert_eq!(body, b"digest auth ok");
}

#[test]
fn digest_auth_with_the_wrong_password_still_fails() {
    // The retry must not paper over bad credentials.
    let stop = stop_flag();
    let port = serve_digest(b"digest auth ok", stop.clone());
    let dir = tempfile::tempdir().expect("tempdir");
    let r = zing(&[
        "-q",
        "-W",
        dir.path().to_str().unwrap(),
        "--digest",
        "-u",
        "tester:wrong",
        &format!("http://127.0.0.1:{port}/auth"),
    ]);
    stop.store(true, Ordering::Relaxed);
    assert_ne!(r.code, 0, "wrong credentials must fail: {}", r.stderr);
}

// --- Bug 3: -o silently discarded -W ---------------------------------------

#[test]
fn output_option_takes_precedence_over_output_dir_with_a_warning() {
    // The behaviour itself is curl-compatible; what was missing was any word
    // that --output-dir had stopped applying.
    let stop = stop_flag();
    let port = serve_bytes(b"payload", stop.clone());
    let dir = tempfile::tempdir().expect("tempdir");
    // A relative -o resolves against the working directory, which is the
    // crate root here, so remove it afterwards rather than littering the tree.
    let r = zing(&[
        "-W",
        dir.path().to_str().unwrap(),
        &format!("http://127.0.0.1:{port}/payload"),
        "-o",
        "named.txt",
    ]);
    stop.store(true, Ordering::Relaxed);
    let _ = std::fs::remove_file("named.txt");
    let reported = r.stderr.replace('\x1b', "");
    assert!(
        reported.contains("takes precedence"),
        "expected a precedence warning, got: {reported:?}"
    );
}

#[test]
fn output_dir_alone_is_unaffected() {
    // The warning must not appear, and the file must land in the directory.
    let stop = stop_flag();
    let port = serve_bytes(b"payload", stop.clone());
    let dir = tempfile::tempdir().expect("tempdir");
    let r = zing(&[
        "-q",
        "-W",
        dir.path().to_str().unwrap(),
        &format!("http://127.0.0.1:{port}/payload"),
    ]);
    stop.store(true, Ordering::Relaxed);
    assert_eq!(r.code, 0, "stderr: {}", r.stderr);
    assert!(
        dir.path().join("payload").exists(),
        "the file should be in the output dir"
    );
    assert!(!r.stderr.contains("takes precedence"));
}

// --- Existing-file conflicts, decided the way curl and aria2 do ------------

#[test]
fn an_existing_file_is_overwritten_when_nobody_can_be_asked() {
    // The regression: with no terminal, the default used to raise a prompt
    // nobody could see, cancel the download, print one line, and still exit 0.
    // A script checking only the exit code read that as success.
    let stop = stop_flag();
    let port = serve_bytes(b"new bytes", stop.clone());
    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir.path().join("payload");
    std::fs::write(&target, b"stale bytes").expect("seed existing file");

    let r = zing(&[
        "-q",
        "-W",
        dir.path().to_str().unwrap(),
        &format!("http://127.0.0.1:{port}/payload"),
    ]);
    stop.store(true, Ordering::Relaxed);
    assert_eq!(r.code, 0, "stderr: {}", r.stderr);
    assert_eq!(
        std::fs::read(&target).expect("file"),
        b"new bytes",
        "the existing file should have been replaced"
    );
    assert!(
        !r.stderr.contains("cancelled"),
        "nothing should be cancelled: {:?}",
        r.stderr
    );
}

#[test]
fn auto_file_renaming_still_keeps_the_existing_file() {
    // The flag must keep working in a non-interactive run.
    let stop = stop_flag();
    let port = serve_bytes(b"new bytes", stop.clone());
    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir.path().join("payload");
    std::fs::write(&target, b"stale bytes").expect("seed existing file");

    let r = zing(&[
        "-q",
        "--auto-file-renaming",
        "-W",
        dir.path().to_str().unwrap(),
        &format!("http://127.0.0.1:{port}/payload"),
    ]);
    stop.store(true, Ordering::Relaxed);
    assert_eq!(r.code, 0, "stderr: {}", r.stderr);
    assert_eq!(
        std::fs::read(&target).expect("original"),
        b"stale bytes",
        "the original must be left alone"
    );
    let kept: Vec<_> = std::fs::read_dir(dir.path())
        .expect("read dir")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("payload-") || n.starts_with("payload."))
        .collect();
    assert_eq!(kept.len(), 1, "expected one renamed copy, got {kept:?}");
}
