//! 模型计价模块
//!
//! 数据源:
//! - 主源: LiteLLM `model_prices_and_context_window.json`(GitHub Raw)
//! - 兜底: 内置静态快照(Anthropic Claude 系列,见 `BUILTIN_SNAPSHOT`)
//!
//! 启动行为:
//! - 读 `app_config.pricing_bootstrap_done`,若为 false 且 `pricing_auto_sync_enabled` 为 true,
//!   异步启动一次 LiteLLM 同步,完成后写 `pricing_bootstrap_done = true`。
//! - 同步失败时把内置快照写入 `model_prices` 作为兜底。
//!
//! 运行时:
//! - `compute_cost(model, usage)` 返回单次请求的美元成本,字段缺失时返回 None。
//! - 管理员可手动触发 `/api/admin/pricing/sync` 强制重拉。

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Context;
use chrono::{DateTime, Utc};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::Row;

use crate::app_config::AppConfigService;
use crate::storage::Db;

mod builtin;
pub use builtin::BUILTIN_SNAPSHOT;

/// 运行时一次请求的 token 用量(用于成本计算)。
#[derive(Debug, Clone, Copy, Default)]
pub struct PricingUsage {
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_input_tokens: i64,
    pub cache_creation_input_tokens: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelPrice {
    pub model_id: String,
    pub display_name: Option<String>,
    pub provider: String,
    pub input_cost_per_token: Option<f64>,
    pub output_cost_per_token: Option<f64>,
    pub cache_read_input_token_cost: Option<f64>,
    pub cache_creation_input_token_cost: Option<f64>,
    pub max_input_tokens: Option<i32>,
    pub max_output_tokens: Option<i32>,
    pub source: String,
    pub synced_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PricingSyncSummary {
    pub source: String,
    pub fetched_count: usize,
    pub upserted: usize,
    pub anthropic_only_filtered: usize,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub used_fallback: bool,
}

pub struct ModelPricingRegistry {
    db: Db,
    config: Arc<AppConfigService>,
    http: reqwest::Client,
    /// 内存价格表,key 为各种归一化候选模型 id,value 为单价四元组
    cache: RwLock<HashMap<String, CachedPrice>>,
}

#[derive(Debug, Clone, Copy)]
struct CachedPrice {
    input: f64,
    output: f64,
    cache_read: Option<f64>,
    cache_create: Option<f64>,
}

impl ModelPricingRegistry {
    pub fn new(db: Db, config: Arc<AppConfigService>) -> Arc<Self> {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("构建 reqwest 客户端失败");
        Arc::new(Self {
            db,
            config,
            http,
            cache: RwLock::new(HashMap::new()),
        })
    }

    /// 把当前 PG 表里的所有价格刷进内存缓存,供 `compute_cost_sync` 使用。
    pub async fn warm_cache(&self) -> anyhow::Result<usize> {
        let prices = self.list().await?;
        let mut cache = HashMap::with_capacity(prices.len() * 4);
        for p in &prices {
            let (Some(input), Some(output)) = (p.input_cost_per_token, p.output_cost_per_token)
            else {
                continue;
            };
            let entry = CachedPrice {
                input,
                output,
                cache_read: p.cache_read_input_token_cost,
                cache_create: p.cache_creation_input_token_cost,
            };
            for cand in normalize_model_candidates(&p.model_id) {
                cache.insert(cand, entry);
            }
            // 显式 modelId 自身也写一份(若 normalize 已包含会等价覆写)
            cache.insert(p.model_id.clone(), entry);
        }
        let n = cache.len();
        *self.cache.write() = cache;
        tracing::info!("model price 内存缓存已刷新,共 {} 个 key", n);
        Ok(n)
    }

    /// 同步成本计算,基于内存缓存。缓存里没有的模型返回 None。
    pub fn compute_cost_sync(&self, model: &str, usage: PricingUsage) -> Option<f64> {
        let cache = self.cache.read();
        let entry = normalize_model_candidates(model)
            .into_iter()
            .find_map(|cand| cache.get(&cand).copied())?;
        let cache_read_cost = entry.cache_read.unwrap_or(entry.input * 0.1);
        let cache_create_cost = entry.cache_create.unwrap_or(entry.input * 1.25);
        let total = (usage.input_tokens as f64) * entry.input
            + (usage.output_tokens as f64) * entry.output
            + (usage.cache_read_input_tokens as f64) * cache_read_cost
            + (usage.cache_creation_input_tokens as f64) * cache_create_cost;
        Some(total)
    }

    /// 启动期 bootstrap:若未同步过且开关开,异步同步一次。
    pub async fn bootstrap(self: &Arc<Self>) -> anyhow::Result<()> {
        let done: bool = self
            .config
            .get_as::<bool>("pricing_bootstrap_done")
            .unwrap_or(false);
        if done {
            tracing::info!("model_prices 已 bootstrap,跳过启动同步");
            return Ok(());
        }
        let auto: bool = self
            .config
            .get_as::<bool>("pricing_auto_sync_enabled")
            .unwrap_or(true);
        if !auto {
            tracing::info!("pricing_auto_sync_enabled = false,跳过启动同步");
            return Ok(());
        }

        let me = self.clone();
        tokio::spawn(async move {
            match me.sync(false).await {
                Ok(summary) => tracing::info!(
                    "模型计价启动同步完成: source={} fetched={} upserted={} fallback={}",
                    summary.source,
                    summary.fetched_count,
                    summary.upserted,
                    summary.used_fallback
                ),
                Err(err) => tracing::error!("模型计价启动同步失败: {:#}", err),
            }
            if let Err(err) = me.warm_cache().await {
                tracing::warn!("warm_cache 失败: {:#}", err);
            }
            if let Err(err) = me
                .config
                .set("pricing_bootstrap_done", Value::Bool(true), "system")
                .await
            {
                tracing::warn!("写 pricing_bootstrap_done 失败: {:#}", err);
            }
        });
        Ok(())
    }

    /// 列出全部模型(供 Admin API)
    pub async fn list(&self) -> anyhow::Result<Vec<ModelPrice>> {
        let rows = sqlx::query(
            "SELECT model_id, display_name, provider, \
                    input_cost_per_token::float8 AS input_cost_per_token, \
                    output_cost_per_token::float8 AS output_cost_per_token, \
                    cache_read_input_token_cost::float8 AS cache_read_input_token_cost, \
                    cache_creation_input_token_cost::float8 AS cache_creation_input_token_cost, \
                    max_input_tokens, max_output_tokens, source, synced_at \
             FROM model_prices ORDER BY provider, model_id",
        )
        .fetch_all(&self.db)
        .await
        .context("查询 model_prices 失败")?;

        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            out.push(ModelPrice {
                model_id: row.try_get("model_id")?,
                display_name: row.try_get("display_name").ok(),
                provider: row.try_get("provider")?,
                input_cost_per_token: row.try_get("input_cost_per_token").ok(),
                output_cost_per_token: row.try_get("output_cost_per_token").ok(),
                cache_read_input_token_cost: row.try_get("cache_read_input_token_cost").ok(),
                cache_creation_input_token_cost: row
                    .try_get("cache_creation_input_token_cost")
                    .ok(),
                max_input_tokens: row.try_get("max_input_tokens").ok(),
                max_output_tokens: row.try_get("max_output_tokens").ok(),
                source: row.try_get("source")?,
                synced_at: row.try_get("synced_at")?,
            });
        }
        Ok(out)
    }

    /// 异步从 PG 拉单价并计算成本。当前主路径走 `compute_cost_sync`(基于 warm cache),
    /// 这个 async 版本保留作工具函数 + 测试入口。
    #[allow(dead_code)]
    pub async fn compute_cost(
        &self,
        model: &str,
        usage: PricingUsage,
    ) -> anyhow::Result<Option<f64>> {
        // model 可能形如 "anthropic.claude-opus-4-7-v1:0",尝试多种归一化
        let candidates = normalize_model_candidates(model);
        let mut row_opt = None;
        for cand in &candidates {
            let row = sqlx::query(
                "SELECT input_cost_per_token::float8 AS i, \
                        output_cost_per_token::float8 AS o, \
                        cache_read_input_token_cost::float8 AS r, \
                        cache_creation_input_token_cost::float8 AS w \
                 FROM model_prices WHERE model_id = $1 LIMIT 1",
            )
            .bind(cand)
            .fetch_optional(&self.db)
            .await
            .context("查询 model_prices 失败")?;
            if row.is_some() {
                row_opt = row;
                break;
            }
        }
        let Some(row) = row_opt else {
            return Ok(None);
        };
        let i: Option<f64> = row.try_get("i").ok();
        let o: Option<f64> = row.try_get("o").ok();
        let r: Option<f64> = row.try_get("r").ok();
        let w: Option<f64> = row.try_get("w").ok();
        let (Some(i), Some(o)) = (i, o) else {
            return Ok(None);
        };
        // cache_read 价格通常为 input * 0.1,缺失时回退到 input
        let cache_read_cost = r.unwrap_or(i * 0.1);
        let cache_create_cost = w.unwrap_or(i * 1.25);
        let total = (usage.input_tokens as f64) * i
            + (usage.output_tokens as f64) * o
            + (usage.cache_read_input_tokens as f64) * cache_read_cost
            + (usage.cache_creation_input_tokens as f64) * cache_create_cost;
        Ok(Some(total))
    }

    /// 立即从 LiteLLM 同步,失败时(可选)落地 builtin 快照。
    pub async fn sync(&self, force_builtin: bool) -> anyhow::Result<PricingSyncSummary> {
        let started_at = Utc::now();
        let url: String = self
            .config
            .get_as::<String>("pricing_source_url")
            .unwrap_or_else(|| {
                "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json"
                    .to_string()
            });

        let (raw, source, used_fallback) = if force_builtin {
            (BUILTIN_SNAPSHOT.to_string(), "builtin".to_string(), true)
        } else {
            match self.http.get(&url).send().await {
                Ok(resp) if resp.status().is_success() => {
                    let body = resp.text().await.context("读取 LiteLLM 响应体失败")?;
                    (body, "litellm".to_string(), false)
                }
                Ok(resp) => {
                    tracing::warn!("LiteLLM 返回非 2xx ({}),回退到内置快照", resp.status());
                    (BUILTIN_SNAPSHOT.to_string(), "builtin".to_string(), true)
                }
                Err(err) => {
                    tracing::warn!("拉取 LiteLLM 失败: {:#},回退到内置快照", err);
                    (BUILTIN_SNAPSHOT.to_string(), "builtin".to_string(), true)
                }
            }
        };

        let parsed: serde_json::Map<String, Value> = serde_json::from_str(&raw)
            .with_context(|| format!("解析 LiteLLM JSON 失败({})", source))?;

        let mut anthropic_only_filtered = 0usize;
        let mut upserted = 0usize;
        let mut tx = self.db.begin().await?;

        // 同步前清掉表里所有非 Anthropic 官方原生的历史行(Bedrock / Vertex / OpenRouter / Replicate 等都不要)
        // 同时清掉非 builtin 来源,只保留官方原生 anthropic provider
        let purged: i64 = sqlx::query_scalar(
            "WITH d AS ( \
                DELETE FROM model_prices \
                WHERE LOWER(model_id) NOT LIKE '%claude%' \
                   OR provider <> 'anthropic' \
                RETURNING 1 \
             ) SELECT COUNT(*)::bigint FROM d",
        )
        .fetch_one(&mut *tx)
        .await
        .context("清理非官方 Anthropic 历史行失败")?;
        if purged > 0 {
            tracing::info!("model_prices 同步前清掉 {} 行非官方原生模型", purged);
        }

        for (model_id, body) in &parsed {
            // 跳过 sample_spec 等元数据
            if model_id == "sample_spec" {
                continue;
            }
            // **严格只保留 Anthropic 官方原生模型**:
            // - LiteLLM provider 必须是 "anthropic"(不要 bedrock / vertex / openrouter / replicate 等转售)
            // - model_id 必须含 "claude"(双重保险)
            // 这样最终入库就是官方 API 直接接受的 model id,如 claude-opus-4-7 / claude-sonnet-4-6 等
            let provider = body
                .get("litellm_provider")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if provider != "anthropic" {
                anthropic_only_filtered += 1;
                continue;
            }
            if !model_id.to_ascii_lowercase().contains("claude") {
                anthropic_only_filtered += 1;
                continue;
            }
            let display_name = body
                .get("display_name")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let input = body.get("input_cost_per_token").and_then(|v| v.as_f64());
            let output = body.get("output_cost_per_token").and_then(|v| v.as_f64());
            let cache_read = body
                .get("cache_read_input_token_cost")
                .and_then(|v| v.as_f64());
            let cache_create = body
                .get("cache_creation_input_token_cost")
                .and_then(|v| v.as_f64());
            let max_in = body
                .get("max_input_tokens")
                .and_then(|v| v.as_i64())
                .map(|x| x as i32);
            let max_out = body
                .get("max_output_tokens")
                .and_then(|v| v.as_i64())
                .map(|x| x as i32);

            sqlx::query(
                "INSERT INTO model_prices (model_id, display_name, provider, \
                    input_cost_per_token, output_cost_per_token, \
                    cache_read_input_token_cost, cache_creation_input_token_cost, \
                    max_input_tokens, max_output_tokens, source, raw, synced_at, updated_at) \
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,NOW(),NOW()) \
                 ON CONFLICT (model_id) DO UPDATE SET \
                    display_name = EXCLUDED.display_name, \
                    provider = EXCLUDED.provider, \
                    input_cost_per_token = EXCLUDED.input_cost_per_token, \
                    output_cost_per_token = EXCLUDED.output_cost_per_token, \
                    cache_read_input_token_cost = EXCLUDED.cache_read_input_token_cost, \
                    cache_creation_input_token_cost = EXCLUDED.cache_creation_input_token_cost, \
                    max_input_tokens = EXCLUDED.max_input_tokens, \
                    max_output_tokens = EXCLUDED.max_output_tokens, \
                    source = EXCLUDED.source, \
                    raw = EXCLUDED.raw, \
                    synced_at = NOW(), \
                    updated_at = NOW()",
            )
            .bind(model_id)
            .bind(&display_name)
            .bind(if provider.is_empty() {
                "anthropic"
            } else {
                provider
            })
            .bind(input)
            .bind(output)
            .bind(cache_read)
            .bind(cache_create)
            .bind(max_in)
            .bind(max_out)
            .bind(&source)
            .bind(body)
            .execute(&mut *tx)
            .await
            .with_context(|| format!("写入 model_prices[{}] 失败", model_id))?;
            upserted += 1;
        }
        tx.commit().await.context("提交 model_prices 事务失败")?;

        Ok(PricingSyncSummary {
            source,
            fetched_count: parsed.len(),
            upserted,
            anthropic_only_filtered,
            started_at,
            finished_at: Utc::now(),
            used_fallback,
        })
    }
}

fn normalize_model_candidates(model: &str) -> Vec<String> {
    let mut out = vec![model.to_string()];
    let lower = model.to_ascii_lowercase();
    if lower != model {
        out.push(lower.clone());
    }
    // bedrock 风格 -> anthropic.claude-opus-4-1-v1:0  -> claude-opus-4-1
    if let Some(stripped) = model.split(':').next() {
        if stripped != model {
            out.push(stripped.to_string());
        }
    }
    if let Some(s) = model.strip_prefix("anthropic.") {
        out.push(s.split(':').next().unwrap_or(s).to_string());
    }
    if let Some(s) = model.strip_prefix("anthropic/") {
        out.push(s.to_string());
    }
    if let Some(s) = model.strip_prefix("us.") {
        out.push(s.to_string());
    }
    if let Some(s) = model.strip_prefix("eu.") {
        out.push(s.to_string());
    }
    if let Some(s) = model.strip_prefix("apac.") {
        out.push(s.to_string());
    }
    out
}
