//! 公共认证工具函数

use std::collections::HashSet;
use std::sync::Arc;

use axum::{
    body::Body,
    http::{Request, header},
};
use parking_lot::RwLock;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// 已认证的下游请求 API Key 身份。
///
/// 这里只保留 SHA-256 digest。限流、日志关联和请求扩展都不得保存原始 key。
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct RequestApiKeyIdentity {
    digest: [u8; 32],
}

impl RequestApiKeyIdentity {
    pub(crate) fn digest(self) -> [u8; 32] {
        self.digest
    }

    /// 可稳定关联同一个请求渠道、但不可还原原始 key 的 ID。
    ///
    /// 使用完整 SHA-256，避免短摘要碰撞，并与管理端展示的 key ID 保持一致。
    pub fn stable_id(self) -> String {
        hex::encode(self.digest)
    }
}

impl std::fmt::Debug for RequestApiKeyIdentity {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RequestApiKeyIdentity")
            .field("stable_id", &self.stable_id())
            .finish()
    }
}

/// 从请求中提取 API Key
///
/// 支持两种认证方式：
/// - `x-api-key` header
/// - `Authorization: Bearer <token>` header
pub fn extract_api_key(request: &Request<Body>) -> Option<String> {
    // 优先检查 x-api-key
    if let Some(key) = request
        .headers()
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
    {
        return Some(key.to_string());
    }

    // 其次检查 Authorization: Bearer
    request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|s| s.to_string())
}

/// 常量时间字符串比较，防止时序攻击
///
/// 无论字符串内容如何，比较所需的时间都是恒定的，
/// 这可以防止攻击者通过测量响应时间来猜测 API Key。
///
/// 使用经过安全审计的 `subtle` crate 实现
pub fn constant_time_eq(a: &str, b: &str) -> bool {
    a.as_bytes().ct_eq(b.as_bytes()).into()
}

fn api_key_digest(key: &str) -> [u8; 32] {
    let digest = Sha256::digest(key.as_bytes());
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    out
}

/// 返回请求 API Key 的规范化稳定 ID。原始 key 不会被保留。
pub fn request_api_key_id(key: &str) -> String {
    hex::encode(api_key_digest(key.trim()))
}

fn api_key_hashes(keys: impl IntoIterator<Item = impl AsRef<str>>) -> HashSet<[u8; 32]> {
    keys.into_iter()
        .map(|key| key.as_ref().trim().to_string())
        .filter(|key| !key.is_empty())
        .map(|key| api_key_digest(&key))
        .collect()
}

/// 请求 API Key 内存索引。
///
/// 运行时请求鉴权只做一次 SHA-256 和一次内存 HashSet 查询，不访问 PgSQL/Redis。
/// PgSQL 是配置事实源；Redis 只广播配置变更通知。
#[derive(Debug, Clone, Default)]
pub struct RequestApiKeyStore {
    hashes: Arc<RwLock<HashSet<[u8; 32]>>>,
}

impl RequestApiKeyStore {
    pub fn new(keys: impl IntoIterator<Item = impl AsRef<str>>) -> Self {
        Self {
            hashes: Arc::new(RwLock::new(api_key_hashes(keys))),
        }
    }

    pub fn replace_keys(&self, keys: impl IntoIterator<Item = impl AsRef<str>>) {
        *self.hashes.write() = api_key_hashes(keys);
    }

    #[cfg(test)]
    pub fn contains(&self, key: &str) -> bool {
        self.authenticate(key).is_some()
    }

    pub fn authenticate(&self, key: &str) -> Option<RequestApiKeyIdentity> {
        let digest = api_key_digest(key.trim());
        self.hashes
            .read()
            .contains(&digest)
            .then_some(RequestApiKeyIdentity { digest })
    }

    pub fn len(&self) -> usize {
        self.hashes.read().len()
    }
}

#[cfg(test)]
mod tests {
    use super::{RequestApiKeyStore, request_api_key_id};

    #[test]
    fn request_api_key_store_supports_multiple_keys() {
        let store = RequestApiKeyStore::new(["sk-one", "sk-two"]);

        assert!(store.contains("sk-one"));
        assert!(store.contains("sk-two"));
        assert!(!store.contains("sk-three"));
    }

    #[test]
    fn request_api_key_store_replace_keys_updates_lookup_without_db() {
        let store = RequestApiKeyStore::new(["sk-old"]);
        assert!(store.contains("sk-old"));

        store.replace_keys(["sk-new", "sk-extra"]);

        assert!(!store.contains("sk-old"));
        assert!(store.contains("sk-new"));
        assert!(store.contains("sk-extra"));
    }

    #[test]
    fn authenticated_identity_is_stable_and_never_formats_plaintext() {
        let store = RequestApiKeyStore::new(["sk-super-secret"]);

        let first = store.authenticate("sk-super-secret").unwrap();
        let second = store.authenticate(" sk-super-secret ").unwrap();

        assert_eq!(first, second);
        assert_eq!(first.stable_id().len(), 64);
        assert_eq!(first.stable_id(), request_api_key_id(" sk-super-secret "));
        assert!(!format!("{first:?}").contains("sk-super-secret"));
        assert!(store.authenticate("sk-other").is_none());
    }
}
