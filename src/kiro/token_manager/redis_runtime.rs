use chrono::Utc;

use std::sync::Arc;

use crate::storage::postgres::PostgresStore;
use crate::storage::redis_cache::RedisStore;

use super::storage_task::spawn_best_effort_storage_task;

fn runtime_event_payload(kind: &str, version: Option<i64>, reason: &str) -> String {
    serde_json::json!({
        "kind": kind,
        "version": version,
        "reason": reason,
        "changedAt": Utc::now().to_rfc3339(),
    })
    .to_string()
}

pub(super) fn publish_runtime_config_changed(
    redis_store: Option<&Arc<RedisStore>>,
    version: Option<i64>,
    reason: &str,
) {
    let Some(redis) = redis_store else {
        return;
    };
    let redis = redis.clone();
    let payload = runtime_event_payload("runtime_config_changed", version, reason);
    spawn_best_effort_storage_task("发布 Redis 运行配置变更通知", async move {
        redis.publish_runtime_config_changed(payload).await
    });
}

pub(super) fn publish_credentials_changed(
    postgres_store: Option<&Arc<PostgresStore>>,
    redis_store: Option<&Arc<RedisStore>>,
    reason: &str,
) {
    if let Some(store) = postgres_store {
        let store = store.clone();
        let reason_owned = reason.to_string();
        spawn_best_effort_storage_task("记录凭据事件到 PgSQL", async move {
            store
                .record_credential_event(
                    None,
                    "credentials_changed",
                    Some(&reason_owned),
                    serde_json::json!({ "reason": reason_owned }),
                )
                .await
        });
    }
    let Some(redis) = redis_store else {
        return;
    };
    let redis = redis.clone();
    let payload = runtime_event_payload("credentials_changed", None, reason);
    spawn_best_effort_storage_task("发布 Redis 凭据变更通知", async move {
        redis.publish_credentials_changed(payload).await
    });
}
