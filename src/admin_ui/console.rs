//! 新版前端(`frontend/dist`)的静态文件服务,挂在 `/console/`。
//!
//! 与 `router.rs`(旧版 admin-ui)同构,只是 embed 的目录不同。
//! 两者并存,稳定后可以删掉旧版,把 `/admin` 重定向到 `/console`。

use axum::{
    Router,
    body::Body,
    http::{Response, StatusCode, Uri, header},
    response::IntoResponse,
    routing::get,
};
use rust_embed::Embed;

#[derive(Embed)]
#[folder = "frontend/dist"]
struct Asset;

pub fn create_console_router() -> Router {
    Router::new()
        .route("/", get(index_handler))
        .route("/{*file}", get(static_handler))
}

async fn index_handler() -> impl IntoResponse {
    serve_index()
}

async fn static_handler(uri: Uri) -> impl IntoResponse {
    let path = uri.path().trim_start_matches('/');
    if path.contains("..") {
        return Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .body(Body::from("Invalid path"))
            .expect("response");
    }
    if let Some(content) = Asset::get(path) {
        let mime = mime_guess::from_path(path)
            .first_or_octet_stream()
            .to_string();
        let cache_control = if path.ends_with(".html") {
            "no-cache"
        } else if path.starts_with("assets/") {
            "public, max-age=31536000, immutable"
        } else {
            "public, max-age=3600"
        };
        return Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, mime)
            .header(header::CACHE_CONTROL, cache_control)
            .body(Body::from(content.data.into_owned()))
            .expect("response");
    }
    // SPA fallback
    if !is_asset_path(path) {
        return serve_index();
    }
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .body(Body::from("Not found"))
        .expect("response")
}

fn serve_index() -> Response<Body> {
    match Asset::get("index.html") {
        Some(content) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
            .header(header::CACHE_CONTROL, "no-cache")
            .body(Body::from(content.data.into_owned()))
            .expect("response"),
        None => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::from(
                "Console UI not built. Run 'pnpm build' inside frontend/.",
            ))
            .expect("response"),
    }
}

fn is_asset_path(path: &str) -> bool {
    path.rsplit('/')
        .next()
        .map(|f| f.contains('.'))
        .unwrap_or(false)
}
