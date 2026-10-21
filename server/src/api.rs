//! HTTP API (see `docs/api.md`) and the web UI from `/web`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use axum::extract::ws::{Message, Utf8Bytes, WebSocket, WebSocketUpgrade};
use axum::extract::{FromRequestParts, Path, Query, State};
use axum::http::request::Parts;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::json;

use tokio::sync::broadcast;

use crate::based_on::BasedOn;
use crate::device::{Cause, ChainExtra, Command, Device, DeviceError, Live};
use crate::parse;

/// Shared by every request.
pub struct AppState<D> {
    device: D,
    /// Shared token for write requests. `None` turns writes off.
    token: Option<String>,
    /// True after a bypass or parameter change through the bridge, until the
    /// next load or save. Edits made on the pedal itself are not seen.
    unsaved: AtomicBool,
    /// JSON messages for `/api/live` clients, serialised once and shared by
    /// every client (a clone is a reference count, not a copy).
    live: broadcast::Sender<Utf8Bytes>,
    /// The real gear behind each model, for `based_on` in `/api/chain`.
    based_on: BasedOn,
}

impl<D: Device> AppState<D> {
    pub fn new(device: D, token: Option<String>) -> Self {
        AppState {
            device,
            token: token.filter(|t| !t.is_empty()),
            unsaved: AtomicBool::new(false),
            live: broadcast::channel(16).0,
            based_on: BasedOn::new(),
        }
    }

    /// Name the real gear behind each model (`crate::based_on`).
    pub fn with_based_on(mut self, based_on: BasedOn) -> Self {
        self.based_on = based_on;
        self
    }
}

type Shared<D> = State<Arc<AppState<D>>>;

/// Build the app. Must run inside a Tokio runtime: a backend that reports
/// device changes gets a task relaying them to `/api/live`.
pub fn router<D: Device>(state: AppState<D>) -> Router {
    let state = Arc::new(state);
    if let Some(changes) = state.device.subscribe() {
        tokio::spawn(relay(state.clone(), changes));
    }
    Router::new()
        .route("/api/health", get(health::<D>))
        .route("/api/device", get(device_info::<D>))
        .route("/api/presets", get(presets::<D>))
        .route("/api/preset", get(current_preset::<D>))
        .route("/api/chain", get(chain::<D>))
        .route("/api/models/{id}", get(model::<D>))
        .route("/api/select", post(select::<D>))
        .route("/api/catalog", get(catalog::<D>))
        .route(
            "/api/blocks/{position}",
            post(add_block::<D>).delete(remove_block::<D>),
        )
        .route("/api/blocks/{position}/enabled", post(enable::<D>))
        .route("/api/blocks/{position}/footswitch", post(footswitch::<D>))
        .route("/api/blocks/{position}/move", post(move_block::<D>))
        .route("/api/routing", post(junction::<D>))
        .route(
            "/api/blocks/{position}/params/{index}/controller",
            post(controller::<D>),
        )
        .route(
            "/api/blocks/{position}/params/{index}/controller/range",
            post(controller_range::<D>),
        )
        .route(
            "/api/blocks/{position}/params/{index}",
            post(set_param::<D>),
        )
        .route("/api/presets/{index}/rename", post(rename::<D>))
        .route("/api/save", post(save::<D>))
        .route("/api/snapshot", post(snapshot::<D>))
        .route("/api/tempo", post(tempo::<D>))
        .route("/api/live", get(live::<D>))
        .route("/", get(index))
        .route("/app.css", get(app_css))
        .route("/app.js", get(app_js))
        .route("/latest.js", get(latest_js))
        .route("/lib/motion-13.4.6.js", get(motion_js))
        .with_state(state)
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

async fn latest_js() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
            NO_CACHE,
        ],
        include_str!("../../web/latest.js"),
    )
}

/// Motion (motion.dev, MIT, web/lib/motion-LICENSE.md). The version is in the
/// path, so Safari may keep it; bump the name with the file.
async fn motion_js() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
            (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
        ],
        include_str!("../../web/lib/motion-13.4.6.js"),
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
    /// No parameter metadata for this model (no HX Edit catalog, or unknown).
    NoCatalog(u32),
    /// No model list to add blocks from (no HX Edit catalog).
    NoModelList,
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
            ApiError::Device(DeviceError::Stale(_)) => "stale",
            ApiError::Device(DeviceError::Invalid(_)) => "bad_request",
            ApiError::NoCatalog(_) | ApiError::NoModelList => "no_catalog",
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
                DeviceError::Stale(_) => StatusCode::CONFLICT,
                DeviceError::Invalid(_) => StatusCode::BAD_REQUEST,
            },
            ApiError::NoCatalog(_) | ApiError::NoModelList => StatusCode::NOT_FOUND,
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
            ApiError::NoCatalog(id) => format!(
                "no parameter data for model {id}: needs HX Edit data on the server (usb backend)"
            ),
            ApiError::NoModelList => {
                "no model list: needs HX Edit data on the server (usb backend)".into()
            }
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
    /// Whether `/api/live` reports device changes (the `usb` backend does).
    live: bool,
}

/// Server liveness only; does not touch the device.
async fn health<D: Device>(State(app): Shared<D>) -> Json<Health> {
    Json(Health {
        status: "ok",
        backend: app.device.kind(),
        version: env!("CARGO_PKG_VERSION"),
        writes: app.token.is_some(),
        live: app.device.subscribe().is_some(),
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

/// Parse `chain` output and join what the backend knows beyond it: native
/// values, the active snapshot, each block's category from HX Edit's catalog,
/// and the gear its model is based on.
fn full_chain<D: Device>(
    app: &AppState<D>,
    text: &str,
    extra: &ChainExtra,
) -> crate::device::Result<parse::Chain> {
    let mut chain = parse::chain(text)?;
    parse::add_extra(&mut chain, extra);
    for block in &mut chain.blocks {
        block.category = app.device.category(block.model_id, &block.model);
        block.based_on = app.based_on.get(&block.model).cloned();
    }
    Ok(chain)
}

async fn chain<D: Device>(State(app): Shared<D>) -> ApiResult<ChainView> {
    let (out, extra) = app.device.chain().await?;
    Ok(Json(ChainView {
        chain: full_chain(&app, &out, &extra)?,
        unsaved: app.unsaved.load(Ordering::Relaxed),
    }))
}

/// `GET /api/models/{id}`: parameter metadata for sliders. Catalog only, no
/// device call.
async fn model<D: Device>(
    State(app): Shared<D>,
    Path(id): Path<u32>,
) -> ApiResult<crate::controls::ModelInfo> {
    app.device
        .model(id)
        .map(Json)
        .ok_or(ApiError::NoCatalog(id))
}

#[derive(Serialize)]
struct Catalog {
    categories: Vec<crate::controls::ModelGroup>,
}

/// `GET /api/catalog`: the models a block can be added with, by category,
/// with the gear each is based on. Catalog only, no device call.
async fn catalog<D: Device>(State(app): Shared<D>) -> ApiResult<Catalog> {
    let mut categories = app.device.catalog().ok_or(ApiError::NoModelList)?;
    for model in categories
        .iter_mut()
        .flat_map(|c| &mut c.shelves)
        .flat_map(|s| &mut s.models)
    {
        model.based_on = app.based_on.get(&model.name).cloned();
    }
    Ok(Json(Catalog { categories }))
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
    let is_set = matches!(cmd, Command::Set { .. } | Command::SetNative { .. });
    // Every edit of the buffer counts, as TonePush's GUI marks it dirty: a
    // move or a controller left out here let a load drop it unasked.
    let unsaved = match cmd {
        Command::Enable { .. }
        | Command::Set { .. }
        | Command::SetNative { .. }
        | Command::Tempo { .. }
        | Command::AddBlock { .. }
        | Command::RemoveBlock { .. }
        | Command::Footswitch { .. }
        | Command::MoveBlock { .. }
        | Command::Junction { .. }
        | Command::Controller { .. }
        | Command::ControllerRange { .. } => Some(true),
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
struct AddBody {
    /// Model number, from `/api/catalog`.
    model: u32,
}

/// Put a model into an empty slot. Never replaces: a taken slot is 409.
async fn add_block<D: Device>(
    State(app): Shared<D>,
    access: WriteAccess,
    Path(position): Path<u32>,
    Json(body): Json<AddBody>,
) -> ApiResult<WriteResult> {
    let cmd = Command::AddBlock {
        block: checked_position(position)?,
        model: body.model,
    };
    write(&app, access, cmd).await
}

#[derive(Deserialize)]
struct RemoveQuery {
    /// The block's `model_id` as the client saw it; another model there is 409.
    model: Option<u32>,
}

async fn remove_block<D: Device>(
    State(app): Shared<D>,
    access: WriteAccess,
    Path(position): Path<u32>,
    Query(q): Query<RemoveQuery>,
) -> ApiResult<WriteResult> {
    let cmd = Command::RemoveBlock {
        block: checked_position(position)?,
        model: q.model,
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
struct FootswitchBody {
    /// One-based, as on the pedal; `null` takes the block off its switch.
    switch: Option<u8>,
    /// The block's `model_id` as the client saw it; another model there is 409.
    model: Option<u32>,
}

/// Put a block's bypass on a footswitch, or take it off: a block on a
/// footswitch shows in the HX Effects' Stomp view.
async fn footswitch<D: Device>(
    State(app): Shared<D>,
    access: WriteAccess,
    Path(position): Path<u32>,
    Json(body): Json<FootswitchBody>,
) -> ApiResult<WriteResult> {
    if body.switch == Some(0) {
        return Err(ApiError::BadRequest(
            "footswitches are numbered from 1, as on the pedal".into(),
        ));
    }
    let cmd = Command::Footswitch {
        block: checked_position(position)?,
        switch: body.switch,
        model: body.model,
    };
    write(&app, access, cmd).await
}

#[derive(Deserialize)]
struct MoveBody {
    /// The empty cell to move to, as a `position` in `/api/chain`.
    to: u32,
    /// The block's `model_id` as the client saw it; another model there is 409.
    model: Option<u32>,
}

/// Move a block into an empty cell of Path A or Path B.
async fn move_block<D: Device>(
    State(app): Shared<D>,
    access: WriteAccess,
    Path(position): Path<u32>,
    Json(body): Json<MoveBody>,
) -> ApiResult<WriteResult> {
    let cmd = Command::MoveBlock {
        block: checked_position(position)?,
        to: checked_position(body.to)?,
        model: body.model,
    };
    write(&app, access, cmd).await
}

#[derive(Deserialize)]
struct JunctionBody {
    /// `split` or `join` (the merge).
    junction: crate::routing::Junction,
    /// The cell of `routing.main` (zero-based) it is to sit before.
    before: u32,
}

/// Move the split or the merge along Path A.
async fn junction<D: Device>(
    State(app): Shared<D>,
    access: WriteAccess,
    Json(body): Json<JunctionBody>,
) -> ApiResult<WriteResult> {
    let cmd = Command::Junction {
        which: body.junction,
        before: body.before,
    };
    write(&app, access, cmd).await
}

#[derive(Deserialize)]
struct ControllerBody {
    /// Expression pedal 1 or 2; `null` takes the parameter off its controller.
    pedal: Option<u8>,
    /// The block's `model_id` as the client saw it; another model there is 409.
    model: Option<u32>,
}

/// Put a parameter under an expression pedal, or take it off.
async fn controller<D: Device>(
    State(app): Shared<D>,
    access: WriteAccess,
    Path((position, index)): Path<(u32, u32)>,
    Json(body): Json<ControllerBody>,
) -> ApiResult<WriteResult> {
    let cmd = Command::Controller {
        block: checked_position(position)?,
        param: index,
        pedal: body.pedal,
        model: body.model,
    };
    write(&app, access, cmd).await
}

#[derive(Deserialize)]
struct RangeBody {
    /// Heel end, native value (as `params[].native`).
    min: Option<f32>,
    /// Toe end, native value.
    max: Option<f32>,
    model: Option<u32>,
}

/// Move the heel and/or toe end of a parameter's pedal travel.
async fn controller_range<D: Device>(
    State(app): Shared<D>,
    access: WriteAccess,
    Path((position, index)): Path<(u32, u32)>,
    Json(body): Json<RangeBody>,
) -> ApiResult<WriteResult> {
    let cmd = Command::ControllerRange {
        block: checked_position(position)?,
        param: index,
        min: body.min,
        max: body.max,
        model: body.model,
    };
    write(&app, access, cmd).await
}

/// Either `{value}` (display units, typed) or `{native, model}` (a slider
/// position, with the model number the client saw in that slot).
#[derive(Deserialize)]
struct ParamBody {
    value: Option<String>,
    native: Option<f32>,
    model: Option<u32>,
}

async fn set_param<D: Device>(
    State(app): Shared<D>,
    access: WriteAccess,
    Path((position, index)): Path<(u32, u32)>,
    Json(body): Json<ParamBody>,
) -> ApiResult<WriteResult> {
    let block = checked_position(position)?;
    let cmd = match body {
        ParamBody {
            value: Some(value),
            native: None,
            model: None,
        } => Command::Set {
            block,
            param: index,
            value: checked_text("value", &value, MAX_VALUE)?,
        },
        ParamBody {
            value: None,
            native: Some(native),
            model: Some(model),
        } if native.is_finite() => Command::SetNative {
            block,
            param: index,
            native,
            model,
        },
        _ => {
            return Err(ApiError::BadRequest(
                "send {\"value\": text} or {\"native\": number, \"model\": id}".into(),
            ))
        }
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

/// HX devices have at most 8 snapshots (the HX Effects 4); the device
/// refuses a missing one with error -46, this catches nonsense first.
const MAX_SNAPSHOTS: u32 = 8;

#[derive(Deserialize)]
struct SnapshotBody {
    /// Zero-based, as in `/api/chain`'s `snapshots` and `snapshot`.
    index: u32,
}

async fn snapshot<D: Device>(
    State(app): Shared<D>,
    access: WriteAccess,
    Json(body): Json<SnapshotBody>,
) -> ApiResult<WriteResult> {
    if body.index >= MAX_SNAPSHOTS {
        return Err(ApiError::BadRequest(format!(
            "snapshots are numbered 0 to {}",
            MAX_SNAPSHOTS - 1
        )));
    }
    write(&app, access, Command::Snapshot { index: body.index }).await
}

#[derive(Deserialize)]
struct TempoBody {
    bpm: f32,
}

async fn tempo<D: Device>(
    State(app): Shared<D>,
    access: WriteAccess,
    Json(body): Json<TempoBody>,
) -> ApiResult<WriteResult> {
    if !(body.bpm.is_finite() && (40.0..=240.0).contains(&body.bpm)) {
        return Err(ApiError::BadRequest(
            "tempo must be between 40 and 240 BPM".into(),
        ));
    }
    // HX shows one decimal; send what it will show.
    let bpm = (body.bpm * 10.0).round() / 10.0;
    write(&app, access, Command::Tempo { bpm }).await
}

// ---------- Live updates ----------

/// One `/api/live` message: `{type: "chain", chain}` or `{type: "device", connected}`.
// Serialised as soon as it is built, so its size does not matter.
#[allow(clippy::large_enum_variant)]
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum LiveMessage {
    Chain { chain: ChainView },
    Device { connected: bool },
}

/// Turn device changes into client messages, keeping `unsaved` current:
/// a footswitch or knob on the pedal edits the buffer as much as we do.
async fn relay<D: Device>(app: Arc<AppState<D>>, mut changes: broadcast::Receiver<Live>) {
    loop {
        let msg = match changes.recv().await {
            Ok(Live::Chain { text, cause, extra }) => {
                match cause {
                    Cause::Preset => app.unsaved.store(false, Ordering::Relaxed),
                    Cause::Edit => app.unsaved.store(true, Ordering::Relaxed),
                    Cause::Other => {}
                }
                match full_chain(&app, &text, &extra) {
                    Ok(chain) => LiveMessage::Chain {
                        chain: ChainView {
                            chain,
                            unsaved: app.unsaved.load(Ordering::Relaxed),
                        },
                    },
                    Err(e) => {
                        tracing::warn!("live chain unreadable: {e}");
                        continue;
                    }
                }
            }
            Ok(Live::Connected(connected)) => LiveMessage::Device { connected },
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => return,
        };
        let text = match serde_json::to_string(&msg) {
            Ok(text) => text,
            Err(e) => {
                tracing::warn!("live message unserialisable: {e}");
                continue;
            }
        };
        // No listeners is fine.
        let _ = app.live.send(text.into());
    }
}

/// `GET /api/live`: WebSocket of `{type: "chain", chain}` and
/// `{type: "device", connected}` messages. Read-only, so no token.
async fn live<D: Device>(State(app): Shared<D>, ws: WebSocketUpgrade) -> Response {
    let messages = app.live.subscribe();
    ws.on_upgrade(move |socket| live_client(socket, messages))
}

async fn live_client(mut socket: WebSocket, mut messages: broadcast::Receiver<Utf8Bytes>) {
    loop {
        tokio::select! {
            msg = messages.recv() => match msg {
                Ok(text) => {
                    if socket.send(Message::Text(text)).await.is_err() {
                        return;
                    }
                }
                // Missed some: the next message carries the whole chain anyway.
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => return,
            },
            incoming = socket.recv() => match incoming {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => return,
                Some(Ok(_)) => {}
            },
        }
    }
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
            ("/latest.js", "text/javascript"),
            ("/lib/motion-13.4.6.js", "text/javascript"),
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
    async fn chain_carries_natives() {
        let (_, body) = get_json("/api/chain").await;
        let fassel = &body["blocks"][0];
        assert_eq!(fassel["model_id"], 269);
        let position = fassel["params"][0]["native"].as_f64().unwrap();
        assert!((position - 0.017248867).abs() < 1e-6);
        assert_eq!(body["blocks"][3]["params"][0]["value"], "-22.1 dB");
    }

    /// Only models in the table get a `based_on`; the table is Line 6's, so
    /// this one is made up.
    #[tokio::test]
    async fn chain_names_the_gear_a_model_is_based_on() {
        let table = [("Fassel".to_owned(), "A wah pedal".to_owned())].into();
        let app = router(AppState::new(FakeDevice::bundled(), None).with_based_on(table));
        let (_, body) = send(
            &app,
            Request::get("/api/chain").body(Body::empty()).unwrap(),
        )
        .await;
        assert_eq!(body["blocks"][0]["based_on"], "A wah pedal");
        assert!(body["blocks"][1].get("based_on").is_none());
    }

    #[tokio::test]
    async fn models() {
        let (status, body) = get_json("/api/models/269").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["name"], "Fassel");
        assert_eq!(body["synthetic"], true);
        let position = &body["params"][0];
        assert_eq!(position["kind"], "continuous");
        assert_eq!(position["steps"], 100);
        assert_eq!(position["labels"][40], "40 %");
        let (_, pitch) = get_json("/api/models/296").await;
        assert_eq!(pitch["params"][0]["kind"], "menu");
        assert_eq!(pitch["params"][0]["labels"][31], "+7");
        let (_, gate) = get_json("/api/models/123").await;
        assert_eq!(gate["params"][1]["scale"], "log");

        let (status, body) = get_json("/api/models/9999").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["code"], "no_catalog");
    }

    #[tokio::test]
    async fn set_by_native_value() {
        let app = app();
        let t = Some(TOKEN);
        let url = "/api/blocks/2/params/0";
        let (status, res) = post(&app, url, t, json!({ "native": 0.4, "model": 269 })).await;
        assert_eq!(status, StatusCode::OK, "{res}");
        assert_eq!(res["value"], "40 %");
        assert_eq!(res["unsaved"], true);
        let (_, chain) = send(
            &app,
            Request::get("/api/chain").body(Body::empty()).unwrap(),
        )
        .await;
        assert_eq!(chain["blocks"][0]["params"][0]["value"], "40 %");
        assert!((chain["blocks"][0]["params"][0]["native"].as_f64().unwrap() - 0.4).abs() < 1e-6);

        // Another model in the slot than the client saw: 409, re-read.
        let (status, res) = post(&app, url, t, json!({ "native": 0.4, "model": 296 })).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(res["code"], "stale");
        // Out of range, or a malformed body.
        for body in [
            json!({ "native": 2.0, "model": 269 }),
            json!({ "native": 0.4 }),
            json!({ "value": "40 %", "native": 0.4, "model": 269 }),
            json!({}),
        ] {
            let (status, res) = post(&app, url, t, body.clone()).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body}: {res}");
            assert_eq!(res["code"], "bad_request");
        }
    }

    #[tokio::test]
    async fn snapshot_and_tempo() {
        let app = app();
        let t = Some(TOKEN);
        let (_, chain) = send(
            &app,
            Request::get("/api/chain").body(Body::empty()).unwrap(),
        )
        .await;
        assert_eq!(chain["snapshot"], 0);

        let (status, res) = post(&app, "/api/snapshot", t, json!({ "index": 2 })).await;
        assert_eq!(status, StatusCode::OK, "{res}");
        assert_eq!(res["message"], "snapshot 3");
        // A snapshot switch is not an edit.
        assert_eq!(res["unsaved"], false);

        let (status, res) = post(&app, "/api/tempo", t, json!({ "bpm": 96.04 })).await;
        assert_eq!(status, StatusCode::OK, "{res}");
        assert_eq!(res["message"], "tempo 96.0 BPM");
        assert_eq!(res["unsaved"], true);

        let (_, chain) = send(
            &app,
            Request::get("/api/chain").body(Body::empty()).unwrap(),
        )
        .await;
        assert_eq!(chain["snapshot"], 2);
        assert_eq!(chain["tempo"], 96.0);

        for (uri, body) in [
            ("/api/snapshot", json!({ "index": 8 })),
            ("/api/tempo", json!({ "bpm": 39.9 })),
            ("/api/tempo", json!({ "bpm": 241 })),
        ] {
            let (status, res) = post(&app, uri, t, body.clone()).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{uri} {body}: {res}");
        }
        // Snapshot 5 on a pedal with 4: the pedal refuses it (error -46).
        let (status, res) = post(&app, "/api/snapshot", t, json!({ "index": 4 })).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert_eq!(res["code"], "failed");
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
    async fn catalog_lists_models() {
        let (status, body) = get_json("/api/catalog").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let models: Vec<&Value> = body["categories"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|c| c["shelves"].as_array().unwrap())
            .flat_map(|s| s["models"].as_array().unwrap())
            .collect();
        let gate = models.iter().find(|m| m["id"] == 123).expect("Noise Gate");
        assert_eq!(gate["name"], "Noise Gate");
        assert!(body["categories"][0]["colour"]
            .as_str()
            .unwrap()
            .starts_with('#'));
    }

    #[tokio::test]
    async fn add_and_remove_blocks() {
        let app = app();
        let t = Some(TOKEN);
        let delete = |uri: &str, token: Option<&str>| {
            let mut req = Request::delete(uri);
            if let Some(t) = token {
                req = req.header(header::AUTHORIZATION, format!("Bearer {t}"));
            }
            send(&app, req.body(Body::empty()).unwrap())
        };

        let (status, res) = post(&app, "/api/blocks/4", t, json!({ "model": 123 })).await;
        assert_eq!(status, StatusCode::OK, "{res}");
        assert_eq!(res["message"], "block 4 is now Noise Gate");
        assert_eq!(res["unsaved"], true);
        let (_, chain) = send(
            &app,
            Request::get("/api/chain").body(Body::empty()).unwrap(),
        )
        .await;
        assert_eq!(chain["blocks"][2]["position"], 4);
        assert_eq!(chain["blocks"][2]["model_id"], 123);

        // Taken: adding never replaces.
        let (status, res) = post(&app, "/api/blocks/4", t, json!({ "model": 296 })).await;
        assert_eq!(status, StatusCode::CONFLICT, "{res}");
        assert_eq!(res["code"], "stale");
        // Another model than the client saw: nothing removed.
        let (status, res) = delete("/api/blocks/4?model=296", t).await;
        assert_eq!(status, StatusCode::CONFLICT, "{res}");

        let (status, res) = delete("/api/blocks/4", None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{res}");
        let (status, res) = delete("/api/blocks/4?model=123", t).await;
        assert_eq!(status, StatusCode::OK, "{res}");
        assert_eq!(res["message"], "removed Noise Gate from block 4");
        let (status, res) = delete("/api/blocks/4", t).await;
        assert_eq!(status, StatusCode::CONFLICT, "{res}");

        // Not a block slot; not a model; position 0.
        let (status, _) = post(&app, "/api/blocks/1", t, json!({ "model": 123 })).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _) = post(&app, "/api/blocks/4", t, json!({ "model": 999999 })).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _) = post(&app, "/api/blocks/0", t, json!({ "model": 123 })).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn paths() {
        let app = app();
        let t = Some(TOKEN);
        let chain = |app: Router| async move {
            send(
                &app,
                Request::get("/api/chain").body(Body::empty()).unwrap(),
            )
            .await
            .1
        };
        let routing = chain(app.clone()).await["routing"].clone();
        assert!(
            routing["split"].get("before").is_none(),
            "Path B starts empty"
        );

        // A junction cannot move while Path B is empty.
        let body = json!({ "junction": "join", "before": 4 });
        let (status, res) = post(&app, "/api/routing", t, body.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{res}");

        // The first block on Path B (B1 = 13) claims the whole line.
        let (status, res) = post(&app, "/api/blocks/13", t, json!({ "model": 123 })).await;
        assert_eq!(status, StatusCode::OK, "{res}");
        let routing = chain(app.clone()).await["routing"].clone();
        assert_eq!(routing["split"]["before"], 0);
        assert_eq!(routing["join"]["before"], 9);

        // The merge moves in; the split cannot pass it, nor Path B's block.
        // A save first, so the move below has to set the flag itself.
        let (status, res) = post(&app, "/api/save", t, json!({})).await;
        assert_eq!(
            (status, res["unsaved"].clone()),
            (StatusCode::OK, json!(false))
        );
        let (status, res) = post(&app, "/api/routing", t, body).await;
        assert_eq!(status, StatusCode::OK, "{res}");
        assert_eq!(res["message"], "merge now before cell 5");
        assert_eq!(res["unsaved"], true, "a moved merge is an edit");
        let split = json!({ "junction": "split", "before": 1 });
        let (status, _) = post(&app, "/api/routing", t, split).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "B1 would fall outside");

        // Noise Gate (10) onto B2; then outside the bracket (B6 = 18) is refused.
        let uri = "/api/blocks/10/move";
        let (status, res) = post(&app, uri, t, json!({ "to": 14, "model": 123 })).await;
        assert_eq!(status, StatusCode::OK, "{res}");
        let (status, _) = post(&app, "/api/blocks/14/move", t, json!({ "to": 18 })).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        // Into a taken cell: stale.
        let (status, _) = post(&app, "/api/blocks/14/move", t, json!({ "to": 13 })).await;
        assert_eq!(status, StatusCode::CONFLICT);

        let c = chain(app.clone()).await;
        let gate = c["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|b| b["position"] == 14)
            .unwrap();
        assert_eq!(gate["model"], "Noise Gate");
        assert_eq!(
            gate["params"][0]["value"], "-22.1 dB",
            "values move with it"
        );

        // Both off Path B: it empties and its attach points go.
        let (status, _) = post(&app, "/api/blocks/14/move", t, json!({ "to": 10 })).await;
        assert_eq!(status, StatusCode::OK);
        let req = Request::delete("/api/blocks/13")
            .header("authorization", format!("Bearer {TOKEN}"))
            .body(Body::empty())
            .unwrap();
        let (status, res) = send(&app, req).await;
        assert_eq!(status, StatusCode::OK, "{res}");
        let routing = chain(app.clone()).await["routing"].clone();
        assert!(routing["split"].get("before").is_none(), "{routing}");
    }

    #[tokio::test]
    async fn expression_pedals() {
        let app = app();
        let t = Some(TOKEN);
        let blocks = |app: Router| async move {
            send(
                &app,
                Request::get("/api/chain").body(Body::empty()).unwrap(),
            )
            .await
            .1["blocks"]
                .clone()
        };
        // As recorded: Fassel's Position and on/off on expression pedal 1.
        let b = blocks(app.clone()).await;
        let fassel = b[0]["assignments"].as_array().unwrap();
        assert_eq!(fassel.len(), 2, "{fassel:?}");
        let position = fassel.iter().find(|a| a["param"] == 0).unwrap();
        assert_eq!(position["source"], "exp1");
        assert_eq!(position["min_value"], "0 %");
        assert_eq!(position["max_value"], "100 %");
        let on_off = fassel.iter().find(|a| a.get("param").is_none()).unwrap();
        assert_eq!(on_off["source"], "exp1");

        // Simple Pitch (3) Cents (1) onto pedal 2: the whole range at first.
        let uri = "/api/blocks/3/params/1/controller";
        let (status, res) = post(&app, uri, t, json!({ "pedal": 2, "model": 296 })).await;
        assert_eq!(status, StatusCode::OK, "{res}");
        assert_eq!(res["unsaved"], true, "an assignment is an edit");
        let b = blocks(app.clone()).await;
        let a = &b[1]["assignments"][0];
        assert_eq!(
            (a["param"].clone(), a["source"].clone()),
            (json!(1), json!("exp2"))
        );
        assert_eq!(
            (a["min"].clone(), a["max"].clone()),
            (json!(-50.0), json!(50.0))
        );

        // Heel and toe; outside the range is refused.
        let range = "/api/blocks/3/params/1/controller/range";
        let (status, res) = post(&app, range, t, json!({ "min": 0.0, "max": 25.0 })).await;
        assert_eq!(status, StatusCode::OK, "{res}");
        let (status, _) = post(&app, range, t, json!({ "max": 99.0 })).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let b = blocks(app.clone()).await;
        assert_eq!(b[1]["assignments"][0]["min"], 0.0);
        assert_eq!(b[1]["assignments"][0]["max"], 25.0);

        // Off again; then a range has nothing to move. Pedal 3 does not exist.
        let (status, _) = post(&app, uri, t, json!({ "pedal": null })).await;
        assert_eq!(status, StatusCode::OK);
        assert!(blocks(app.clone()).await[1].get("assignments").is_none());
        let (status, _) = post(&app, range, t, json!({ "min": 0.0 })).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _) = post(&app, uri, t, json!({ "pedal": 3 })).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn footswitches() {
        let app = app();
        let t = Some(TOKEN);
        let (_, chain) = get_json("/api/chain").await;
        assert_eq!(chain["footswitches"].as_array().unwrap().len(), 6);
        assert!(chain["blocks"][2].get("footswitch").is_none());

        let uri = "/api/blocks/5/footswitch";
        let (status, res) = post(&app, uri, None, json!({ "switch": 2 })).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{res}");
        let (status, res) = post(&app, uri, t, json!({ "switch": 2, "model": 141 })).await;
        assert_eq!(status, StatusCode::OK, "{res}");
        assert_eq!(res["message"], "block 5 on footswitch 2");
        assert_eq!(res["unsaved"], true);
        let (_, chain) = send(
            &app,
            Request::get("/api/chain").body(Body::empty()).unwrap(),
        )
        .await;
        assert_eq!(chain["blocks"][2]["position"], 5);
        assert_eq!(chain["blocks"][2]["footswitch"], 2);
        assert_eq!(
            chain["footswitches"][1],
            json!({ "switch": 2, "blocks": [5] })
        );

        let (status, res) = post(&app, uri, t, json!({ "switch": null })).await;
        assert_eq!(status, StatusCode::OK, "{res}");
        assert_eq!(res["message"], "block 5 on no footswitch");

        // No switch 0 or 7; no block at 4; another model than the client saw.
        let (status, _) = post(&app, uri, t, json!({ "switch": 0 })).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _) = post(&app, uri, t, json!({ "switch": 7 })).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _) = post(&app, "/api/blocks/4/footswitch", t, json!({ "switch": 1 })).await;
        assert_eq!(status, StatusCode::CONFLICT);
        let (status, _) = post(&app, uri, t, json!({ "switch": 1, "model": 123 })).await;
        assert_eq!(status, StatusCode::CONFLICT);
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

    /// A write from one client reaches every `/api/live` listener.
    #[tokio::test]
    async fn live_echoes_writes() {
        use futures_util::StreamExt;
        use tokio_tungstenite::tungstenite::Message as WsMessage;

        let app = app();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = app.clone();
        tokio::spawn(async move { axum::serve(listener, server).await });
        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/api/live"))
            .await
            .unwrap();

        let (_, health) = send(
            &app,
            Request::get("/api/health").body(Body::empty()).unwrap(),
        )
        .await;
        assert_eq!(health["live"], true);

        post(
            &app,
            "/api/blocks/2/enabled",
            Some(TOKEN),
            json!({ "enabled": true }),
        )
        .await;
        let next = async {
            loop {
                if let Some(Ok(WsMessage::Text(t))) = ws.next().await {
                    return serde_json::from_str::<Value>(&t).unwrap();
                }
            }
        };
        let msg = tokio::time::timeout(std::time::Duration::from_secs(5), next)
            .await
            .expect("live message");
        assert_eq!(msg["type"], "chain");
        assert_eq!(msg["chain"]["blocks"][0]["enabled"], true);
        assert_eq!(msg["chain"]["unsaved"], true);
        assert_eq!(msg["chain"]["label"], "01C");
    }

    #[test]
    fn live_device_message() {
        let msg = serde_json::to_value(LiveMessage::Device { connected: false }).unwrap();
        assert_eq!(msg, json!({ "type": "device", "connected": false }));
    }

    #[test]
    fn token_compare() {
        assert!(same_token(b"abc", b"abc"));
        assert!(!same_token(b"abd", b"abc"));
        assert!(!same_token(b"ab", b"abc"));
    }
}
