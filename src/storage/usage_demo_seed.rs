use std::sync::Arc;

use anyhow::Context;
use chrono::{Datelike, Duration, FixedOffset, Timelike, Utc};
use fastrand::Rng;
use serde_json::json;
use sqlx::Row;

use crate::anthropic::usage::{
    ExternalPoolBilling, ExternalPoolUsageSnapshot, UsageRecord, UsageRecordStatus, UsageRouteKind,
    UsageRouteSubtype, UsageSource,
};
use crate::external_pool::CreateExternalPoolRequest;
use crate::kiro::model::credentials::KiroCredentials;

use super::postgres::{CredentialAccountInfoRow, PostgresStore, PostgresUsageStore};

const DEMO_DATABASE_PREFIX: &str = "kiro_overview_demo_";
const DEMO_ACCOUNT_PREFIX: &str = "demo-acc-";
const DEMO_POOL_NAMES: [&str; 3] = ["demo-pool-alpha", "demo-pool-beta", "demo-pool-gamma"];
const DEMO_MODELS: [&str; 3] = ["claude-opus-5-5", "claude-sonnet-5", "claude-haiku-4-5"];
const DEMO_ENDPOINTS: [&str; 4] = [
    "/cc/v1/messages",
    "/v1/messages",
    "/v1/count_tokens",
    "/api/messages",
];
const DEMO_REQUEST_KEYS: [&str; 3] = ["demo-client-alpha", "demo-client-beta", "demo-client-gamma"];
const DEMO_BATCH_SIZE: usize = 500;

#[derive(Debug, Clone)]
struct DemoPool {
    id: u64,
    name: String,
    markup: f64,
    floor_probability: usize,
}

#[derive(Debug, Clone)]
pub struct UsageDemoSeedReport {
    pub database: String,
    pub days: u32,
    pub records: usize,
    pub local_records: usize,
    pub external_records: usize,
    pub accounts: usize,
    pub pools: usize,
}

pub async fn seed_usage_demo(
    store: &PostgresStore,
    days: u32,
    seed: u64,
    reset: bool,
) -> anyhow::Result<UsageDemoSeedReport> {
    let database: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(store.pool())
        .await?;
    if !database.starts_with(DEMO_DATABASE_PREFIX) {
        anyhow::bail!(
            "usage demo seed 只允许运行在数据库名以 {} 开头的隔离数据库（当前为 {}）",
            DEMO_DATABASE_PREFIX,
            database
        );
    }

    let existing_usage: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM usage_records WHERE deleted_at IS NULL")
            .fetch_one(store.pool())
            .await?;
    if existing_usage > 0 && !reset {
        anyhow::bail!(
            "demo 数据库已有 {} 条 usage 记录；如需重建请显式传 --reset",
            existing_usage
        );
    }
    if reset {
        clear_usage_tables(store).await?;
    }

    let accounts = ensure_demo_accounts(store).await?;
    seed_demo_account_info(store, &accounts).await?;
    let pools = ensure_demo_pools(store).await?;
    let usage = PostgresUsageStore::new(Arc::new(store.clone()));
    let now = Utc::now();
    let days = days.clamp(1, 30);
    let local_offset = FixedOffset::east_opt(8 * 60 * 60).expect("UTC+8 is a valid fixed offset");
    let mut rng = Rng::with_seed(seed);
    let mut batch = Vec::with_capacity(DEMO_BATCH_SIZE);
    let mut total = 0usize;
    let mut local_records = 0usize;
    let mut external_records = 0usize;

    for day in 0..days {
        let day_records = 1_100 + rng.usize(0..350);
        for _ in 0..day_records {
            let offset_secs = rng.i64(0..86_400);
            let created_at =
                now - Duration::days(i64::from(days - day)) + Duration::seconds(offset_secs);
            if created_at >= now - Duration::minutes(2) {
                continue;
            }
            let local_time = created_at.with_timezone(&local_offset);
            let hour = local_time.hour();
            let is_weekend = matches!(
                local_time.weekday(),
                chrono::Weekday::Sat | chrono::Weekday::Sun
            );
            let weekend_factor = if is_weekend { 0.7 } else { 1.0 };
            let external_ratio = if day == 2 && (11..=15).contains(&hour) {
                0.60
            } else {
                0.28
            };
            let is_external = rng.f64() < external_ratio;
            let model = choose_model(&mut rng);
            let input_tokens = 24_000 + rng.i32(0..96_000);
            let output_tokens = 600 + rng.i32(0..3_000);
            let cache_read = if rng.usize(0..100) < 18 {
                input_tokens / 3
            } else {
                0
            };
            let cache_creation = if cache_read == 0 && rng.usize(0..100) < 9 {
                input_tokens / 10
            } else {
                0
            };
            let total_input_tokens = input_tokens;
            let billable_input_tokens = (input_tokens - cache_read - cache_creation).max(0);
            let base_cost = estimate_cost(
                model,
                billable_input_tokens,
                output_tokens,
                cache_read,
                cache_creation,
            ) * weekend_factor;
            let error_spike = day == 4 && (14..=15).contains(&hour) && !is_external;
            let status = choose_status(&mut rng, error_spike);
            let pricing_available = rng.usize(0..100) >= 3;
            let (
                credential_id,
                credential_label,
                pool,
                estimated_cost_usd,
                original_cost_usd,
                billing,
            ) = if is_external {
                let pool = choose_pool(&mut rng, &pools);
                let floor = rng.usize(0..100) < pool.floor_probability;
                let raw_cost_usd = base_cost * (0.73 + rng.f64() * 0.08);
                let mut billable_cost_usd = raw_cost_usd * pool.markup;
                let cost_floor_delta_usd = if floor {
                    let floor_cost = (base_cost * 0.000003).max(raw_cost_usd * 1.05);
                    let delta = (floor_cost - billable_cost_usd).max(0.0);
                    billable_cost_usd += delta;
                    delta
                } else {
                    0.0
                };
                let snapshot = ExternalPoolUsageSnapshot {
                    total_input_tokens,
                    input_tokens: billable_input_tokens,
                    billable_input_tokens,
                    output_tokens,
                    cache_read_input_tokens: cache_read,
                    cache_creation_input_tokens: cache_creation,
                    cache_creation_5m_input_tokens: cache_creation,
                    cache_creation_1h_input_tokens: 0,
                };
                (
                    None,
                    None,
                    Some((pool, floor)),
                    billable_cost_usd,
                    raw_cost_usd,
                    Some(ExternalPoolBilling {
                        request_input_tokens: Some(total_input_tokens),
                        raw_usage: snapshot,
                        shaped_usage: snapshot,
                        reported_usage: snapshot,
                        usage_projection_applied: true,
                        raw_cost_usd,
                        shaped_cost_usd: billable_cost_usd,
                        uplifted_cost_usd: billable_cost_usd,
                        profit_usd: billable_cost_usd - raw_cost_usd,
                        reported_cost_usd: billable_cost_usd,
                        billable_cost_usd,
                        cost_floor_delta_usd,
                        cost_floor_applied: floor,
                        pricing_available,
                        pricing_model: Some(model.to_string()),
                        usage_projection_mode: "demo_normalized".to_string(),
                        stream_response_mode: None,
                        usage_estimated: false,
                        usage_estimate_reason: None,
                        usage_candidate_path: None,
                        body_usage_projection_applied: true,
                    }),
                )
            } else {
                // Keep two disabled accounts in the historical usage set so the
                // UI can demonstrate "consumed history" versus "spendable balance".
                let credential_index = rng.usize(0..22);
                let credential = &accounts[credential_index];
                let original_cost_usd = base_cost * (1.10 + rng.f64() * 0.20);
                (
                    credential.id,
                    credential.email.clone(),
                    None,
                    base_cost,
                    original_cost_usd,
                    None,
                )
            };
            let (external_pool_id, external_pool_name) = pool
                .as_ref()
                .map(|(pool, _)| (Some(pool.id), Some(pool.name.clone())))
                .unwrap_or((None, None));
            let id = format!("demo-{seed}-{total:06}");
            let error_type = if status == UsageRecordStatus::Success {
                None
            } else {
                Some(
                    match status {
                        UsageRecordStatus::ClientDropped => "client_dropped",
                        UsageRecordStatus::StreamError => "stream_error",
                        UsageRecordStatus::UpstreamTimeout => "upstream_timeout",
                        UsageRecordStatus::Error => "upstream_error",
                        UsageRecordStatus::Success => "success",
                    }
                    .to_string(),
                )
            };
            let duration_ms = 450 + rng.u64(0..4_500);
            let record = UsageRecord {
                id,
                created_at: created_at.to_rfc3339(),
                endpoint: choose_endpoint(&mut rng).to_string(),
                stream: true,
                model: model.to_string(),
                requested_max_tokens: Some(output_tokens.saturating_mul(2)),
                downstream_stop_reason: Some("end_turn".to_string()),
                upstream_model: Some(model.to_string()),
                external_outbound_model: external_pool_id.map(|_| model.to_string()),
                model_resolution_source: Some("demo".to_string()),
                model_resolution_note: None,
                conversation_id: Some(format!("demo-conv-{}", rng.u64(0..900_000))),
                request_api_key_id: Some(choose_request_key(&mut rng).to_string()),
                credential_id,
                credential_label,
                status,
                usage_source: UsageSource::UpstreamMetadata,
                raw_usage: billing.as_ref().map(|item| item.raw_usage),
                total_input_tokens,
                compat_input_tokens: billable_input_tokens,
                billable_input_tokens,
                output_tokens,
                cache_read_input_tokens: cache_read,
                cache_creation_input_tokens: cache_creation,
                cache_creation_5m_input_tokens: cache_creation,
                cache_creation_1h_input_tokens: 0,
                estimated_cost_usd: if is_external || pricing_available {
                    estimated_cost_usd
                } else {
                    0.0
                },
                original_cost_usd: if is_external || pricing_available {
                    original_cost_usd
                } else {
                    0.0
                },
                kiro_metering_usage: if is_external {
                    0.0
                } else {
                    0.1 + rng.f64() * 1.9
                },
                pricing_available,
                pricing_model: Some(model.to_string()),
                duration_ms,
                first_token_latency_ms: Some(120 + rng.u64(0..900)),
                response_latency_ms: Some(duration_ms),
                latency_trace: None,
                simulated: false,
                sticky_bound: rng.usize(0..100) < 8,
                fallback_from_sticky: false,
                credential_attempts: Vec::new(),
                route_kind: Some(if is_external {
                    UsageRouteKind::ExternalPool
                } else {
                    UsageRouteKind::LocalCredential
                }),
                route_subtype: Some(if is_external {
                    UsageRouteSubtype::ExternalDirectPolicy
                } else if status == UsageRecordStatus::Success {
                    UsageRouteSubtype::LocalSuccess
                } else {
                    UsageRouteSubtype::LocalErrorNoFallback
                }),
                fallback_reason: None,
                direct_policy_reason: Some("demo_seed".to_string()),
                local_attempted: Some(!is_external),
                local_preflight: None,
                external_pool_id,
                external_pool_name,
                external_attempts: Vec::new(),
                usage_projection_applied: billing.as_ref().map(|_| true),
                external_pool_billing: billing,
                error_type,
                error_message: if status == UsageRecordStatus::Success {
                    None
                } else {
                    Some("demo seeded error".to_string())
                },
                error_detail: None,
                error_status_code: (status != UsageRecordStatus::Success).then_some(502),
                error_source: (status != UsageRecordStatus::Success).then_some("demo".to_string()),
                error_id: None,
                error_metadata: Some(json!({ "seed": seed, "demo": true })),
                raw_upstream_error: None,
                public_error_status_code: None,
                public_error_type: None,
                public_error_message: None,
                payload_breakdown: None,
                payload_guard_report: None,
            };
            if is_external {
                external_records += 1;
            } else {
                local_records += 1;
            }
            batch.push(record);
            total += 1;
            if batch.len() >= DEMO_BATCH_SIZE {
                usage.record_batch(std::mem::take(&mut batch)).await?;
            }
        }
    }
    if !batch.is_empty() {
        usage.record_batch(batch).await?;
    }

    Ok(UsageDemoSeedReport {
        database,
        days,
        records: total,
        local_records,
        external_records,
        accounts: accounts.len(),
        pools: pools.len(),
    })
}

async fn clear_usage_tables(store: &PostgresStore) -> anyhow::Result<()> {
    sqlx::query(
        "TRUNCATE TABLE usage_records, usage_cleanup_watermarks, usage_cleanup_jobs, \
         usage_rollup_totals, usage_rollup_time_buckets, usage_cache_read_totals, \
         usage_cache_read_rollup_time_buckets, usage_duration_rollup_time_buckets, \
         usage_credential_cost_summary",
    )
    .execute(store.pool())
    .await
    .context("清空 demo usage 表失败")?;
    Ok(())
}

async fn ensure_demo_accounts(store: &PostgresStore) -> anyhow::Result<Vec<KiroCredentials>> {
    let rows = sqlx::query(
        "SELECT id, data, revision FROM credentials WHERE deleted_at IS NULL ORDER BY id ASC",
    )
    .fetch_all(store.pool())
    .await?;
    let mut accounts = Vec::new();
    for row in rows {
        let id: i64 = row.try_get("id")?;
        let revision: i64 = row.try_get("revision")?;
        let value: serde_json::Value = row.try_get("data")?;
        let mut credential: KiroCredentials = serde_json::from_value(value)?;
        credential.id = Some(id.max(0) as u64);
        credential.storage_revision = revision.max(0) as u64;
        if credential
            .email
            .as_deref()
            .is_some_and(|email| email.starts_with(DEMO_ACCOUNT_PREFIX))
        {
            accounts.push(credential);
        }
    }
    accounts.sort_by_key(|credential| credential.id);
    for index in accounts.len()..24 {
        let credential = KiroCredentials {
            email: Some(format!(
                "demo-acc-{index_plus_one:02}@example.com",
                index_plus_one = index + 1
            )),
            refresh_token: Some(format!("demo-refresh-token-{index:02}")),
            auth_method: Some("social".to_string()),
            provider: Some("Demo".to_string()),
            subscription_title: Some(if index < 2 {
                "KIRO STUDENT".to_string()
            } else if index < 20 {
                "KIRO PRO+".to_string()
            } else {
                "KIRO FREE".to_string()
            }),
            disabled: index >= 20,
            endpoint: Some("ide".to_string()),
            ..Default::default()
        };
        accounts.push(store.insert_credential(&credential).await?);
    }
    for (index, account) in accounts.iter_mut().enumerate() {
        let expected_title = if index < 2 {
            "KIRO STUDENT"
        } else if index < 20 {
            "KIRO PRO+"
        } else {
            "KIRO FREE"
        };
        if account.subscription_title.as_deref() == Some(expected_title) {
            continue;
        }
        let mut updated = account.clone();
        updated.subscription_title = Some(expected_title.to_string());
        if let Some(saved) = store
            .save_credentials(std::slice::from_ref(&updated))
            .await?
            .into_iter()
            .next()
        {
            *account = saved;
        }
    }
    Ok(accounts)
}

async fn seed_demo_account_info(
    store: &PostgresStore,
    accounts: &[KiroCredentials],
) -> anyhow::Result<()> {
    let checked_at = Utc::now().to_rfc3339();
    for (index, account) in accounts.iter().enumerate() {
        let Some(account_id) = account.id else {
            continue;
        };
        let credit_base = 1_000.0 + (index as f64 * 75.0);
        let credit_bonus = 120.0 + ((index % 4) as f64 * 25.0);
        let credit_limit = credit_base + credit_bonus;
        let credit_remaining = if account.disabled {
            260.0 + ((index % 3) as f64 * 35.0)
        } else {
            520.0 + (((index * 37) % 320) as f64)
        };
        // Keep the monthly usage quota empty so the admin credit snapshot
        // preserves the deterministic demo credit fields below.
        let usage_limit = 0.0;
        let current_usage = 0.0;
        store
            .save_credential_account_info(
                account_id,
                &CredentialAccountInfoRow {
                    subscription_title: account.subscription_title.clone(),
                    current_usage,
                    usage_limit,
                    remaining: (usage_limit - current_usage).max(0.0),
                    usage_percentage: 0.0,
                    credit_limit,
                    credit_remaining,
                    credit_base,
                    credit_bonus,
                    overage_status: Some("enabled".to_string()),
                    overage_capability: Some("demo".to_string()),
                    overage_cap: 0.0,
                    overage_rate: 0.0,
                    current_overages: 0.0,
                    next_reset_at: None,
                    checked_at: checked_at.clone(),
                },
            )
            .await?;
    }
    Ok(())
}

async fn ensure_demo_pools(store: &PostgresStore) -> anyhow::Result<Vec<DemoPool>> {
    let existing = store.list_external_pools(false).await?;
    let mut pools = Vec::with_capacity(DEMO_POOL_NAMES.len());
    for (index, name) in DEMO_POOL_NAMES.iter().enumerate() {
        let pool = if let Some(pool) = existing.iter().find(|pool| pool.name == *name) {
            pool.clone()
        } else {
            let request: CreateExternalPoolRequest = serde_json::from_value(json!({
                "name": name,
                "baseUrl": "http://127.0.0.1:1",
                "apiKey": format!("demo-pool-key-{index}"),
                "enabled": true,
                "priority": index as i32,
                "maxConcurrentRequests": 64,
                "supportedModels": DEMO_MODELS,
                "notes": "overview demo seed; never sends real traffic"
            }))?;
            store.create_external_pool(request).await?
        };
        pools.push(DemoPool {
            id: pool.id,
            name: pool.name,
            markup: match index {
                0 => 1.35,
                1 => 1.05,
                _ => 1.20,
            },
            floor_probability: match index {
                0 => 2,
                1 => 22,
                _ => 5,
            },
        });
    }
    Ok(pools)
}

fn choose_model(rng: &mut Rng) -> &'static str {
    let roll = rng.usize(0..100);
    if roll < 55 {
        DEMO_MODELS[0]
    } else if roll < 90 {
        DEMO_MODELS[1]
    } else {
        DEMO_MODELS[2]
    }
}

fn choose_endpoint(rng: &mut Rng) -> &'static str {
    let roll = rng.usize(0..100);
    if roll < 62 {
        DEMO_ENDPOINTS[0]
    } else if roll < 82 {
        DEMO_ENDPOINTS[1]
    } else if roll < 94 {
        DEMO_ENDPOINTS[2]
    } else {
        DEMO_ENDPOINTS[3]
    }
}

fn choose_request_key(rng: &mut Rng) -> &'static str {
    let roll = rng.usize(0..100);
    if roll < 58 {
        DEMO_REQUEST_KEYS[0]
    } else if roll < 88 {
        DEMO_REQUEST_KEYS[1]
    } else {
        DEMO_REQUEST_KEYS[2]
    }
}

fn choose_pool<'a>(rng: &mut Rng, pools: &'a [DemoPool]) -> &'a DemoPool {
    let roll = rng.usize(0..100);
    if roll < 55 {
        &pools[0]
    } else if roll < 90 {
        &pools[1]
    } else {
        &pools[2]
    }
}

fn choose_status(rng: &mut Rng, error_spike: bool) -> UsageRecordStatus {
    let roll = rng.usize(0..1000);
    if error_spike && roll < 150 {
        return UsageRecordStatus::Error;
    }
    if roll < 15 {
        UsageRecordStatus::Error
    } else if roll < 23 {
        UsageRecordStatus::ClientDropped
    } else if roll < 25 {
        UsageRecordStatus::StreamError
    } else if roll < 27 {
        UsageRecordStatus::UpstreamTimeout
    } else {
        UsageRecordStatus::Success
    }
}

fn estimate_cost(
    model: &str,
    input_tokens: i32,
    output_tokens: i32,
    cache_read_tokens: i32,
    cache_creation_tokens: i32,
) -> f64 {
    let (input, output, cache_read, cache_creation) = match model {
        "claude-opus-5-5" => (0.000015, 0.000075, 0.0000015, 0.00001875),
        "claude-sonnet-5" => (0.000003, 0.000015, 0.0000003, 0.00000375),
        _ => (0.0000008, 0.000004, 0.00000008, 0.000001),
    };
    input_tokens.max(0) as f64 * input
        + output_tokens.max(0) as f64 * output
        + cache_read_tokens.max(0) as f64 * cache_read
        + cache_creation_tokens.max(0) as f64 * cache_creation
}
