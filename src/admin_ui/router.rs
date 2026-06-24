//! Admin UI 路由配置

use axum::{
    Router,
    body::Body,
    http::{Response, StatusCode, Uri, header},
    response::IntoResponse,
    routing::get,
};
use rust_embed::Embed;

/// 嵌入旧版前端构建产物
#[derive(Embed)]
#[folder = "admin-ui/dist"]
struct AdminAsset;

/// 嵌入新版 Daisy 前端构建产物
#[derive(Embed)]
#[folder = "admin-ui-daisy/dist"]
struct ConsoleAsset;

/// 嵌入重构版前端构建产物(shadcn + Tailwind v4)
#[derive(Embed)]
#[folder = "ui/dist"]
struct NewUiAsset;

trait UiAsset {
    const BUILD_HINT: &'static str;

    fn get(path: &str) -> Option<rust_embed::EmbeddedFile>;
}

impl UiAsset for AdminAsset {
    const BUILD_HINT: &'static str = "Admin UI not built. Run 'pnpm build' in admin-ui directory.";

    fn get(path: &str) -> Option<rust_embed::EmbeddedFile> {
        <Self as rust_embed::RustEmbed>::get(path)
    }
}

impl UiAsset for ConsoleAsset {
    const BUILD_HINT: &'static str =
        "Console UI not built. Run 'pnpm build' in admin-ui-daisy directory.";

    fn get(path: &str) -> Option<rust_embed::EmbeddedFile> {
        <Self as rust_embed::RustEmbed>::get(path)
    }
}

impl UiAsset for NewUiAsset {
    const BUILD_HINT: &'static str = "New UI not built. Run 'pnpm build' in ui directory.";

    fn get(path: &str) -> Option<rust_embed::EmbeddedFile> {
        <Self as rust_embed::RustEmbed>::get(path)
    }
}

/// 创建旧版 Admin UI 路由
pub fn create_admin_ui_router() -> Router {
    Router::new()
        .route("/", get(admin_index_handler))
        .route("/{*file}", get(admin_static_handler))
}

/// 创建新版 Console UI 路由
pub fn create_console_ui_router() -> Router {
    Router::new()
        .route("/", get(console_index_handler))
        .route("/{*file}", get(console_static_handler))
}

/// 创建重构版 UI 路由
pub fn create_new_ui_router() -> Router {
    Router::new()
        .route("/", get(new_ui_index_handler))
        .route("/{*file}", get(new_ui_static_handler))
}

/// 处理旧版首页请求
async fn admin_index_handler() -> impl IntoResponse {
    serve_index::<AdminAsset>()
}

/// 处理新版首页请求
async fn console_index_handler() -> impl IntoResponse {
    serve_index::<ConsoleAsset>()
}

/// 处理旧版静态文件请求
async fn admin_static_handler(uri: Uri) -> impl IntoResponse {
    static_handler::<AdminAsset>(uri)
}

/// 处理新版静态文件请求
async fn console_static_handler(uri: Uri) -> impl IntoResponse {
    static_handler::<ConsoleAsset>(uri)
}

/// 处理重构版首页请求
async fn new_ui_index_handler() -> impl IntoResponse {
    serve_index::<NewUiAsset>()
}

/// 处理重构版静态文件请求
async fn new_ui_static_handler(uri: Uri) -> impl IntoResponse {
    static_handler::<NewUiAsset>(uri)
}

/// 处理静态文件请求
fn static_handler<A: UiAsset>(uri: Uri) -> Response<Body> {
    let path = uri.path().trim_start_matches('/');

    // 安全检查：拒绝包含 .. 的路径
    if path.contains("..") {
        return Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .body(Body::from("Invalid path"))
            .expect("Failed to build response");
    }

    // 尝试获取请求的文件
    if let Some(content) = A::get(path) {
        let mime = mime_guess::from_path(path)
            .first_or_octet_stream()
            .to_string();

        // 根据文件类型设置不同的缓存策略
        let cache_control = get_cache_control(path);

        return Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, mime)
            .header(header::CACHE_CONTROL, cache_control)
            .body(Body::from(content.data.into_owned()))
            .expect("Failed to build response");
    }

    // SPA fallback: 如果文件不存在且不是资源文件，返回 index.html
    if !is_asset_path(path) {
        return serve_index::<A>();
    }

    // 404
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .body(Body::from("Not found"))
        .expect("Failed to build response")
}

/// 提供 index.html
fn serve_index<A: UiAsset>() -> Response<Body> {
    match A::get("index.html") {
        Some(content) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
            .header(header::CACHE_CONTROL, "no-cache")
            .body(Body::from(content.data.into_owned()))
            .expect("Failed to build response"),
        None => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::from(A::BUILD_HINT))
            .expect("Failed to build response"),
    }
}

/// 根据文件类型返回合适的缓存策略
fn get_cache_control(path: &str) -> &'static str {
    if path.ends_with(".html") {
        // HTML 文件不缓存，确保用户获取最新版本
        "no-cache"
    } else if path.starts_with("assets/") {
        // assets/ 目录下的文件带有内容哈希，可以长期缓存
        "public, max-age=31536000, immutable"
    } else {
        // 其他文件（如 favicon）使用较短的缓存
        "public, max-age=3600"
    }
}

/// 判断是否为资源文件路径（有扩展名的文件）
fn is_asset_path(path: &str) -> bool {
    // 检查最后一个路径段是否包含扩展名
    path.rsplit('/')
        .next()
        .map(|filename| filename.contains('.'))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_ui_indexes_use_expected_mount_prefixes() {
        let admin = <AdminAsset as rust_embed::RustEmbed>::get("index.html")
            .expect("admin-ui index should be embedded");
        let admin = std::str::from_utf8(admin.data.as_ref()).expect("admin-ui index is utf-8");
        assert!(admin.contains("/admin/assets/"));
        assert!(!admin.contains("/console/assets/"));

        let console = <ConsoleAsset as rust_embed::RustEmbed>::get("index.html")
            .expect("console index should be embedded");
        let console = std::str::from_utf8(console.data.as_ref()).expect("console index is utf-8");
        assert!(console.contains("/console/assets/"));
        assert!(!console.contains("/admin/assets/"));
    }
}
