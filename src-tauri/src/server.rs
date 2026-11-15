use axum::{
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Json, Response},
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
use tokio::sync::oneshot;
use tower_http::cors::{Any, CorsLayer};
use xcap::Monitor;

#[derive(Clone)]
pub struct ServerState {
    pub token: Arc<String>,
}

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
    version: &'static str,
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

async fn auth_middleware(
    State(state): State<ServerState>,
    headers: HeaderMap,
    req: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let path = req.uri().path();
    if path == "/health" || path == "/openapi.json" {
        return Ok(next.run(req).await);
    }

    let auth_header = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());

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

    if let Some(t) = query_token {
        if t == state.token.as_str() {
            return Ok(next.run(req).await);
        } else {
            return Err(StatusCode::UNAUTHORIZED);
        }
    }

    match auth_header {
        Some(h) if h.starts_with("Bearer ") => {
            let provided = &h[7..];
            if provided == state.token.as_str() {
                Ok(next.run(req).await)
            } else {
                Err(StatusCode::UNAUTHORIZED)
            }
        }
        _ => Err(StatusCode::UNAUTHORIZED),
    }
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
    })
}

async fn openapi_spec() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "openapi": "3.0.3",
        "info": {
            "title": "Spark Bridge PC Agent API",
            "version": env!("CARGO_PKG_VERSION"),
            "description": "Control screen, mouse, keyboard, and terminal on the user's PC."
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

#[derive(Deserialize)]
struct McpRequest {
    #[allow(dead_code)]
    jsonrpc: Option<String>,
    id: Option<serde_json::Value>,
    method: String,
    params: Option<serde_json::Value>,
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

async fn mcp_handler(Json(req): Json<McpRequest>) -> impl IntoResponse {
    let id = req.id.unwrap_or(serde_json::Value::Null);

    match req.method.as_str() {
        "initialize" => (
            StatusCode::OK,
            Json(serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": { "tools": {} },
                    "serverInfo": {
                        "name": "spark-bridge",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                }
            })),
        )
            .into_response(),
        "notifications/initialized" | "ping" => (
            StatusCode::OK,
            Json(serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {}
            })),
        )
            .into_response(),
        "tools/list" => (
            StatusCode::OK,
            Json(serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": mcp_tools_list()
            })),
        )
            .into_response(),
        "tools/call" => {
            let params = req.params.unwrap_or_default();
            let name = params
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let args = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));

            let content = match name {
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
                        Ok(Ok(b64)) => serde_json::json!([{
                            "type": "image",
                            "data": b64,
                            "mimeType": "image/png"
                        }]),
                        Ok(Err(e)) => serde_json::json!([{ "type": "text", "text": format!("Error: {}", e) }]),
                        Err(e) => serde_json::json!([{ "type": "text", "text": format!("Error: {}", e) }]),
                    }
                }
                "mouse_move" => {
                    let parsed: Result<MouseMoveReq, _> = serde_json::from_value(args);
                    match parsed {
                        Ok(r) => {
                            let _ = mouse_move(Json(r)).await;
                            serde_json::json!([{ "type": "text", "text": "Mouse moved" }])
                        }
                        Err(e) => serde_json::json!([{ "type": "text", "text": format!("Invalid args: {}", e) }]),
                    }
                }
                "mouse_click" => {
                    let parsed: Result<MouseClickReq, _> = serde_json::from_value(args);
                    match parsed {
                        Ok(r) => {
                            let _ = mouse_click(Json(r)).await;
                            serde_json::json!([{ "type": "text", "text": "Mouse clicked" }])
                        }
                        Err(e) => serde_json::json!([{ "type": "text", "text": format!("Invalid args: {}", e) }]),
                    }
                }
                "mouse_scroll" => {
                    let parsed: Result<MouseScrollReq, _> = serde_json::from_value(args);
                    match parsed {
                        Ok(r) => {
                            let _ = mouse_scroll(Json(r)).await;
                            serde_json::json!([{ "type": "text", "text": "Scrolled" }])
                        }
                        Err(e) => serde_json::json!([{ "type": "text", "text": format!("Invalid args: {}", e) }]),
                    }
                }
                "keyboard_type" => {
                    let parsed: Result<KeyboardTypeReq, _> = serde_json::from_value(args);
                    match parsed {
                        Ok(r) => {
                            let _ = keyboard_type(Json(r)).await;
                            serde_json::json!([{ "type": "text", "text": "Text typed" }])
                        }
                        Err(e) => serde_json::json!([{ "type": "text", "text": format!("Invalid args: {}", e) }]),
                    }
                }
                "keyboard_key" => {
                    let parsed: Result<KeyboardKeyReq, _> = serde_json::from_value(args);
                    match parsed {
                        Ok(r) => {
                            let _ = keyboard_key(Json(r)).await;
                            serde_json::json!([{ "type": "text", "text": "Key pressed" }])
                        }
                        Err(e) => serde_json::json!([{ "type": "text", "text": format!("Invalid args: {}", e) }]),
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
                                    serde_json::json!([{
                                        "type": "text",
                                        "text": format!("exit_code: {}\nstdout:\n{}\nstderr:\n{}", code, stdout, stderr)
                                    }])
                                }
                                Err(e) => serde_json::json!([{ "type": "text", "text": format!("Error: {}", e) }]),
                            }
                        }
                        Err(e) => serde_json::json!([{ "type": "text", "text": format!("Invalid args: {}", e) }]),
                    }
                }
                _ => serde_json::json!([{ "type": "text", "text": format!("Unknown tool: {}", name) }]),
            };

            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": { "content": content }
                })),
            )
                .into_response()
        }
        _ => (
            StatusCode::OK,
            Json(serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": "Method not found" }
            })),
        )
            .into_response(),
    }
}

pub async fn start_http_server(
    port: u16,
    token: String,
    shutdown_rx: oneshot::Receiver<()>,
) -> Result<(), String> {
    let state = ServerState {
        token: Arc::new(token),
    };

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let app = Router::new()
        .route("/health", get(health))
        .route("/openapi.json", get(openapi_spec))
        .route("/", post(mcp_handler).get(health))
        .route("/mcp", post(mcp_handler).get(health))
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
