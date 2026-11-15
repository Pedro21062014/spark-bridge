mod server;
mod tunnel;

use rand::Rng;
use serde::Serialize;
use std::sync::Arc;
use tauri::{
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
    AppHandle, Manager, State, WindowEvent,
};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};
use tokio::process::Child;
use tokio::sync::{oneshot, Mutex};

struct BridgeInner {
    running: bool,
    public_url: Option<String>,
    local_url: String,
    token: String,
    background_mode: bool,
    server_shutdown: Option<oneshot::Sender<()>>,
    tunnel_child: Option<Child>,
}

struct AppState(Arc<Mutex<BridgeInner>>);

#[derive(Serialize, Clone)]
pub struct BridgeStatus {
    pub running: bool,
    pub public_url: Option<String>,
    pub local_url: String,
    pub token: String,
    pub background_mode: bool,
}

fn generate_token() -> String {
    let mut rng = rand::thread_rng();
    let chars: Vec<char> = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789"
        .chars()
        .collect();
    let suffix: String = (0..32)
        .map(|_| chars[rng.gen_range(0..chars.len())])
        .collect();
    format!("sb_{}", suffix)
}

#[tauri::command]
async fn get_status(state: State<'_, AppState>) -> Result<BridgeStatus, String> {
    let inner = state.0.lock().await;
    Ok(BridgeStatus {
        running: inner.running,
        public_url: inner.public_url.clone(),
        local_url: inner.local_url.clone(),
        token: inner.token.clone(),
        background_mode: inner.background_mode,
    })
}

#[tauri::command]
async fn set_background_mode(
    state: State<'_, AppState>,
    enabled: bool,
) -> Result<bool, String> {
    let mut inner = state.0.lock().await;
    inner.background_mode = enabled;
    Ok(inner.background_mode)
}

#[tauri::command]
async fn get_autostart(app: AppHandle) -> Result<bool, String> {
    app.autolaunch()
        .is_enabled()
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn set_autostart(app: AppHandle, enabled: bool) -> Result<bool, String> {
    let manager = app.autolaunch();
    if enabled {
        manager.enable().map_err(|e| e.to_string())?;
    } else {
        manager.disable().map_err(|e| e.to_string())?;
    }
    manager.is_enabled().map_err(|e| e.to_string())
}

#[tauri::command]
async fn start_bridge(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<BridgeStatus, String> {
    let port: u16 = 4891;
    let token = {
        let inner = state.0.lock().await;
        if inner.running {
            return Ok(BridgeStatus {
                running: inner.running,
                public_url: inner.public_url.clone(),
                local_url: inner.local_url.clone(),
                token: inner.token.clone(),
                background_mode: inner.background_mode,
            });
        }
        inner.token.clone()
    };

    let (tx, rx) = oneshot::channel::<()>();
    let token_clone = token.clone();
    tokio::spawn(async move {
        if let Err(e) = server::start_http_server(port, token_clone, rx).await {
            eprintln!("HTTP server error: {}", e);
        }
    });

    let app_data_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("Failed to get app data dir: {}", e))?;

    let bin_path = tunnel::ensure_cloudflared(&app_data_dir).await?;
    let (child, public_url) = match tunnel::start_quick_tunnel(&bin_path, port).await {
        Ok(res) => res,
        Err(e) => {
            let _ = tx.send(());
            return Err(e);
        }
    };

    let mut inner = state.0.lock().await;
    inner.running = true;
    inner.public_url = Some(public_url);
    inner.local_url = format!("http://127.0.0.1:{}", port);
    inner.server_shutdown = Some(tx);
    inner.tunnel_child = Some(child);

    Ok(BridgeStatus {
        running: inner.running,
        public_url: inner.public_url.clone(),
        local_url: inner.local_url.clone(),
        token: inner.token.clone(),
        background_mode: inner.background_mode,
    })
}

#[tauri::command]
async fn stop_bridge(state: State<'_, AppState>) -> Result<BridgeStatus, String> {
    let mut inner = state.0.lock().await;
    if let Some(tx) = inner.server_shutdown.take() {
        let _ = tx.send(());
    }
    if let Some(mut child) = inner.tunnel_child.take() {
        let _ = child.kill().await;
    }
    inner.running = false;
    inner.public_url = None;

    Ok(BridgeStatus {
        running: inner.running,
        public_url: inner.public_url.clone(),
        local_url: inner.local_url.clone(),
        token: inner.token.clone(),
        background_mode: inner.background_mode,
    })
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let initial_state = AppState(Arc::new(Mutex::new(BridgeInner {
        running: false,
        public_url: None,
        local_url: "http://127.0.0.1:4891".to_string(),
        token: generate_token(),
        background_mode: true,
        server_shutdown: None,
        tunnel_child: None,
    })));

    tauri::Builder::default()
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            Some(vec!["--minimized"]),
        ))
        .manage(initial_state)
        .setup(|app| {
            let show_i = MenuItem::with_id(app, "show", "Mostrar Janela", true, None::<&str>)?;
            let quit_i = MenuItem::with_id(app, "quit", "Sair", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show_i, &quit_i])?;

            let _tray = TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .menu(&menu)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => {
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                    "quit" => {
                        app.exit(0);
                    }
                    _ => {}
                })
                .build(app)?;

            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                let state = window.state::<AppState>();
                let bg = tauri::async_runtime::block_on(async {
                    state.0.lock().await.background_mode
                });
                if bg {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_status,
            set_background_mode,
            get_autostart,
            set_autostart,
            start_bridge,
            stop_bridge
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
