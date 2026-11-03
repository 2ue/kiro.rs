//! Kiro 客户端版本自动同步。
//!
//! 打开 `kiroVersionAutoSyncEnabled` 后，后台定时从官方发布元数据读取当前版本
//! （`currentRelease`），所有上游请求头里的 `KiroIDE-{版本}` 改用读到的版本；读取失败或尚未
//! 读到时继续使用配置的 `kiroVersion`。开关关闭时不发任何请求，也不改变请求头。

use std::sync::OnceLock;
use std::time::Duration;

use parking_lot::RwLock;
use serde::Deserialize;

/// 官方稳定版发布元数据。`currentRelease` 与平台无关，选一个能访问的平台路径即可。
pub(crate) const METADATA_URL: &str =
    "https://prod.download.desktop.kiro.dev/stable/metadata-linux-x64-stable.json";
const REFRESH_INTERVAL: Duration = Duration::from_secs(12 * 60 * 60);
/// 拉取失败后的重试间隔：比正常刷新短，避免启动时一次网络抖动就要等 12 小时。
const RETRY_INTERVAL: Duration = Duration::from_secs(10 * 60);
/// 开关关闭时多久看一次运行配置。只读内存里的配置、不发请求，所以可以很短，
/// 让管理后台打开开关后很快生效。
const SWITCH_POLL_INTERVAL: Duration = Duration::from_secs(15);
const FETCH_TIMEOUT_SECS: u64 = 15;
const METADATA_MAX_BYTES: usize = 256 * 1024;

static LATEST: OnceLock<RwLock<Option<String>>> = OnceLock::new();

fn cell() -> &'static RwLock<Option<String>> {
    LATEST.get_or_init(|| RwLock::new(None))
}

/// 最近一次读到的官方版本；从未读到时为 `None`。
pub fn latest() -> Option<String> {
    cell().read().clone()
}

fn store(version: String) -> bool {
    let mut guard = cell().write();
    let changed = guard.as_deref() != Some(version.as_str());
    *guard = Some(version);
    changed
}

#[cfg(test)]
pub(crate) fn set_latest_for_tests(version: Option<&str>) {
    *cell().write() = version.map(str::to_string);
}

#[derive(Deserialize)]
struct Metadata {
    #[serde(rename = "currentRelease")]
    current_release: Option<String>,
}

/// 版本号只接受 `数字.数字.数字`（每段最多 6 位），其他内容一律丢弃，避免把异常内容写进请求头。
pub(crate) fn sanitize_version(raw: &str) -> Option<String> {
    let version = raw.trim();
    let parts: Vec<&str> = version.split('.').collect();
    let valid = parts.len() == 3
        && parts.iter().all(|part| {
            !part.is_empty() && part.len() <= 6 && part.bytes().all(|byte| byte.is_ascii_digit())
        });
    valid.then(|| version.to_string())
}

pub(crate) fn parse_metadata(body: &[u8]) -> anyhow::Result<String> {
    let metadata: Metadata = serde_json::from_slice(body)?;
    let raw = metadata
        .current_release
        .ok_or_else(|| anyhow::anyhow!("发布元数据缺少 currentRelease"))?;
    sanitize_version(&raw).ok_or_else(|| anyhow::anyhow!("发布元数据里的版本号格式不合法"))
}

async fn fetch_latest(
    url: &str,
    proxy: Option<&crate::http_client::ProxyConfig>,
    tls_backend: crate::model::config::TlsBackend,
) -> anyhow::Result<String> {
    let client = crate::http_client::build_client(proxy, FETCH_TIMEOUT_SECS, tls_backend)?;
    let response = client.get(url).send().await?;
    let status = response.status();
    if !status.is_success() {
        anyhow::bail!("读取 Kiro 发布元数据失败: HTTP {}", status.as_u16());
    }
    let body = crate::http_client::response_bytes_with_limit_and_body_timeout(
        response,
        FETCH_TIMEOUT_SECS,
        METADATA_MAX_BYTES,
    )
    .await?;
    parse_metadata(&body)
}

/// 后台同步任务。每轮先读一次运行配置：开关关闭时不发请求，15 秒后再看；打开后读取官方版本，
/// 成功后 12 小时刷新一次，失败 10 分钟后重试。
pub fn spawn_auto_sync(
    token_manager: std::sync::Arc<crate::kiro::token_manager::MultiTokenManager>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let config = token_manager.runtime_config();
            let next = if !config.kiro_version_auto_sync_enabled {
                SWITCH_POLL_INTERVAL
            } else {
                let proxy = config.proxy_url.as_deref().map(|url| {
                    let proxy = crate::http_client::ProxyConfig::new(url);
                    match (&config.proxy_username, &config.proxy_password) {
                        (Some(username), Some(password)) => proxy.with_auth(username, password),
                        _ => proxy,
                    }
                });
                match fetch_latest(METADATA_URL, proxy.as_ref(), config.tls_backend).await {
                    Ok(version) => {
                        if store(version.clone()) {
                            tracing::info!(
                                version = %version,
                                configured = %config.kiro_version,
                                "已同步 Kiro 客户端版本号，上游请求头改用该版本"
                            );
                        }
                        REFRESH_INTERVAL
                    }
                    Err(err) => {
                        tracing::warn!(
                            configured = %config.kiro_version,
                            "同步 Kiro 客户端版本号失败，继续使用配置的版本: {}",
                            err
                        );
                        RETRY_INTERVAL
                    }
                }
            };
            tokio::time::sleep(next).await;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_plain_three_part_versions_are_accepted() {
        assert_eq!(sanitize_version(" 1.4.62 ").as_deref(), Some("1.4.62"));
        for bad in [
            "",
            "1.4",
            "1.4.62.1",
            "1.4.x",
            "1.4.62-beta",
            "1.4.62\r\nX-Evil: 1",
            "1234567.1.1",
        ] {
            assert_eq!(sanitize_version(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn metadata_parsing_rejects_missing_or_malformed_versions() {
        assert_eq!(
            parse_metadata(br#"{"currentRelease":"1.4.62","releases":[]}"#).unwrap(),
            "1.4.62"
        );
        assert!(parse_metadata(br#"{"releases":[]}"#).is_err());
        assert!(parse_metadata(br#"{"currentRelease":"<script>"}"#).is_err());
        assert!(parse_metadata(b"not json").is_err());
    }

    /// The configured version stays in use while the switch is off, and also when it is on but
    /// no release has been read yet.
    #[test]
    fn effective_version_follows_the_switch() {
        let mut config = crate::model::config::Config::default();
        config.kiro_version = "0.11.107".to_string();
        set_latest_for_tests(None);
        config.kiro_version_auto_sync_enabled = true;
        assert_eq!(config.effective_kiro_version(), "0.11.107");

        set_latest_for_tests(Some("1.4.62"));
        assert_eq!(config.effective_kiro_version(), "1.4.62");
        config.kiro_version_auto_sync_enabled = false;
        assert_eq!(config.effective_kiro_version(), "0.11.107");
        set_latest_for_tests(None);
    }
}
