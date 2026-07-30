mod common;

use common::{test_payload, TestServer};
use tokio::sync::broadcast;
use zing_core::connection::ConnectionPool;
use zing_core::downloader::DownloadTask;
use zing_core::engine::event::EventBus;
use zing_core::storage::ControlFile;

/// test that the mock server serves the full file on a plain GET
#[tokio::test]
async fn test_server_full_request() {
    let payload = test_payload(64 * 1024);
    let server = TestServer::new(payload.clone()).await;

    let pool =
        ConnectionPool::new(false, None, 30, 300, None, None).with_event_bus(EventBus::new());
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

    let pool =
        ConnectionPool::new(false, None, 30, 300, None, None).with_event_bus(EventBus::new());
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

    let pool =
        ConnectionPool::new(false, None, 30, 300, None, None).with_event_bus(EventBus::new());
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
        4,
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
        2,
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
