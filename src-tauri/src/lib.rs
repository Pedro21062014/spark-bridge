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
use tokio::sync::{oneshot, Mutex, RwLock};

struct BridgeInner {
    running: bool,
    public_url: Option<String>,
    local_url: String,
    token: Arc<RwLock<String>>,
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

#[derive(Serialize, Clone)]
pub struct TestResult {
    pub ok: bool,
    pub message: String,
    pub latency_ms: u64,
    pub tools_count: Option<usize>,
    pub server_version: Option<String>,
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

async fn snapshot(inner: &BridgeInner) -> BridgeStatus {
    BridgeStatus {
        running: inner.running,
        public_url: inner.public_url.clone(),
        local_url: inner.local_url.clone(),
        token: inner.token.read().await.clone(),
        background_mode: inner.background_mode,
    }
}

#[tauri::command]
async fn get_status(state: State<'_, AppState>) -> Result<BridgeStatus, String> {
    let inner = state.0.lock().await;
    Ok(snapshot(&inner).await)
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
async fn regenerate_token(state: State<'_, AppState>) -> Result<BridgeStatus, String> {
    let inner = state.0.lock().await;
    *inner.token.write().await = generate_token();
    Ok(snapshot(&inner).await)
}

#[tauri::command]
async fn test_connection(state: State<'_, AppState>) -> Result<TestResult, String> {
    let (running, token) = {
        let inner = state.0.lock().await;
        let token = inner.token.read().await.clone();
        (inner.running, token)
    };

    if !running {
        return Ok(TestResult {
            ok: false,
            message: "Conexao nao iniciada. Clique em Iniciar Conexao primeiro.".into(),
            latency_ms: 0,
            tools_count: None,
            server_version: None,
        });
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| e.to_string())?;

    let url = "http://127.0.0.1:4891/mcp";
    let start = std::time::Instant::now();

    let init_body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-03-26",
            "capabilities": {},
            "clientInfo": { "name": "spark-bridge-ui", "version": env!("CARGO_PKG_VERSION") }
        }
    });

    let resp = client
        .post(url)
        .query(&[("token", token.as_str())])
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .json(&init_body)
        .send()
        .await
        .map_err(|e| format!("Falha de rede ao contatar o servidor local: {}", e))?;

    let latency_ms = start.elapsed().as_millis() as u64;

    if !resp.status().is_success() {
        return Ok(TestResult {
            ok: false,
            message: format!("Servidor respondeu HTTP {}", resp.status()),
            latency_ms,
            tools_count: None,
            server_version: None,
        });
    }

    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("Resposta invalida do servidor: {}", e))?;

    let server_version = body
        .get("result")
        .and_then(|r| r.get("serverInfo"))
        .and_then(|s| s.get("version"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    // Confirma que tools/list tambem funciona (handshake completo).
    let list_body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list",
        "params": {}
    });

    let resp2 = client
        .post(url)
        .query(&[("token", token.as_str())])
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .json(&list_body)
        .send()
        .await
        .map_err(|e| e.to_string())?;

    let body2: serde_json::Value = resp2.json().await.unwrap_or(serde_json::json!({}));
    let tools_count = body2
        .get("result")
        .and_then(|r| r.get("tools"))
        .and_then(|t| t.as_array())
        .map(|a| a.len());

    Ok(TestResult {
        ok: true,
        message: "Conexao MCP estabelecida com sucesso".into(),
        latency_ms,
        tools_count,
        server_version,
    })
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
    let token_arc = {
        let inner = state.0.lock().await;
        if inner.running {
            return Ok(snapshot(&inner).await);
        }
        inner.token.clone()
    };

    let (tx, rx) = oneshot::channel::<()>();
    tokio::spawn(async move {
        if let Err(e) = server::start_http_server(port, token_arc, rx).await {
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

    Ok(snapshot(&inner).await)
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

    Ok(snapshot(&inner).await)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let initial_state = AppState(Arc::new(Mutex::new(BridgeInner {
        running: false,
        public_url: None,
        local_url: "http://127.0.0.1:4891".to_string(),
        token: Arc::new(RwLock::new(generate_token())),
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
            regenerate_token,
            test_connection,
            get_autostart,
            set_autostart,
            start_bridge,
            stop_bridge
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
