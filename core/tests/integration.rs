mod common;

use common::{test_payload, RecordingServer, TestServer};
use tokio::sync::broadcast;
use zing_core::connection::ConnectionPool;
use zing_core::downloader::DownloadTask;
use zing_core::engine::event::EventBus;
use zing_core::http_method::{HttpMethod, RequestSpec};
use zing_core::storage::ControlFile;

/// test that the mock server serves the full file on a plain GET
#[tokio::test]
async fn test_server_full_request() {
    let payload = test_payload(64 * 1024);
    let server = TestServer::new(payload.clone()).await;

    let pool = ConnectionPool::new(false, None, 30, 300, None, None, None, None, &[])
        .with_event_bus(EventBus::new());
    let resp = pool.get(&server.url(), 0).await.unwrap();
    let body = resp.resp.bytes().await.unwrap();

    assert_eq!(body.len(), 64 * 1024);
    assert_eq!(&body[..], &payload[..]);
}

/// test 206 Partial Content for a range request
#[tokio::test]
async fn test_server_range_request() {
    let payload = test_payload(1000);
    let server = TestServer::new(payload.clone()).await;

    let pool = ConnectionPool::new(false, None, 30, 300, None, None, None, None, &[])
        .with_event_bus(EventBus::new());
    let resp = pool.get_range(&server.url(), 100, 200, 0).await.unwrap();
    assert_eq!(resp.resp.status(), 206);

    let body = resp.resp.bytes().await.unwrap();
    assert_eq!(body.len(), 200);
    assert_eq!(&body[..], &payload[100..300]);
}

/// test 416 Range Not Satisfiable
#[tokio::test]
async fn test_server_range_out_of_bounds() {
    let payload = test_payload(500);
    let server = TestServer::new(payload.clone()).await;

    let pool = ConnectionPool::new(false, None, 30, 300, None, None, None, None, &[])
        .with_event_bus(EventBus::new());
    let resp = pool.get_range(&server.url(), 1000, 200, 0).await.unwrap();
    assert_eq!(resp.resp.status(), 416);
}

/// Test a full download through DownloadTask
#[tokio::test]
async fn test_full_download() {
    let payload = test_payload(128 * 1024);
    let server = TestServer::new(payload.clone()).await;
    let tmp = tempfile::tempdir().unwrap();
    let output = tmp.path().join("downloaded.bin");

    let bus = EventBus::new();
    let (_shutdown_tx, shutdown_rx) = broadcast::channel::<()>(1);
    let _rx = bus.subscribe();

    let task = DownloadTask::new(
        0,
        &server.url(),
        output.to_str().unwrap(),
        false,
        false,
        Some(4),
        bus,
        false,
        0,
        None,
        vec![],
        None,
        vec![],
        0,
        5,
        500,
        30,
        300,
        None,
        true,
        None,
        None,
        0,
        30,
        5,
        None,
        None,
        None,
        false,
        true,
        true,
        RequestSpec::get(),
    );

    let result = task.run_with_shutdown(shutdown_rx).await;
    assert!(result.is_ok(), "download failed: {:?}", result.err());

    let downloaded = tokio::fs::read(&output).await.unwrap();
    assert_eq!(downloaded.len(), payload.len(), "file size mismatch");
    assert_eq!(downloaded, payload, "file content mismatch");

    // no control file left behind
    let control_path = output.with_extension("zing");
    assert!(!control_path.exists(), "control file should be removed");
}

/// Test resume from a control file with partial progress
#[tokio::test]
async fn test_resume_download() {
    // 128KB = 2 blocks of 64KB
    let payload = test_payload(128 * 1024);
    let server = TestServer::new(payload.clone()).await;
    let tmp = tempfile::tempdir().unwrap();
    let output = tmp.path().join("resume.bin");

    // Simulate first block (64KB) already fully downloaded
    let already = 64 * 1024u64;
    let mut f = tokio::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&output)
        .await
        .unwrap();
    f.set_len(128 * 1024).await.unwrap();
    use tokio::io::AsyncWriteExt;
    f.write_all(&payload[..already as usize]).await.unwrap();
    drop(f);

    // Create control file with block 0 marked complete
    let mut cf = ControlFile::new(128 * 1024, 65536);
    cf.bitfield.mark_complete(0);
    let control_path = ControlFile::control_path(&output);
    cf.save(&control_path).await.unwrap();

    // Run download — should resume block 1
    let bus = EventBus::new();
    let (_shutdown_tx, shutdown_rx) = broadcast::channel::<()>(1);
    let _rx = bus.subscribe();

    let task = DownloadTask::new(
        0,
        &server.url(),
        output.to_str().unwrap(),
        false,
        false,
        Some(2),
        bus,
        false,
        0,
        None,
        vec![],
        None,
        vec![],
        0,
        5,
        500,
        30,
        300,
        None,
        true,
        None,
        None,
        0,
        30,
        5,
        None,
        None,
        None,
        false,
        true,
        true,
        RequestSpec::get(),
    );

    let result = task.run_with_shutdown(shutdown_rx).await;
    assert!(result.is_ok(), "resume download failed: {:?}", result.err());

    let downloaded = tokio::fs::read(&output).await.unwrap();
    assert_eq!(downloaded.len(), payload.len(), "file size mismatch");
    assert_eq!(downloaded, payload, "file content mismatch");

    // control file should be removed on completion
    assert!(
        !control_path.exists(),
        "control file should be removed after resume"
    );
}

/// Test pause and resume mid-download. Uses a throttled server so the pause
/// lands while the download is actually in flight, and asserts that progress
/// resumes after resume() (the "stall at 0 B/s forever" regression).
#[tokio::test]
async fn test_pause_resume_download() {
    let payload = test_payload(4 * 1024 * 1024);
    let server = TestServer::new_throttled(payload.clone(), 20).await;
    let tmp = tempfile::tempdir().unwrap();
    let output = tmp.path().join("pause_resume.bin");

    let bus = EventBus::new();
    let (_shutdown_tx, shutdown_rx) = broadcast::channel::<()>(1);

    let task = std::sync::Arc::new(DownloadTask::new(
        0,
        &server.url(),
        output.to_str().unwrap(),
        false,
        false,
        Some(2),
        bus,
        false,
        0,
        None,
        vec![],
        None,
        vec![],
        0,
        5,
        500,
        30,
        300,
        None,
        true,
        None,
        None,
        0,
        30,
        5,
        None,
        None,
        None,
        false,
        true,
        true,
        RequestSpec::get(),
    ));

    let task_for_run = std::sync::Arc::clone(&task);
    let handle = tokio::spawn(async move { task_for_run.run_with_shutdown(shutdown_rx).await });

    // Wait for download to actually start and make progress.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let snapshot = task.snapshot().await;
    assert!(
        snapshot.bytes_downloaded > 0,
        "download should have started before pause"
    );

    task.pause();
    assert!(task.is_paused(), "should be paused");

    // Give workers time to park; bytes should freeze while paused.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let snapshot = task.snapshot().await;
    let frozen_bytes = snapshot.bytes_downloaded;
    assert!(snapshot.paused, "snapshot should report paused");

    task.resume();
    assert!(!task.is_paused(), "should be resumed");

    // Progress must actually resume after resume(); poll until bytes advance
    // past the frozen value. Regression: stale watch receiver version made
    // every download_range() return Ok(0) instantly, stalling at 0 B/s.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let snapshot = task.snapshot().await;
        if snapshot.bytes_downloaded > frozen_bytes {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "bytes did not advance after resume (stuck at {frozen_bytes})"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    let result = tokio::time::timeout(std::time::Duration::from_secs(20), handle)
        .await
        .expect("timed out waiting for download");
    let result = result.expect("task panicked");
    assert!(result.is_ok(), "download failed: {:?}", result.err());

    let downloaded = tokio::fs::read(&output).await.unwrap();
    assert_eq!(downloaded.len(), payload.len(), "file size mismatch");
    assert_eq!(downloaded, payload, "file content mismatch");
}

/// Regression: a paused task must still exit when shutdown arrives.
///
/// Paused workers used to park on `pause_rx.wait_for(|p| !*p)` and only recheck
/// `done` at the top of their loop, which they never reached while parked. The
/// TUI's `q` therefore hung forever in the graceful-shutdown join and left the
/// terminal in raw/alternate-screen mode.
#[tokio::test]
async fn test_shutdown_while_paused_exits() {
    let payload = test_payload(4 * 1024 * 1024);
    let server = TestServer::new_throttled(payload.clone(), 20).await;
    let tmp = tempfile::tempdir().unwrap();
    let output = tmp.path().join("shutdown_while_paused.bin");

    let bus = EventBus::new();
    let (shutdown_tx, shutdown_rx) = broadcast::channel::<()>(1);

    let task = std::sync::Arc::new(DownloadTask::new(
        0,
        &server.url(),
        output.to_str().unwrap(),
        false,
        false,
        Some(2),
        bus,
        false,
        0,
        None,
        vec![],
        None,
        vec![],
        0,
        5,
        500,
        30,
        300,
        None,
        true,
        None,
        None,
        0,
        30,
        5,
        None,
        None,
        None,
        false,
        true,
        true,
        RequestSpec::get(),
    ));

    let task_for_run = std::sync::Arc::clone(&task);
    let handle = tokio::spawn(async move { task_for_run.run_with_shutdown(shutdown_rx).await });

    // Let the download start so real workers are running and can park.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(
        task.snapshot().await.bytes_downloaded > 0,
        "download should have started before pause"
    );

    task.pause();
    assert!(task.is_paused(), "should be paused");

    // Wait for every worker to park on the pause gate.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    // This is what the TUI does on `q` while a task is paused.
    let _ = shutdown_tx.send(());

    tokio::time::timeout(std::time::Duration::from_secs(10), handle)
        .await
        .expect("paused task did not exit after shutdown (TUI q would hang)")
        .expect("task panicked");
}

/// Test pause → save control file → resume via new task from control file.
#[tokio::test]
async fn test_shutdown_pause_then_resume() {
    let payload = test_payload(4 * 1024 * 1024);
    let server = TestServer::new_throttled(payload.clone(), 20).await;
    let tmp = tempfile::tempdir().unwrap();
    let output = tmp.path().join("shutdown_pause.bin");

    let bus = EventBus::new();
    let (_shutdown_tx, shutdown_rx) = broadcast::channel::<()>(1);

    let task = std::sync::Arc::new(DownloadTask::new(
        0,
        &server.url(),
        output.to_str().unwrap(),
        false,
        false,
        Some(2),
        bus,
        false,
        0,
        None,
        vec![],
        None,
        vec![],
        0,
        5,
        500,
        30,
        300,
        None,
        true,
        None,
        None,
        0,
        30,
        1,
        None,
        None,
        None,
        false,
        true,
        true,
        RequestSpec::get(),
    ));

    let task_for_run = std::sync::Arc::clone(&task);
    let _handle = tokio::spawn(async move { task_for_run.run_with_shutdown(shutdown_rx).await });

    // Wait for download to start
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    // Pause the download via the normal API
    task.pause();
    assert!(task.is_paused(), "should be paused");

    // Wait for workers to park and the periodic save to write the control file
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    // Control file should exist (paused state)
    let control_path = output.with_file_name(format!(
        "{}.zing",
        output.file_name().unwrap().to_str().unwrap()
    ));
    assert!(
        control_path.exists(),
        "control file should exist after pause"
    );

    // Now resume: create a new task from the control file (same as standalone CLI)
    let bus2 = EventBus::new();
    let (_shutdown_tx2, shutdown_rx2) = broadcast::channel::<()>(1);

    let task2 = DownloadTask::new(
        1,
        &server.url(),
        output.to_str().unwrap(),
        false,
        false,
        Some(2),
        bus2,
        false,
        0,
        None,
        vec![],
        None,
        vec![],
        0,
        5,
        500,
        30,
        300,
        None,
        true,
        None,
        None,
        0,
        30,
        5,
        None,
        None,
        None,
        false,
        true,
        true,
        RequestSpec::get(),
    );

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        task2.run_with_shutdown(shutdown_rx2),
    )
    .await
    .expect("timed out waiting for resume");
    assert!(result.is_ok(), "resume failed: {:?}", result.err());

    let downloaded = tokio::fs::read(&output).await.unwrap();
    assert_eq!(downloaded.len(), payload.len(), "file size mismatch");
    assert_eq!(downloaded, payload, "file content mismatch");
}

// ---------------------------------------------------------------------------
// HTTP method support
// ---------------------------------------------------------------------------

/// Build a task with an explicit request spec, using test-friendly defaults.
#[allow(clippy::too_many_arguments)]
fn task_with_spec(
    url: &str,
    filename: &str,
    bus: EventBus,
    spec: RequestSpec,
    headers: Vec<(String, String)>,
) -> DownloadTask {
    DownloadTask::new(
        0,
        url,
        filename,
        false,
        false,
        Some(4),
        bus,
        false,
        0,
        None,
        vec![],
        None,
        headers,
        0,
        5,
        500,
        30,
        300,
        None,
        true,
        None,
        None,
        0,
        30,
        5,
        None,
        None,
        None,
        false,
        true,
        true,
        spec,
    )
}

#[tokio::test]
async fn test_head_method_sends_single_head_request() {
    let server = RecordingServer::start().await;
    let tmp = tempfile::tempdir().unwrap();
    let output = tmp.path().join("head.bin");
    let bus = EventBus::new();

    let task = task_with_spec(
        &server.url(),
        output.to_str().unwrap(),
        bus,
        RequestSpec::with_body(HttpMethod::parse("HEAD").unwrap(), None, None),
        vec![],
    );
    task.run().await.expect("HEAD should succeed");

    let reqs = server.recorded();
    assert!(!reqs.is_empty(), "server saw no requests");
    for r in &reqs {
        assert_eq!(r.method, "HEAD", "expected HEAD, got {}", r.method);
        assert!(
            r.header("Range").is_none(),
            "HEAD must not carry a Range header: {:?}",
            r.header("Range")
        );
    }
}

#[tokio::test]
async fn test_post_sends_body_and_content_type_once() {
    let server = RecordingServer::start().await;
    let tmp = tempfile::tempdir().unwrap();
    let output = tmp.path().join("post.bin");
    let bus = EventBus::new();

    let spec = RequestSpec::with_body(
        HttpMethod::parse("POST").unwrap(),
        Some(b"q=foo&limit=10".to_vec()),
        Some("application/x-www-form-urlencoded".into()),
    );
    let task = task_with_spec(&server.url(), output.to_str().unwrap(), bus, spec, vec![]);
    task.run().await.expect("POST should succeed");

    let reqs = server.recorded();
    assert_eq!(reqs.len(), 1, "POST must be sent exactly once: {reqs:?}");
    assert_eq!(reqs[0].method, "POST");
    assert_eq!(reqs[0].body, b"q=foo&limit=10");
    assert_eq!(
        reqs[0].header("Content-Type"),
        Some("application/x-www-form-urlencoded")
    );
    assert!(
        reqs[0].header("Range").is_none(),
        "non-GET must never be segmented"
    );
    assert_eq!(
        std::fs::read(&output).unwrap(),
        b"hello",
        "response body should be written to the output file"
    );
}

#[tokio::test]
async fn test_delete_does_not_create_empty_file() {
    let server = RecordingServer::with_response("204 No Content", Vec::new()).await;
    let tmp = tempfile::tempdir().unwrap();
    let output = tmp.path().join("deleted.bin");
    let bus = EventBus::new();

    let spec = RequestSpec::with_body(HttpMethod::parse("DELETE").unwrap(), None, None);
    let task = task_with_spec(&server.url(), output.to_str().unwrap(), bus, spec, vec![]);
    task.run().await.expect("DELETE should succeed");

    let reqs = server.recorded();
    assert_eq!(reqs.len(), 1, "DELETE must be sent exactly once");
    assert_eq!(reqs[0].method, "DELETE");
    assert!(
        !output.exists(),
        "empty response must not leave a zero-byte file behind"
    );
}

#[tokio::test]
async fn test_non_idempotent_method_is_not_retried() {
    let server =
        RecordingServer::with_response("500 Internal Server Error", b"boom".to_vec()).await;
    let tmp = tempfile::tempdir().unwrap();
    let output = tmp.path().join("fail.bin");
    let bus = EventBus::new();

    let spec = RequestSpec::with_body(HttpMethod::parse("POST").unwrap(), None, None);
    let task = task_with_spec(&server.url(), output.to_str().unwrap(), bus, spec, vec![]);
    let result = task.run().await;

    assert!(result.is_err(), "HTTP 500 should fail the task");
    assert_eq!(
        server.request_count(),
        1,
        "a non-idempotent request must never be retried"
    );
}

#[tokio::test]
async fn test_query_method_is_sent_as_query() {
    let server = RecordingServer::start().await;
    let tmp = tempfile::tempdir().unwrap();
    let output = tmp.path().join("query.json");
    let bus = EventBus::new();

    // QUERY is range-capable and idempotent (RFC 10008) but must not be probed,
    // since a probe would be a second execution of the request.
    let spec = RequestSpec::with_body(HttpMethod::parse("QUERY").unwrap(), None, None);
    let task = task_with_spec(&server.url(), output.to_str().unwrap(), bus, spec, vec![]);
    task.run().await.expect("QUERY should succeed");

    let reqs = server.recorded();
    assert_eq!(reqs.len(), 1, "QUERY must be sent exactly once: {reqs:?}");
    assert_eq!(reqs[0].method, "QUERY");
}

#[tokio::test]
async fn test_get_still_uses_ranges_and_segments() {
    let payload = test_payload(4 * 1024 * 1024);
    let server = TestServer::new(payload.clone()).await;
    let tmp = tempfile::tempdir().unwrap();
    let output = tmp.path().join("segmented.bin");
    let bus = EventBus::new();

    let task = task_with_spec(
        &server.url(),
        output.to_str().unwrap(),
        bus,
        RequestSpec::get(),
        vec![],
    );
    task.run().await.expect("GET download should succeed");

    assert_eq!(std::fs::read(&output).unwrap(), payload);
}

#[tokio::test]
async fn test_probe_sends_custom_headers() {
    let payload = test_payload(1024);
    let server = RecordingServer::with_response("200 OK", payload.clone()).await;
    let tmp = tempfile::tempdir().unwrap();
    let output = tmp.path().join("probed.bin");
    let bus = EventBus::new();

    // Regression: probe used `pool.client()` and skipped the header list, so an
    // authenticated resource answered 401 and the size came back unknown.
    let headers = vec![("X-Probe-Token".to_string(), "secret".to_string())];
    let task = task_with_spec(
        &server.url(),
        output.to_str().unwrap(),
        bus,
        RequestSpec::get(),
        headers,
    );
    task.run().await.expect("download should succeed");

    let reqs = server.recorded();
    let probe = reqs
        .first()
        .expect("expected at least one request (the probe)");
    assert_eq!(
        probe.header("X-Probe-Token"),
        Some("secret"),
        "probe request must carry custom headers"
    );
}
