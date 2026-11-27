use std::collections::HashMap;

use serde_json::Value;

use crate::anthropic::usage::UsageRecord;

fn sample_id(sample: &Value) -> Option<u64> {
    sample
        .get("accountId")
        .or_else(|| sample.get("poolId"))
        .and_then(Value::as_u64)
}

fn sample_name(sample: &Value) -> Option<&str> {
    ["accountLabel", "accountName", "poolName"]
        .into_iter()
        .filter_map(|key| sample.get(key).and_then(Value::as_str))
        .find(|name| !name.trim().is_empty())
}

fn samples(record: &UsageRecord) -> impl Iterator<Item = &Value> {
    record
        .error_metadata
        .as_ref()
        .and_then(|metadata| metadata.pointer("/selectionFailure/sampledAccounts"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
}

pub(super) fn collect_names(record: &UsageRecord, names: &mut HashMap<u64, String>) {
    if let Some(id) = record.account_id
        && let Some(name) = record
            .account_name
            .as_ref()
            .filter(|name| !name.trim().is_empty())
    {
        names.entry(id).or_insert_with(|| name.clone());
    }
    for attempt in &record.external_attempts {
        if !attempt.pool_name.trim().is_empty() {
            names
                .entry(attempt.pool_id)
                .or_insert_with(|| attempt.pool_name.clone());
        }
    }
    for sample in samples(record) {
        if let Some(id) = sample_id(sample)
            && let Some(name) = sample_name(sample)
        {
            names.entry(id).or_insert_with(|| name.to_string());
        }
    }
}

pub(super) fn needs_names(record: &UsageRecord, names: &HashMap<u64, String>) -> bool {
    record.account_id.is_some_and(|id| {
        record
            .account_name
            .as_deref()
            .is_none_or(|name| name.trim().is_empty())
            && !names.contains_key(&id)
    }) || record
        .external_attempts
        .iter()
        .any(|attempt| attempt.pool_name.trim().is_empty() && !names.contains_key(&attempt.pool_id))
        || samples(record).any(|sample| {
            sample_name(sample).is_none()
                && sample_id(sample).is_some_and(|id| !names.contains_key(&id))
        })
}

pub(super) fn enrich_names(record: &mut UsageRecord, names: &HashMap<u64, String>) {
    if let Some(id) = record.account_id
        && record
            .account_name
            .as_deref()
            .is_none_or(|name| name.trim().is_empty())
    {
        record.account_name = names.get(&id).cloned();
    }
    for attempt in &mut record.external_attempts {
        if attempt.pool_name.trim().is_empty()
            && let Some(name) = names.get(&attempt.pool_id)
        {
            attempt.pool_name.clone_from(name);
        }
    }
    if let Some(samples) = record
        .error_metadata
        .as_mut()
        .and_then(|metadata| metadata.pointer_mut("/selectionFailure/sampledAccounts"))
        .and_then(Value::as_array_mut)
    {
        for sample in samples {
            if sample_name(sample).is_some() {
                continue;
            }
            if let Some(name) = sample_id(sample).and_then(|id| names.get(&id))
                && let Some(object) = sample.as_object_mut()
            {
                object.insert("accountLabel".to_string(), name.clone().into());
                object.insert("poolName".to_string(), name.clone().into());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn historical_error() -> UsageRecord {
        serde_json::from_value(json!({
            "id": "historical-error", "createdAt": "2026-11-27T00:00:00Z",
            "endpoint": "/v1/messages", "stream": true, "model": "claude-sonnet-4-5",
            "status": "error", "usageSource": "none", "totalInputTokens": 0,
            "compatInputTokens": 0, "billableInputTokens": 0, "outputTokens": 0,
            "cacheReadInputTokens": 0, "cacheCreationInputTokens": 0,
            "cacheCreation5mInputTokens": 0, "cacheCreation1hInputTokens": 0,
            "durationMs": 1, "simulated": false, "accountId": 23,
            "externalAttempts": [
                {"attempt": 1, "poolId": 17, "poolName": "deleted-account", "action": "retry", "durationMs": 1},
                {"attempt": 2, "poolId": 23, "poolName": "", "action": "fail", "durationMs": 1}
            ],
            "errorMetadata": {"selectionFailure": {"sampledAccounts": [
                {"accountId": 23, "reason": "cooldown"},
                {"poolId": 17, "reason": "disabled"}
            ]}}
        })).unwrap()
    }

    #[test]
    fn historical_errors_fill_attempt_and_sample_names_without_replacing_saved_names() {
        let mut record = historical_error();
        let mut names = HashMap::new();
        collect_names(&record, &mut names);
        assert!(needs_names(&record, &names));
        names.insert(23, "bob@example.com".to_string());
        assert!(!needs_names(&record, &names));
        enrich_names(&mut record, &names);
        assert_eq!(record.account_name.as_deref(), Some("bob@example.com"));
        assert_eq!(record.external_attempts[0].pool_name, "deleted-account");
        assert_eq!(record.external_attempts[1].pool_name, "bob@example.com");
        let metadata = record.error_metadata.unwrap();
        assert_eq!(
            metadata["selectionFailure"]["sampledAccounts"][0]["accountLabel"],
            "bob@example.com"
        );
        assert_eq!(
            metadata["selectionFailure"]["sampledAccounts"][1]["poolName"],
            "deleted-account"
        );
    }
}
