use std::collections::{HashMap, HashSet};
use std::time::Instant;

use crate::kiro::model::credentials::KiroCredentials;

use super::account_state::{CredentialEntry, ProxyResourceAvailability, ProxyResourceRuntime};
use super::cooldown::entry_cooldown_remaining;
use super::rpm::entry_rate_limit_remaining;

pub(super) fn is_opus_model(model: Option<&str>) -> bool {
    model
        .map(|m| m.to_lowercase().contains("opus"))
        .unwrap_or(false)
}

pub(super) fn credential_is_usable_for_model(entry: &CredentialEntry, model: Option<&str>) -> bool {
    if entry.disabled {
        return false;
    }
    if is_opus_model(model) && !entry.credentials.supports_opus() {
        return false;
    }
    true
}

pub(super) fn credential_is_dispatchable(
    proxy_resources: &HashMap<u64, ProxyResourceRuntime>,
    entry: &CredentialEntry,
    model: Option<&str>,
    now: Instant,
    max_concurrent_requests: u32,
    global_rpm: u32,
) -> bool {
    credential_is_usable_for_model(entry, model)
        && credential_proxy_is_dispatchable(&entry.credentials, proxy_resources)
        && entry_cooldown_remaining(entry, model, now).is_none()
        && entry_rate_limit_remaining(entry, global_rpm, now).is_none()
        && entry_has_concurrency_capacity(entry, max_concurrent_requests)
}

pub(super) fn credential_is_temporarily_available(
    proxy_resources: &HashMap<u64, ProxyResourceRuntime>,
    entry: &CredentialEntry,
    model: Option<&str>,
    now: Instant,
    global_rpm: u32,
) -> bool {
    credential_is_usable_for_model(entry, model)
        && credential_proxy_is_dispatchable(&entry.credentials, proxy_resources)
        && entry_cooldown_remaining(entry, model, now).is_none()
        && entry_rate_limit_remaining(entry, global_rpm, now).is_none()
}

pub(super) fn credential_is_dispatch_candidate(
    proxy_resources: &HashMap<u64, ProxyResourceRuntime>,
    entry: &CredentialEntry,
    model: Option<&str>,
    excluded_ids: &HashSet<u64>,
) -> bool {
    !excluded_ids.contains(&entry.id)
        && credential_is_usable_for_model(entry, model)
        && credential_proxy_is_dispatchable(&entry.credentials, proxy_resources)
}

pub(super) fn credential_proxy_availability(
    credentials: &KiroCredentials,
    proxy_resources: &HashMap<u64, ProxyResourceRuntime>,
) -> Option<ProxyResourceAvailability> {
    if credentials.proxy_url.is_some() {
        return None;
    }
    let resource_id = credentials.proxy_resource_id?;
    let Some(resource) = proxy_resources.get(&resource_id) else {
        return Some(ProxyResourceAvailability::Missing(resource_id));
    };
    if !resource.enabled {
        return Some(ProxyResourceAvailability::Disabled(resource.clone()));
    }
    Some(ProxyResourceAvailability::Available(resource.clone()))
}

pub(super) fn credential_proxy_is_dispatchable(
    credentials: &KiroCredentials,
    proxy_resources: &HashMap<u64, ProxyResourceRuntime>,
) -> bool {
    match credential_proxy_availability(credentials, proxy_resources) {
        Some(ProxyResourceAvailability::Missing(_))
        | Some(ProxyResourceAvailability::Disabled(_)) => false,
        Some(ProxyResourceAvailability::Available(_)) | None => true,
    }
}

pub(super) fn proxy_unavailable_error(
    credential_id: Option<u64>,
    availability: ProxyResourceAvailability,
) -> anyhow::Error {
    match availability {
        ProxyResourceAvailability::Missing(resource_id) => anyhow::anyhow!(
            "凭据 #{} 绑定的代理资源 #{} 不存在，已阻止回退到全局代理/直连",
            credential_id.unwrap_or_default(),
            resource_id
        ),
        ProxyResourceAvailability::Disabled(resource) => anyhow::anyhow!(
            "凭据 #{} 绑定的代理资源「{}」已禁用，已阻止回退到全局代理/直连",
            credential_id.unwrap_or_default(),
            resource.name
        ),
        ProxyResourceAvailability::Available(_) => {
            anyhow::anyhow!("代理资源可用状态异常")
        }
    }
}

pub(super) fn effective_max_concurrent_requests(
    entry: &CredentialEntry,
    global_max_concurrent_requests: u32,
) -> u32 {
    entry
        .credentials
        .max_concurrent_requests
        .unwrap_or(global_max_concurrent_requests)
}

pub(super) fn entry_has_concurrency_capacity(
    entry: &CredentialEntry,
    global_max_concurrent_requests: u32,
) -> bool {
    let max_concurrent_requests =
        effective_max_concurrent_requests(entry, global_max_concurrent_requests);
    max_concurrent_requests == 0 || entry.in_flight_requests < max_concurrent_requests
}

pub(super) fn global_has_concurrency_capacity(global_in_flight: u32, global_max: u32) -> bool {
    global_max == 0 || global_in_flight < global_max
}
