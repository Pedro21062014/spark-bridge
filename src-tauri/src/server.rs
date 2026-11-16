use axum::{
    body::Bytes,
    extract::{Form, Json as ExtractJson, Query, Request, State},
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    middleware::{self, Next},
    response::{Html, IntoResponse, Json, Redirect, Response, Sse},
    routing::{get, post},
    Router,
};
use base64::{engine::general_purpose::{STANDARD as BASE64, URL_SAFE_NO_PAD}, Engine as _};
use enigo::{Button, Coordinate, Direction, Enigo, Key, Keyboard, Mouse, Settings};
use image::{DynamicImage, ImageBuffer, Rgba};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::io::Cursor;
use std::process::Stdio;
use std::sync::{atomic::{AtomicBool, Ordering}, Arc, Mutex as StdMutex, OnceLock};
use tauri::{AppHandle, Emitter};
use tokio::process::Command;
use tokio::sync::{oneshot, RwLock};
use sha2::{Digest, Sha256};
use tower_http::cors::{Any, CorsLayer};
use xcap::Monitor;

static APP_HANDLE: OnceLock<AppHandle> = OnceLock::new();
static AGENT_ACTIVE: AtomicBool = AtomicBool::new(false);

fn set_agent_activity(active: bool) {
    AGENT_ACTIVE.store(active, Ordering::SeqCst);
    if let Some(app) = APP_HANDLE.get() {
        let _ = app.emit("agent-activity", serde_json::json!({ "active": active }));
    }
}

#[derive(Clone)]
pub struct ServerState {
    pub token: Arc<RwLock<String>>,
    pub permissions: Arc<RwLock<BTreeMap<String, bool>>>,
}

#[derive(Clone)]
struct OAuthCode {
    client_id: String,
    redirect_uri: String,
    code_challenge: Option<String>,
    expires_at: std::time::Instant,
}

struct OAuthStore {
    client_id: String,
    client_secret: String,
    codes: HashMap<String, OAuthCode>,
    access_tokens: HashMap<String, String>,
}

static OAUTH_STORE: OnceLock<StdMutex<OAuthStore>> = OnceLock::new();
static PERMISSIONS: OnceLock<Arc<RwLock<BTreeMap<String, bool>>>> = OnceLock::new();

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

#[derive(Deserialize)]
pub struct MouseDragReq {
    pub x1: i32,
    pub y1: i32,
    pub x2: i32,
    pub y2: i32,
    pub button: Option<String>,
}

#[derive(Deserialize)]
pub struct ClipboardWriteReq {
    pub text: String,
}

#[derive(Deserialize)]
pub struct OpenUrlReq {
    pub url: String,
}

#[derive(Deserialize)]
pub struct WaitReq {
    pub ms: u64,
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

fn oauth_store() -> &'static StdMutex<OAuthStore> {
    OAUTH_STORE.get_or_init(|| StdMutex::new(OAuthStore {
        client_id: "spark-bridge-gemini".to_string(),
        client_secret: String::new(),
        codes: HashMap::new(),
        access_tokens: HashMap::new(),
    }))
}

fn random_oauth_value(prefix: &str) -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let bytes: Vec<u8> = (0..32).map(|_| rng.gen()).collect();
    format!("{}_{}", prefix, URL_SAFE_NO_PAD.encode(bytes))
}

fn request_origin(headers: &HeaderMap) -> String {
    let scheme = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("https");
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("127.0.0.1:4891");
    format!("{}://{}", scheme, host)
}

fn html_escape(value: &str) -> String {
    value.replace('&', "&amp;").replace('"', "&quot;").replace('<', "&lt;").replace('>', "&gt;")
}

fn oauth_query_value(value: &str) -> String {
    value.bytes().map(|b| match b {
        b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
        _ => format!("%{:02X}", b),
    }).collect()
}

fn oauth_valid_access_token(token: &str) -> bool {
    oauth_store().lock().map(|store| store.access_tokens.contains_key(token)).unwrap_or(false)
}

async fn oauth_protected_resource(headers: HeaderMap) -> impl IntoResponse {
    let origin = request_origin(&headers);
    Json(serde_json::json!({
        "resource": format!("{}/mcp", origin),
        "authorization_servers": [origin],
        "scopes_supported": ["spark:use"]
    }))
}

async fn oauth_authorization_server(headers: HeaderMap) -> impl IntoResponse {
    let origin = request_origin(&headers);
    Json(serde_json::json!({
        "issuer": origin,
        "authorization_endpoint": format!("{}/oauth/authorize", origin),
        "token_endpoint": format!("{}/oauth/token", origin),
        "registration_endpoint": format!("{}/oauth/register", origin),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["client_secret_post", "client_secret_basic"]
    }))
}

async fn oauth_authorize_page(Query(params): Query<HashMap<String, String>>) -> impl IntoResponse {
    let hidden = ["client_id", "redirect_uri", "response_type", "scope", "state", "code_challenge", "code_challenge_method"]
        .iter()
        .filter_map(|key| params.get(*key).map(|value| format!("<input type=hidden name=\"{}\" value=\"{}\">", key, html_escape(value))))
        .collect::<Vec<_>>()
        .join("\n");
    Html(format!(r#"<!doctype html><meta charset=utf-8><title>Autorizar Spark Bridge</title>
<style>body{{font:16px system-ui;max-width:520px;margin:12vh auto;padding:24px;background:#0b1020;color:#f8fafc}}button{{padding:12px 18px;border:0;border-radius:8px;background:#3b82f6;color:white;font-weight:600}}.box{{padding:24px;border:1px solid #334155;border-radius:14px}}</style>
<div class=box><h1>Autorizar o Spark Bridge</h1><p>O cliente Gemini solicitou acesso às ferramentas habilitadas na aba Segurança deste aplicativo.</p><p>Somente as permissões que você ativou serão disponibilizadas ao agente.</p><form method=post action="/oauth/authorize">{}<button type=submit>Autorizar acesso</button></form></div>"#, hidden))
}

async fn oauth_authorize_submit(Form(params): Form<HashMap<String, String>>) -> impl IntoResponse {
    let client_id = params.get("client_id").cloned().unwrap_or_default();
    let redirect_uri = params.get("redirect_uri").cloned().unwrap_or_default();
    let state = params.get("state").cloned().unwrap_or_default();
    if client_id.is_empty() || redirect_uri.is_empty() {
        return (StatusCode::BAD_REQUEST, "client_id e redirect_uri sao obrigatorios").into_response();
    }
    let code = random_oauth_value("code");
    if let Ok(mut store) = oauth_store().lock() {
        store.codes.insert(code.clone(), OAuthCode {
            client_id,
            redirect_uri: redirect_uri.clone(),
            code_challenge: params.get("code_challenge").cloned(),
            expires_at: std::time::Instant::now() + std::time::Duration::from_secs(300),
        });
    }
    let mut location = format!("{}{}code={}", redirect_uri, if redirect_uri.contains('?') { '&' } else { '?' }, oauth_query_value(&code));
    if !state.is_empty() { location.push_str(&format!("&state={}", oauth_query_value(&state))); }
    Redirect::temporary(&location).into_response()
}

async fn oauth_register(ExtractJson(body): ExtractJson<serde_json::Value>) -> impl IntoResponse {
    let (client_id, client_secret) = oauth_store().lock().map(|store| (store.client_id.clone(), store.client_secret.clone())).unwrap_or_else(|_| ("spark-bridge-gemini".to_string(), String::new()));
    Json(serde_json::json!({
        "client_id": client_id,
        "client_name": body.get("client_name").and_then(|v| v.as_str()).unwrap_or("MCP client"),
        "client_secret": client_secret,
        "redirect_uris": body.get("redirect_uris").cloned().unwrap_or_else(|| serde_json::json!([])),
        "token_endpoint_auth_method": "client_secret_post"
    })).into_response()
}

async fn oauth_token(headers: HeaderMap, Form(params): Form<HashMap<String, String>>) -> impl IntoResponse {
    if params.get("grant_type").map(String::as_str) != Some("authorization_code") {
        return (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error":"unsupported_grant_type"}))).into_response();
    }
    let code = params.get("code").cloned().unwrap_or_default();
    let record = oauth_store().lock().ok().and_then(|mut store| store.codes.remove(&code));
    let Some(record) = record else { return (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error":"invalid_grant"}))).into_response(); };
    let basic_credentials = headers.get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Basic "))
        .and_then(|value| BASE64.decode(value).ok())
        .and_then(|value| String::from_utf8(value).ok())
        .and_then(|value| value.split_once(':').map(|(client_id, secret)| (client_id.to_string(), secret.to_string())));
    let provided_client_id = params.get("client_id").cloned().or_else(|| basic_credentials.as_ref().map(|(client_id, _)| client_id.clone()));
    if record.expires_at < std::time::Instant::now() || provided_client_id.as_deref() != Some(record.client_id.as_str()) || params.get("redirect_uri") != Some(&record.redirect_uri) {
        return (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error":"invalid_grant"}))).into_response();
    }
    let provided_secret = params.get("client_secret").cloned().or_else(|| basic_credentials.map(|(_, secret)| secret));
    let expected_secret = oauth_store().lock().map(|store| store.client_secret.clone()).unwrap_or_default();
    if expected_secret.is_empty() || provided_secret.as_deref() != Some(expected_secret.as_str()) {
        return (StatusCode::UNAUTHORIZED, Json(serde_json::json!({"error":"invalid_client"}))).into_response();
    }
    if let Some(challenge) = record.code_challenge {
        let verifier = params.get("code_verifier").cloned().unwrap_or_default();
        let actual = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        if actual != challenge { return (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error":"invalid_grant","error_description":"PKCE verification failed"}))).into_response(); }
    }
    let access_token = random_oauth_value("sb_oauth");
    if let Ok(mut store) = oauth_store().lock() { store.access_tokens.insert(access_token.clone(), record.client_id); }
    Json(serde_json::json!({"access_token":access_token,"token_type":"Bearer","expires_in":3600,"scope":"spark:use"})).into_response()
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
        || path.starts_with("/.well-known/")
        || path.starts_with("/oauth/")
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
        Some(t) if t == expected || oauth_valid_access_token(&t) => Ok(next.run(req).await),
        _ => {
            let origin = request_origin(&headers);
            let mut response = unauthorized_response();
            let challenge = format!("Bearer resource_metadata=\"{}/.well-known/oauth-protected-resource\", scope=\"spark:use\"", origin);
            if let Ok(value) = HeaderValue::from_str(&challenge) {
                response.headers_mut().insert(header::WWW_AUTHENTICATE, value);
            }
            Err(response)
        }
    }
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
        logo: "/logo.png",
    })
}

/// Serve a ultima captura de tela (JPEG) para visualizacao no navegador.
/// Util para clientes MCP que nao repassam imagens ao modelo: o link
/// (com token) e devolvido no bloco de texto de screen_capture.
async fn latest_screenshot() -> Response {
    match get_latest_screenshot() {
        Some(bytes) => {
            let mut resp = Bytes::from(bytes.as_ref().clone()).into_response();
            resp.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("image/jpeg"),
            );
            resp.headers_mut().insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static("no-store"),
            );
            resp
        }
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": "Nenhuma captura ainda. Chame screen_capture primeiro."
            })),
        )
            .into_response(),
    }
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
                    "summary": "Capture primary screen as base64 JPEG (downscaled to max 1600px)",
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
            },
            "/mouse/drag": {
                "post": {
                    "summary": "Drag mouse from (x1, y1) to (x2, y2)",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object", "properties": { "x1": { "type": "integer" }, "y1": { "type": "integer" }, "x2": { "type": "integer" }, "y2": { "type": "integer" }, "button": { "type": "string" } }, "required": ["x1", "y1", "x2", "y2"] } } }
                    },
                    "responses": { "200": { "description": "Mouse dragged" } }
                }
            },
            "/screen/info": {
                "get": {
                    "summary": "Monitor info (name, resolution, primary)",
                    "responses": { "200": { "description": "Monitor info" } }
                }
            },
            "/clipboard": {
                "get": {
                    "summary": "Read clipboard text",
                    "responses": { "200": { "description": "Clipboard text" } }
                },
                "post": {
                    "summary": "Write clipboard text",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object", "properties": { "text": { "type": "string" } }, "required": ["text"] } } }
                    },
                    "responses": { "200": { "description": "Clipboard updated" } }
                }
            },
            "/open-url": {
                "post": {
                    "summary": "Open an http(s) URL in the default browser",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "type": "object", "properties": { "url": { "type": "string" } }, "required": ["url"] } } }
                    },
                    "responses": { "200": { "description": "URL opened" } }
                }
            },
            "/screenshot.jpg": {
                "get": {
                    "summary": "Latest screenshot as JPEG (for viewing in browser)",
                    "responses": { "200": { "description": "JPEG image" } }
                }
            },
            "/screen/ocr": {
                "get": {
                    "summary": "OCR the screen via tesseract (words + coordinates)",
                    "responses": { "200": { "description": "OCR words with coordinates" } }
                }
            }
        }
    }))
}

/// Verifica se uma imagem RGB esta "em branco" (toda preta).
/// Capturas falhas (Wayland sem permissao, primeiro frame do PipeWire,
/// tela bloqueada/desligada) costumam retornar frames pretos — nesse
/// caso tentamos de novo em vez de devolver uma imagem vazia para a IA.
fn is_blank_rgb(buf: &[u8], width: u32, height: u32) -> bool {
    let w = width as usize;
    let h = height as usize;
    if w == 0 || h == 0 || buf.len() < w * h * 3 {
        return true;
    }
    let step_x = (w / 64).max(1);
    let step_y = (h / 64).max(1);
    let mut sampled = 0usize;
    let mut y = 0;
    while y < h {
        let mut x = 0;
        while x < w {
            let i = (y * w + x) * 3;
            if i + 2 < buf.len() {
                sampled += 1;
                if buf[i] > 10 || buf[i + 1] > 10 || buf[i + 2] > 10 {
                    return false;
                }
            }
            x += step_x;
        }
        y += step_y;
    }
    sampled > 0
}

/// Reduz a imagem para no maximo `max_dim` no maior lado (mantem proporcao).
/// Capturas em resolucao total geram PNGs de varios MB em base64, que muitos
/// clientes MCP descartam/truncam — a IA entao relata a captura como "vazia".
fn downscale_to_max(img: DynamicImage, max_dim: u32) -> DynamicImage {
    let (w, h) = (img.width(), img.height());
    let m = w.max(h);
    if m <= max_dim || m == 0 {
        return img;
    }
    let ratio = max_dim as f32 / m as f32;
    let nw = ((w as f32 * ratio) as u32).max(1);
    let nh = ((h as f32 * ratio) as u32).max(1);
    img.resize_exact(nw, nh, image::imageops::FilterType::Triangle)
}

/// Captura a tela principal e devolve (bytes JPEG, largura, altura).
/// Tenta ate 3 vezes, descartando frames em branco/0x0, e sempre devolve
/// JPEG reduzido (max. 1600px) para caber nos limites dos clientes MCP.
/// Ultima captura JPEG (para GET /screenshot.jpg) e token atual do servidor
/// (para montar a URL da captura no texto de screen_capture).
static LATEST_SCREENSHOT: std::sync::OnceLock<std::sync::Mutex<Option<std::sync::Arc<Vec<u8>>>>> =
    std::sync::OnceLock::new();
static SERVER_TOKEN: std::sync::OnceLock<
    std::sync::Mutex<Option<std::sync::Arc<tokio::sync::RwLock<String>>>>,
> = std::sync::OnceLock::new();

fn latest_screenshot_slot() -> &'static std::sync::Mutex<Option<std::sync::Arc<Vec<u8>>>> {
    LATEST_SCREENSHOT.get_or_init(|| std::sync::Mutex::new(None))
}

fn server_token_slot(
) -> &'static std::sync::Mutex<Option<std::sync::Arc<tokio::sync::RwLock<String>>>> {
    SERVER_TOKEN.get_or_init(|| std::sync::Mutex::new(None))
}

fn set_latest_screenshot(bytes: Vec<u8>) {
    *latest_screenshot_slot().lock().unwrap() = Some(std::sync::Arc::new(bytes));
}

fn get_latest_screenshot() -> Option<std::sync::Arc<Vec<u8>>> {
    latest_screenshot_slot().lock().unwrap().clone()
}

/// Captura a tela principal e devolve (bytes RGB, largura, altura).
/// `max_dim = Some(n)` reduz para no maximo n px no maior lado;
/// `None` devolve a resolucao original (usado pelo OCR, para as
/// coordenadas baterem com a tela real).
fn capture_primary_screen_rgb(max_dim: Option<u32>) -> Result<(Vec<u8>, u32, u32), String> {
    // A camada e apenas visual para a pessoa; nunca deve entrar em screenshots/OCR.
    set_agent_activity(false);
    let mut monitors =
        Monitor::all().map_err(|e| format!("Failed to list monitors: {}", e))?;
    if monitors.is_empty() {
        return Err("Nenhum monitor encontrado".to_string());
    }
    let primary_idx = monitors
        .iter()
        .position(|m| m.is_primary().unwrap_or(false))
        .unwrap_or(0);
    let monitor = monitors.remove(primary_idx);

    let mut last_err = String::new();
    for attempt in 0..3 {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_millis(300));
        }
        match monitor.capture_image() {
            Ok(img) => {
                let (w, h) = (img.width(), img.height());
                if w == 0 || h == 0 {
                    last_err = "imagem com dimensao 0x0".to_string();
                    continue;
                }
                let rgba: ImageBuffer<Rgba<u8>, Vec<u8>> =
                    match ImageBuffer::from_raw(w, h, img.into_raw()) {
                        Some(b) => b,
                        None => {
                            last_err = "Failed to build image buffer".to_string();
                            continue;
                        }
                    };
                let rgb = DynamicImage::ImageRgba8(rgba).to_rgb8();
                if is_blank_rgb(rgb.as_raw(), w, h) {
                    last_err = "frame em branco/preto".to_string();
                    continue;
                }
                let (out_w, out_h, out_raw) = match max_dim {
                    Some(m) => {
                        let small = downscale_to_max(DynamicImage::ImageRgb8(rgb), m);
                        let (sw, sh) = (small.width(), small.height());
                        (sw, sh, small.to_rgb8().into_raw())
                    }
                    None => (w, h, rgb.into_raw()),
                };
                return Ok((out_raw, out_w, out_h));
            }
            Err(e) => {
                last_err = e.to_string();
            }
        }
    }

    Err(format!(
        "Falha ao capturar a tela apos 3 tentativas ({}). Verifique se a tela esta ligada/desbloqueada e, no Linux com Wayland, se a permissao de captura de tela foi concedida ao Spark Bridge.",
        last_err
    ))
}

/// Captura a tela e devolve (bytes JPEG, largura, altura), atualizando o
/// cache servido em GET /screenshot.jpg.
fn capture_primary_screen_jpeg(max_dim: Option<u32>) -> Result<(Vec<u8>, u32, u32), String> {
    let (rgb_raw, w, h) = capture_primary_screen_rgb(max_dim)?;
    let img = DynamicImage::ImageRgb8(
        ImageBuffer::from_raw(w, h, rgb_raw).ok_or("Failed to build image buffer".to_string())?,
    );
    let mut jpeg_bytes: Vec<u8> = Vec::new();
    let mut cursor = Cursor::new(&mut jpeg_bytes);
    img.write_to(&mut cursor, image::ImageFormat::Jpeg)
        .map_err(|e| format!("Failed to encode JPEG: {}", e))?;
    set_latest_screenshot(jpeg_bytes.clone());
    Ok((jpeg_bytes, w, h))
}

/// Renderiza a captura como arte ASCII (texto) para clientes MCP que
/// nao repassam imagens ao modelo — a IA enxerga o layout da tela em texto.
fn rgb_to_ascii(rgb: &[u8], w: u32, h: u32, cols: usize) -> String {
    let img = match ImageBuffer::from_raw(w, h, rgb.to_vec()) {
        Some(b) => DynamicImage::ImageRgb8(b),
        None => return String::new(),
    };
    let rows = ((cols as f32 * h as f32 / w as f32 / 2.0) as u32).max(1);
    let small = img.resize(cols as u32, rows, image::imageops::FilterType::Triangle);
    let gray = small.to_luma8();
    let chars = " .:-=+*#%@";
    let mut out = String::new();
    for y in 0..gray.height() {
        for x in 0..gray.width() {
            let lum = gray.get_pixel(x, y)[0] as usize;
            // Telas sao majoritariamente claras: inverte para fundo=espaco.
            let idx = (255 - lum) * (chars.len() - 1) / 255;
            out.push(chars.as_bytes()[idx] as char);
        }
        out.push('\n');
    }
    out
}

/// Extrai palavras do TSV do tesseract (nivel 5 = palavra).
fn parse_tsv_words(tsv: &str) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    for line in tsv.lines().skip(1) {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 12 || f[0] != "5" {
            continue;
        }
        let conf: f32 = f[10].parse().unwrap_or(-1.0);
        let text = f[11].trim();
        if conf <= 0.0 || text.is_empty() {
            continue;
        }
        out.push(serde_json::json!({
            "text": text,
            "x": f[6].parse::<u32>().unwrap_or(0),
            "y": f[7].parse::<u32>().unwrap_or(0),
            "w": f[8].parse::<u32>().unwrap_or(0),
            "h": f[9].parse::<u32>().unwrap_or(0),
            "conf": conf,
        }));
    }
    out
}

/// Captura em resolucao original e roda OCR via `tesseract` (se instalado).
/// Devolve {width, height, words: [{text, x, y, w, h, conf}]} com
/// coordenadas na escala real da tela.
async fn ocr_current_screen() -> Result<serde_json::Value, String> {
    let (jpeg, w, h) = tokio::task::spawn_blocking(|| capture_primary_screen_jpeg(None))
        .await
        .map_err(|e| e.to_string())??;

    let tmp_path = std::env::temp_dir().join(format!(
        "spark-ocr-{}-{}.jpg",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::write(&tmp_path, &jpeg).map_err(|e| e.to_string())?;

    let output = tokio::process::Command::new("tesseract")
        .arg(&tmp_path)
        .arg("stdout")
        .arg("tsv")
        .output()
        .await;
    let _ = std::fs::remove_file(&tmp_path);

    let output = match output {
        Ok(o) => o,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(
                "tesseract nao encontrado no PATH. Instale para usar OCR: Windows: winget install UB-Mannheim.TesseractOCR | Linux: sudo apt install tesseract-ocr | macOS: brew install tesseract"
                    .to_string(),
            );
        }
        Err(e) => return Err(e.to_string()),
    };

    if !output.status.success() {
        return Err(format!(
            "tesseract falhou: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let tsv = String::from_utf8_lossy(&output.stdout).to_string();
    Ok(serde_json::json!({
        "width": w,
        "height": h,
        "words": parse_tsv_words(&tsv),
    }))
}

async fn get_screen() -> impl IntoResponse {
    let res =
        tokio::task::spawn_blocking(|| capture_primary_screen_jpeg(Some(1600))).await;

    match res {
        Ok(Ok((bytes, width, height))) => (
            StatusCode::OK,
            Json(ScreenResponse {
                width,
                height,
                format: "jpeg",
                image_base64: BASE64.encode(&bytes),
            }),
        )
            .into_response(),
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
    set_agent_activity(true);
    let res = tokio::task::spawn_blocking(move || -> Result<(), String> {
        let mut enigo = Enigo::new(&Settings::default()).map_err(|e| e.to_string())?;
        enigo
            .move_mouse(req.x, req.y, Coordinate::Abs)
            .map_err(|e| e.to_string())?;
        Ok(())
    })
    .await;
    set_agent_activity(false);

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
    set_agent_activity(true);
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
    set_agent_activity(false);

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
    set_agent_activity(true);
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
    set_agent_activity(false);

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
    set_agent_activity(true);
    let res = tokio::task::spawn_blocking(move || -> Result<(), String> {
        let mut enigo = Enigo::new(&Settings::default()).map_err(|e| e.to_string())?;
        enigo.text(&req.text).map_err(|e| e.to_string())?;
        Ok(())
    })
    .await;
    set_agent_activity(false);

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
    set_agent_activity(true);
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
    set_agent_activity(false);

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

async fn mouse_drag(Json(req): Json<MouseDragReq>) -> impl IntoResponse {
    set_agent_activity(true);
    let res = tokio::task::spawn_blocking(move || -> Result<(), String> {
        let mut enigo = Enigo::new(&Settings::default()).map_err(|e| e.to_string())?;
        let btn = match req.button.as_deref() {
            Some("right") => Button::Right,
            Some("middle") => Button::Middle,
            _ => Button::Left,
        };
        enigo
            .move_mouse(req.x1, req.y1, Coordinate::Abs)
            .map_err(|e| e.to_string())?;
        enigo.button(btn, Direction::Press).map_err(|e| e.to_string())?;
        enigo
            .move_mouse(req.x2, req.y2, Coordinate::Abs)
            .map_err(|e| e.to_string())?;
        enigo.button(btn, Direction::Release).map_err(|e| e.to_string())?;
        Ok(())
    })
    .await;
    set_agent_activity(false);

    match res {
        Ok(Ok(())) => (
            StatusCode::OK,
            Json(ActionRes {
                ok: true,
                message: "Mouse dragged".into(),
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

async fn screen_info() -> impl IntoResponse {
    let res = tokio::task::spawn_blocking(|| -> Result<serde_json::Value, String> {
        let monitors = Monitor::all().map_err(|e| format!("Failed to list monitors: {}", e))?;
        let infos: Vec<serde_json::Value> = monitors
            .iter()
            .map(|m| {
                serde_json::json!({
                    "name": m.name().unwrap_or_default(),
                    "width": m.width().unwrap_or(0),
                    "height": m.height().unwrap_or(0),
                    "is_primary": m.is_primary().unwrap_or(false),
                })
            })
            .collect();
        Ok(serde_json::json!({ "monitors": infos, "count": infos.len() }))
    })
    .await;

    match res {
        Ok(Ok(data)) => (StatusCode::OK, Json(data)).into_response(),
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

async fn ocr_screen_rest() -> impl IntoResponse {
    match ocr_current_screen().await {
        Ok(data) => (StatusCode::OK, Json(data)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e })),
        )
            .into_response(),
    }
}

async fn clipboard_read() -> impl IntoResponse {
    let res = tokio::task::spawn_blocking(|| -> Result<String, String> {
        let mut cb = arboard::Clipboard::new().map_err(|e| e.to_string())?;
        cb.get_text().map_err(|e| e.to_string())
    })
    .await;

    match res {
        Ok(Ok(text)) => (StatusCode::OK, Json(serde_json::json!({ "text": text }))).into_response(),
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

async fn clipboard_write(Json(req): Json<ClipboardWriteReq>) -> impl IntoResponse {
    let res = tokio::task::spawn_blocking(move || -> Result<(), String> {
        let mut cb = arboard::Clipboard::new().map_err(|e| e.to_string())?;
        cb.set_text(req.text).map_err(|e| e.to_string())
    })
    .await;

    match res {
        Ok(Ok(())) => (
            StatusCode::OK,
            Json(ActionRes {
                ok: true,
                message: "Clipboard updated".into(),
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

async fn open_url(Json(req): Json<OpenUrlReq>) -> impl IntoResponse {
    let url = req.url.trim().to_string();
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return (
            StatusCode::BAD_REQUEST,
            Json(ActionRes {
                ok: false,
                message: "URL must start with http:// or https://".into(),
            }),
        )
            .into_response();
    }

    let res = tokio::task::spawn_blocking(move || -> Result<(), String> {
        let status = if cfg!(target_os = "windows") {
            std::process::Command::new("cmd")
                .args(["/C", "start", "", &url])
                .status()
        } else if cfg!(target_os = "macos") {
            std::process::Command::new("open").arg(&url).status()
        } else {
            std::process::Command::new("xdg-open").arg(&url).status()
        };
        match status {
            Ok(s) if s.success() => Ok(()),
            Ok(s) => Err(format!("Failed to open URL (exit {})", s)),
            Err(e) => Err(e.to_string()),
        }
    })
    .await;

    match res {
        Ok(Ok(())) => (
            StatusCode::OK,
            Json(ActionRes {
                ok: true,
                message: "URL opened".into(),
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
                "description": "Capture the user's primary screen as a JPEG image (downscaled to max 1600px).",
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
            },
            {
                "name": "mouse_drag",
                "description": "Drag the mouse from (x1, y1) to (x2, y2) holding a button (for drag & drop, text selection, resizing).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "x1": { "type": "integer" },
                        "y1": { "type": "integer" },
                        "x2": { "type": "integer" },
                        "y2": { "type": "integer" },
                        "button": { "type": "string", "enum": ["left", "right", "middle"] }
                    },
                    "required": ["x1", "y1", "x2", "y2"]
                }
            },
            {
                "name": "get_screen_info",
                "description": "Get monitor info (name, resolution, primary) to calibrate coordinates.",
                "inputSchema": { "type": "object", "properties": {} }
            },
            {
                "name": "clipboard_read",
                "description": "Read the system clipboard text.",
                "inputSchema": { "type": "object", "properties": {} }
            },
            {
                "name": "clipboard_write",
                "description": "Write text to the system clipboard.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "text": { "type": "string" }
                    },
                    "required": ["text"]
                }
            },
            {
                "name": "open_url",
                "description": "Open an http(s) URL in the default browser.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "url": { "type": "string" }
                    },
                    "required": ["url"]
                }
            },
            {
                "name": "wait",
                "description": "Wait for a number of milliseconds (max 30000). Useful after opening apps.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "ms": { "type": "integer" }
                    },
                    "required": ["ms"]
                }
            },
            {
                "name": "screen_ascii",
                "description": "Text-only preview of the screen as ASCII art (for clients that do not forward images to the model). Shows layout to decide where to click/scroll.",
                "inputSchema": { "type": "object", "properties": {} }
            },
            {
                "name": "ocr_screen",
                "description": "OCR the screen (requires tesseract installed) and return visible words with (x, y) coordinates in real screen pixels. Best for precise clicking on text-only clients.",
                "inputSchema": { "type": "object", "properties": {} }
            }
        ]
    })
}

async fn call_tool(name: &str, args: serde_json::Value) -> (serde_json::Value, bool) {
    if let Some(permissions) = PERMISSIONS.get() {
        if !permissions.read().await.get(name).copied().unwrap_or(false) {
            return (serde_json::json!([{ "type": "text", "text": format!("Ferramenta bloqueada pela aba Segurança: {}", name) }]), true);
        }
    }
    match name {
        "screen_capture" => {
            let res =
                tokio::task::spawn_blocking(|| capture_primary_screen_jpeg(Some(1600))).await;
            match res {
                Ok(Ok((bytes, w, h))) => {
                    // A imagem vem PRIMEIRO: alguns clientes MCP leem apenas o
                    // primeiro bloco de conteudo ou truncam o resultado — assim
                    // a captura chega ao modelo em vez de so a confirmacao textual.
                    // O texto inclui a URL da captura (para clientes so-texto
                    // e para o usuario visualizar o print no navegador).
                    let token_arc = server_token_slot().lock().unwrap().clone();
                    let token_str = match token_arc {
                        Some(t) => t.read().await.clone(),
                        None => String::new(),
                    };
                    (
                        serde_json::json!([
                            {
                                "type": "image",
                                "data": BASE64.encode(&bytes),
                                "mimeType": "image/jpeg"
                            },
                            { "type": "text", "text": format!(
                                "Screenshot {}x{} (JPEG). Ver imagem: /screenshot.jpg?token={} (prefixe com a URL do tunel). Para clientes sem suporte a imagem, use screen_ascii ou ocr_screen.",
                                w, h, token_str
                            ) }
                        ]),
                        false,
                    )
                }
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
        "mouse_drag" => {
            let parsed: Result<MouseDragReq, _> = serde_json::from_value(args);
            match parsed {
                Ok(r) => {
                    let _ = mouse_drag(Json(r)).await;
                    (serde_json::json!([{ "type": "text", "text": "Mouse dragged" }]), false)
                }
                Err(e) => (
                    serde_json::json!([{ "type": "text", "text": format!("Invalid args: {}", e) }]),
                    true,
                ),
            }
        }
        "get_screen_info" => {
            let res = tokio::task::spawn_blocking(|| -> Result<serde_json::Value, String> {
                let monitors = Monitor::all().map_err(|e| e.to_string())?;
                let infos: Vec<serde_json::Value> = monitors
                    .iter()
                    .map(|m| {
                        serde_json::json!({
                            "name": m.name().unwrap_or_default(),
                            "width": m.width().unwrap_or(0),
                            "height": m.height().unwrap_or(0),
                            "is_primary": m.is_primary().unwrap_or(false),
                        })
                    })
                    .collect();
                Ok(serde_json::json!({ "monitors": infos, "count": infos.len() }))
            })
            .await;
            match res {
                Ok(Ok(v)) => (
                    serde_json::json!([{ "type": "text", "text": v.to_string() }]),
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
        "clipboard_read" => {
            let res = tokio::task::spawn_blocking(|| -> Result<String, String> {
                let mut cb = arboard::Clipboard::new().map_err(|e| e.to_string())?;
                cb.get_text().map_err(|e| e.to_string())
            })
            .await;
            match res {
                Ok(Ok(text)) => (
                    serde_json::json!([{ "type": "text", "text": text }]),
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
        "clipboard_write" => {
            let parsed: Result<ClipboardWriteReq, _> = serde_json::from_value(args);
            match parsed {
                Ok(r) => {
                    let _ = clipboard_write(Json(r)).await;
                    (serde_json::json!([{ "type": "text", "text": "Clipboard updated" }]), false)
                }
                Err(e) => (
                    serde_json::json!([{ "type": "text", "text": format!("Invalid args: {}", e) }]),
                    true,
                ),
            }
        }
        "open_url" => {
            let parsed: Result<OpenUrlReq, _> = serde_json::from_value(args);
            match parsed {
                Ok(r) => {
                    let _ = open_url(Json(r)).await;
                    (serde_json::json!([{ "type": "text", "text": "URL opened" }]), false)
                }
                Err(e) => (
                    serde_json::json!([{ "type": "text", "text": format!("Invalid args: {}", e) }]),
                    true,
                ),
            }
        }
        "wait" => {
            let parsed: Result<WaitReq, _> = serde_json::from_value(args);
            match parsed {
                Ok(r) => {
                    let ms = r.ms.min(30000);
                    tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
                    (
                        serde_json::json!([{ "type": "text", "text": format!("Waited {}ms", ms) }]),
                        false,
                    )
                }
                Err(e) => (
                    serde_json::json!([{ "type": "text", "text": format!("Invalid args: {}", e) }]),
                    true,
                ),
            }
        }
        "screen_ascii" => {
            let res = tokio::task::spawn_blocking(|| -> Result<String, String> {
                let (rgb, w, h) = capture_primary_screen_rgb(Some(1600))?;
                let art = rgb_to_ascii(&rgb, w, h, 120);
                Ok(format!("Screen {}x{} (ASCII preview):\n{}", w, h, art))
            })
            .await;
            match res {
                Ok(Ok(text)) => (
                    serde_json::json!([{ "type": "text", "text": text }]),
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
        "ocr_screen" => match ocr_current_screen().await {
            Ok(v) => (
                serde_json::json!([{ "type": "text", "text": v.to_string() }]),
                false,
            ),
            Err(e) => (
                serde_json::json!([{ "type": "text", "text": format!("Error: {}", e) }]),
                true,
            ),
        },
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
    app_handle: Option<AppHandle>,
    permissions: Arc<RwLock<BTreeMap<String, bool>>>,
    oauth_client_id: String,
    oauth_client_secret: String,
) -> Result<(), String> {
    if let Some(app_handle) = app_handle {
        let _ = APP_HANDLE.set(app_handle);
    }
    let _ = PERMISSIONS.set(permissions.clone());
    if let Ok(mut store) = oauth_store().lock() {
        store.client_id = oauth_client_id;
        store.client_secret = oauth_client_secret;
    }
    // Registra o token atual para montar a URL da captura no screen_capture.
    *server_token_slot().lock().unwrap() = Some(token.clone());
    let state = ServerState { token, permissions };

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
        .route("/.well-known/oauth-protected-resource", get(oauth_protected_resource))
        .route("/.well-known/oauth-authorization-server", get(oauth_authorization_server))
        .route("/oauth/authorize", get(oauth_authorize_page).post(oauth_authorize_submit))
        .route("/oauth/register", post(oauth_register))
        .route("/oauth/token", post(oauth_token))
        .route("/", post(mcp_post).get(health))
        .route(
            "/mcp",
            post(mcp_post).get(mcp_sse).delete(mcp_delete),
        )
        .route("/screen", get(get_screen))
        .route("/screen/info", get(screen_info))
        .route("/screen/ocr", get(ocr_screen_rest))
        .route("/screenshot.jpg", get(latest_screenshot))
        .route("/mouse/move", post(mouse_move))
        .route("/mouse/click", post(mouse_click))
        .route("/mouse/scroll", post(mouse_scroll))
        .route("/mouse/drag", post(mouse_drag))
        .route("/keyboard/type", post(keyboard_type))
        .route("/keyboard/key", post(keyboard_key))
        .route("/terminal/exec", post(terminal_exec))
        .route("/clipboard", get(clipboard_read).post(clipboard_write))
        .route("/open-url", post(open_url))
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
    use super::{downscale_to_max, is_blank_rgb, parse_tsv_words, rgb_to_ascii, start_http_server};
    use image::DynamicImage;
    use std::{collections::BTreeMap, sync::Arc};
    use tokio::sync::RwLock;

    #[test]
    fn tsv_words_parsing() {
        let tsv = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n5\t1\t1\t1\t1\t1\t10\t20\t50\t12\t95.5\tOla\n5\t1\t1\t1\t1\t2\t70\t20\t60\t12\t88.0\tmundo\n1\t1\t1\t1\t1\t0\t0\t0\t100\t100\t-1\t";
        let words = parse_tsv_words(tsv);
        assert_eq!(words.len(), 2);
        assert_eq!(words[0]["text"], "Ola");
        assert_eq!(words[0]["x"], 10);
        assert_eq!(words[1]["text"], "mundo");
    }

    #[test]
    fn ascii_renders() {
        // Imagem branca 64x32 -> arte so com espacos/quebras.
        let white = vec![255u8; 64 * 32 * 3];
        let art = rgb_to_ascii(&white, 64, 32, 32);
        assert!(!art.is_empty());
        assert!(art.contains('\n'));
        // Imagem preta -> caracteres densos presentes.
        let black = vec![0u8; 64 * 32 * 3];
        let art2 = rgb_to_ascii(&black, 64, 32, 32);
        assert!(art2.contains('@'));
    }

    #[test]
    fn blank_detection() {
        let black = vec![0u8; 64 * 64 * 3];
        assert!(is_blank_rgb(&black, 64, 64));
        let mut img = vec![0u8; 64 * 64 * 3];
        img[100] = 255; // um pixel claro ja basta para nao ser "vazia"
        assert!(!is_blank_rgb(&img, 64, 64));
        assert!(is_blank_rgb(&[], 0, 0));
    }

    #[test]
    fn downscale_dims() {
        let big = DynamicImage::new_rgb8(3840, 2160);
        let small = downscale_to_max(big, 1920);
        assert_eq!((small.width(), small.height()), (1920, 1080));
        let small_img = DynamicImage::new_rgb8(800, 600);
        let kept = downscale_to_max(small_img, 1920);
        assert_eq!((kept.width(), kept.height()), (800, 600));
    }

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
        let server = tokio::spawn(start_http_server(4899, token, rx, None, Arc::new(RwLock::new(BTreeMap::new())), "spark-bridge-gemini".to_string(), "test-oauth-secret".to_string()));

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

        // tools/list -> 15 ferramentas
        let list = post_mcp(
            &client,
            base,
            serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
        )
        .await;
        let list_body: serde_json::Value = list.json().await.unwrap();
        assert_eq!(list_body["result"]["tools"].as_array().unwrap().len(), 15);

        // /screenshot.jpg sem captura previa -> 404 (com token)
        let shot = client
            .get("http://127.0.0.1:4899/screenshot.jpg?token=test-token")
            .send()
            .await
            .unwrap();
        assert_eq!(shot.status().as_u16(), 404);

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
