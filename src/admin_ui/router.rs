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
    routing::get,
};

#[cfg(not(debug_assertions))]
use rust_embed::Embed;

/// 嵌入旧版 Admin 前端构建产物
#[cfg(not(debug_assertions))]
#[derive(Embed)]
#[folder = "admin-ui/dist"]
struct AdminAsset;
#[cfg(debug_assertions)]
struct AdminAsset;

/// 嵌入新版前端构建产物(shadcn + Tailwind v4)
#[cfg(not(debug_assertions))]
#[derive(Embed)]
#[folder = "ui/dist"]
struct NewUiAsset;
#[cfg(debug_assertions)]
struct NewUiAsset;

trait UiAsset {
    const BUILD_HINT: &'static str;

    fn get(path: &str) -> Option<rust_embed::EmbeddedFile>;
}

impl UiAsset for AdminAsset {
    const BUILD_HINT: &'static str = "Admin UI not built. Run 'pnpm build' in admin-ui directory.";

    #[cfg(not(debug_assertions))]
    fn get(path: &str) -> Option<rust_embed::EmbeddedFile> {
        <Self as rust_embed::RustEmbed>::get(path)
    }

    #[cfg(debug_assertions)]
    fn get(_path: &str) -> Option<rust_embed::EmbeddedFile> {
        None
    }
}

impl UiAsset for NewUiAsset {
    const BUILD_HINT: &'static str = "New UI not built. Run 'pnpm build' in ui directory.";

    #[cfg(not(debug_assertions))]
    fn get(path: &str) -> Option<rust_embed::EmbeddedFile> {
        <Self as rust_embed::RustEmbed>::get(path)
    }

    #[cfg(debug_assertions)]
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
    name: &'static str,
    mount_prefix: &'static str,
    env_prefix: &'static str,
    mode: UiServeMode,
    filesystem_dir: PathBuf,
    external_url: Option<String>,
    build_hint: &'static str,
}

impl UiServeState {
    fn from_env(
        name: &'static str,
        mount_prefix: &'static str,
        env_prefix: &'static str,
        fallback_env_prefix: Option<&'static str>,
        default_dir: &'static str,
        default_dev_server: &'static str,
        build_hint: &'static str,
    ) -> Self {
        let mode_from_env = read_ui_env(env_prefix, fallback_env_prefix, "MODE")
            .as_deref()
            .and_then(UiServeMode::from_env);
        let external_url_from_env = read_ui_env(env_prefix, fallback_env_prefix, "DEV_SERVER")
            .or_else(|| read_ui_env(env_prefix, fallback_env_prefix, "EXTERNAL_URL"));
        let mode = mode_from_env
            .or_else(|| {
                external_url_from_env
                    .as_ref()
                    .map(|_| UiServeMode::Redirect)
            })
            .unwrap_or_else(default_ui_serve_mode);
        let filesystem_dir = read_ui_env(env_prefix, fallback_env_prefix, "DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(default_dir));
        let external_url = external_url_from_env
            .or_else(|| default_dev_external_url(mode, default_dev_server))
            .map(|value| value.trim().trim_end_matches('/').to_string())
            .filter(|value| !value.is_empty());

        tracing::info!(
            ui = name,
            mount = mount_prefix,
            mode = mode.as_str(),
            dir = %filesystem_dir.display(),
            external_url = external_url.as_deref().unwrap_or(""),
            "UI 服务模式"
        );

        Self {
            name,
            mount_prefix,
            env_prefix,
            mode,
            filesystem_dir,
            external_url,
            build_hint,
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

fn read_ui_env(
    env_prefix: &str,
    fallback_env_prefix: Option<&str>,
    suffix: &str,
) -> Option<String> {
    let specific_key = format!("{env_prefix}_{suffix}");
    env::var(&specific_key).ok().or_else(|| {
        fallback_env_prefix.and_then(|prefix| env::var(format!("{prefix}_{suffix}")).ok())
    })
}

/// 创建旧版 Admin UI 路由
pub fn create_admin_ui_router() -> Router {
    create_ui_router::<AdminAsset>(UiServeState::from_env(
        "admin",
        "/admin",
        "KIRO_ADMIN_UI",
        None,
        "admin-ui/dist",
        "http://127.0.0.1:9025/admin",
        AdminAsset::BUILD_HINT,
    ))
}

/// 创建新版管理后台 UI 路由
pub fn create_new_ui_router() -> Router {
    create_ui_router::<NewUiAsset>(UiServeState::from_env(
        "ui",
        "/ui",
        "KIRO_NEW_UI",
        Some("KIRO_UI"),
        "ui/dist",
        "http://127.0.0.1:9023/ui",
        NewUiAsset::BUILD_HINT,
    ))
}

fn create_ui_router<A: UiAsset + Send + Sync + 'static>(state: UiServeState) -> Router {
    Router::new()
        .route("/", get(ui_index_handler::<A>))
        .route("/{*file}", get(ui_static_handler::<A>))
        .with_state(state)
}

async fn ui_index_handler<A: UiAsset>(
    State(state): State<UiServeState>,
    uri: Uri,
) -> Response<Body> {
    match state.mode {
        UiServeMode::Embedded => serve_embedded_index::<A>(),
        UiServeMode::Filesystem => serve_filesystem_index(&state).await,
        UiServeMode::Redirect => redirect_to_external(&state, &uri),
        UiServeMode::Proxy => proxy_external(&state, &uri).await,
        UiServeMode::Disabled => ui_disabled_response(&state),
    }
}

async fn ui_static_handler<A: UiAsset>(
    State(state): State<UiServeState>,
    uri: Uri,
) -> Response<Body> {
    match state.mode {
        UiServeMode::Embedded => serve_embedded_static::<A>(&uri),
        UiServeMode::Filesystem => serve_filesystem_static(&state, &uri).await,
        UiServeMode::Redirect => redirect_to_external(&state, &uri),
        UiServeMode::Proxy => proxy_external(&state, &uri).await,
        UiServeMode::Disabled => ui_disabled_response(&state),
    }
}

/// 处理静态文件请求
fn serve_embedded_static<A: UiAsset>(uri: &Uri) -> Response<Body> {
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
        return serve_embedded_index::<A>();
    }

    // 404
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .body(Body::from("Not found"))
        .expect("Failed to build response")
}

/// 提供 index.html
fn serve_embedded_index<A: UiAsset>() -> Response<Body> {
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
                    "{} Filesystem UI directory not found or not built: {}",
                    state.build_hint,
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
        return missing_external_url_response(state);
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
        return missing_external_url_response(state);
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
            match response.bytes().await {
                Ok(bytes) => {
                    let mut builder = Response::builder()
                        .status(status)
                        .header(header::CACHE_CONTROL, "no-cache");
                    if let Some(content_type) = content_type {
                        builder = builder.header(header::CONTENT_TYPE, content_type);
                    }
                    builder
                        .body(Body::from(bytes))
                        .expect("Failed to build response")
                }
                Err(err) => Response::builder()
                    .status(StatusCode::BAD_GATEWAY)
                    .body(Body::from(format!(
                        "Failed to read UI proxy response: {err}"
                    )))
                    .expect("Failed to build response"),
            }
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
    let mount_prefix = state.mount_prefix.trim_end_matches('/');
    let path_and_query = if path_and_query == "/" {
        "/"
    } else {
        path_and_query
    };

    let mounted_path = if base.ends_with(mount_prefix) {
        path_and_query.to_string()
    } else if path_and_query == "/" {
        format!("{mount_prefix}/")
    } else {
        format!("{mount_prefix}{path_and_query}")
    };
    Some(format!("{base}{mounted_path}"))
}

fn missing_external_url_response(state: &UiServeState) -> Response<Body> {
    Response::builder()
        .status(StatusCode::SERVICE_UNAVAILABLE)
        .body(Body::from(format!(
            "{} UI mode requires {}_DEV_SERVER or {}_EXTERNAL_URL",
            state.name.to_uppercase(),
            state.env_prefix,
            state.env_prefix
        )))
        .expect("Failed to build response")
}

fn ui_disabled_response(state: &UiServeState) -> Response<Body> {
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .body(Body::from(format!("{} UI is disabled", state.name)))
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

    #[cfg(not(debug_assertions))]
    #[test]
    fn embedded_ui_index_uses_expected_mount_prefix() {
        let new_ui = <NewUiAsset as rust_embed::RustEmbed>::get("index.html")
            .expect("ui index should be embedded");
        let new_ui = std::str::from_utf8(new_ui.data.as_ref()).expect("ui index is utf-8");
        assert!(new_ui.contains("/ui/assets/"));
    }

    #[test]
    fn default_ui_mode_uses_dev_redirect_in_debug_and_embedded_in_release() {
        if cfg!(debug_assertions) {
            assert_eq!(default_ui_serve_mode(), UiServeMode::Redirect);
            assert_eq!(
                default_dev_external_url(UiServeMode::Redirect, "http://127.0.0.1:9023/ui")
                    .as_deref(),
                Some("http://127.0.0.1:9023/ui")
            );
        } else {
            assert_eq!(default_ui_serve_mode(), UiServeMode::Embedded);
            assert!(
                default_dev_external_url(UiServeMode::Redirect, "http://127.0.0.1:9023/ui")
                    .is_none()
            );
        }
    }

    #[test]
    fn external_ui_url_keeps_mount_prefix_once() {
        let state = UiServeState {
            name: "ui",
            mount_prefix: "/ui",
            env_prefix: "KIRO_UI",
            mode: UiServeMode::Redirect,
            filesystem_dir: PathBuf::from("ui/dist"),
            external_url: Some("http://127.0.0.1:9023".to_string()),
            build_hint: NewUiAsset::BUILD_HINT,
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
        let base = Path::new("ui/dist");
        assert_eq!(
            safe_join(base, "").as_deref(),
            Some(Path::new("ui/dist/index.html"))
        );
        assert!(safe_join(base, "../config.json").is_none());
        assert!(safe_join(base, "/etc/passwd").is_none());
    }
}
