//! Kiro 客户端版本自动同步。
//!
//! 打开 `kiroVersionAutoSyncEnabled` 后，后台定时从官方发布元数据读取当前版本
//! （`currentRelease`）作为 candidate。candidate 不会自动改变请求头；只有管理员显式
//! apply/pin/rollback 后才会持久化 active override。开关关闭时不发检查请求，也不回退已应用版本。

use std::sync::OnceLock;
use std::time::Duration;

use chrono::Utc;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

/// 官方稳定版发布元数据。`currentRelease` 与平台无关，选一个能访问的平台路径即可。
pub(crate) const METADATA_URL: &str =
    "https://prod.download.desktop.kiro.dev/stable/metadata-linux-x64-stable.json";
/// IDE/streaming package pairs extracted from official installers.
const VERIFIED_STREAMING_SDK_PAIRS: &[(&str, &str)] =
    &[("1.4.121", "1.0.44"), ("1.4.154", "1.0.44")];
pub(crate) const VERIFIED_IDE_CLIENT_VERSION: &str = "1.4.154";
/// Keep the existing service identity for configured or pinned IDE releases without package evidence.
const LEGACY_STREAMING_SDK_VERSION: &str = "1.0.34";
const REFRESH_INTERVAL: Duration = Duration::from_secs(12 * 60 * 60);
/// 拉取失败后的重试间隔：比正常刷新短，避免启动时一次网络抖动就要等 12 小时。
const RETRY_INTERVAL: Duration = Duration::from_secs(10 * 60);
/// 开关关闭时多久看一次运行配置。只读内存里的配置、不发请求，所以可以很短，
/// 让管理后台打开开关后很快生效。
const SWITCH_POLL_INTERVAL: Duration = Duration::from_secs(15);
const FETCH_TIMEOUT_SECS: u64 = 15;
const METADATA_MAX_BYTES: usize = 256 * 1024;

/// 版本状态来源。自动同步只发现 candidate；active 来源只由显式 apply/pin/rollback
/// 产生，关闭自动检查不会把 active 隐式切回旧配置。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActiveVersionSource {
    Configured,
    /// 显式 apply 的版本。旧状态中的 `auto`/`auto_applied` 仍可读取。
    #[serde(alias = "auto", alias = "auto_applied")]
    Applied,
    Pinned,
}

/// 独立于完整运行配置的版本生命周期状态。
///
/// `active_version=None` 表示使用当前运行配置的 `kiroVersion`；这让旧数据库和旧
/// 配置可以自然迁移，同时避免把版本检查时间和 candidate 写进完整 runtime config。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct VersionState {
    pub active_version: Option<String>,
    pub active_source: Option<ActiveVersionSource>,
    pub previous_version: Option<String>,
    pub previous_source: Option<ActiveVersionSource>,
    pub candidate_version: Option<String>,
    pub candidate_checked_at: Option<String>,
    pub last_error: Option<String>,
    pub generation: u64,
    /// One-time bridge for databases created before version state was persisted.
    /// The old active `LATEST` value was process-local and cannot survive restart.
    pub legacy_auto_sync_pending: bool,
}

impl VersionState {
    /// 去掉不符合请求头约束的历史值，避免状态表里的坏数据污染上游请求。
    pub fn normalized(mut self) -> Self {
        self.active_version = self.active_version.as_deref().and_then(sanitize_version);
        self.previous_version = self.previous_version.as_deref().and_then(sanitize_version);
        self.candidate_version = self.candidate_version.as_deref().and_then(sanitize_version);
        if self.active_version.is_none() {
            self.active_source = None;
        }
        if self.previous_version.is_none() {
            self.previous_source = None;
        }
        self
    }
}

static STATE: OnceLock<RwLock<VersionState>> = OnceLock::new();

fn cell() -> &'static RwLock<VersionState> {
    STATE.get_or_init(|| RwLock::new(VersionState::default()))
}

/// 读取当前独立版本状态。
pub fn state() -> VersionState {
    cell().read().clone()
}

/// 最近一次读到的官方版本；从未读到时为 `None`。
pub fn latest() -> Option<String> {
    cell().read().candidate_version.clone()
}

/// 从持久化存储恢复版本状态。调用方应在服务启动时调用一次。
pub fn replace_state(new_state: VersionState) {
    let new_state = new_state.normalized();
    let mut current = cell().write();
    replace_state_if_newer(&mut current, new_state);
}

/// Create the version row once, preserving legacy auto-sync behavior on the first successful
/// check. The configured version remains untouched and is used until that check succeeds.
pub async fn initialize_state_if_missing(
    store: &crate::storage::postgres::PostgresStore,
    legacy_auto_sync_pending: bool,
) -> anyhow::Result<VersionState> {
    let initial = VersionState {
        legacy_auto_sync_pending,
        ..VersionState::default()
    };
    store
        .initialize_kiro_version_state_if_absent(&initial)
        .await?;
    let state = store
        .load_kiro_version_state()
        .await?
        .unwrap_or(initial)
        .normalized();
    replace_state(state.clone());
    Ok(state)
}

fn replace_state_if_newer(current: &mut VersionState, next: VersionState) {
    if next.generation >= current.generation {
        *current = next;
    }
}

/// 将一次成功检查记录到独立状态。
pub fn record_candidate(state: &mut VersionState, version: &str) -> anyhow::Result<bool> {
    let version =
        sanitize_version(version).ok_or_else(|| anyhow::anyhow!("Kiro 客户端版本号格式不合法"))?;
    let changed = state.candidate_version.as_deref() != Some(version.as_str());
    state.candidate_version = Some(version);
    state.candidate_checked_at = Some(Utc::now().to_rfc3339());
    state.last_error = None;
    state.generation = state.generation.saturating_add(1);
    Ok(changed)
}

/// 记录检查失败，不清除上次成功读到的 candidate。
pub fn record_check_error(state: &mut VersionState, error: &str) {
    state.candidate_checked_at = Some(Utc::now().to_rfc3339());
    state.last_error = Some(error.to_string());
    state.generation = state.generation.saturating_add(1);
}

/// 持久化一次版本检查结果。冲突时基于数据库最新状态重放 candidate/error 更新，
/// 避免较早开始的后台检查覆盖另一实例刚写入的 active 状态。
pub async fn persist_check_result(
    store: &crate::storage::postgres::PostgresStore,
    candidate: Option<&str>,
    error: Option<&str>,
    configured_version: &str,
    apply_legacy_auto_sync: bool,
) -> anyhow::Result<VersionState> {
    const CAS_RETRIES: usize = 8;
    if candidate.is_some() == error.is_some() {
        anyhow::bail!("版本检查结果必须且只能包含 candidate 或 error");
    }

    for _ in 0..CAS_RETRIES {
        let current = store
            .load_kiro_version_state()
            .await?
            .unwrap_or_default()
            .normalized();
        let mut next = current.clone();
        if let Some(candidate) = candidate {
            record_successful_check(
                &mut next,
                candidate,
                configured_version,
                apply_legacy_auto_sync,
            )?;
        } else if let Some(error) = error {
            record_check_error(&mut next, error);
        }
        if store
            .save_kiro_version_state_if_generation(&next, current.generation)
            .await?
        {
            replace_state(next.clone());
            return Ok(next);
        }
    }
    anyhow::bail!("Kiro 版本状态持续并发更新，检查结果未能保存")
}

fn record_successful_check(
    state: &mut VersionState,
    candidate: &str,
    configured_version: &str,
    apply_legacy_auto_sync: bool,
) -> anyhow::Result<()> {
    record_candidate(state, candidate)?;
    if apply_legacy_auto_sync && state.legacy_auto_sync_pending {
        // 旧配置的自动检查只迁移为 candidate 发现能力。active 仍必须由管理员
        // 显式 apply/pin/rollback 改变，避免升级后首次后台检查悄悄替换请求身份。
        let _ = configured_version;
        state.legacy_auto_sync_pending = false;
    }
    Ok(())
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

/// Resolve the CodeWhisperer streaming service-client version from the same IDE version snapshot
/// used in request headers. Only the installer-verified pair is advanced; unknown releases retain
/// the existing service identity until their package metadata is inspected.
pub(crate) fn streaming_sdk_version_for_ide(ide_version: &str) -> &'static str {
    let Some(version) = sanitize_version(ide_version) else {
        return LEGACY_STREAMING_SDK_VERSION;
    };
    VERIFIED_STREAMING_SDK_PAIRS
        .iter()
        .find_map(|(ide, sdk)| (*ide == version).then_some(*sdk))
        .unwrap_or(LEGACY_STREAMING_SDK_VERSION)
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

/// 立即读取官方发布元数据。调用方应先持久化 `record_candidate` 的结果，再发布状态。
pub async fn check_now(config: &crate::model::config::Config) -> anyhow::Result<String> {
    let proxy = config.proxy_url.as_deref().map(|url| {
        let proxy = crate::http_client::ProxyConfig::new(url);
        match (&config.proxy_username, &config.proxy_password) {
            (Some(username), Some(password)) => proxy.with_auth(username, password),
            _ => proxy,
        }
    });
    fetch_latest(METADATA_URL, proxy.as_ref(), config.tls_backend).await
}

fn current_active(state: &VersionState, configured_version: &str) -> (String, ActiveVersionSource) {
    if let Some(version) = state.active_version.as_deref().and_then(sanitize_version) {
        return (
            version,
            state.active_source.unwrap_or(ActiveVersionSource::Pinned),
        );
    }
    (
        sanitize_version(configured_version).unwrap_or_else(|| configured_version.to_string()),
        ActiveVersionSource::Configured,
    )
}

/// 判断一次状态写入是否改变了请求实际使用的 active 来源。
/// candidate、检查时间和错误字段变化不应触发其他实例重载。
pub fn active_changed(before: &VersionState, after: &VersionState) -> bool {
    before.active_version != after.active_version || before.active_source != after.active_source
}

fn target_version(state: &VersionState, version: Option<&str>) -> anyhow::Result<String> {
    let target = version
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or(state.candidate_version.as_deref())
        .ok_or_else(|| anyhow::anyhow!("没有可应用的 Kiro 客户端版本，请先执行版本检查"))?
        .to_string();
    sanitize_version(&target).ok_or_else(|| anyhow::anyhow!("Kiro 客户端版本号格式不合法"))
}

fn apply_to_state(
    state: &mut VersionState,
    version: Option<&str>,
    source: ActiveVersionSource,
    configured_version: &str,
) -> anyhow::Result<()> {
    let target = target_version(state, version)?;
    let (current, current_source) = current_active(state, configured_version);
    let cleared_legacy_marker = state.legacy_auto_sync_pending;
    state.legacy_auto_sync_pending = false;
    if state.active_version.as_deref() == Some(target.as_str())
        && state.active_source == Some(source)
    {
        if cleared_legacy_marker {
            state.generation = state.generation.saturating_add(1);
        }
        return Ok(());
    }
    state.previous_version = Some(current);
    state.previous_source = Some(current_source);
    state.active_version = Some(target);
    state.active_source = Some(source);
    state.legacy_auto_sync_pending = false;
    state.generation = state.generation.saturating_add(1);
    Ok(())
}

/// 将 candidate（或显式版本）应用为 active，返回待持久化的新状态。
pub fn apply_version(
    current: &VersionState,
    version: Option<&str>,
    configured_version: &str,
) -> anyhow::Result<VersionState> {
    let mut state = current.clone();
    apply_to_state(
        &mut state,
        version,
        ActiveVersionSource::Applied,
        configured_version,
    )?;
    Ok(state)
}

/// 显式固定版本，返回待持久化的新状态。
pub fn pin_version(
    current: &VersionState,
    version: &str,
    configured_version: &str,
) -> anyhow::Result<VersionState> {
    let mut state = current.clone();
    apply_to_state(
        &mut state,
        Some(version),
        ActiveVersionSource::Pinned,
        configured_version,
    )?;
    Ok(state)
}

/// 回滚到 previous active 版本，并保留当前版本作为下一次 rollback 的 previous。
pub fn rollback_version(
    current_state: &VersionState,
    configured_version: &str,
) -> anyhow::Result<VersionState> {
    let mut state = current_state.clone();
    let previous = state
        .previous_version
        .take()
        .ok_or_else(|| anyhow::anyhow!("没有可回滚的 Kiro 客户端版本"))?;
    let previous_source = state
        .previous_source
        .take()
        .unwrap_or(ActiveVersionSource::Pinned);
    let (current, current_source) = current_active(&state, configured_version);
    state.previous_version = Some(current);
    state.previous_source = Some(current_source);
    state.active_version = Some(previous);
    state.active_source = Some(previous_source);
    state.generation = state.generation.saturating_add(1);
    Ok(state)
}

/// 当前运行配置实际使用的版本。显式 active override 优先于自动发现的 candidate。
pub fn effective_version(configured_version: &str) -> String {
    current_active(&cell().read(), configured_version).0
}

/// Admin/API 层可直接返回的生命周期快照。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LifecycleSnapshot {
    pub configured_version: String,
    pub active_version: String,
    pub active_source: ActiveVersionSource,
    pub previous_version: Option<String>,
    pub candidate_version: Option<String>,
    pub checked_at: Option<String>,
    pub last_error: Option<String>,
    pub generation: u64,
    pub auto_sync_enabled: bool,
}

pub fn lifecycle_snapshot(configured_version: &str, auto_sync_enabled: bool) -> LifecycleSnapshot {
    let state = state();
    let (active_version, active_source) = current_active(&state, configured_version);
    LifecycleSnapshot {
        configured_version: configured_version.to_string(),
        active_version,
        active_source,
        previous_version: state.previous_version.clone(),
        candidate_version: state.candidate_version.clone(),
        checked_at: state.candidate_checked_at.clone(),
        last_error: state.last_error.clone(),
        generation: state.generation,
        auto_sync_enabled,
    }
}

/// 后台同步任务。每轮先读一次运行配置：开关关闭时不发请求，15 秒后再看；打开后读取官方版本，
/// 成功后 12 小时刷新一次，失败 10 分钟后重试。
pub fn spawn_auto_sync(
    token_manager: std::sync::Arc<crate::kiro::token_manager::MultiTokenManager>,
    postgres_store: std::sync::Arc<crate::storage::postgres::PostgresStore>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut next_check = tokio::time::Instant::now();
        loop {
            let config = token_manager.runtime_config();
            if !config.kiro_version_auto_sync_enabled {
                tokio::time::sleep(SWITCH_POLL_INTERVAL).await;
                continue;
            }
            let now = tokio::time::Instant::now();
            if now < next_check {
                tokio::time::sleep((next_check - now).min(SWITCH_POLL_INTERVAL)).await;
                continue;
            }
            match check_now(&config).await {
                Ok(version) => {
                    if let Err(error) = persist_check_result(
                        &postgres_store,
                        Some(&version),
                        None,
                        &config.kiro_version,
                        true,
                    )
                    .await
                    {
                        tracing::warn!("保存 Kiro 版本候选状态失败: {}", error);
                        next_check = tokio::time::Instant::now() + RETRY_INTERVAL;
                    } else {
                        tracing::info!(
                            version = %version,
                            configured = %config.kiro_version,
                            "已同步 Kiro 客户端版本候选"
                        );
                        next_check = tokio::time::Instant::now() + REFRESH_INTERVAL;
                    }
                }
                Err(err) => {
                    if let Err(save_error) = persist_check_result(
                        &postgres_store,
                        None,
                        Some(&err.to_string()),
                        &config.kiro_version,
                        false,
                    )
                    .await
                    {
                        tracing::warn!("保存 Kiro 版本检查错误状态失败: {}", save_error);
                    }
                    tracing::warn!(
                        configured = %config.kiro_version,
                        "同步 Kiro 客户端版本号失败，继续使用配置的版本: {}",
                        err
                    );
                    next_check = tokio::time::Instant::now() + RETRY_INTERVAL;
                }
            }
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
    fn streaming_sdk_pair_tracks_the_snapshotted_ide_release() {
        assert_eq!(
            streaming_sdk_version_for_ide(VERIFIED_IDE_CLIENT_VERSION),
            "1.0.44"
        );
        assert_eq!(streaming_sdk_version_for_ide("1.4.121"), "1.0.44");
        for ide_version in ["0.11.107", "1.4.153", "1.4.121-beta", "invalid"] {
            assert_eq!(
                streaming_sdk_version_for_ide(ide_version),
                LEGACY_STREAMING_SDK_VERSION,
                "{ide_version} has no inspected SDK package pair"
            );
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

    /// Candidate discovery never changes the active version; only an explicit lifecycle action
    /// can install an active override.
    #[test]
    fn candidate_does_not_auto_apply_and_apply_survives_check_toggle() {
        let mut state = VersionState::default();
        record_candidate(&mut state, "1.4.121").unwrap();

        assert_eq!(current_active(&state, "0.11.107").0, "0.11.107");

        let active = apply_version(&state, None, "0.11.107").unwrap();
        assert_eq!(active.active_version.as_deref(), Some("1.4.121"));
        assert_eq!(active.active_source, Some(ActiveVersionSource::Applied));
        assert_eq!(active.previous_version.as_deref(), Some("0.11.107"));
        assert_eq!(current_active(&active, "0.11.107").0, "1.4.121");
    }

    #[test]
    fn legacy_auto_sync_migration_discovers_candidate_without_changing_active() {
        let mut state = VersionState {
            legacy_auto_sync_pending: true,
            ..VersionState::default()
        };

        record_successful_check(&mut state, "1.4.154", "0.11.107", true).unwrap();

        assert_eq!(state.active_version, None);
        assert_eq!(state.previous_version, None);
        assert_eq!(state.candidate_version.as_deref(), Some("1.4.154"));
        assert!(!state.legacy_auto_sync_pending);

        record_successful_check(&mut state, "1.4.200", "0.11.107", true).unwrap();

        assert_eq!(state.active_version, None);
        assert_eq!(state.candidate_version.as_deref(), Some("1.4.200"));
    }

    #[test]
    fn legacy_migration_waits_for_enabled_check_and_explicit_apply_consumes_marker() {
        let mut state = VersionState {
            legacy_auto_sync_pending: true,
            ..VersionState::default()
        };

        record_successful_check(&mut state, "1.4.154", "0.11.107", false).unwrap();
        assert_eq!(state.active_version, None);
        assert!(state.legacy_auto_sync_pending);

        let applied = apply_version(&state, Some("1.4.154"), "0.11.107").unwrap();
        assert!(!applied.legacy_auto_sync_pending);
        assert_eq!(applied.previous_version.as_deref(), Some("0.11.107"));
    }

    #[test]
    fn migrated_active_state_round_trips_for_restart_and_disabled_checks() {
        let mut migrated = VersionState {
            legacy_auto_sync_pending: true,
            ..VersionState::default()
        };
        record_successful_check(&mut migrated, "1.4.154", "0.11.107", true).unwrap();

        let serialized = serde_json::to_value(&migrated).unwrap();
        let restored: VersionState = serde_json::from_value(serialized).unwrap();

        assert_eq!(current_active(&restored, "0.11.107").0, "0.11.107");
        assert!(!restored.legacy_auto_sync_pending);
        assert_eq!(restored.previous_version, None);
    }

    #[test]
    fn pin_and_rollback_restore_previous_versions_without_requiring_candidate() {
        let state = VersionState::default();
        let pinned = pin_version(&state, "1.4.100", "0.11.107").unwrap();
        assert_eq!(pinned.active_version.as_deref(), Some("1.4.100"));
        assert_eq!(pinned.active_source, Some(ActiveVersionSource::Pinned));
        assert_eq!(pinned.previous_version.as_deref(), Some("0.11.107"));

        let rolled_back = rollback_version(&pinned, "0.11.107").unwrap();
        assert_eq!(rolled_back.active_version.as_deref(), Some("0.11.107"));
        assert_eq!(current_active(&rolled_back, "0.11.107").0, "0.11.107");
        assert_eq!(rolled_back.previous_version.as_deref(), Some("1.4.100"));
    }

    #[test]
    fn rollback_restores_the_configured_version_snapshot_after_config_changes() {
        let applied = apply_version(&VersionState::default(), Some("1.4.154"), "0.11.107").unwrap();

        let rolled_back = rollback_version(&applied, "1.4.121").unwrap();

        assert_eq!(rolled_back.active_version.as_deref(), Some("0.11.107"));
        assert_eq!(
            rolled_back.active_source,
            Some(ActiveVersionSource::Configured)
        );
        assert_eq!(current_active(&rolled_back, "1.4.121").0, "0.11.107");
    }

    #[test]
    fn invalid_versions_and_missing_candidate_are_rejected_without_mutation() {
        let state = VersionState::default();
        assert!(apply_version(&state, None, "0.11.107").is_err());
        assert!(pin_version(&state, "1.4.x", "0.11.107").is_err());
        assert_eq!(state, VersionState::default());
    }

    #[test]
    fn disabling_candidate_checks_does_not_revert_applied_active_version() {
        let mut state = VersionState::default();
        record_candidate(&mut state, "1.4.121").unwrap();
        let applied = apply_version(&state, None, "0.11.107").unwrap();

        // The check toggle controls discovery only; resolution uses persisted active state.
        assert_eq!(current_active(&applied, "0.11.107").0, "1.4.121");
        assert_eq!(applied.active_source, Some(ActiveVersionSource::Applied));
    }

    #[test]
    fn candidate_and_error_updates_advance_cas_generation_without_active_change() {
        let mut checked = VersionState::default();
        let before = checked.clone();
        record_candidate(&mut checked, "1.4.121").unwrap();
        assert_eq!(checked.generation, 1);
        assert!(!active_changed(&before, &checked));

        let before_error = checked.clone();
        record_check_error(&mut checked, "temporary metadata failure");
        assert_eq!(checked.generation, 2);
        assert!(!active_changed(&before_error, &checked));

        let applied = apply_version(&checked, Some("1.4.121"), "0.11.107").unwrap();
        assert!(active_changed(&checked, &applied));
        assert_eq!(applied.generation, 3);
    }

    #[test]
    fn stale_runtime_reload_cannot_replace_newer_version_generation() {
        let mut current = VersionState {
            active_version: Some("1.4.122".into()),
            generation: 4,
            ..VersionState::default()
        };
        replace_state_if_newer(
            &mut current,
            VersionState {
                active_version: Some("1.4.121".into()),
                generation: 3,
                ..VersionState::default()
            },
        );
        assert_eq!(current.active_version.as_deref(), Some("1.4.122"));
        assert_eq!(current.generation, 4);
    }

    #[test]
    fn lifecycle_state_round_trips_and_normalizes_unsafe_versions() {
        let state = VersionState {
            active_version: Some("1.4.121".into()),
            active_source: Some(ActiveVersionSource::Pinned),
            previous_version: Some("0.11.107".into()),
            previous_source: Some(ActiveVersionSource::Configured),
            candidate_version: Some("1.4.122".into()),
            candidate_checked_at: Some("2026-11-10T00:00:00Z".into()),
            last_error: None,
            generation: 4,
            legacy_auto_sync_pending: false,
        };
        let value = serde_json::to_value(&state).unwrap();
        let decoded: VersionState = serde_json::from_value(value).unwrap();
        assert_eq!(decoded, state);

        let normalized = VersionState {
            active_version: Some("1.4.121\r\nX: y".into()),
            active_source: Some(ActiveVersionSource::Pinned),
            ..VersionState::default()
        }
        .normalized();
        assert_eq!(normalized.active_version, None);
        assert_eq!(normalized.active_source, None);
    }

    #[test]
    fn legacy_active_sources_deserialize_to_applied() {
        for legacy_source in ["auto", "auto_applied"] {
            let value = serde_json::json!({
                "activeVersion": "1.4.121",
                "activeSource": legacy_source,
            });
            let state: VersionState = serde_json::from_value(value).unwrap();
            assert_eq!(state.active_source, Some(ActiveVersionSource::Applied));
            assert_eq!(
                serde_json::to_value(state).unwrap()["activeSource"],
                "applied"
            );
        }
    }
}
