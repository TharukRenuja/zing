mod common;

use common::{test_payload, TestServer};
use rxcore::connection::ConnectionPool;
use rxcore::downloader::DownloadTask;
use rxcore::engine::event::EventBus;
use rxcore::storage::{ControlFile, SegmentEntry};
use tokio::sync::broadcast;

/// test that the mock server serves the full file on a plain GET
#[tokio::test]
async fn test_server_full_request() {
    let payload = test_payload(64 * 1024);
    let server = TestServer::new(payload.clone()).await;

    let pool = ConnectionPool::new(false, None).with_event_bus(EventBus::new());
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

    let pool = ConnectionPool::new(false, None).with_event_bus(EventBus::new());
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

    let pool = ConnectionPool::new(false, None).with_event_bus(EventBus::new());
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
        4,
        bus,
        false,
        0,
        None,
        vec![],
        None,
    );

    let result = task.run_with_shutdown(shutdown_rx).await;
    assert!(result.is_ok(), "download failed: {:?}", result.err());

    let downloaded = tokio::fs::read(&output).await.unwrap();
    assert_eq!(downloaded.len(), payload.len(), "file size mismatch");
    assert_eq!(downloaded, payload, "file content mismatch");

    // no control file left behind
    let control_path = output.with_extension("rxdl");
    assert!(!control_path.exists(), "control file should be removed");
}

/// Test resume from a control file with partial progress
#[tokio::test]
async fn test_resume_download() {
    let payload = test_payload(64 * 1024);
    let server = TestServer::new(payload.clone()).await;
    let tmp = tempfile::tempdir().unwrap();
    let output = tmp.path().join("resume.bin");

    // Simulate 30KB already downloaded: pre-allocate file and write control file
    let already = 30 * 1024u64;
    let mut f = tokio::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&output)
        .await
        .unwrap();
    f.set_len(64 * 1024).await.unwrap();
    // Write the first 30KB correctly
    use tokio::io::AsyncWriteExt;
    f.write_all(&payload[..already as usize]).await.unwrap();
    drop(f);

    // Create control file with partial segment
    let cf = ControlFile {
        version: 1,
        url: server.url(),
        total_size: Some(64 * 1024),
        filename: output.to_str().unwrap().to_string(),
        segments: vec![SegmentEntry {
            id: 0,
            offset: 0,
            length: 64 * 1024,
            downloaded: already,
        }],
        metadata: std::collections::HashMap::new(),
        base_downloaded: 0,
    };
    let control_path = ControlFile::control_path(&output);
    cf.save(&control_path).await.unwrap();

    // Run download — should resume from 30KB
    let bus = EventBus::new();
    let (_shutdown_tx, shutdown_rx) = broadcast::channel::<()>(1);
    let _rx = bus.subscribe();

    let task = DownloadTask::new(
        0,
        &server.url(),
        output.to_str().unwrap(),
        false,
        2,
        bus,
        false,
        0,
        None,
        vec![],
        None,
    );

    let result = task.run_with_shutdown(shutdown_rx).await;
    assert!(result.is_ok(), "resume download failed: {:?}", result.err());

    let downloaded = tokio::fs::read(&output).await.unwrap();
    assert_eq!(downloaded.len(), payload.len(), "file size mismatch");
    assert_eq!(downloaded, payload, "file content mismatch");

    // control file should be removed on completion
    assert!(!control_path.exists(), "control file should be removed after resume");
}
