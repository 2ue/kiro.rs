use std::sync::Arc;

use anyhow::Context;
use chrono::{Datelike, Duration, FixedOffset, Timelike, Utc};
use fastrand::Rng;
use serde_json::json;

use crate::account::CreateAccountRequest;
use crate::anthropic::usage::{
    AccountBilling, AccountUsageSnapshot, UsageRecord, UsageRecordStatus, UsageRouteKind,
    UsageRouteSubtype, UsageSource,
};

use super::postgres::{PostgresStore, PostgresUsageStore};

const DEMO_DATABASE_PREFIX: &str = "pool_overview_demo_";

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
    pub external_records: usize,
    pub accounts: usize,
    pub pools: usize,
}

/// Simulates warm and cold upstream prompt caches.
fn demo_cache_split(rng: &mut fastrand::Rng, input_tokens: i32) -> (i32, i32) {
    let (hit_probability, read_share) = (88, 0.86);
    if rng.usize(0..100) < hit_probability {
        let jitter = 0.9 + rng.f64() * 0.1;
        let read = (f64::from(input_tokens) * read_share * jitter) as i32;
        let creation = (f64::from(input_tokens - read) * 0.25) as i32;
        (read, creation)
    } else {
        // Cold request: the prompt is written to the cache instead.
        (0, (f64::from(input_tokens) * 0.8) as i32)
    }
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

    let pools = ensure_demo_pools(store).await?;
    let usage = PostgresUsageStore::new(Arc::new(store.clone()));
    let now = Utc::now();
    let days = days.clamp(1, 30);
    let local_offset = FixedOffset::east_opt(8 * 60 * 60).expect("UTC+8 is a valid fixed offset");
    let mut rng = Rng::with_seed(seed);
    let mut batch = Vec::with_capacity(DEMO_BATCH_SIZE);
    let mut total = 0usize;
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
            let model = choose_model(&mut rng);
            let input_tokens = 24_000 + rng.i32(0..96_000);
            let output_tokens = 600 + rng.i32(0..3_000);
            let (cache_read, cache_creation) = demo_cache_split(&mut rng, input_tokens);
            let total_input_tokens = input_tokens;
            let billable_input_tokens = (input_tokens - cache_read - cache_creation).max(0);
            let base_cost = estimate_cost(
                model,
                billable_input_tokens,
                output_tokens,
                cache_read,
                cache_creation,
            ) * weekend_factor;
            let error_spike = day == 4 && (14..=15).contains(&hour);
            let status = choose_status(&mut rng, error_spike);
            let pricing_available = rng.usize(0..100) >= 3;
            let (
                credential_id,
                credential_label,
                pool,
                estimated_cost_usd,
                original_cost_usd,
                billing,
            ) = {
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
                let snapshot = AccountUsageSnapshot {
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
                    None::<u64>,
                    None::<String>,
                    Some((pool, floor)),
                    billable_cost_usd,
                    raw_cost_usd,
                    Some(AccountBilling {
                        request_input_tokens: Some(total_input_tokens),
                        upstream_response_model: None,
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
                        raw_usage_source: Default::default(),
                        usage_projection_mode: "demo_normalized".to_string(),
                        stream_response_mode: None,
                        usage_estimated: false,
                        usage_estimate_reason: None,
                        usage_candidate_path: None,
                        body_usage_projection_applied: true,
                    }),
                )
            };
            let (account_id, account_name) = pool
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
                account_cost_usd: 0.0,
                id,
                created_at: created_at.to_rfc3339(),
                endpoint: choose_endpoint(&mut rng).to_string(),
                stream: true,
                model: model.to_string(),
                requested_max_tokens: Some(output_tokens.saturating_mul(2)),
                downstream_stop_reason: Some("end_turn".to_string()),
                upstream_model: Some(model.to_string()),
                external_outbound_model: account_id.map(|_| model.to_string()),
                upstream_response_model: None,
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
                estimated_cost_usd,
                original_cost_usd,
                pricing_available,
                pricing_model: Some(model.to_string()),
                duration_ms,
                first_token_latency_ms: Some(120 + rng.u64(0..900)),
                response_latency_ms: Some(duration_ms),
                latency_trace: None,
                simulated: false,
                route_kind: Some(UsageRouteKind::Account),
                route_subtype: Some(UsageRouteSubtype::Account),

                account_id,
                account_name,
                external_attempts: Vec::new(),
                usage_projection_applied: billing.as_ref().map(|_| true),
                account_billing: billing,
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
            external_records += 1;
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
        external_records,
        accounts: pools.len(),
        pools: pools.len(),
    })
}

async fn clear_usage_tables(store: &PostgresStore) -> anyhow::Result<()> {
    sqlx::query(
        "TRUNCATE TABLE usage_records, usage_cleanup_watermarks, usage_cleanup_jobs, \
         usage_rollup_totals, usage_rollup_time_buckets, usage_cache_read_totals, \
         usage_cache_read_rollup_time_buckets, usage_duration_rollup_time_buckets",
    )
    .execute(store.pool())
    .await
    .context("清空 demo usage 表失败")?;
    Ok(())
}

async fn ensure_demo_pools(store: &PostgresStore) -> anyhow::Result<Vec<DemoPool>> {
    let existing = store.list_accounts(false).await?;
    let mut pools = Vec::with_capacity(DEMO_POOL_NAMES.len());
    for (index, name) in DEMO_POOL_NAMES.iter().enumerate() {
        let pool = if let Some(pool) = existing.iter().find(|pool| pool.name == *name) {
            pool.clone()
        } else {
            let request: CreateAccountRequest = serde_json::from_value(json!({
                "name": name,
                "baseUrl": "http://127.0.0.1:1",
                "apiKey": format!("demo-pool-key-{index}"),
                "enabled": true,
                "priority": index as i32,
                "maxConcurrentRequests": 64,
                "supportedModels": DEMO_MODELS,
                "notes": "overview demo seed; never sends real traffic"
            }))?;
            store.create_account(request).await?
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
