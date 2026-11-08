//! End-to-end integration test: two SingleApp instances on localhost go
//! through discovery (manual add), PIN + approval pairing, browsing,
//! upload and download — the full encrypted stack.

use single_core::pairing::ReqState;
use single_core::{SingleApp, TransferSnapshot};
use std::time::Duration;

async fn wait_for<F: FnMut() -> bool>(mut f: F, secs: u64) -> bool {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    while tokio::time::Instant::now() < deadline {
        if f() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    f()
}

fn tmp_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("single-it-{}-{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[tokio::test(flavor = "multi_thread")]
async fn two_devices_pair_and_transfer() {
    // --- two devices ------------------------------------------------------
    let dir_a = tmp_dir("a");
    let dir_b = tmp_dir("b");
    std::env::set_var("SINGLE_DATA_DIR", &dir_a);
    let app_a = SingleApp::new(dir_a.join(".lmalakar").join("Single")).unwrap();
    std::env::set_var("SINGLE_DATA_DIR", &dir_b);
    let app_b = SingleApp::new(dir_b.join(".lmalakar").join("Single")).unwrap();
    std::env::remove_var("SINGLE_DATA_DIR");

    // different ports so both servers can bind on localhost
    app_a.set_settings(Some("Device-A".into()), None, Some(27580), None, None, None).await.unwrap();
    app_b.set_settings(Some("Device-B".into()), None, Some(27581), None, None, None).await.unwrap();
    app_a.start().await.unwrap();
    app_b.start().await.unwrap();

    // --- B shares a file ---------------------------------------------------
    let share_b = app_b.share_root();
    std::fs::create_dir_all(&share_b).unwrap();
    let payload = b"hello from B".repeat(1000);
    std::fs::write(share_b.join("hello.txt"), &payload).unwrap();
    std::fs::create_dir_all(share_b.join("sub dir")).unwrap();
    std::fs::write(share_b.join("sub dir").join("nested&file.txt"), "nested").unwrap();

    // --- A adds B manually (discovery fallback) ----------------------------
    let name = app_a.add_manual("127.0.0.1".parse().unwrap(), 27581).await.unwrap();
    assert_eq!(name, "Device-B");

    // --- pairing ------------------------------------------------------------
    let b_id = {
        let devices = app_a.list_devices().await;
        let b = devices.iter().find(|d| d.name == "Device-B").expect("B visible to A");
        assert!(!b.paired);
        b.id.clone()
    };

    let req_id = app_a.pair_start(&b_id).await.unwrap();
    assert!(!req_id.is_empty());

    // B sees the incoming request with a PIN
    let incoming = app_b.pair_incoming().await;
    assert_eq!(incoming.len(), 1);
    assert_eq!(incoming[0].state, ReqState::Pending);
    assert_eq!(incoming[0].pin.len(), 6);
    let pin = incoming[0].pin.clone();

    // B approves, A submits the PIN
    app_b.pair_approve(&req_id).await.unwrap();
    let status = app_a.pair_submit_pin(&req_id, &pin).await.unwrap();
    assert_eq!(status, "verified");

    // pairing stored on both sides
    assert!(app_a.pairs.pairing_by_id(&b_id).await.is_some());
    // B's pairing: find A's id
    let a_id = app_b.pairs.list_pairings().await[0].id.clone();
    assert!(app_b.pairs.pairing_by_id(&a_id).await.is_some());

    // --- browse B from A ----------------------------------------------------
    let entries = app_a.browse_peer(&b_id, "").await.unwrap();
    assert!(entries.iter().any(|e| e.name == "hello.txt" && e.size == payload.len() as u64));
    assert!(entries.iter().any(|e| e.name == "sub dir" && e.is_dir));

    let nested = app_a.browse_peer(&b_id, "sub dir").await.unwrap();
    assert!(nested.iter().any(|e| e.name == "nested&file.txt"));

    // --- download B's file to A ----------------------------------------------
    let tid = app_a.download_file(&b_id, "hello.txt", None).await.unwrap();
    let done = wait_for(
        || {
            app_a.transfers.list().iter().any(|t: &TransferSnapshot| t.id == tid && t.state == "done")
        },
        10,
    )
    .await;
    assert!(done, "download did not finish");
    let downloaded = std::fs::read(dir_a.join(".lmalakar").join("Single").join("Downloads").join("hello.txt")).unwrap();
    assert_eq!(downloaded, payload);

    // --- upload A's file to B --------------------------------------------------
    let up = dir_a.join("upload-me.txt");
    std::fs::write(&up, b"hello from A").unwrap();
    let ids = app_a.upload_files(&b_id, "", vec![up.to_string_lossy().to_string()]).await;
    assert_eq!(ids.len(), 1);
    let up_done = wait_for(
        || {
            app_a.transfers.list().iter().any(|t| t.id == ids[0] && t.state == "done")
        },
        10,
    )
    .await;
    assert!(up_done, "upload did not finish");
    assert_eq!(std::fs::read(share_b.join("upload-me.txt")).unwrap(), b"hello from A");

    // --- remote fs ops ----------------------------------------------------------
    app_a.peer_mkdir(&b_id, "made-by-a").await.unwrap();
    assert!(share_b.join("made-by-a").is_dir());
    app_a.peer_rename(&b_id, "upload-me.txt", "upload-renamed.txt").await.unwrap();
    assert!(share_b.join("upload-renamed.txt").is_file());
    app_a.peer_delete(&b_id, "upload-renamed.txt").await.unwrap();
    assert!(!share_b.join("upload-renamed.txt").exists());

    // --- local fs ops guard traversal -------------------------------------------
    assert!(app_a.local_mkdir("ok-dir").await.is_ok());
    assert!(app_a.local_mkdir("../escape").await.is_err());

    // --- unpair -------------------------------------------------------------------
    app_a.unpair(&b_id).await.unwrap();
    let mut unpaired = false;
    for _ in 0..40 {
        if app_b.pairs.list_pairings().await.is_empty() {
            unpaired = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    assert!(unpaired, "B should have removed the pairing after unpair");

    // cleanup
    let _ = std::fs::remove_dir_all(&dir_a);
    let _ = std::fs::remove_dir_all(&dir_b);
}
