//! HTTP API. Read-only in phase 3; see `docs/api.md`.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::device::{Command, Device, DeviceError};
use crate::parse;

pub fn router<D: Device>(device: Arc<D>) -> Router {
    Router::new()
        .route("/api/health", get(health::<D>))
        .route("/api/device", get(device_info::<D>))
        .route("/api/presets", get(presets::<D>))
        .route("/api/preset", get(current_preset::<D>))
        .route("/api/chain", get(chain::<D>))
        .with_state(device)
}

struct ApiError(DeviceError);

impl From<DeviceError> for ApiError {
    fn from(e: DeviceError) -> Self {
        ApiError(e)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match self.0 {
            DeviceError::Spawn(_) => StatusCode::SERVICE_UNAVAILABLE,
            DeviceError::Timeout(_) => StatusCode::GATEWAY_TIMEOUT,
            DeviceError::Failed(_) | DeviceError::Parse(_) => StatusCode::BAD_GATEWAY,
            DeviceError::NoFixture(_) => StatusCode::NOT_FOUND,
        };
        tracing::warn!(%status, error = %self.0, "device request failed");
        (status, Json(json!({ "error": self.0.to_string() }))).into_response()
    }
}

type ApiResult<T> = Result<Json<T>, ApiError>;

#[derive(Serialize)]
struct Health {
    status: &'static str,
    backend: &'static str,
    version: &'static str,
}

/// Server liveness only; does not touch the device.
async fn health<D: Device>(State(device): State<Arc<D>>) -> Json<Health> {
    Json(Health {
        status: "ok",
        backend: device.kind(),
        version: env!("CARGO_PKG_VERSION"),
    })
}

async fn device_info<D: Device>(State(device): State<Arc<D>>) -> ApiResult<parse::DeviceInfo> {
    let out = device.run(Command::Info).await?;
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
    State(device): State<Arc<D>>,
    Query(q): Query<SetlistQuery>,
) -> ApiResult<PresetList> {
    let out = device.run(Command::Presets { setlist: q.setlist }).await?;
    Ok(Json(PresetList {
        setlist: q.setlist,
        presets: parse::presets(&out)?,
    }))
}

async fn current_preset<D: Device>(
    State(device): State<Arc<D>>,
) -> ApiResult<parse::CurrentPreset> {
    let out = device.run(Command::Preset).await?;
    Ok(Json(parse::preset(&out)?))
}

async fn chain<D: Device>(State(device): State<Arc<D>>) -> ApiResult<parse::Chain> {
    let out = device.run(Command::Chain).await?;
    Ok(Json(parse::chain(&out)?))
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

    async fn get_json(uri: &str) -> (StatusCode, Value) {
        let app = router(Arc::new(FakeDevice::bundled()));
        let res = app
            .oneshot(Request::get(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = res.status();
        let body = res.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&body).unwrap())
    }

    #[tokio::test]
    async fn health() {
        let (status, body) = get_json("/api/health").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["status"], "ok");
        assert_eq!(body["backend"], "fake");
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
    }
}
