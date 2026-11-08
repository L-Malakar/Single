//! Single — Tauri shell.
//!
//! This crate is a thin adapter: every command simply forwards to
//! `single-core`, which holds all of the protocol logic (discovery,
//! pairing, encrypted WebDAV, drive mapping). Events from the core are
//! forwarded verbatim to the web UI.

use single_core::SingleApp;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, State};

pub struct AppState {
    pub app: Arc<SingleApp>,
}

type CmdResult<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            // App system folder: ~/.lmalakar/Single (desktop) or the
            // app-private folder on Android.
            let base = if cfg!(target_os = "android") {
                let dir = app
                    .path()
                    .app_data_dir()
                    .expect("no app data dir on android");
                dir.join(".lmalakar").join("Single")
            } else {
                single_core::config::Paths::default_base(None)
            };

            let single = single_core::SingleApp::new(base).map_err(|e| e.to_string())?;

            // forward core events to the UI
            let handle = app.handle().clone();
            let mut rx = single.events.subscribe();
            tauri::async_runtime::spawn(async move {
                loop {
                    match rx.recv().await {
                        Ok(ev) => {
                            let name = ev.name();
                            let _ = handle.emit(name, &ev);
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(_) => break,
                    }
                }
            });

            app.manage(AppState { app: single.clone() });

            // start the network stack (server + discovery + heartbeat)
            tauri::async_runtime::spawn(async move {
                if let Err(e) = single.start().await {
                    log_err(format!("start failed: {e:#}"));
                    let _ = single.events.send(single_core::CoreEvent::Toast {
                        message: format!("Failed to start: {e}"),
                    });
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_app_info,
            set_settings,
            list_devices,
            add_manual,
            pair_start,
            pair_status,
            pair_submit_pin,
            pair_incoming,
            pair_approve,
            pair_deny,
            unpair,
            browse,
            local_mkdir,
            local_rename,
            local_delete,
            peer_mkdir,
            peer_rename,
            peer_delete,
            upload_files,
            download_file,
            list_transfers,
            win_map_drive,
            win_unmap_drive,
            win_list_drives,
            win_drive_support,
            android_root_setup,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

fn log_err(msg: impl std::fmt::Display) {
    eprintln!("[single] {}", msg);
}

// --------------------------------------------------------------- commands

#[tauri::command]
async fn get_app_info(state: State<'_, AppState>) -> CmdResult<single_core::SelfInfo> {
    Ok(state.app.self_info())
}

#[tauri::command]
async fn set_settings(
    state: State<'_, AppState>,
    device_name: Option<String>,
    theme: Option<String>,
    tcp_port: Option<u16>,
    share_dir: Option<String>,
    auto_map_drives: Option<bool>,
    root_mode: Option<bool>,
) -> CmdResult<single_core::SelfInfo> {
    state
        .app
        .set_settings(device_name, theme, tcp_port, share_dir, auto_map_drives, root_mode)
        .await
        .map_err(err)
}

#[tauri::command]
async fn list_devices(state: State<'_, AppState>) -> CmdResult<Vec<single_core::DeviceView>> {
    Ok(state.app.list_devices().await)
}

#[tauri::command]
async fn add_manual(state: State<'_, AppState>, ip: String, port: u16) -> CmdResult<String> {
    let ip: std::net::IpAddr = ip.parse().map_err(|_| "invalid IP address".to_string())?;
    state.app.add_manual(ip, port).await.map_err(err)
}

#[tauri::command]
async fn pair_start(state: State<'_, AppState>, peer_id: String) -> CmdResult<String> {
    state.app.pair_start(&peer_id).await.map_err(err)
}

#[tauri::command]
async fn pair_status(state: State<'_, AppState>, req_id: String) -> CmdResult<String> {
    state.app.pair_status(&req_id).await.map_err(err)
}

#[tauri::command]
async fn pair_submit_pin(
    state: State<'_, AppState>,
    req_id: String,
    pin: String,
) -> CmdResult<String> {
    state.app.pair_submit_pin(&req_id, &pin).await.map_err(err)
}

#[tauri::command]
async fn pair_incoming(
    state: State<'_, AppState>,
) -> CmdResult<Vec<single_core::IncomingReq>> {
    Ok(state.app.pair_incoming().await)
}

#[tauri::command]
async fn pair_approve(state: State<'_, AppState>, req_id: String) -> CmdResult<()> {
    state.app.pair_approve(&req_id).await.map_err(err)
}

#[tauri::command]
async fn pair_deny(state: State<'_, AppState>, req_id: String) -> CmdResult<()> {
    state.app.pair_deny(&req_id).await.map_err(err)
}

#[tauri::command]
async fn unpair(state: State<'_, AppState>, peer_id: String) -> CmdResult<()> {
    state.app.unpair(&peer_id).await.map_err(err)
}

#[tauri::command]
async fn browse(
    state: State<'_, AppState>,
    peer: Option<String>,
    path: String,
) -> CmdResult<Vec<single_core::FsEntry>> {
    match peer {
        None => state.app.browse_local(&path).await.map_err(err),
        Some(peer_id) => state.app.browse_peer(&peer_id, &path).await.map_err(err),
    }
}

#[tauri::command]
async fn local_mkdir(state: State<'_, AppState>, path: String) -> CmdResult<()> {
    state.app.local_mkdir(&path).await.map_err(err)
}

#[tauri::command]
async fn local_rename(state: State<'_, AppState>, path: String, dest: String) -> CmdResult<()> {
    state.app.local_rename(&path, &dest).await.map_err(err)
}

#[tauri::command]
async fn local_delete(state: State<'_, AppState>, path: String) -> CmdResult<()> {
    state.app.local_delete(&path).await.map_err(err)
}

#[tauri::command]
async fn peer_mkdir(
    state: State<'_, AppState>,
    peer_id: String,
    path: String,
) -> CmdResult<()> {
    state.app.peer_mkdir(&peer_id, &path).await.map_err(err)
}

#[tauri::command]
async fn peer_rename(
    state: State<'_, AppState>,
    peer_id: String,
    path: String,
    dest: String,
) -> CmdResult<()> {
    state.app.peer_rename(&peer_id, &path, &dest).await.map_err(err)
}

#[tauri::command]
async fn peer_delete(
    state: State<'_, AppState>,
    peer_id: String,
    path: String,
) -> CmdResult<()> {
    state.app.peer_delete(&peer_id, &path).await.map_err(err)
}

#[tauri::command]
async fn upload_files(
    state: State<'_, AppState>,
    peer_id: String,
    remote_dir: String,
    local_paths: Vec<String>,
) -> CmdResult<Vec<u64>> {
    Ok(state.app.upload_files(&peer_id, &remote_dir, local_paths).await)
}

#[tauri::command]
async fn download_file(
    state: State<'_, AppState>,
    peer_id: String,
    path: String,
    local_dir: Option<String>,
) -> CmdResult<u64> {
    state
        .app
        .download_file(&peer_id, &path, local_dir)
        .await
        .map_err(err)
}

#[tauri::command]
async fn list_transfers(
    state: State<'_, AppState>,
) -> CmdResult<Vec<single_core::TransferSnapshot>> {
    Ok(state.app.transfers.list())
}

#[tauri::command]
async fn win_map_drive(state: State<'_, AppState>, peer_id: String) -> CmdResult<String> {
    state.app.win_map_drive(&peer_id).await.map_err(err)
}

#[tauri::command]
async fn win_unmap_drive(state: State<'_, AppState>, letter: String) -> CmdResult<()> {
    state.app.win_unmap_drive(&letter).await.map_err(err)
}

#[tauri::command]
async fn win_list_drives(state: State<'_, AppState>) -> CmdResult<Vec<(String, String)>> {
    Ok(state.app.win_list_drives())
}

#[tauri::command]
async fn win_drive_support(state: State<'_, AppState>) -> CmdResult<()> {
    state.app.win_drive_support().map_err(err)
}

#[tauri::command]
async fn android_root_setup(state: State<'_, AppState>) -> CmdResult<Vec<String>> {
    state.app.setup_android_root().await.map_err(err)
}

#[allow(unused)]
fn _keep_app_handle(_h: AppHandle) {}
