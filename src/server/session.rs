use tracing::info;
use utoipa_axum::{router::OpenApiRouter, routes};

pub const TAG: &str = "session";

#[utoipa::path(
    method(get, head),
    path = "/health",
    tag = TAG,
    responses(
        (status = OK, description = "Success", body = str, content_type = "text/plain")
    )
)]
async fn health() -> &'static str {
    info!("health check requested");
    "ok"
}

pub fn routes() -> OpenApiRouter {
    OpenApiRouter::new().routes(routes!(health))
}
