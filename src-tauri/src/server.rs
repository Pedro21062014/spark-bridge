use axum::{
    body::Bytes,
    extract::{Request, State},
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Json, Response, Sse},
    routing::{get, post},
    Router,
};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use enigo::{Button, Coordinate, Direction, Enigo, Key, Keyboard, Mouse, Settings};
use image::{ImageBuffer, Rgba};
use serde::{Deserialize, Serialize};
use std::io::Cursor;
use std::process::Stdio;
use std::sync::Arc;
use tokio::process::Command;
use tokio::sync::{oneshot, RwLock};
use tower_http::cors::{Any, CorsLayer};
use xcap::Monitor;

#[derive(Clone)]
pub struct ServerState {
    pub token: Arc<RwLock<String>>,
}

/// Logo oficial do Spark Bridge (embutido no binario).
/// Servido em GET /logo.png e GET /icon.png para clientes MCP/agentes
/// que solicitem o icone do servidor.
const LOGO_PNG: &[u8] = include_bytes!("../icons/icon.png");

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
    version: &'static str,
    logo: &'static str,
}

#[derive(Serialize)]
struct ScreenResponse {
    width: u32,
    height: u32,
    format: &'static str,
    image_base64: String,
}

#[derive(Deserialize)]
pub struct MouseMoveReq {
    pub x: i32,
    pub y: i32,
}

#[derive(Deserialize)]
pub struct MouseClickReq {
    pub x: Option<i32>,
    pub y: Option<i32>,
    pub button: Option<String>,
    pub double: Option<bool>,
}

#[derive(Deserialize)]
pub struct MouseScrollReq {
    pub delta_x: Option<i32>,
    pub delta_y: Option<i32>,
}

#[derive(Deserialize)]
pub struct KeyboardTypeReq {
    pub text: String,
}

#[derive(Deserialize)]
pub struct KeyboardKeyReq {
    pub key: String,
    pub modifiers: Option<Vec<String>>,
}

#[derive(Deserialize)]
pub struct TerminalExecReq {
    pub command: String,
    pub cwd: Option<String>,
}

#[derive(Serialize)]
struct TerminalExecRes {
    stdout: String,
    stderr: String,
    exit_code: i32,
}

#[derive(Serialize)]
struct ActionRes {
    ok: bool,
    message: String,
}

fn unauthorized_response() -> Response {
    let mut resp = (
        StatusCode::UNAUTHORIZED,
        Json(serde_json::json!({
            "error": "unauthorized",
            "message": "Token invalido ou ausente. Use ?token=... na URL ou o header Authorization: Bearer ..."
        })),
    )
        .into_response();
    resp.headers_mut().insert(
        header::WWW_AUTHENTICATE,
        HeaderValue::from_static("Bearer"),
    );
    resp
}

async fn auth_middleware(
    State(state): State<ServerState>,
    headers: HeaderMap,
    req: Request,
    next: Next,
) -> Result<Response, Response> {
    let path = req.uri().path().to_string();

    // Preflight CORS e rotas publicas nao exigem autenticacao.
    if req.method() == Method::OPTIONS
        || path == "/health"
        || path == "/openapi.json"
        || path == "/logo.png"
        || path == "/icon.png"
    {
        return Ok(next.run(req).await);
    }

    let query_token = req.uri().query().and_then(|q| {
        q.split('&').find_map(|pair| {
            let mut parts = pair.splitn(2, '=');
            if parts.next() == Some("token") {
                parts.next()
            } else {
                None
            }
        })
    });

    let bearer_token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer ").map(|s| s.to_string()));

    let api_key = headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let provided = query_token.map(|s| s.to_string()).or(bearer_token).or(api_key);
    let expected = state.token.read().await.clone();

    match provided {
        Some(t) if t == expected => Ok(next.run(req).await),
        _ => Err(unauthorized_response()),
    }
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
        logo: "/logo.png",
    })
}

/// Serve o logo oficial do Spark Bridge (PNG embutido no binario).
/// Qualquer cliente MCP/agente que solicite o icone do servidor
/// recebe este arquivo em GET /logo.png (ou /icon.png).
async fn logo() -> Response {
    let mut resp = Bytes::from_static(LOGO_PNG).into_response();
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("image/png"),
    );
    resp.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=86400"),
    );
    resp
}

async fn openapi_spec() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "openapi": "3.0.3",
        "info": {
            "title": "Spark Bridge PC Agent API",
            "version": env!("CARGO_PKG_VERSION"),
            "description": "Control screen, mouse, keyboard, and terminal on the user's PC. MCP Streamable HTTP endpoint: POST /mcp",
            "x-logo": { "url": "/logo.png" }
        },
        "components": {
            "securitySchemes": {
                "bearerAuth": {
                    "type": "http",
                    "scheme": "bearer"
                }
            }
        },
        "security": [{ "bearerAuth": [] }],
        "paths": {
            "/screen": {
                "get": {
                    "summary": "Capture primary screen as base64 PNG",
                    "responses": { "200": { "description": "Screenshot captured" } }
                }
            },
            "/mouse/move": {
                "post": {
                    "summary": "Move mouse cursor to (x, y)",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object", "properties": { "x": { "type": "integer" }, "y": { "type": "integer" } }, "required": ["x", "y"] } } }
                    },
                    "responses": { "200": { "description": "Mouse moved" } }
                }
            },
            "/mouse/click": {
                "post": {
                    "summary": "Click mouse button (left, right, middle)",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object", "properties": { "x": { "type": "integer" }, "y": { "type": "integer" }, "button": { "type": "string" }, "double": { "type": "boolean" } } } } }
                    },
                    "responses": { "200": { "description": "Mouse clicked" } }
                }
            },
            "/mouse/scroll": {
                "post": {
                    "summary": "Scroll mouse wheel",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object", "properties": { "delta_x": { "type": "integer" }, "delta_y": { "type": "integer" } } } } }
                    },
                    "responses": { "200": { "description": "Scrolled" } }
                }
            },
            "/keyboard/type": {
                "post": {
                    "summary": "Type text",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object", "properties": { "text": { "type": "string" } }, "required": ["text"] } } }
                    },
                    "responses": { "200": { "description": "Text typed" } }
                }
            },
            "/keyboard/key": {
                "post": {
                    "summary": "Press key with optional modifiers (ctrl, alt, shift, meta)",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object", "properties": { "key": { "type": "string" }, "modifiers": { "type": "array", "items": { "type": "string" } } }, "required": ["key"] } } }
                    },
                    "responses": { "200": { "description": "Key pressed" } }
                }
            },
            "/terminal/exec": {
                "post": {
                    "summary": "Execute a terminal command",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object", "properties": { "command": { "type": "string" }, "cwd": { "type": "string" } }, "required": ["command"] } } }
                    },
                    "responses": { "200": { "description": "Command output" } }
                }
            }
        }
    }))
}

async fn get_screen() -> impl IntoResponse {
    let res = tokio::task::spawn_blocking(|| -> Result<ScreenResponse, String> {
        let monitors = Monitor::all().map_err(|e| format!("Failed to list monitors: {}", e))?;
        let monitor = monitors
            .into_iter()
            .find(|m| m.is_primary().unwrap_or(false))
            .or_else(|| Monitor::all().ok()?.into_iter().next())
            .ok_or_else(|| "No monitor found".to_string())?;

        let img = monitor
            .capture_image()
            .map_err(|e| format!("Failed to capture screen: {}", e))?;

        let width = img.width();
        let height = img.height();
        let rgba: ImageBuffer<Rgba<u8>, Vec<u8>> =
            ImageBuffer::from_raw(width, height, img.into_raw())
                .ok_or_else(|| "Failed to build image buffer".to_string())?;

        let mut png_bytes: Vec<u8> = Vec::new();
        let mut cursor = Cursor::new(&mut png_bytes);
        rgba.write_to(&mut cursor, image::ImageFormat::Png)
            .map_err(|e| format!("Failed to encode PNG: {}", e))?;

        Ok(ScreenResponse {
            width,
            height,
            format: "png",
            image_base64: BASE64.encode(&png_bytes),
        })
    })
    .await;

    match res {
        Ok(Ok(data)) => (StatusCode::OK, Json(serde_json::to_value(data).unwrap())).into_response(),
        Ok(Err(e)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

async fn mouse_move(Json(req): Json<MouseMoveReq>) -> impl IntoResponse {
    let res = tokio::task::spawn_blocking(move || -> Result<(), String> {
        let mut enigo = Enigo::new(&Settings::default()).map_err(|e| e.to_string())?;
        enigo
            .move_mouse(req.x, req.y, Coordinate::Abs)
            .map_err(|e| e.to_string())?;
        Ok(())
    })
    .await;

    match res {
        Ok(Ok(())) => (
            StatusCode::OK,
            Json(ActionRes {
                ok: true,
                message: "Mouse moved".into(),
            }),
        )
            .into_response(),
        Ok(Err(e)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ActionRes {
                ok: false,
                message: e,
            }),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ActionRes {
                ok: false,
                message: e.to_string(),
            }),
        )
            .into_response(),
    }
}

async fn mouse_click(Json(req): Json<MouseClickReq>) -> impl IntoResponse {
    let res = tokio::task::spawn_blocking(move || -> Result<(), String> {
        let mut enigo = Enigo::new(&Settings::default()).map_err(|e| e.to_string())?;
        if let (Some(x), Some(y)) = (req.x, req.y) {
            enigo
                .move_mouse(x, y, Coordinate::Abs)
                .map_err(|e| e.to_string())?;
        }
        let btn = match req.button.as_deref() {
            Some("right") => Button::Right,
            Some("middle") => Button::Middle,
            _ => Button::Left,
        };
        enigo
            .button(btn, Direction::Click)
            .map_err(|e| e.to_string())?;
        if req.double.unwrap_or(false) {
            enigo
                .button(btn, Direction::Click)
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    })
    .await;

    match res {
        Ok(Ok(())) => (
            StatusCode::OK,
            Json(ActionRes {
                ok: true,
                message: "Mouse clicked".into(),
            }),
        )
            .into_response(),
        Ok(Err(e)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ActionRes {
                ok: false,
                message: e,
            }),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ActionRes {
                ok: false,
                message: e.to_string(),
            }),
        )
            .into_response(),
    }
}

async fn mouse_scroll(Json(req): Json<MouseScrollReq>) -> impl IntoResponse {
    let res = tokio::task::spawn_blocking(move || -> Result<(), String> {
        let mut enigo = Enigo::new(&Settings::default()).map_err(|e| e.to_string())?;
        if let Some(dy) = req.delta_y {
            if dy != 0 {
                enigo
                    .scroll(dy, enigo::Axis::Vertical)
                    .map_err(|e| e.to_string())?;
            }
        }
        if let Some(dx) = req.delta_x {
            if dx != 0 {
                enigo
                    .scroll(dx, enigo::Axis::Horizontal)
                    .map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    })
    .await;

    match res {
        Ok(Ok(())) => (
            StatusCode::OK,
            Json(ActionRes {
                ok: true,
                message: "Scrolled".into(),
            }),
        )
            .into_response(),
        Ok(Err(e)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ActionRes {
                ok: false,
                message: e,
            }),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ActionRes {
                ok: false,
                message: e.to_string(),
            }),
        )
            .into_response(),
    }
}

async fn keyboard_type(Json(req): Json<KeyboardTypeReq>) -> impl IntoResponse {
    let res = tokio::task::spawn_blocking(move || -> Result<(), String> {
        let mut enigo = Enigo::new(&Settings::default()).map_err(|e| e.to_string())?;
        enigo.text(&req.text).map_err(|e| e.to_string())?;
        Ok(())
    })
    .await;

    match res {
        Ok(Ok(())) => (
            StatusCode::OK,
            Json(ActionRes {
                ok: true,
                message: "Typed text".into(),
            }),
        )
            .into_response(),
        Ok(Err(e)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ActionRes {
                ok: false,
                message: e,
            }),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ActionRes {
                ok: false,
                message: e.to_string(),
            }),
        )
            .into_response(),
    }
}

fn parse_key(k: &str) -> Option<Key> {
    match k.to_lowercase().as_str() {
        "enter" | "return" => Some(Key::Return),
        "tab" => Some(Key::Tab),
        "space" | " " => Some(Key::Space),
        "backspace" => Some(Key::Backspace),
        "escape" | "esc" => Some(Key::Escape),
        "delete" | "del" => Some(Key::Delete),
        "up" | "arrowup" => Some(Key::UpArrow),
        "down" | "arrowdown" => Some(Key::DownArrow),
        "left" | "arrowleft" => Some(Key::LeftArrow),
        "right" | "arrowright" => Some(Key::RightArrow),
        "home" => Some(Key::Home),
        "end" => Some(Key::End),
        "pageup" => Some(Key::PageUp),
        "pagedown" => Some(Key::PageDown),
        "ctrl" | "control" => Some(Key::Control),
        "alt" => Some(Key::Alt),
        "shift" => Some(Key::Shift),
        "meta" | "win" | "cmd" | "super" => Some(Key::Meta),
        s if s.chars().count() == 1 => Some(Key::Unicode(s.chars().next().unwrap())),
        _ => None,
    }
}

async fn keyboard_key(Json(req): Json<KeyboardKeyReq>) -> impl IntoResponse {
    let res = tokio::task::spawn_blocking(move || -> Result<(), String> {
        let mut enigo = Enigo::new(&Settings::default()).map_err(|e| e.to_string())?;
        let mut mods = Vec::new();
        if let Some(m_list) = req.modifiers {
            for m in m_list {
                if let Some(k) = parse_key(&m) {
                    enigo.key(k, Direction::Press).map_err(|e| e.to_string())?;
                    mods.push(k);
                }
            }
        }
        let main_key = parse_key(&req.key).ok_or_else(|| format!("Unknown key: {}", req.key))?;
        enigo
            .key(main_key, Direction::Click)
            .map_err(|e| e.to_string())?;
        for k in mods.into_iter().rev() {
            let _ = enigo.key(k, Direction::Release);
        }
        Ok(())
    })
    .await;

    match res {
        Ok(Ok(())) => (
            StatusCode::OK,
            Json(ActionRes {
                ok: true,
                message: "Key pressed".into(),
            }),
        )
            .into_response(),
        Ok(Err(e)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ActionRes {
                ok: false,
                message: e,
            }),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ActionRes {
                ok: false,
                message: e.to_string(),
            }),
        )
            .into_response(),
    }
}

async fn terminal_exec(Json(req): Json<TerminalExecReq>) -> impl IntoResponse {
    let mut cmd = if cfg!(target_os = "windows") {
        let mut c = Command::new("cmd");
        c.args(["/C", &req.command]);
        c
    } else {
        let mut c = Command::new("sh");
        c.args(["-c", &req.command]);
        c
    };

    if let Some(cwd) = req.cwd {
        if !cwd.is_empty() {
            cmd.current_dir(cwd);
        }
    }

    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

    match cmd.output().await {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            let exit_code = output.status.code().unwrap_or(-1);
            (
                StatusCode::OK,
                Json(TerminalExecRes {
                    stdout,
                    stderr,
                    exit_code,
                }),
            )
                .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

// ---------------------------------------------------------------------------
// MCP (Model Context Protocol) — Streamable HTTP
// Spec: https://modelcontextprotocol.io/specification/2025-03-26/basic/transports
//
// - POST /mcp (e POST / por compatibilidade) aceita JSON-RPC 2.0 simples ou
//   em lote (batch) e responde application/json.
// - GET /mcp abre um stream SSE (text/event-stream) para mensagens
//   servidor -> cliente, como exigem os clientes modernos.
// - Notificacoes (sem "id") retornam 202 Accepted com corpo vazio.
// - "initialize" negocia a versao do protocolo e devolve Mcp-Session-Id.
// ---------------------------------------------------------------------------

fn generate_session_id() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let chars: Vec<char> = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789"
        .chars()
        .collect();
    (0..32)
        .map(|_| chars[rng.gen_range(0..chars.len())])
        .collect()
}

fn negotiate_protocol_version(client_version: Option<&str>) -> &'static str {
    match client_version {
        Some("2025-06-18") => "2025-06-18",
        Some("2025-03-26") => "2025-03-26",
        Some("2024-11-05") => "2024-11-05",
        _ => "2025-03-26",
    }
}

fn mcp_tools_list() -> serde_json::Value {
    serde_json::json!({
        "tools": [
            {
                "name": "screen_capture",
                "description": "Capture the user's primary screen as a PNG image.",
                "inputSchema": { "type": "object", "properties": {} }
            },
            {
                "name": "mouse_move",
                "description": "Move the mouse cursor to (x, y) coordinates.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "x": { "type": "integer" },
                        "y": { "type": "integer" }
                    },
                    "required": ["x", "y"]
                }
            },
            {
                "name": "mouse_click",
                "description": "Click a mouse button (left, right, middle) optionally at (x, y).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "x": { "type": "integer" },
                        "y": { "type": "integer" },
                        "button": { "type": "string", "enum": ["left", "right", "middle"] },
                        "double": { "type": "boolean" }
                    }
                }
            },
            {
                "name": "mouse_scroll",
                "description": "Scroll the mouse wheel horizontally or vertically.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "delta_x": { "type": "integer" },
                        "delta_y": { "type": "integer" }
                    }
                }
            },
            {
                "name": "keyboard_type",
                "description": "Type text on the keyboard.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "text": { "type": "string" }
                    },
                    "required": ["text"]
                }
            },
            {
                "name": "keyboard_key",
                "description": "Press a key with optional modifiers (ctrl, alt, shift, meta).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "key": { "type": "string" },
                        "modifiers": { "type": "array", "items": { "type": "string" } }
                    },
                    "required": ["key"]
                }
            },
            {
                "name": "terminal_exec",
                "description": "Execute a shell command in the PC terminal.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "command": { "type": "string" },
                        "cwd": { "type": "string" }
                    },
                    "required": ["command"]
                }
            }
        ]
    })
}

async fn call_tool(name: &str, args: serde_json::Value) -> (serde_json::Value, bool) {
    match name {
        "screen_capture" => {
            let res = tokio::task::spawn_blocking(|| -> Result<String, String> {
                let monitors = Monitor::all().map_err(|e| e.to_string())?;
                let monitor = monitors
                    .into_iter()
                    .find(|m| m.is_primary().unwrap_or(false))
                    .or_else(|| Monitor::all().ok()?.into_iter().next())
                    .ok_or_else(|| "No monitor found".to_string())?;
                let img = monitor.capture_image().map_err(|e| e.to_string())?;
                let rgba: ImageBuffer<Rgba<u8>, Vec<u8>> =
                    ImageBuffer::from_raw(img.width(), img.height(), img.into_raw())
                        .ok_or_else(|| "Failed to build image buffer".to_string())?;
                let mut png_bytes = Vec::new();
                let mut cursor = Cursor::new(&mut png_bytes);
                rgba.write_to(&mut cursor, image::ImageFormat::Png)
                    .map_err(|e| e.to_string())?;
                Ok(BASE64.encode(&png_bytes))
            })
            .await;
            match res {
                Ok(Ok(b64)) => (
                    serde_json::json!([{
                        "type": "image",
                        "data": b64,
                        "mimeType": "image/png"
                    }]),
                    false,
                ),
                Ok(Err(e)) => (
                    serde_json::json!([{ "type": "text", "text": format!("Error: {}", e) }]),
                    true,
                ),
                Err(e) => (
                    serde_json::json!([{ "type": "text", "text": format!("Error: {}", e) }]),
                    true,
                ),
            }
        }
        "mouse_move" => {
            let parsed: Result<MouseMoveReq, _> = serde_json::from_value(args);
            match parsed {
                Ok(r) => {
                    let _ = mouse_move(Json(r)).await;
                    (serde_json::json!([{ "type": "text", "text": "Mouse moved" }]), false)
                }
                Err(e) => (
                    serde_json::json!([{ "type": "text", "text": format!("Invalid args: {}", e) }]),
                    true,
                ),
            }
        }
        "mouse_click" => {
            let parsed: Result<MouseClickReq, _> = serde_json::from_value(args);
            match parsed {
                Ok(r) => {
                    let _ = mouse_click(Json(r)).await;
                    (serde_json::json!([{ "type": "text", "text": "Mouse clicked" }]), false)
                }
                Err(e) => (
                    serde_json::json!([{ "type": "text", "text": format!("Invalid args: {}", e) }]),
                    true,
                ),
            }
        }
        "mouse_scroll" => {
            let parsed: Result<MouseScrollReq, _> = serde_json::from_value(args);
            match parsed {
                Ok(r) => {
                    let _ = mouse_scroll(Json(r)).await;
                    (serde_json::json!([{ "type": "text", "text": "Scrolled" }]), false)
                }
                Err(e) => (
                    serde_json::json!([{ "type": "text", "text": format!("Invalid args: {}", e) }]),
                    true,
                ),
            }
        }
        "keyboard_type" => {
            let parsed: Result<KeyboardTypeReq, _> = serde_json::from_value(args);
            match parsed {
                Ok(r) => {
                    let _ = keyboard_type(Json(r)).await;
                    (serde_json::json!([{ "type": "text", "text": "Text typed" }]), false)
                }
                Err(e) => (
                    serde_json::json!([{ "type": "text", "text": format!("Invalid args: {}", e) }]),
                    true,
                ),
            }
        }
        "keyboard_key" => {
            let parsed: Result<KeyboardKeyReq, _> = serde_json::from_value(args);
            match parsed {
                Ok(r) => {
                    let _ = keyboard_key(Json(r)).await;
                    (serde_json::json!([{ "type": "text", "text": "Key pressed" }]), false)
                }
                Err(e) => (
                    serde_json::json!([{ "type": "text", "text": format!("Invalid args: {}", e) }]),
                    true,
                ),
            }
        }
        "terminal_exec" => {
            let parsed: Result<TerminalExecReq, _> = serde_json::from_value(args);
            match parsed {
                Ok(r) => {
                    let mut cmd = if cfg!(target_os = "windows") {
                        let mut c = Command::new("cmd");
                        c.args(["/C", &r.command]);
                        c
                    } else {
                        let mut c = Command::new("sh");
                        c.args(["-c", &r.command]);
                        c
                    };
                    if let Some(cwd) = r.cwd {
                        if !cwd.is_empty() {
                            cmd.current_dir(cwd);
                        }
                    }
                    match cmd.output().await {
                        Ok(out) => {
                            let stdout = String::from_utf8_lossy(&out.stdout);
                            let stderr = String::from_utf8_lossy(&out.stderr);
                            let code = out.status.code().unwrap_or(-1);
                            (
                                serde_json::json!([{
                                    "type": "text",
                                    "text": format!("exit_code: {}\nstdout:\n{}\nstderr:\n{}", code, stdout, stderr)
                                }]),
                                false,
                            )
                        }
                        Err(e) => (
                            serde_json::json!([{ "type": "text", "text": format!("Error: {}", e) }]),
                            true,
                        ),
                    }
                }
                Err(e) => (
                    serde_json::json!([{ "type": "text", "text": format!("Invalid args: {}", e) }]),
                    true,
                ),
            }
        }
        _ => (
            serde_json::json!([{ "type": "text", "text": format!("Unknown tool: {}", name) }]),
            true,
        ),
    }
}

/// Processa uma unica requisicao JSON-RPC.
/// Retorna `None` para notificacoes (sem "id"), que devem gerar 202 vazio.
async fn handle_mcp_request(
    req: &serde_json::Value,
    session_id: &mut Option<String>,
    protocol_version: &mut Option<String>,
) -> Option<serde_json::Value> {
    let method = req.get("method").and_then(|v| v.as_str()).unwrap_or("");
    let id = req.get("id").cloned().unwrap_or(serde_json::Value::Null);
    let is_notification = !req.as_object().map(|o| o.contains_key("id")).unwrap_or(false);

    // Notificacoes: nunca retornam resposta (HTTP 202 vazio).
    if is_notification {
        return None;
    }

    let params = req.get("params").cloned().unwrap_or_default();

    let result_or_error = match method {
        "initialize" => {
            let client_version = params
                .get("protocolVersion")
                .and_then(|v| v.as_str());
            let negotiated = negotiate_protocol_version(client_version);
            *protocol_version = Some(negotiated.to_string());
            if session_id.is_none() {
                *session_id = Some(generate_session_id());
            }
            serde_json::json!({
                "protocolVersion": negotiated,
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": {
                    "name": "spark-bridge",
                    "version": env!("CARGO_PKG_VERSION")
                }
            })
        }
        "ping" => serde_json::json!({}),
        "tools/list" => mcp_tools_list(),
        "tools/call" => {
            let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let args = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));
            let (content, is_error) = call_tool(name, args).await;
            serde_json::json!({ "content": content, "isError": is_error })
        }
        "resources/list" => serde_json::json!({ "resources": [] }),
        "resources/templates/list" => serde_json::json!({ "resourceTemplates": [] }),
        "prompts/list" => serde_json::json!({ "prompts": [] }),
        "logging/setLevel" => serde_json::json!({}),
        _ => {
            return Some(serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": format!("Method not found: {}", method) }
            }));
        }
    };

    Some(serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result_or_error
    }))
}

fn mcp_response_with_headers(
    status: StatusCode,
    body: serde_json::Value,
    session_id: &Option<String>,
    protocol_version: &Option<String>,
) -> Response {
    let mut resp = (status, Json(body)).into_response();
    if let Some(s) = session_id {
        if let Ok(v) = HeaderValue::from_str(s) {
            resp.headers_mut()
                .insert(axum::http::HeaderName::from_static("mcp-session-id"), v);
        }
    }
    if let Some(p) = protocol_version {
        if let Ok(v) = HeaderValue::from_str(p) {
            resp.headers_mut().insert(
                axum::http::HeaderName::from_static("mcp-protocol-version"),
                v,
            );
        }
    }
    resp
}

async fn mcp_post(headers: HeaderMap, body: Bytes) -> Response {
    // Ecoa sessao/protocolo enviados pelo cliente, se houver.
    let mut session_id = headers
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let mut protocol_version = headers
        .get("mcp-protocol-version")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let parsed: Result<serde_json::Value, _> = serde_json::from_slice(&body);
    let value = match parsed {
        Ok(v) => v,
        Err(e) => {
            return mcp_response_with_headers(
                StatusCode::BAD_REQUEST,
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": serde_json::Value::Null,
                    "error": { "code": -32700, "message": format!("Parse error: {}", e) }
                }),
                &session_id,
                &protocol_version,
            );
        }
    };

    // Batch (array) ou requisicao simples.
    if let Some(items) = value.as_array() {
        if items.is_empty() {
            return mcp_response_with_headers(
                StatusCode::BAD_REQUEST,
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": serde_json::Value::Null,
                    "error": { "code": -32700, "message": "Parse error: empty batch" }
                }),
                &session_id,
                &protocol_version,
            );
        }
        let mut responses = Vec::new();
        for item in items {
            if let Some(r) = handle_mcp_request(item, &mut session_id, &mut protocol_version).await {
                responses.push(r);
            }
        }
        if responses.is_empty() {
            return StatusCode::ACCEPTED.into_response();
        }
        return mcp_response_with_headers(
            StatusCode::OK,
            serde_json::Value::Array(responses),
            &session_id,
            &protocol_version,
        );
    }

    match handle_mcp_request(&value, &mut session_id, &mut protocol_version).await {
        None => StatusCode::ACCEPTED.into_response(),
        Some(resp) => mcp_response_with_headers(StatusCode::OK, resp, &session_id, &protocol_version),
    }
}

/// GET /mcp — stream SSE (server -> client) exigido pelo transporte
/// Streamable HTTP. Mantem a conexao aberta com comentarios de keep-alive.
async fn mcp_sse() -> Response {
    let stream = async_stream::stream! {
        yield Ok::<_, std::convert::Infallible>(axum::response::sse::Event::default().comment("connected"));
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(15)).await;
            yield Ok(axum::response::sse::Event::default().comment("ping"));
        }
    };
    Sse::new(stream).into_response()
}

async fn mcp_delete() -> StatusCode {
    StatusCode::NO_CONTENT
}

pub async fn start_http_server(
    port: u16,
    token: Arc<RwLock<String>>,
    shutdown_rx: oneshot::Receiver<()>,
) -> Result<(), String> {
    let state = ServerState { token };

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([Method::GET, Method::POST, Method::DELETE, Method::OPTIONS])
        .allow_headers(Any)
        .expose_headers([
            axum::http::HeaderName::from_static("mcp-session-id"),
            axum::http::HeaderName::from_static("mcp-protocol-version"),
            header::CONTENT_TYPE,
        ])
        .max_age(std::time::Duration::from_secs(3600));

    let app = Router::new()
        .route("/health", get(health))
        .route("/logo.png", get(logo))
        .route("/icon.png", get(logo))
        .route("/openapi.json", get(openapi_spec))
        .route("/", post(mcp_post).get(health))
        .route(
            "/mcp",
            post(mcp_post).get(mcp_sse).delete(mcp_delete),
        )
        .route("/screen", get(get_screen))
        .route("/mouse/move", post(mouse_move))
        .route("/mouse/click", post(mouse_click))
        .route("/mouse/scroll", post(mouse_scroll))
        .route("/keyboard/type", post(keyboard_type))
        .route("/keyboard/key", post(keyboard_key))
        .route("/terminal/exec", post(terminal_exec))
        .layer(middleware::from_fn_with_state(state.clone(), auth_middleware))
        .layer(cors)
        .with_state(state);

    let addr = format!("127.0.0.1:{}", port);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(|e| format!("Failed to bind {}: {}", addr, e))?;

    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = shutdown_rx.await;
        })
        .await
        .map_err(|e| format!("Server error: {}", e))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::start_http_server;

    async fn post_mcp(
        client: &reqwest::Client,
        url: &str,
        body: serde_json::Value,
    ) -> reqwest::Response {
        client
            .post(url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .json(&body)
            .send()
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn mcp_streamable_http_smoke() {
        let token = std::sync::Arc::new(tokio::sync::RwLock::new("test-token".to_string()));
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(start_http_server(4899, token, rx));

        let client = reqwest::Client::new();

        // Aguarda o servidor subir (retry no /health).
        let mut up = false;
        for _ in 0..50 {
            if client
                .get("http://127.0.0.1:4899/health")
                .send()
                .await
                .map(|r| r.status().is_success())
                .unwrap_or(false)
            {
                up = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        assert!(up, "servidor nao subiu");

        // /health sem auth -> 200
        let h = client.get("http://127.0.0.1:4899/health").send().await.unwrap();
        assert_eq!(h.status().as_u16(), 200);

        // /logo.png sem auth -> 200 image/png com magic bytes de PNG
        let lg = client
            .get("http://127.0.0.1:4899/logo.png")
            .send()
            .await
            .unwrap();
        assert_eq!(lg.status().as_u16(), 200);
        let lg_ct = lg.headers().get("content-type").unwrap().to_str().unwrap();
        assert!(lg_ct.contains("image/png"));
        let lg_bytes = lg.bytes().await.unwrap();
        assert!(lg_bytes.starts_with(&[0x89, b'P', b'N', b'G']));

        // Sem token -> 401
        let u = post_mcp(
            &client,
            "http://127.0.0.1:4899/mcp",
            serde_json::json!({"jsonrpc":"2.0","id":1,"method":"ping"}),
        )
        .await;
        assert_eq!(u.status().as_u16(), 401);

        let base = "http://127.0.0.1:4899/mcp?token=test-token";

        // initialize: negocia a versao do cliente e devolve Mcp-Session-Id
        let init = post_mcp(
            &client,
            base,
            serde_json::json!({
                "jsonrpc":"2.0","id":1,"method":"initialize",
                "params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"0"}}
            }),
        )
        .await;
        assert_eq!(init.status().as_u16(), 200);
        assert!(init.headers().contains_key("mcp-session-id"));
        let init_body: serde_json::Value = init.json().await.unwrap();
        assert_eq!(init_body["result"]["protocolVersion"], "2025-06-18");

        // notificacao (sem id) -> 202 vazio
        let notif = post_mcp(
            &client,
            base,
            serde_json::json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        )
        .await;
        assert_eq!(notif.status().as_u16(), 202);

        // tools/list -> 7 ferramentas
        let list = post_mcp(
            &client,
            base,
            serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
        )
        .await;
        let list_body: serde_json::Value = list.json().await.unwrap();
        assert_eq!(list_body["result"]["tools"].as_array().unwrap().len(), 7);

        // tools/call desconhecida -> isError true
        let call = post_mcp(
            &client,
            base,
            serde_json::json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"nope","arguments":{}}}),
        )
        .await;
        let call_body: serde_json::Value = call.json().await.unwrap();
        assert_eq!(call_body["result"]["isError"], true);

        // GET /mcp -> stream SSE
        let sse = client
            .get(base)
            .header("Accept", "text/event-stream")
            .send()
            .await
            .unwrap();
        assert_eq!(sse.status().as_u16(), 200);
        let ct = sse
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap();
        assert!(ct.contains("text/event-stream"));

        let _ = tx.send(());
        let _ = tokio::time::timeout(std::time::Duration::from_secs(3), server).await;
    }
}
