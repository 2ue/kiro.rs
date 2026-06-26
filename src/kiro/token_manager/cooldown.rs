use std::time::{Duration as StdDuration, Instant};

use super::account_state::{CredentialEntry, CredentialModelCooldown};
use super::admin_snapshot::CredentialCooldownSnapshot;

pub(super) fn model_state_key(model: &str) -> String {
    model.trim().to_ascii_lowercase()
}

pub(super) fn entry_global_cooldown_remaining(
    entry: &CredentialEntry,
    now: Instant,
) -> Option<StdDuration> {
    entry
        .cooldown_until
        .and_then(|until| until.checked_duration_since(now))
}

pub(super) fn entry_model_cooldown<'a>(
    entry: &'a CredentialEntry,
    model: Option<&str>,
) -> Option<&'a CredentialModelCooldown> {
    model
        .map(model_state_key)
        .and_then(|key| entry.model_cooldowns.get(&key))
}

pub(super) fn entry_model_cooldown_remaining(
    entry: &CredentialEntry,
    model: Option<&str>,
    now: Instant,
) -> Option<StdDuration> {
    entry_model_cooldown(entry, model)
        .and_then(|cooldown| cooldown.until.checked_duration_since(now))
}

pub(super) fn entry_cooldown_remaining(
    entry: &CredentialEntry,
    model: Option<&str>,
    now: Instant,
) -> Option<StdDuration> {
    match (
        entry_global_cooldown_remaining(entry, now),
        entry_model_cooldown_remaining(entry, model, now),
    ) {
        (Some(global), Some(model)) => Some(global.max(model)),
        (Some(global), None) => Some(global),
        (None, Some(model)) => Some(model),
        (None, None) => None,
    }
}

pub(super) fn entry_any_cooldown_remaining(
    entry: &CredentialEntry,
    now: Instant,
) -> Option<StdDuration> {
    entry
        .model_cooldowns
        .values()
        .filter_map(|cooldown| cooldown.until.checked_duration_since(now))
        .chain(entry_global_cooldown_remaining(entry, now))
        .max()
}

pub(super) fn entry_cooldown_snapshots(
    entry: &CredentialEntry,
    now: Instant,
) -> Vec<CredentialCooldownSnapshot> {
    let mut cooldowns = Vec::new();
    if let Some(remaining) = entry_global_cooldown_remaining(entry, now) {
        cooldowns.push(CredentialCooldownSnapshot {
            model: None,
            global: true,
            remaining_secs: remaining.as_secs().saturating_add(1),
            reason: entry.cooldown_reason.clone(),
        });
    }
    let mut model_cooldowns: Vec<_> = entry.model_cooldowns.values().collect();
    model_cooldowns.sort_by(|left, right| left.model.cmp(&right.model));
    for cooldown in model_cooldowns {
        if let Some(remaining) = cooldown.until.checked_duration_since(now) {
            cooldowns.push(CredentialCooldownSnapshot {
                model: Some(cooldown.model.clone()),
                global: false,
                remaining_secs: remaining.as_secs().saturating_add(1),
                reason: cooldown.reason.clone(),
            });
        }
    }
    cooldowns
}
