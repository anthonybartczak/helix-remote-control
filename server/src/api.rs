//! HTTP API (see `docs/api.md`) and the web UI from `/web`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use axum::extract::{FromRequestParts, Path, Query, State};
use axum::http::request::Parts;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::device::{Command, Device, DeviceError};
use crate::parse;

/// Shared by every request.
pub struct AppState<D> {
    device: D,
    /// Shared token for write requests. `None` turns writes off.
    token: Option<String>,
    /// True after a bypass or parameter change through the bridge, until the
    /// next load or save. Edits made on the pedal itself are not seen.
    unsaved: AtomicBool,
}

impl<D: Device> AppState<D> {
    pub fn new(device: D, token: Option<String>) -> Self {
        AppState {
            device,
            token: token.filter(|t| !t.is_empty()),
            unsaved: AtomicBool::new(false),
        }
    }
}

type Shared<D> = State<Arc<AppState<D>>>;

pub fn router<D: Device>(state: AppState<D>) -> Router {
    Router::new()
        .route("/api/health", get(health::<D>))
        .route("/api/device", get(device_info::<D>))
        .route("/api/presets", get(presets::<D>))
        .route("/api/preset", get(current_preset::<D>))
        .route("/api/chain", get(chain::<D>))
        .route("/api/select", post(select::<D>))
        .route("/api/blocks/{position}/enabled", post(enable::<D>))
        .route(
            "/api/blocks/{position}/params/{index}",
            post(set_param::<D>),
        )
        .route("/api/presets/{index}/rename", post(rename::<D>))
        .route("/api/save", post(save::<D>))
        .route("/", get(index))
        .route("/app.css", get(app_css))
        .route("/app.js", get(app_js))
        .with_state(Arc::new(state))
}

// The UI is compiled into the binary so the Pi gets one file to deploy.
// `no-cache` makes Safari revalidate after an update instead of running stale JS.
const NO_CACHE: (header::HeaderName, &str) = (header::CACHE_CONTROL, "no-cache");

async fn index() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8"), NO_CACHE],
        include_str!("../../web/index.html"),
    )
}

async fn app_css() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8"), NO_CACHE],
        include_str!("../../web/app.css"),
    )
}

async fn app_js() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
            NO_CACHE,
        ],
        include_str!("../../web/app.js"),
    )
}

enum ApiError {
    Device(DeviceError),
    /// Write sent without the right token.
    Unauthorized,
    /// No `HELIX_TOKEN` configured, so the server takes no writes.
    WritesDisabled,
    /// Request body failed our own checks.
    BadRequest(String),
}

impl ApiError {
    /// Stable machine-readable cause, so the UI can tell "no device" (blocking
    /// screen) from a one-off failure (banner) without parsing messages.
    fn code(&self) -> &'static str {
        match self {
            ApiError::Device(DeviceError::Spawn(_)) => "spawn",
            ApiError::Device(DeviceError::NotConnected(_)) => "no_device",
            ApiError::Device(DeviceError::Failed(_)) => "failed",
            ApiError::Device(DeviceError::Timeout(_)) => "timeout",
            ApiError::Device(DeviceError::NoFixture(_)) => "no_fixture",
            ApiError::Device(DeviceError::Parse(_)) => "parse",
            ApiError::Unauthorized => "unauthorized",
            ApiError::WritesDisabled => "writes_disabled",
            ApiError::BadRequest(_) => "bad_request",
        }
    }

    fn status(&self) -> StatusCode {
        match self {
            ApiError::Device(e) => match e {
                DeviceError::Spawn(_) | DeviceError::NotConnected(_) => {
                    StatusCode::SERVICE_UNAVAILABLE
                }
                DeviceError::Timeout(_) => StatusCode::GATEWAY_TIMEOUT,
                DeviceError::Failed(_) | DeviceError::Parse(_) => StatusCode::BAD_GATEWAY,
                DeviceError::NoFixture(_) => StatusCode::NOT_FOUND,
            },
            ApiError::Unauthorized => StatusCode::UNAUTHORIZED,
            ApiError::WritesDisabled => StatusCode::FORBIDDEN,
            ApiError::BadRequest(_) => StatusCode::BAD_REQUEST,
        }
    }

    fn message(&self) -> String {
        match self {
            ApiError::Device(e) => e.to_string(),
            ApiError::Unauthorized => "missing or wrong token".into(),
            ApiError::WritesDisabled => {
                "writes are off: set HELIX_TOKEN on the server to turn them on".into()
            }
            ApiError::BadRequest(msg) => msg.clone(),
        }
    }
}

impl From<DeviceError> for ApiError {
    fn from(e: DeviceError) -> Self {
        ApiError::Device(e)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.status();
        let message = self.message();
        tracing::warn!(%status, error = %message, "request failed");
        (
            status,
            Json(json!({ "error": message, "code": self.code() })),
        )
            .into_response()
    }
}

type ApiResult<T> = Result<Json<T>, ApiError>;

#[derive(Serialize)]
struct Health {
    status: &'static str,
    backend: &'static str,
    version: &'static str,
    /// Whether the server takes write requests (a token is configured).
    writes: bool,
}

/// Server liveness only; does not touch the device.
async fn health<D: Device>(State(app): Shared<D>) -> Json<Health> {
    Json(Health {
        status: "ok",
        backend: app.device.kind(),
        version: env!("CARGO_PKG_VERSION"),
        writes: app.token.is_some(),
    })
}

async fn device_info<D: Device>(State(app): Shared<D>) -> ApiResult<parse::DeviceInfo> {
    let out = app.device.run(Command::Info).await?;
    Ok(Json(parse::info(&out)?))
}

#[derive(Deserialize)]
struct SetlistQuery {
    #[serde(default)]
    setlist: u32,
}

#[derive(Serialize)]
struct PresetList {
    setlist: u32,
    presets: Vec<parse::PresetEntry>,
}

async fn presets<D: Device>(
    State(app): Shared<D>,
    Query(q): Query<SetlistQuery>,
) -> ApiResult<PresetList> {
    let out = app
        .device
        .run(Command::Presets { setlist: q.setlist })
        .await?;
    Ok(Json(PresetList {
        setlist: q.setlist,
        presets: parse::presets(&out)?,
    }))
}

async fn current_preset<D: Device>(State(app): Shared<D>) -> ApiResult<parse::CurrentPreset> {
    let out = app.device.run(Command::Preset).await?;
    Ok(Json(parse::preset(&out)?))
}

#[derive(Serialize)]
struct ChainView {
    #[serde(flatten)]
    chain: parse::Chain,
    unsaved: bool,
}

async fn chain<D: Device>(State(app): Shared<D>) -> ApiResult<ChainView> {
    let out = app.device.run(Command::Chain).await?;
    Ok(Json(ChainView {
        chain: parse::chain(&out)?,
        unsaved: app.unsaved.load(Ordering::Relaxed),
    }))
}

// ---------- Writes ----------

/// HX preset names are at most 16 characters (front panel and HX Edit).
const MAX_NAME: usize = 16;
/// Longest parameter value worth sending, e.g. `-22.1 dB`, `1/8 Dotted`.
const MAX_VALUE: usize = 32;

/// Compare in time that does not depend on where the strings differ.
fn same_token(given: &[u8], want: &[u8]) -> bool {
    given.len() == want.len() && given.iter().zip(want).fold(0, |acc, (a, b)| acc | (a ^ b)) == 0
}

/// Proof that a request carried `Authorization: Bearer <HELIX_TOKEN>`
/// (AGENTS.md 5.7). Every write handler takes it first, so a request without
/// the token is refused before its body is even read.
struct WriteAccess(());

impl<D: Device> FromRequestParts<Arc<AppState<D>>> for WriteAccess {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        app: &Arc<AppState<D>>,
    ) -> Result<Self, ApiError> {
        authorize(app.token.as_deref(), &parts.headers).map(WriteAccess)
    }
}

fn authorize(token: Option<&str>, headers: &HeaderMap) -> Result<(), ApiError> {
    let want = token.ok_or(ApiError::WritesDisabled)?;
    let given = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or(ApiError::Unauthorized)?;
    if same_token(given.trim().as_bytes(), want.as_bytes()) {
        Ok(())
    } else {
        Err(ApiError::Unauthorized)
    }
}

/// Printable ASCII, trimmed, 1..=max characters.
fn checked_text(what: &str, text: &str, max: usize) -> Result<String, ApiError> {
    let text = text.trim();
    if text.is_empty() {
        return Err(ApiError::BadRequest(format!("{what} is empty")));
    }
    if text.len() > max {
        return Err(ApiError::BadRequest(format!(
            "{what} is longer than {max} characters"
        )));
    }
    if !text.bytes().all(|b| (0x20..0x7f).contains(&b)) {
        return Err(ApiError::BadRequest(format!(
            "{what} may only use plain letters, digits and punctuation"
        )));
    }
    Ok(text.to_owned())
}

fn checked_position(position: u32) -> Result<u32, ApiError> {
    if position == 0 {
        return Err(ApiError::BadRequest(
            "blocks are numbered from 1, as in /api/chain".into(),
        ));
    }
    Ok(position)
}

#[derive(Serialize)]
struct WriteResult {
    /// What `tonepush` printed, e.g. `block 2 engaged`.
    message: String,
    unsaved: bool,
    /// For a parameter write: the value as the device now shows it
    /// (`40 %`), so the UI can update without re-reading the chain.
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<String>,
}

/// `block 2: Position = 40 %` (or `parameter 0 = 0.4` without the catalog).
fn set_value(message: &str) -> Option<String> {
    message
        .rsplit_once(" = ")
        .map(|(_, v)| v.trim().to_owned())
        .filter(|v| !v.is_empty())
}

/// Run one write and update the unsaved flag on success.
async fn write<D: Device>(
    app: &AppState<D>,
    _: WriteAccess,
    cmd: Command,
) -> ApiResult<WriteResult> {
    let is_set = matches!(cmd, Command::Set { .. });
    let unsaved = match cmd {
        Command::Enable { .. } | Command::Set { .. } => Some(true),
        Command::Select { .. } | Command::Save => Some(false),
        _ => None,
    };
    let out = app.device.run(cmd).await?;
    if let Some(flag) = unsaved {
        app.unsaved.store(flag, Ordering::Relaxed);
    }
    let message = out.trim().to_owned();
    Ok(Json(WriteResult {
        value: is_set.then(|| set_value(&message)).flatten(),
        message,
        unsaved: app.unsaved.load(Ordering::Relaxed),
    }))
}

#[derive(Deserialize)]
struct SelectBody {
    index: u32,
    #[serde(default)]
    setlist: u32,
}

async fn select<D: Device>(
    State(app): Shared<D>,
    access: WriteAccess,
    Json(body): Json<SelectBody>,
) -> ApiResult<WriteResult> {
    let cmd = Command::Select {
        setlist: body.setlist,
        index: body.index,
    };
    write(&app, access, cmd).await
}

#[derive(Deserialize)]
struct EnableBody {
    enabled: bool,
}

async fn enable<D: Device>(
    State(app): Shared<D>,
    access: WriteAccess,
    Path(position): Path<u32>,
    Json(body): Json<EnableBody>,
) -> ApiResult<WriteResult> {
    let cmd = Command::Enable {
        block: checked_position(position)?,
        on: body.enabled,
    };
    write(&app, access, cmd).await
}

#[derive(Deserialize)]
struct ParamBody {
    value: String,
}

async fn set_param<D: Device>(
    State(app): Shared<D>,
    access: WriteAccess,
    Path((position, index)): Path<(u32, u32)>,
    Json(body): Json<ParamBody>,
) -> ApiResult<WriteResult> {
    let cmd = Command::Set {
        block: checked_position(position)?,
        param: index,
        value: checked_text("value", &body.value, MAX_VALUE)?,
    };
    write(&app, access, cmd).await
}

#[derive(Deserialize)]
struct RenameBody {
    name: String,
    #[serde(default)]
    setlist: u32,
}

async fn rename<D: Device>(
    State(app): Shared<D>,
    access: WriteAccess,
    Path(index): Path<u32>,
    Json(body): Json<RenameBody>,
) -> ApiResult<WriteResult> {
    let cmd = Command::Rename {
        setlist: body.setlist,
        index,
        name: checked_text("name", &body.name, MAX_NAME)?,
    };
    write(&app, access, cmd).await
}

async fn save<D: Device>(State(app): Shared<D>, access: WriteAccess) -> ApiResult<WriteResult> {
    write(&app, access, Command::Save).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::FakeDevice;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use serde_json::Value;
    use tower::ServiceExt;

    const TOKEN: &str = "test-token";

    fn app() -> Router {
        router(AppState::new(FakeDevice::bundled(), Some(TOKEN.into())))
    }

    async fn send(app: &Router, req: Request<Body>) -> (StatusCode, Value) {
        let res = app.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let body = res.into_body().collect().await.unwrap().to_bytes();
        let json = serde_json::from_slice(&body)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&body).into()));
        (status, json)
    }

    async fn get_json(uri: &str) -> (StatusCode, Value) {
        send(&app(), Request::get(uri).body(Body::empty()).unwrap()).await
    }

    async fn post(
        app: &Router,
        uri: &str,
        token: Option<&str>,
        body: Value,
    ) -> (StatusCode, Value) {
        let mut req = Request::post(uri).header(header::CONTENT_TYPE, "application/json");
        if let Some(t) = token {
            req = req.header(header::AUTHORIZATION, format!("Bearer {t}"));
        }
        send(app, req.body(Body::from(body.to_string())).unwrap()).await
    }

    #[tokio::test]
    async fn health() {
        let (status, body) = get_json("/api/health").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["status"], "ok");
        assert_eq!(body["backend"], "fake");
        assert_eq!(body["writes"], true);
    }

    #[tokio::test]
    async fn device() {
        let (status, body) = get_json("/api/device").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["name"], "HX Effects");
        assert_eq!(body["firmware"], "3.80");
    }

    #[tokio::test]
    async fn presets() {
        let (status, body) = get_json("/api/presets").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["setlist"], 0);
        assert_eq!(body["presets"].as_array().unwrap().len(), 128);
        assert_eq!(body["presets"][2]["loaded"], true);
    }

    #[tokio::test]
    async fn presets_unknown_setlist() {
        let (status, body) = get_json("/api/presets?setlist=3").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body["error"].as_str().unwrap().contains("no fixture"));
        assert_eq!(body["code"], "no_fixture");
    }

    #[tokio::test]
    async fn serves_the_ui() {
        for (uri, mime) in [
            ("/", "text/html"),
            ("/app.css", "text/css"),
            ("/app.js", "text/javascript"),
        ] {
            let res = app()
                .oneshot(Request::get(uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::OK, "{uri}");
            let ct = res.headers()[header::CONTENT_TYPE].to_str().unwrap();
            assert!(ct.starts_with(mime), "{uri}: {ct}");
        }
    }

    #[tokio::test]
    async fn current_preset() {
        let (status, body) = get_json("/api/preset").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["label"], "01C");
        assert_eq!(body["index"], 2);
    }

    #[tokio::test]
    async fn chain() {
        let (status, body) = get_json("/api/chain").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["tempo"], 120.0);
        assert_eq!(body["blocks"][0]["model"], "Fassel");
        assert_eq!(body["blocks"][0]["enabled"], false);
        assert_eq!(body["blocks"][0]["params"][2]["value"], "2.2 kHz");
        assert!(body["blocks"][0].get("cab").is_none());
        assert_eq!(body["unsaved"], false);
    }

    #[tokio::test]
    async fn writes_need_the_token() {
        let app = app();
        let body = json!({ "enabled": true });
        for token in [None, Some("wrong"), Some("")] {
            let (status, res) = post(&app, "/api/blocks/2/enabled", token, body.clone()).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{token:?}");
            assert_eq!(res["code"], "unauthorized");
        }
        // Refused before the body is checked.
        let (status, _) = post(&app, "/api/blocks/0/enabled", None, body).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        // Reads stay open.
        let (status, _) = send(
            &app,
            Request::get("/api/chain").body(Body::empty()).unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn writes_off_without_a_token() {
        let app = router(AppState::new(FakeDevice::bundled(), Some(String::new())));
        let (status, res) = post(&app, "/api/save", Some(""), json!({})).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(res["code"], "writes_disabled");
        let (_, health) = send(
            &app,
            Request::get("/api/health").body(Body::empty()).unwrap(),
        )
        .await;
        assert_eq!(health["writes"], false);
    }

    #[tokio::test]
    async fn edit_then_save_tracks_unsaved() {
        let app = app();
        let t = Some(TOKEN);
        let (status, res) = post(
            &app,
            "/api/blocks/2/params/0",
            t,
            json!({ "value": " 40 % " }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{res}");
        assert_eq!(res["message"], "block 2: Position = 40 %");
        assert_eq!(res["unsaved"], true);
        assert_eq!(res["value"], "40 %");

        let (_, res) = post(&app, "/api/blocks/2/enabled", t, json!({ "enabled": true })).await;
        assert_eq!(res["message"], "block 2 engaged");
        assert!(res.get("value").is_none());

        let (_, chain) = send(
            &app,
            Request::get("/api/chain").body(Body::empty()).unwrap(),
        )
        .await;
        assert_eq!(chain["unsaved"], true);
        assert_eq!(chain["blocks"][0]["enabled"], true);
        assert_eq!(chain["blocks"][0]["params"][0]["value"], "40 %");

        let (status, res) = post(&app, "/api/save", t, json!({})).await;
        assert_eq!(status, StatusCode::OK, "{res}");
        assert_eq!(res["unsaved"], false);
    }

    #[tokio::test]
    async fn select_clears_unsaved() {
        let app = app();
        let t = Some(TOKEN);
        post(&app, "/api/blocks/2/enabled", t, json!({ "enabled": true })).await;
        let (status, res) = post(&app, "/api/select", t, json!({ "index": 104 })).await;
        assert_eq!(status, StatusCode::OK, "{res}");
        assert_eq!(res["message"], "selected 27A (setlist 0)");
        assert_eq!(res["unsaved"], false);
        let (_, preset) = send(
            &app,
            Request::get("/api/preset").body(Body::empty()).unwrap(),
        )
        .await;
        assert_eq!(preset["label"], "27A");
    }

    #[tokio::test]
    async fn rename_checks_the_name() {
        let app = app();
        let t = Some(TOKEN);
        let url = "/api/presets/2/rename";
        for bad in ["", "   ", "Seventeen chars!!", "Café"] {
            let (status, res) = post(&app, url, t, json!({ "name": bad })).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{bad:?}");
            assert_eq!(res["code"], "bad_request");
        }
        let (status, res) = post(&app, url, t, json!({ "name": "Clean Verb" })).await;
        assert_eq!(status, StatusCode::OK, "{res}");
        let (_, list) = send(
            &app,
            Request::get("/api/presets").body(Body::empty()).unwrap(),
        )
        .await;
        assert_eq!(list["presets"][2]["name"], "Clean Verb");
    }

    #[tokio::test]
    async fn bad_write_targets() {
        let app = app();
        let t = Some(TOKEN);
        let (status, _) = post(&app, "/api/blocks/0/enabled", t, json!({ "enabled": true })).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, res) =
            post(&app, "/api/blocks/4/enabled", t, json!({ "enabled": true })).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert_eq!(res["code"], "failed");
        let (status, _) = post(
            &app,
            "/api/blocks/2/params/0",
            t,
            json!({ "value": "\u{7}" }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        // Malformed body: axum's own 4xx.
        let (status, _) = post(&app, "/api/select", t, json!({ "index": "x" })).await;
        assert!(status.is_client_error());
    }

    #[test]
    fn token_compare() {
        assert!(same_token(b"abc", b"abc"));
        assert!(!same_token(b"abd", b"abc"));
        assert!(!same_token(b"ab", b"abc"));
    }
}
