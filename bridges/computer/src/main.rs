mod computer;
mod server;

use std::net::SocketAddr;

use anyhow::Result;
use tracing::info;
use tracing_subscriber::EnvFilter;

use crate::server::{build_router, ServerState};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .with_thread_ids(false)
        .init();

    let port: u16 = std::env::var("COMPUTER_BRIDGE_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(9878);

    let max_w: u32 = std::env::var("SCREENSHOT_MAX_WIDTH")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1280);

    let max_h: u32 = std::env::var("SCREENSHOT_MAX_HEIGHT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(800);

    info!("OMAR Computer Bridge starting up...");
    info!("Port: {}", port);
    info!("Default screenshot max: {}x{}", max_w, max_h);

    // Check tool availability
    if computer::is_xdotool_available() {
        info!("xdotool: available");
    } else {
        tracing::warn!("xdotool: NOT available — mouse/keyboard actions will fail");
    }
    if computer::is_import_available() {
        info!("ImageMagick import: available");
    } else {
        tracing::warn!("ImageMagick import: NOT available — screenshots will fail");
    }

    let state = ServerState {
        max_screenshot_width: max_w,
        max_screenshot_height: max_h,
    };

    let router = build_router(state);
    let addr = SocketAddr::from(([0, 0, 0, 0], port));

    info!("Listening on {}", addr);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, router).await?;

    Ok(())
}
