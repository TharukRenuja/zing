//! The exit status and stderr wording of a failed request.
//!
//! A script that calls zing has no other way to notice a 4xx or 5xx, so these
//! cover what a caller actually branches on: the exit code, and whether the
//! response body stays clean on stdout.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::process::{Command, Stdio};

/// Serve one canned response on an ephemeral port, then stop.
fn serve_once(status_line: &'static str, body: &'static str) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    std::thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        let mut reader = BufReader::new(match stream.try_clone() {
            Ok(s) => s,
            Err(_) => return,
        });
        // Read the request head so the client is not left blocked on write.
        let mut line = String::new();
        while reader.read_line(&mut line).unwrap_or(0) > 0 {
            if line == "\r\n" || line == "\n" {
                break;
            }
            line.clear();
        }
        let response = format!(
            "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(response.as_bytes());
        let _ = stream.flush();
    });
    port
}

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

fn zing(args: &[&str]) -> Run {
    let out = Command::new(env!("CARGO_BIN_EXE_zing"))
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

#[test]
fn a_successful_request_exits_zero_and_prints_only_the_body() {
    let port = serve_once("200 OK", r#"{"ok":true}"#);
    let r = zing(&[
        "-q",
        "-X",
        "GET",
        &format!("http://127.0.0.1:{port}/lyrics"),
    ]);
    assert_eq!(r.code, 0, "stderr: {}", r.stderr);
    assert_eq!(r.stdout, r#"{"ok":true}"#, "stdout must be the body alone");
    assert!(!r.stderr.contains("ERROR"), "stderr: {}", r.stderr);
}

#[test]
fn a_failed_request_exits_non_zero() {
    // This is the bug: a 404 used to exit 0, so `set -e` and `&&` chains could
    // not tell a failed call from a successful one.
    let port = serve_once("404 Not Found", r#"{"error":"no such track"}"#);
    let r = zing(&[
        "-q",
        "-X",
        "GET",
        &format!("http://127.0.0.1:{port}/lyrics"),
    ]);
    assert_ne!(r.code, 0, "a failed request must not exit 0");
    assert!(
        r.stdout.is_empty(),
        "a failed request must not put anything on stdout: {:?}",
        r.stdout
    );
    assert!(r.stderr.contains("404"), "stderr: {}", r.stderr);
}

#[test]
fn a_failed_request_does_not_name_a_file() {
    // Request mode writes no file, so naming the path the URL implied would be
    // noise pointing at something that does not exist.
    let port = serve_once("500 Internal Server Error", "boom");
    let r = zing(&["-q", "-X", "GET", &format!("http://127.0.0.1:{port}/thing")]);
    assert_ne!(r.code, 0);
    let stderr = r.stderr.replace('\x1b', "");
    assert!(
        !stderr.contains("/lyrics") && !stderr.contains("/thing"),
        "request-mode error should not name a path: {stderr:?}"
    );
    assert!(stderr.contains("500"), "stderr: {stderr:?}");
}

#[test]
fn a_connection_failure_also_exits_non_zero() {
    // Port 1 on loopback refuses connections, which is the other common way an
    // API call goes wrong.
    let r = zing(&["-q", "-X", "GET", "http://127.0.0.1:1/lyrics"]);
    assert_ne!(r.code, 0, "an unreachable host must not exit 0");
    assert!(r.stdout.is_empty(), "stdout: {:?}", r.stdout);
}
