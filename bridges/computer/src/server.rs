//! HTTP server exposing computer-use endpoints.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use tracing::{error, info};

use crate::computer;

/// Shared server state.
#[derive(Clone)]
pub struct ServerState {
    /// Default max screenshot dimensions (0 = no limit).
    pub max_screenshot_width: u32,
    pub max_screenshot_height: u32,
}

pub fn build_router(state: ServerState) -> Router {
    Router::new()
        .route("/screenshot", post(handle_screenshot))
        .route("/click", post(handle_click))
        .route("/type", post(handle_type))
        .route("/key", post(handle_key))
        .route("/move", post(handle_move))
        .route("/drag", post(handle_drag))
        .route("/scroll", post(handle_scroll))
        .route("/screen-size", get(handle_screen_size))
        .route("/health", get(handle_health))
        .with_state(state)
}

// ── Request types ──

#[derive(Debug, Deserialize)]
pub struct ScreenshotRequest {
    pub max_width: Option<u32>,
    pub max_height: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct ClickRequest {
    pub x: i32,
    pub y: i32,
    #[serde(default = "default_button")]
    pub button: u8,
}

fn default_button() -> u8 {
    1
}

#[derive(Debug, Deserialize)]
pub struct TypeRequest {
    pub text: String,
}

#[derive(Debug, Deserialize)]
pub struct KeyRequest {
    pub keys: String,
}

#[derive(Debug, Deserialize)]
pub struct MoveRequest {
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Deserialize)]
pub struct DragRequest {
    pub from_x: i32,
    pub from_y: i32,
    pub to_x: i32,
    pub to_y: i32,
    #[serde(default = "default_button")]
    pub button: u8,
}

#[derive(Debug, Deserialize)]
pub struct ScrollRequest {
    pub x: i32,
    pub y: i32,
    pub direction: String,
    #[serde(default = "default_scroll_amount")]
    pub amount: u32,
}

fn default_scroll_amount() -> u32 {
    3
}

// ── Handlers ──

/// POST /screenshot — capture screen, return base64 PNG.
async fn handle_screenshot(
    State(state): State<ServerState>,
    body: Option<Json<ScreenshotRequest>>,
) -> impl IntoResponse {
    let (max_w, max_h) = match body {
        Some(Json(req)) => (req.max_width, req.max_height),
        None => {
            let w = if state.max_screenshot_width > 0 {
                Some(state.max_screenshot_width)
            } else {
                None
            };
            let h = if state.max_screenshot_height > 0 {
                Some(state.max_screenshot_height)
            } else {
                None
            };
            (w, h)
        }
    };

    info!(
        "screenshot max={}x{}",
        max_w.unwrap_or(0),
        max_h.unwrap_or(0)
    );

    // Run blocking I/O on a dedicated thread
    let result = tokio::task::spawn_blocking(move || computer::take_screenshot(max_w, max_h))
        .await
        .unwrap_or_else(|e| Err(anyhow::anyhow!("task join error: {}", e)));

    match result {
        Ok(b64) => (
            StatusCode::OK,
            Json(serde_json::json!({"ok": true, "image": b64})),
        ),
        Err(e) => {
            error!("screenshot failed: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"ok": false, "error": e.to_string()})),
            )
        }
    }
}

/// POST /click — click at coordinates.
async fn handle_click(Json(req): Json<ClickRequest>) -> impl IntoResponse {
    info!("click x={} y={} button={}", req.x, req.y, req.button);

    let result =
        tokio::task::spawn_blocking(move || computer::mouse_click(req.x, req.y, req.button))
            .await
            .unwrap_or_else(|e| Err(anyhow::anyhow!("task join error: {}", e)));

    ok_or_err(result)
}

/// POST /type — type text.
async fn handle_type(Json(req): Json<TypeRequest>) -> impl IntoResponse {
    info!("type text_len={}", req.text.len());

    let result = tokio::task::spawn_blocking(move || computer::type_text(&req.text))
        .await
        .unwrap_or_else(|e| Err(anyhow::anyhow!("task join error: {}", e)));

    ok_or_err(result)
}

/// POST /key — press key combination.
async fn handle_key(Json(req): Json<KeyRequest>) -> impl IntoResponse {
    info!("key keys={}", req.keys);

    let result = tokio::task::spawn_blocking(move || computer::key_press(&req.keys))
        .await
        .unwrap_or_else(|e| Err(anyhow::anyhow!("task join error: {}", e)));

    ok_or_err(result)
}

/// POST /move — move mouse cursor.
async fn handle_move(Json(req): Json<MoveRequest>) -> impl IntoResponse {
    info!("move x={} y={}", req.x, req.y);

    let result = tokio::task::spawn_blocking(move || computer::mouse_move(req.x, req.y))
        .await
        .unwrap_or_else(|e| Err(anyhow::anyhow!("task join error: {}", e)));

    ok_or_err(result)
}

/// POST /drag — drag from one point to another.
async fn handle_drag(Json(req): Json<DragRequest>) -> impl IntoResponse {
    info!(
        "drag from=({},{}) to=({},{}) button={}",
        req.from_x, req.from_y, req.to_x, req.to_y, req.button
    );

    let result = tokio::task::spawn_blocking(move || {
        computer::mouse_drag(req.from_x, req.from_y, req.to_x, req.to_y, req.button)
    })
    .await
    .unwrap_or_else(|e| Err(anyhow::anyhow!("task join error: {}", e)));

    ok_or_err(result)
}

/// POST /scroll — scroll at coordinates.
async fn handle_scroll(Json(req): Json<ScrollRequest>) -> impl IntoResponse {
    info!(
        "scroll x={} y={} dir={} amount={}",
        req.x, req.y, req.direction, req.amount
    );

    let result = tokio::task::spawn_blocking(move || {
        computer::mouse_scroll(req.x, req.y, &req.direction, req.amount)
    })
    .await
    .unwrap_or_else(|e| Err(anyhow::anyhow!("task join error: {}", e)));

    ok_or_err(result)
}

/// GET /screen-size — return screen dimensions.
async fn handle_screen_size() -> impl IntoResponse {
    let result = tokio::task::spawn_blocking(computer::get_screen_size)
        .await
        .unwrap_or_else(|e| Err(anyhow::anyhow!("task join error: {}", e)));

    match result {
        Ok(size) => (
            StatusCode::OK,
            Json(serde_json::json!({"ok": true, "width": size.width, "height": size.height})),
        ),
        Err(e) => {
            error!("screen-size failed: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"ok": false, "error": e.to_string()})),
            )
        }
    }
}

/// GET /health — health check.
async fn handle_health() -> impl IntoResponse {
    let xdotool = computer::is_xdotool_available();
    let screenshot = computer::is_screenshot_available();
    let display = computer::get_screen_size().is_ok();
    let screenshot_ready = display && screenshot;

    Json(serde_json::json!({
        "ok": true,
        "available": xdotool && screenshot_ready,
        "service": "omar-computer-bridge",
        "xdotool": xdotool,
        "display": display,
        "screenshot": screenshot,
        "screenshot_ready": screenshot_ready,
        "imagemagick": screenshot,
    }))
}

/// Helper: convert Result<()> to JSON response.
fn ok_or_err(result: anyhow::Result<()>) -> (StatusCode, Json<serde_json::Value>) {
    match result {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({"ok": true}))),
        Err(e) => {
            error!("action failed: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"ok": false, "error": e.to_string()})),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn test_state() -> ServerState {
        ServerState {
            max_screenshot_width: 0,
            max_screenshot_height: 0,
        }
    }

    #[tokio::test]
    async fn test_health_endpoint() {
        let app = build_router(test_state());
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["ok"], true);
        assert_eq!(json["service"], "omar-computer-bridge");
    }

    #[tokio::test]
    async fn test_click_missing_body_returns_error() {
        let app = build_router(test_state());
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/click")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();

        // Missing required fields should return 422
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn test_type_missing_body_returns_error() {
        let app = build_router(test_state());
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/type")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn test_key_missing_body_returns_error() {
        let app = build_router(test_state());
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/key")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn test_screen_size_endpoint_exists() {
        let app = build_router(test_state());
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/screen-size")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        // Will succeed or fail depending on display, but shouldn't 404
        assert_ne!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_screenshot_endpoint_exists() {
        let app = build_router(test_state());
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/screenshot")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();

        // Shouldn't 404
        assert_ne!(resp.status(), StatusCode::NOT_FOUND);
    }
}
