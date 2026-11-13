//! 管理后台 UI 路由配置

use std::{
    env,
    path::{Component, Path, PathBuf},
    sync::OnceLock,
};

use axum::{
    Router,
    body::Body,
    extract::State,
    http::{Response, StatusCode, Uri, header},
    response::Redirect,
    routing::get,
};

#[cfg(not(debug_assertions))]
use rust_embed::Embed;

const MOUNT_PREFIX: &str = "/ui";
const ENV_PREFIX: &str = "KIRO_NEW_UI";
const FALLBACK_ENV_PREFIX: &str = "KIRO_UI";
const DEFAULT_DIR: &str = "ui/dist";
const DEFAULT_DEV_SERVER: &str = "http://127.0.0.1:9023/ui";
const BUILD_HINT: &str = "UI not built. Run 'pnpm build' in ui directory.";

/// 嵌入前端构建产物(shadcn + Tailwind v4)；`get` 由 rust-embed 派生生成。
#[cfg(not(debug_assertions))]
#[derive(Embed)]
#[folder = "ui/dist"]
struct UiAsset;

/// debug 构建不嵌入 dist，默认跳转到 Vite 服务。
#[cfg(debug_assertions)]
struct UiAsset;

#[cfg(debug_assertions)]
impl UiAsset {
    fn get(_path: &str) -> Option<rust_embed::EmbeddedFile> {
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UiServeMode {
    Embedded,
    Filesystem,
    Redirect,
    Proxy,
    Disabled,
}

impl UiServeMode {
    fn from_env(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "embedded" | "embed" => Some(Self::Embedded),
            "filesystem" | "fs" | "dir" | "dist" => Some(Self::Filesystem),
            "redirect" | "external" => Some(Self::Redirect),
            "proxy" => Some(Self::Proxy),
            "disabled" | "disable" | "off" | "none" => Some(Self::Disabled),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Embedded => "embedded",
            Self::Filesystem => "filesystem",
            Self::Redirect => "redirect",
            Self::Proxy => "proxy",
            Self::Disabled => "disabled",
        }
    }
}

#[derive(Debug, Clone)]
struct UiServeState {
    mode: UiServeMode,
    filesystem_dir: PathBuf,
    external_url: Option<String>,
}

impl UiServeState {
    fn from_env() -> Self {
        let mode_from_env = read_ui_env("MODE")
            .as_deref()
            .and_then(UiServeMode::from_env);
        let external_url_from_env =
            read_ui_env("DEV_SERVER").or_else(|| read_ui_env("EXTERNAL_URL"));
        let mode = mode_from_env
            .or_else(|| {
                external_url_from_env
                    .as_ref()
                    .map(|_| UiServeMode::Redirect)
            })
            .unwrap_or_else(default_ui_serve_mode);
        let filesystem_dir = read_ui_env("DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_DIR));
        let external_url = external_url_from_env
            .or_else(|| default_dev_external_url(mode, DEFAULT_DEV_SERVER))
            .map(|value| value.trim().trim_end_matches('/').to_string())
            .filter(|value| !value.is_empty());

        tracing::info!(
            mount = MOUNT_PREFIX,
            mode = mode.as_str(),
            dir = %filesystem_dir.display(),
            external_url = external_url.as_deref().unwrap_or(""),
            "UI 服务模式"
        );

        Self {
            mode,
            filesystem_dir,
            external_url,
        }
    }
}

fn default_ui_serve_mode() -> UiServeMode {
    if cfg!(debug_assertions) {
        UiServeMode::Redirect
    } else {
        UiServeMode::Embedded
    }
}

fn default_dev_external_url(mode: UiServeMode, default_dev_server: &str) -> Option<String> {
    if cfg!(debug_assertions) && matches!(mode, UiServeMode::Redirect | UiServeMode::Proxy) {
        Some(default_dev_server.to_string())
    } else {
        None
    }
}

fn read_ui_env(suffix: &str) -> Option<String> {
    env::var(format!("{ENV_PREFIX}_{suffix}"))
        .ok()
        .or_else(|| env::var(format!("{FALLBACK_ENV_PREFIX}_{suffix}")).ok())
}

/// 创建管理后台 UI 路由：`/ui` 及其子路径，外加旧版 `/admin` 书签的跳转。
pub fn create_ui_router() -> Router {
    ui_router(UiServeState::from_env())
}

fn ui_router(state: UiServeState) -> Router {
    let ui = Router::new()
        .route("/", get(ui_index_handler))
        .route("/{*file}", get(ui_static_handler))
        .with_state(state);
    Router::new()
        .route("/ui/", get(|| async { Redirect::permanent(MOUNT_PREFIX) }))
        .nest(MOUNT_PREFIX, ui)
        // 旧版 /admin 界面已移除，旧书签(含任意子路径)统一跳到 /ui。
        .nest_service(
            "/admin",
            get(|| async { Redirect::permanent(MOUNT_PREFIX) }),
        )
}

async fn ui_index_handler(State(state): State<UiServeState>, uri: Uri) -> Response<Body> {
    match state.mode {
        UiServeMode::Embedded => serve_embedded_index(),
        UiServeMode::Filesystem => serve_filesystem_index(&state).await,
        UiServeMode::Redirect => redirect_to_external(&state, &uri),
        UiServeMode::Proxy => proxy_external(&state, &uri).await,
        UiServeMode::Disabled => ui_disabled_response(),
    }
}

async fn ui_static_handler(State(state): State<UiServeState>, uri: Uri) -> Response<Body> {
    match state.mode {
        UiServeMode::Embedded => serve_embedded_static(&uri),
        UiServeMode::Filesystem => serve_filesystem_static(&state, &uri).await,
        UiServeMode::Redirect => redirect_to_external(&state, &uri),
        UiServeMode::Proxy => proxy_external(&state, &uri).await,
        UiServeMode::Disabled => ui_disabled_response(),
    }
}

/// 处理静态文件请求
fn serve_embedded_static(uri: &Uri) -> Response<Body> {
    let path = uri.path().trim_start_matches('/');

    // 安全检查：拒绝包含 .. 的路径
    if path.contains("..") {
        return Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .body(Body::from("Invalid path"))
            .expect("Failed to build response");
    }

    // 尝试获取请求的文件
    if let Some(content) = UiAsset::get(path) {
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
        return serve_embedded_index();
    }

    // 404
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .body(Body::from("Not found"))
        .expect("Failed to build response")
}

/// 提供 index.html
fn serve_embedded_index() -> Response<Body> {
    match UiAsset::get("index.html") {
        Some(content) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
            .header(header::CACHE_CONTROL, "no-cache")
            .body(Body::from(content.data.into_owned()))
            .expect("Failed to build response"),
        None => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::from(BUILD_HINT))
            .expect("Failed to build response"),
    }
}

async fn serve_filesystem_index(state: &UiServeState) -> Response<Body> {
    serve_filesystem_file(state, "index.html", true).await
}

async fn serve_filesystem_static(state: &UiServeState, uri: &Uri) -> Response<Body> {
    let path = uri.path().trim_start_matches('/');
    if path.contains("..") {
        return Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .body(Body::from("Invalid path"))
            .expect("Failed to build response");
    }

    let response = serve_filesystem_file(state, path, false).await;
    if response.status() != StatusCode::NOT_FOUND || is_asset_path(path) {
        return response;
    }
    serve_filesystem_index(state).await
}

async fn serve_filesystem_file(state: &UiServeState, path: &str, index: bool) -> Response<Body> {
    let Some(file_path) = safe_join(&state.filesystem_dir, path) else {
        return Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .body(Body::from("Invalid path"))
            .expect("Failed to build response");
    };
    match tokio::fs::read(&file_path).await {
        Ok(bytes) => {
            let mime = mime_guess::from_path(path)
                .first_or_octet_stream()
                .to_string();
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, mime)
                .header(header::CACHE_CONTROL, get_cache_control(path))
                .body(Body::from(bytes))
                .expect("Failed to build response")
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            let body = if index {
                format!(
                    "{BUILD_HINT} Filesystem UI directory not found or not built: {}",
                    state.filesystem_dir.display()
                )
            } else {
                "Not found".to_string()
            };
            Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Body::from(body))
                .expect("Failed to build response")
        }
        Err(err) => Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .body(Body::from(format!("Failed to read UI file: {err}")))
            .expect("Failed to build response"),
    }
}

fn safe_join(base: &Path, path: &str) -> Option<PathBuf> {
    let relative = if path.trim().is_empty() {
        Path::new("index.html")
    } else {
        Path::new(path)
    };
    let mut output = base.to_path_buf();
    for component in relative.components() {
        match component {
            Component::Normal(part) => output.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(output)
}

fn redirect_to_external(state: &UiServeState, uri: &Uri) -> Response<Body> {
    let Some(url) = external_ui_url(state, uri) else {
        return missing_external_url_response();
    };
    Response::builder()
        .status(StatusCode::TEMPORARY_REDIRECT)
        .header(header::LOCATION, url)
        .header(header::CACHE_CONTROL, "no-cache")
        .body(Body::empty())
        .expect("Failed to build response")
}

async fn proxy_external(state: &UiServeState, uri: &Uri) -> Response<Body> {
    let Some(url) = external_ui_url(state, uri) else {
        return missing_external_url_response();
    };
    let client = proxy_client();
    match client.get(&url).send().await {
        Ok(response) => {
            let status = response.status();
            let content_type = response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string);
            let mut builder = Response::builder()
                .status(status)
                .header(header::CACHE_CONTROL, "no-cache");
            if let Some(content_type) = content_type {
                builder = builder.header(header::CONTENT_TYPE, content_type);
            }
            builder
                .body(Body::from_stream(response.bytes_stream()))
                .expect("Failed to build response")
        }
        Err(err) => Response::builder()
            .status(StatusCode::BAD_GATEWAY)
            .body(Body::from(format!("UI dev server is unavailable: {err}")))
            .expect("Failed to build response"),
    }
}

fn proxy_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(reqwest::Client::new)
}

fn external_ui_url(state: &UiServeState, uri: &Uri) -> Option<String> {
    let base = state.external_url.as_deref()?.trim_end_matches('/');
    let path_and_query = uri
        .path_and_query()
        .map(|value| value.as_str())
        .unwrap_or("/");

    let mounted_path = if base.ends_with(MOUNT_PREFIX) {
        path_and_query.to_string()
    } else if path_and_query == "/" {
        format!("{MOUNT_PREFIX}/")
    } else {
        format!("{MOUNT_PREFIX}{path_and_query}")
    };
    Some(format!("{base}{mounted_path}"))
}

fn missing_external_url_response() -> Response<Body> {
    Response::builder()
        .status(StatusCode::SERVICE_UNAVAILABLE)
        .body(Body::from(format!(
            "UI redirect/proxy mode requires {ENV_PREFIX}_DEV_SERVER or {ENV_PREFIX}_EXTERNAL_URL"
        )))
        .expect("Failed to build response")
}

fn ui_disabled_response() -> Response<Body> {
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .body(Body::from("UI is disabled"))
        .expect("Failed to build response")
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
    use axum::http::Request;
    use tower::ServiceExt;

    #[cfg(not(debug_assertions))]
    #[test]
    fn embedded_ui_index_uses_expected_mount_prefix() {
        let index = UiAsset::get("index.html").expect("ui index should be embedded");
        let index = std::str::from_utf8(index.data.as_ref()).expect("ui index is utf-8");
        assert!(index.contains("/ui/assets/"));
    }

    #[test]
    fn default_ui_mode_uses_dev_redirect_in_debug_and_embedded_in_release() {
        if cfg!(debug_assertions) {
            assert_eq!(default_ui_serve_mode(), UiServeMode::Redirect);
            assert_eq!(
                default_dev_external_url(UiServeMode::Redirect, DEFAULT_DEV_SERVER).as_deref(),
                Some(DEFAULT_DEV_SERVER)
            );
        } else {
            assert_eq!(default_ui_serve_mode(), UiServeMode::Embedded);
            assert!(default_dev_external_url(UiServeMode::Redirect, DEFAULT_DEV_SERVER).is_none());
        }
    }

    #[test]
    fn external_ui_url_keeps_mount_prefix_once() {
        let state = UiServeState {
            mode: UiServeMode::Redirect,
            filesystem_dir: PathBuf::from(DEFAULT_DIR),
            external_url: Some("http://127.0.0.1:9023".to_string()),
        };
        assert_eq!(
            external_ui_url(&state, &"/".parse::<Uri>().unwrap()).as_deref(),
            Some("http://127.0.0.1:9023/ui/")
        );
        assert_eq!(
            external_ui_url(&state, &"/assets/app.js?x=1".parse::<Uri>().unwrap()).as_deref(),
            Some("http://127.0.0.1:9023/ui/assets/app.js?x=1")
        );

        let mut state_with_mount = state.clone();
        state_with_mount.external_url = Some("http://127.0.0.1:9023/ui".to_string());
        assert_eq!(
            external_ui_url(&state_with_mount, &"/assets/app.js".parse::<Uri>().unwrap())
                .as_deref(),
            Some("http://127.0.0.1:9023/ui/assets/app.js")
        );
    }

    #[test]
    fn safe_join_rejects_parent_and_absolute_paths() {
        let base = Path::new(DEFAULT_DIR);
        assert_eq!(
            safe_join(base, "").as_deref(),
            Some(Path::new("ui/dist/index.html"))
        );
        assert!(safe_join(base, "../config.json").is_none());
        assert!(safe_join(base, "/etc/passwd").is_none());
    }

    #[tokio::test]
    async fn legacy_admin_paths_and_ui_trailing_slash_redirect_to_ui() {
        let app = ui_router(UiServeState {
            mode: UiServeMode::Disabled,
            filesystem_dir: PathBuf::from(DEFAULT_DIR),
            external_url: None,
        });
        for path in ["/admin", "/admin/", "/admin/credentials?tab=1", "/ui/"] {
            let response = app
                .clone()
                .oneshot(Request::get(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::PERMANENT_REDIRECT, "{path}");
            assert_eq!(
                response.headers().get(header::LOCATION).unwrap(),
                MOUNT_PREFIX,
                "{path}"
            );
        }

        let response = app
            .oneshot(Request::get("/ui").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}
