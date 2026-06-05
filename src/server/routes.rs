use std::net::SocketAddr;
use tracing::info;
use utoipa::OpenApi;
use utoipa_axum::{router::OpenApiRouter, routes};
use utoipa_scalar::{Scalar, Servable};

use super::{project, session};

#[derive(OpenApi)]
#[openapi(
    tags(
        (name = project::TAG, description = "Project API endpoints"),
        (name = session::TAG, description = "Session API endpoints")
    )
)]
struct ApiDoc;

pub async fn run(addr: SocketAddr) -> std::io::Result<()> {
    let (app, docs) = OpenApiRouter::with_openapi(ApiDoc::openapi())
        .routes(routes!(health))
        .nest("/session", session::routes())
        .nest("/project", project::routes())
        .split_for_parts();

    let app = app.merge(Scalar::with_url("/docs", docs));

    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!(%addr, "axum server listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
}

#[utoipa::path(
    method(get, head),
    path = "/health",
    responses(
        (status = OK, description = "Success", body = str, content_type = "text/plain")
    )
)]
async fn health() -> &'static str {
    info!("health check requested");
    "ok"
}

async fn shutdown_signal() {
    info!("waiting for shutdown signal");
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        use tokio::signal::unix::{SignalKind, signal};

        if let Ok(mut sigterm) = signal(SignalKind::terminate()) {
            sigterm.recv().await;
        }
    };

    #[cfg(unix)]
    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    #[cfg(not(unix))]
    ctrl_c.await;

    info!("shutdown signal received");
}
