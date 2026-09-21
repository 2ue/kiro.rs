//! 设备指纹生成器
//!

use sha2::{Digest, Sha256};

use crate::kiro::model::credentials::KiroCredentials;
use crate::model::config::Config;

/// 当前账号身份派生规则版本。
///
/// 该值会进入 fallback 派生域，后续调整算法时可以避免新旧身份意外复用。
pub const MACHINE_ID_DERIVATION_VERSION: &str = "account-v2";

/// machineId 的来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MachineIdSource {
    /// 凭据中显式配置并通过格式校验的值。
    StoredCredential,
    /// 从 API key 派生。
    DerivedApiKey,
    /// 从 OAuth refresh token 派生。
    DerivedRefreshToken,
    /// 从持久化账号 ID/稳定账号字段确定性派生。
    DeterministicFallback,
    /// 兼容旧版行为，使用全局 machineId。
    GlobalFallback,
}

impl MachineIdSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::StoredCredential => "stored_credential",
            Self::DerivedApiKey => "derived_api_key",
            Self::DerivedRefreshToken => "derived_refresh_token",
            Self::DeterministicFallback => "deterministic_fallback",
            Self::GlobalFallback => "global_fallback",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "stored_credential" => Some(Self::StoredCredential),
            "derived_api_key" => Some(Self::DerivedApiKey),
            "derived_refresh_token" => Some(Self::DerivedRefreshToken),
            "deterministic_fallback" => Some(Self::DeterministicFallback),
            "global_fallback" => Some(Self::GlobalFallback),
            _ => None,
        }
    }
}

/// 一次账号身份解析的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineIdResolution {
    pub machine_id: String,
    pub source: MachineIdSource,
}

/// 标准化 machineId 格式
///
/// 支持以下格式：
/// - 64 字符十六进制字符串（直接返回）
/// - UUID 格式（如 "2582956e-cc88-4669-b546-07adbffcb894"，移除连字符后补齐到 64 字符）
fn normalize_machine_id(machine_id: &str) -> Option<String> {
    let trimmed = machine_id.trim();

    // 如果已经是 64 字符，直接返回
    if trimmed.len() == 64 && trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
        return Some(trimmed.to_string());
    }

    // 尝试解析 UUID 格式（移除连字符）
    let without_dashes: String = trimmed.chars().filter(|c| *c != '-').collect();

    // UUID 去掉连字符后是 32 字符
    if without_dashes.len() == 32 && without_dashes.chars().all(|c| c.is_ascii_hexdigit()) {
        // 补齐到 64 字符（重复一次）
        return Some(format!("{}{}", without_dashes, without_dashes));
    }

    // 无法识别的格式
    None
}

/// 根据凭证信息生成账号级 machineId。
///
/// 优先级：
/// 1. 凭据级 `machineId`（若配置且格式合法）
/// 2. 根据凭据类型派生（互斥，由 [`KiroCredentials::is_api_key_credential`] 分流）：
///    - API Key 凭据：基于 `kiroApiKey` 派生
///    - OAuth 凭据：基于 `refreshToken` 派生
/// 3. 仅在显式打开 `globalMachineIdFallbackEnabled` 时使用全局 `machineId`
/// 4. 基于账号 ID/稳定账号字段确定性派生 fallback
///
/// 全局 machineId 不再覆盖有账号级派生材料的账号，避免多账号池共享同一个
/// 设备身份。正常服务启动会先分配凭据 ID，再把 fallback 写回现有 credentials.data，
/// 因此该 fallback 跨重启稳定。
pub fn generate_from_credentials(credentials: &KiroCredentials, config: &Config) -> String {
    resolve_from_credentials(credentials, config).machine_id
}

/// 解析 machineId 及其来源，供 Admin 快照、审计和测试使用。
pub fn resolve_from_credentials(
    credentials: &KiroCredentials,
    config: &Config,
) -> MachineIdResolution {
    let recorded_source = credentials
        .machine_id_source
        .as_deref()
        .and_then(MachineIdSource::parse);

    // A persisted account machineId is authoritative after the first assignment.
    // Refresh-token/API-key rotation must not silently change the account identity.
    if let Some(source) = recorded_source {
        if source == MachineIdSource::GlobalFallback {
            if config.global_machine_id_fallback_enabled {
                if let Some(machine_id) = source_machine_id(credentials, config, source) {
                    return MachineIdResolution { machine_id, source };
                }
            }
        } else if let Some(machine_id) = credentials
            .machine_id
            .as_deref()
            .and_then(normalize_machine_id)
        {
            return MachineIdResolution { machine_id, source };
        }
    }

    // Recover the source for credentials persisted by older versions without metadata.
    if credentials.machine_id_source.is_none() {
        for source in [
            MachineIdSource::DerivedApiKey,
            MachineIdSource::DerivedRefreshToken,
            MachineIdSource::DeterministicFallback,
        ] {
            if let Some(machine_id) = source_machine_id(credentials, config, source)
                && credentials.machine_id.as_deref() == Some(machine_id.as_str())
            {
                return MachineIdResolution { machine_id, source };
            }
        }
    }

    // Legacy credentials may have a persisted machineId without source metadata.
    // Preserve that value instead of deriving a new identity from a rotated token.
    if let Some(ref machine_id) = credentials.machine_id {
        if let Some(normalized) = normalize_machine_id(machine_id) {
            return MachineIdResolution {
                machine_id: normalized,
                source: MachineIdSource::StoredCredential,
            };
        }
    }

    // 按凭据类型派生（API Key 与 refreshToken 两条路径互斥，不回落）
    if credentials.is_api_key_credential() {
        // API Key 凭据：基于 kiroApiKey 派生
        if let Some(ref api_key) = credentials.kiro_api_key {
            if !api_key.is_empty() {
                return MachineIdResolution {
                    machine_id: sha256_hex(&format!("KiroAPIKey/{}", api_key)),
                    source: MachineIdSource::DerivedApiKey,
                };
            }
        }
    } else if let Some(ref refresh_token) = credentials.refresh_token {
        // OAuth 凭据：基于 refreshToken 派生
        if !refresh_token.is_empty() {
            return MachineIdResolution {
                machine_id: sha256_hex(&format!("KotlinNativeAPI/{}", refresh_token)),
                source: MachineIdSource::DerivedRefreshToken,
            };
        }
    }

    // 全局 machineId 只作为显式兼容开关，不参与正常账号身份隔离。
    if config.global_machine_id_fallback_enabled {
        if let Some(ref machine_id) = config.machine_id {
            if let Some(normalized) = normalize_machine_id(machine_id) {
                return MachineIdResolution {
                    machine_id: normalized,
                    source: MachineIdSource::GlobalFallback,
                };
            }
        }
    }

    MachineIdResolution {
        machine_id: deterministic_fallback_machine_id(credentials),
        source: MachineIdSource::DeterministicFallback,
    }
}

fn source_machine_id(
    credentials: &KiroCredentials,
    config: &Config,
    source: MachineIdSource,
) -> Option<String> {
    match source {
        MachineIdSource::StoredCredential => {
            normalize_machine_id(credentials.machine_id.as_deref()?)
        }
        MachineIdSource::DerivedApiKey => credentials
            .kiro_api_key
            .as_deref()
            .filter(|value| !value.is_empty())
            .map(|value| sha256_hex(&format!("KiroAPIKey/{value}"))),
        MachineIdSource::DerivedRefreshToken => credentials
            .refresh_token
            .as_deref()
            .filter(|value| !value.is_empty())
            .map(|value| sha256_hex(&format!("KotlinNativeAPI/{value}"))),
        MachineIdSource::GlobalFallback => config
            .global_machine_id_fallback_enabled
            .then(|| config.machine_id.as_deref().and_then(normalize_machine_id))
            .flatten(),
        MachineIdSource::DeterministicFallback => {
            Some(deterministic_fallback_machine_id(credentials))
        }
    }
}

/// 补齐并持久化前可写入凭据的身份字段。
///
/// 返回值表示凭据是否发生变化。已有 machineId 不会被静默替换；只有缺失或
/// 格式非法时才生成新值。这样管理员显式配置的身份仍然拥有最高优先级。
pub fn ensure_identity_fields(credentials: &mut KiroCredentials, config: &Config) -> bool {
    let mut changed = false;
    let resolution = resolve_from_credentials(credentials, config);
    if credentials.machine_id.as_deref() != Some(resolution.machine_id.as_str()) {
        credentials.machine_id = Some(resolution.machine_id);
        changed = true;
    }
    if credentials.machine_id_source.as_deref() != Some(resolution.source.as_str()) {
        credentials.machine_id_source = Some(resolution.source.as_str().to_string());
        changed = true;
    }
    if credentials.fingerprint_version.as_deref() != Some(MACHINE_ID_DERIVATION_VERSION) {
        credentials.fingerprint_version = Some(MACHINE_ID_DERIVATION_VERSION.to_string());
        changed = true;
    }
    changed
}

/// 判断旧版本是否把全局 machineId 持久化到了当前账号。
///
/// 旧数据没有 `machineIdSource` 标记，因此只有在值与全局 machineId 完全一致、
/// 且管理员没有显式开启兼容开关时才迁移。已有来源标记的账号不被改写。
pub fn is_legacy_global_machine_id(credentials: &KiroCredentials, config: &Config) -> bool {
    if config.global_machine_id_fallback_enabled {
        return false;
    }
    if credentials.machine_id_source.as_deref() == Some("global_fallback") {
        return true;
    }
    if credentials.machine_id_source.is_some() {
        return false;
    }
    let Some(global) = config.machine_id.as_deref().and_then(normalize_machine_id) else {
        return false;
    };
    credentials
        .machine_id
        .as_deref()
        .and_then(normalize_machine_id)
        .is_some_and(|machine_id| machine_id == global)
}

/// 为缺失派生材料的凭据生成确定性 fallback machineId。
///
/// 优先使用数据库分配的账号 ID；无 ID 的临时凭据再使用稳定元数据。这里不把
/// access token、refresh token 或 API key 写入 seed，避免日志/调试时意外传播敏感材料。
fn deterministic_fallback_machine_id(credentials: &KiroCredentials) -> String {
    let mut hasher = Sha256::new();
    hasher.update(MACHINE_ID_DERIVATION_VERSION.as_bytes());
    hasher.update([0]);
    if let Some(id) = credentials.id {
        hasher.update(b"id");
        hasher.update(id.to_be_bytes());
    } else {
        hasher.update(b"metadata");
        for (label, value) in [
            ("auth_method", credentials.auth_method.as_deref()),
            ("provider", credentials.provider.as_deref()),
            ("client_id", credentials.client_id.as_deref()),
            ("token_endpoint", credentials.token_endpoint.as_deref()),
            ("issuer_url", credentials.issuer_url.as_deref()),
            ("region", credentials.region.as_deref()),
            ("auth_region", credentials.auth_region.as_deref()),
            ("api_region", credentials.api_region.as_deref()),
            ("endpoint", credentials.endpoint.as_deref()),
            ("email", credentials.email.as_deref()),
        ] {
            hasher.update((label.len() as u64).to_be_bytes());
            hasher.update(label.as_bytes());
            match value.map(str::trim).filter(|value| !value.is_empty()) {
                Some(value) => {
                    hasher.update([1]);
                    hasher.update((value.len() as u64).to_be_bytes());
                    hasher.update(value.as_bytes());
                }
                None => hasher.update([0]),
            }
        }
        if let Some(proxy_resource_id) = credentials.proxy_resource_id {
            hasher.update(b"proxy_resource_id");
            hasher.update(proxy_resource_id.to_be_bytes());
        }
    }

    tracing::warn!(
        credential_id = ?credentials.id,
        fallback_source = "deterministic_account_identity",
        "凭据缺少派生材料（kiroApiKey/refreshToken 均不可用），使用确定性账号 fallback machineId"
    );
    let digest = hasher.finalize();
    hex::encode(digest)
}

/// SHA256 哈希实现（返回十六进制字符串）
fn sha256_hex(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    let result = hasher.finalize();
    hex::encode(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sha256_hex() {
        let result = sha256_hex("test");
        assert_eq!(result.len(), 64);
        assert_eq!(
            result,
            "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"
        );
    }

    #[test]
    fn test_global_machine_id_requires_explicit_compatibility_switch() {
        let credentials = KiroCredentials::default();
        let mut config = Config::default();
        config.machine_id = Some("a".repeat(64));

        let result = generate_from_credentials(&credentials, &config);
        assert_ne!(result, "a".repeat(64));

        config.global_machine_id_fallback_enabled = true;
        let result = generate_from_credentials(&credentials, &config);
        assert_eq!(result, "a".repeat(64));
    }

    #[test]
    fn test_generate_with_credential_machine_id_overrides_config() {
        let mut credentials = KiroCredentials::default();
        credentials.machine_id = Some("b".repeat(64));

        let mut config = Config::default();
        config.machine_id = Some("a".repeat(64));

        let result = generate_from_credentials(&credentials, &config);
        assert_eq!(result, "b".repeat(64));
    }

    #[test]
    fn test_generate_with_refresh_token() {
        let mut credentials = KiroCredentials::default();
        credentials.refresh_token = Some("test_refresh_token".to_string());
        let config = Config::default();

        let result = generate_from_credentials(&credentials, &config);
        assert_eq!(result.len(), 64);
    }

    #[test]
    fn test_generate_without_credentials_uses_fallback() {
        // 完全空凭据会走确定性兜底分支。
        let credentials = KiroCredentials::default();
        let config = Config::default();

        let result = generate_from_credentials(&credentials, &config);
        let second = generate_from_credentials(&credentials, &config);
        assert_eq!(result.len(), 64);
        assert!(result.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(result, second);
    }

    #[test]
    fn test_generate_with_api_key() {
        let mut credentials = KiroCredentials::default();
        credentials.kiro_api_key = Some("ksk_test_api_key".to_string());
        let config = Config::default();

        let result = generate_from_credentials(&credentials, &config);
        assert_eq!(result.len(), 64);
        // 应与 KiroAPIKey/<api_key> 的哈希一致
        assert_eq!(result, sha256_hex("KiroAPIKey/ksk_test_api_key"));
    }

    #[test]
    fn test_api_key_and_refresh_token_are_mutually_exclusive() {
        // 同时存在 kiroApiKey 和 refreshToken 时，应走 API Key 分支
        let mut credentials = KiroCredentials::default();
        credentials.kiro_api_key = Some("ksk_test".to_string());
        credentials.refresh_token = Some("should_not_be_used".to_string());
        let config = Config::default();

        let result = generate_from_credentials(&credentials, &config);
        assert_eq!(result, sha256_hex("KiroAPIKey/ksk_test"));
    }

    #[test]
    fn test_api_key_auth_method_empty_uses_fallback_not_refresh_token() {
        // auth_method=api_key 但 kiro_api_key 为空：不回落到 refreshToken，走兜底分支
        let mut credentials = KiroCredentials::default();
        credentials.id = Some(u64::MAX - 1);
        credentials.auth_method = Some("api_key".to_string());
        credentials.refresh_token = Some("should_not_be_used".to_string());
        let config = Config::default();

        let result = generate_from_credentials(&credentials, &config);
        assert_eq!(result.len(), 64);
        // 必须不是基于 refresh_token 派生的值（互斥性验证）
        assert_ne!(result, sha256_hex("KotlinNativeAPI/should_not_be_used"));
    }

    #[test]
    fn test_fallback_is_stable_per_credential() {
        // 同一凭据（按 id 区分）多次调用兜底应返回同一值
        let mut credentials = KiroCredentials::default();
        credentials.id = Some(u64::MAX - 10);
        let config = Config::default();

        let first = generate_from_credentials(&credentials, &config);
        let second = generate_from_credentials(&credentials, &config);
        assert_eq!(first, second);
    }

    #[test]
    fn test_fallback_differs_across_credentials() {
        // 不同凭据（不同 id）的兜底值应互不相同
        let mut cred_a = KiroCredentials::default();
        cred_a.id = Some(u64::MAX - 20);
        let mut cred_b = KiroCredentials::default();
        cred_b.id = Some(u64::MAX - 21);
        let config = Config::default();

        let id_a = generate_from_credentials(&cred_a, &config);
        let id_b = generate_from_credentials(&cred_b, &config);
        assert_ne!(id_a, id_b);
    }

    #[test]
    fn test_resolution_reports_account_material_source() {
        let mut credentials = KiroCredentials::default();
        credentials.id = Some(42);
        credentials.refresh_token = Some("refresh".to_string());
        let resolution = resolve_from_credentials(&credentials, &Config::default());
        assert_eq!(resolution.source, MachineIdSource::DerivedRefreshToken);

        credentials.refresh_token = None;
        let resolution = resolve_from_credentials(&credentials, &Config::default());
        assert_eq!(resolution.source, MachineIdSource::DeterministicFallback);
    }

    #[test]
    fn legacy_persisted_derived_machine_id_recovers_its_source() {
        let mut credentials = KiroCredentials::default();
        credentials.refresh_token = Some("refresh".to_string());
        credentials.machine_id = Some(sha256_hex("KotlinNativeAPI/refresh"));
        let resolution = resolve_from_credentials(&credentials, &Config::default());
        assert_eq!(resolution.source, MachineIdSource::DerivedRefreshToken);
    }

    #[test]
    fn persisted_account_machine_id_survives_refresh_token_rotation() {
        let mut credentials = KiroCredentials::default();
        credentials.id = Some(42);
        credentials.refresh_token = Some("old-refresh".to_string());
        ensure_identity_fields(&mut credentials, &Config::default());
        let original = credentials.machine_id.clone().unwrap();
        credentials.refresh_token = Some("new-refresh".to_string());

        let resolution = resolve_from_credentials(&credentials, &Config::default());
        assert_eq!(resolution.machine_id, original);
        assert_eq!(resolution.source, MachineIdSource::DerivedRefreshToken);
        assert_ne!(
            resolution.machine_id,
            sha256_hex("KotlinNativeAPI/new-refresh")
        );
    }

    #[test]
    fn persisted_account_machine_id_survives_api_key_rotation() {
        let mut credentials = KiroCredentials::default();
        credentials.id = Some(43);
        credentials.auth_method = Some("api_key".to_string());
        credentials.kiro_api_key = Some("old-api-key".to_string());
        ensure_identity_fields(&mut credentials, &Config::default());
        let original = credentials.machine_id.clone().unwrap();
        credentials.kiro_api_key = Some("new-api-key".to_string());

        let resolution = resolve_from_credentials(&credentials, &Config::default());
        assert_eq!(resolution.machine_id, original);
        assert_eq!(resolution.source, MachineIdSource::DerivedApiKey);
        assert_ne!(resolution.machine_id, sha256_hex("KiroAPIKey/new-api-key"));
    }

    #[test]
    fn legacy_global_machine_id_is_detected_only_without_source_marker() {
        let mut credentials = KiroCredentials::default();
        let mut config = Config::default();
        config.machine_id = Some("a".repeat(64));
        credentials.machine_id = Some("a".repeat(64));
        assert!(is_legacy_global_machine_id(&credentials, &config));

        credentials.machine_id_source = Some("stored_credential".to_string());
        assert!(!is_legacy_global_machine_id(&credentials, &config));
    }

    #[test]
    fn disabling_global_fallback_migrates_persisted_global_identity() {
        let mut credentials = KiroCredentials::default();
        credentials.id = Some(7);
        credentials.machine_id = Some("a".repeat(64));
        credentials.machine_id_source = Some("global_fallback".to_string());

        let mut config = Config::default();
        config.machine_id = Some("a".repeat(64));
        config.global_machine_id_fallback_enabled = false;

        assert!(is_legacy_global_machine_id(&credentials, &config));
        credentials.machine_id = None;
        let changed = ensure_identity_fields(&mut credentials, &config);
        assert!(changed);
        assert_ne!(
            credentials.machine_id.as_deref(),
            Some("a".repeat(64).as_str())
        );
        assert_eq!(
            credentials.machine_id_source.as_deref(),
            Some("deterministic_fallback")
        );
    }

    #[test]
    fn test_fallback_is_not_shared_for_missing_id_when_stable_metadata_differs() {
        let mut first = KiroCredentials::default();
        first.email = Some("first@example.invalid".to_string());
        let mut second = KiroCredentials::default();
        second.email = Some("second@example.invalid".to_string());
        let config = Config::default();

        assert_ne!(
            generate_from_credentials(&first, &config),
            generate_from_credentials(&second, &config)
        );
    }

    #[test]
    fn test_normalize_uuid_format() {
        // UUID 格式应该被转换为 64 字符
        let uuid = "2582956e-cc88-4669-b546-07adbffcb894";
        let result = normalize_machine_id(uuid);
        assert!(result.is_some());
        let normalized = result.unwrap();
        assert_eq!(normalized.len(), 64);
        // UUID 去掉连字符后重复一次
        assert_eq!(
            normalized,
            "2582956ecc884669b54607adbffcb8942582956ecc884669b54607adbffcb894"
        );
    }

    #[test]
    fn test_normalize_64_char_hex() {
        // 64 字符十六进制应该直接返回
        let hex64 = "a".repeat(64);
        let result = normalize_machine_id(&hex64);
        assert_eq!(result, Some(hex64));
    }

    #[test]
    fn test_normalize_invalid_format() {
        // 无效格式应该返回 None
        assert!(normalize_machine_id("invalid").is_none());
        assert!(normalize_machine_id("too-short").is_none());
        assert!(normalize_machine_id(&"g".repeat(64)).is_none()); // 非十六进制
    }

    #[test]
    fn test_generate_with_uuid_machine_id() {
        let mut credentials = KiroCredentials::default();
        credentials.machine_id = Some("2582956e-cc88-4669-b546-07adbffcb894".to_string());

        let config = Config::default();

        let result = generate_from_credentials(&credentials, &config);
        assert_eq!(result.len(), 64);
    }
}
