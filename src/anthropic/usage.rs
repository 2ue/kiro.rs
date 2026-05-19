use std::collections::{HashMap, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

const DEFAULT_QUERY_LIMIT: usize = 100;
const MAX_QUERY_LIMIT: usize = 1000;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum UsageRecordStatus {
    Success,
    Error,
    StreamError,
    UpstreamTimeout,
    ClientDropped,
}

impl UsageRecordStatus {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "success" => Some(Self::Success),
            "error" => Some(Self::Error),
            "stream_error" => Some(Self::StreamError),
            "upstream_timeout" => Some(Self::UpstreamTimeout),
            "client_dropped" => Some(Self::ClientDropped),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum UsageSource {
    UpstreamMetadata,
    LocalPromptCache,
    ContextEstimate,
    RequestEstimate,
    None,
}

impl UsageSource {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "upstream_metadata" => Some(Self::UpstreamMetadata),
            "local_prompt_cache" => Some(Self::LocalPromptCache),
            "context_estimate" => Some(Self::ContextEstimate),
            "request_estimate" => Some(Self::RequestEstimate),
            "none" => Some(Self::None),
            _ => None,
        }
    }

    pub fn is_simulated(self) -> bool {
        matches!(self, Self::LocalPromptCache)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageRecord {
    pub id: String,
    pub created_at: String,
    pub endpoint: String,
    pub stream: bool,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential_label: Option<String>,
    /// 本次客户端请求在 provider 内部实际尝试过的凭据 ID（按尝试顺序）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attempted_credential_ids: Vec<u64>,
    /// 本次请求中收到 429 并进入冷却的凭据 ID。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rate_limited_credential_ids: Vec<u64>,
    /// 最后一个被实际调度/尝试的凭据 ID。失败记录用它作为主要排查入口。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_attempted_credential_id: Option<u64>,
    /// 调度阶段已被全池退避/冷却拦截，可能没有新的实际上游尝试。
    #[serde(default)]
    pub scheduler_blocked: bool,
    pub status: UsageRecordStatus,
    pub usage_source: UsageSource,
    pub total_input_tokens: i32,
    pub compat_input_tokens: i32,
    pub billable_input_tokens: i32,
    pub output_tokens: i32,
    pub cache_read_input_tokens: i32,
    pub cache_creation_input_tokens: i32,
    pub cache_creation_5m_input_tokens: i32,
    pub cache_creation_1h_input_tokens: i32,
    pub duration_ms: u64,
    pub simulated: bool,
    pub sticky_bound: bool,
    pub fallback_from_sticky: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    /// 客户端 User-Agent(自 v2026.4)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_user_agent: Option<String>,
    /// 客户端 IP(尊重 X-Forwarded-For,自 v2026.4)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_ip: Option<String>,
    /// 透传给客户端的 request id(响应头 x-request-id 同值,自 v2026.4)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// 当次请求的美元成本估算(基于 model_prices,自 v2026.4)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct UsageRecordQuery {
    pub limit: usize,
    pub q: Option<String>,
    pub conversation_id: Option<String>,
    pub credential_id: Option<u64>,
    pub model: Option<String>,
    pub status: Option<UsageRecordStatus>,
    pub source: Option<UsageSource>,
    pub stream: Option<bool>,
    pub min_cache_read: Option<i32>,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
}

impl Default for UsageRecordQuery {
    fn default() -> Self {
        Self {
            limit: DEFAULT_QUERY_LIMIT,
            q: None,
            conversation_id: None,
            credential_id: None,
            model: None,
            status: None,
            source: None,
            stream: None,
            min_cache_read: None,
            since: None,
            until: None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageRecordsResult {
    pub total: usize,
    pub records: Vec<UsageRecord>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageRecordsPageResult {
    pub total: usize,
    pub page: usize,
    pub limit: usize,
    pub total_pages: usize,
    pub records: Vec<UsageRecord>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageAggregate {
    pub key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub requests: usize,
    pub cache_read_input_tokens: i64,
    pub cache_creation_input_tokens: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageSummary {
    pub total_requests: usize,
    pub success_requests: usize,
    pub error_requests: usize,
    pub high_cache_requests: usize,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub total_cache_read_input_tokens: i64,
    pub total_cache_creation_input_tokens: i64,
    pub local_prompt_cache_requests: usize,
    pub local_prompt_cache_input_tokens: i64,
    pub local_prompt_cache_read_input_tokens: i64,
    pub local_prompt_cache_creation_input_tokens: i64,
    pub simulated_requests: usize,
    pub upstream_metadata_requests: usize,
    pub top_credentials: Vec<UsageAggregate>,
    pub top_conversations: Vec<UsageAggregate>,
}

pub struct UsageRecorder {
    records: Mutex<VecDeque<UsageRecord>>,
    limit: usize,
    persist_path: Option<PathBuf>,
    db: Option<crate::storage::Db>,
    pricing: Option<std::sync::Arc<crate::pricing::ModelPricingRegistry>>,
}

impl UsageRecorder {
    pub fn new(limit: usize, persist_path: Option<PathBuf>) -> Self {
        let limit = limit.max(1);
        let recorder = Self {
            records: Mutex::new(VecDeque::with_capacity(limit.min(1024))),
            limit,
            persist_path,
            db: None,
            pricing: None,
        };
        recorder.load_recent_records();
        recorder
    }

    /// 启动期注入 PG 和计价层。注入后:
    /// - 每条 record 异步写入 PG 并实时计算 `cost_usd`
    /// - 内存 ringbuffer 与 JSONL 文件继续保留作为热备
    /// - 若 PG `usage_records` 表为空,把内存中已加载的历史 JSONL 全量回放进 PG,
    ///   保证仪表盘 SQL 聚合(`/usage-stats`)与用量列表数据一致
    /// - **同时把 cost_usd 回写进内存 ringbuffer**,这样列表 API 也能看到成本
    pub async fn attach_storage(
        &mut self,
        db: crate::storage::Db,
        pricing: std::sync::Arc<crate::pricing::ModelPricingRegistry>,
    ) {
        self.db = Some(db.clone());
        self.pricing = Some(pricing.clone());

        // 给内存里所有缺 cost_usd 的旧记录补算成本
        // (JSONL 里没存 cost_usd 字段,加载进来都是 None)
        {
            let mut records = self.records.lock();
            let mut filled = 0usize;
            for r in records.iter_mut() {
                if r.cost_usd.is_none() {
                    let billable = if r.billable_input_tokens > 0 {
                        r.billable_input_tokens
                    } else {
                        r.compat_input_tokens
                    };
                    let usage = crate::pricing::PricingUsage {
                        input_tokens: billable as i64,
                        output_tokens: r.output_tokens as i64,
                        cache_read_input_tokens: r.cache_read_input_tokens as i64,
                        cache_creation_input_tokens: r.cache_creation_input_tokens as i64,
                    };
                    if let Some(c) = pricing.compute_cost_sync(&r.model, usage) {
                        r.cost_usd = Some(c);
                        filled += 1;
                    }
                }
            }
            if filled > 0 {
                tracing::info!("内存 ringbuffer 补算 cost_usd 完成: {} 条", filled);
            }
        }

        // 若 PG 表是空的,把内存里(刚补算过的)历史回放进去
        let count_res: Result<(i64,), sqlx::Error> =
            sqlx::query_as("SELECT COUNT(*)::bigint FROM usage_records")
                .fetch_one(&db)
                .await;
        match count_res {
            Ok((0,)) => {
                let snapshot: Vec<UsageRecord> = self.records.lock().iter().cloned().collect();
                if snapshot.is_empty() {
                    return;
                }
                tracing::info!(
                    "PG usage_records 表为空,从内存 JSONL 历史回放 {} 条",
                    snapshot.len()
                );
                let mut ok = 0usize;
                let mut fail = 0usize;
                for r in snapshot {
                    match insert_usage_record_to_pg(&db, &r).await {
                        Ok(_) => ok += 1,
                        Err(err) => {
                            fail += 1;
                            tracing::debug!("回放 usage_record 失败: {:#}", err);
                        }
                    }
                }
                tracing::info!("usage_records 回放完成: 成功 {} 失败 {}", ok, fail);
            }
            Ok((n,)) => {
                tracing::info!("PG usage_records 表已有 {} 条,跳过 JSONL 回放", n);
                // PG 已有数据时,反向把 PG 中的 cost_usd 回灌到内存 ringbuffer
                // 避免 PG 已存有正确成本但内存里仍是旧值
                if let Err(err) = self.backfill_cost_from_pg(&db).await {
                    tracing::warn!("从 PG 回灌 cost_usd 到内存失败: {:#}", err);
                }
            }
            Err(err) => tracing::warn!("查询 usage_records 行数失败: {:#}", err),
        }
    }

    /// 用 PG 中已存在的 cost_usd 反向更新内存 ringbuffer 里的记录。
    /// 通过 `request_id` 或 record id 匹配。
    async fn backfill_cost_from_pg(&self, db: &crate::storage::Db) -> anyhow::Result<()> {
        use sqlx::Row;
        // 把内存里 cost_usd 还为空的记录的 request_id 收集起来
        let need: Vec<String> = {
            let records = self.records.lock();
            records
                .iter()
                .filter(|r| r.cost_usd.is_none())
                .map(|r| r.request_id.clone().unwrap_or_else(|| r.id.clone()))
                .collect()
        };
        if need.is_empty() {
            return Ok(());
        }
        let rows = sqlx::query(
            "SELECT request_id, cost_usd::float8 AS c FROM usage_records \
             WHERE request_id = ANY($1) AND cost_usd IS NOT NULL",
        )
        .bind(&need)
        .fetch_all(db)
        .await?;
        let mut map = std::collections::HashMap::new();
        for row in rows {
            let rid: Option<String> = row.try_get("request_id").ok();
            let c: Option<f64> = row.try_get("c").ok();
            if let (Some(rid), Some(c)) = (rid, c) {
                map.insert(rid, c);
            }
        }
        if map.is_empty() {
            return Ok(());
        }
        let mut filled = 0usize;
        let mut records = self.records.lock();
        for r in records.iter_mut() {
            if r.cost_usd.is_some() {
                continue;
            }
            let key = r.request_id.clone().unwrap_or_else(|| r.id.clone());
            if let Some(c) = map.get(&key).copied() {
                r.cost_usd = Some(c);
                filled += 1;
            }
        }
        if filled > 0 {
            tracing::info!("从 PG 回灌 cost_usd 到内存 ringbuffer: {} 条", filled);
        }
        Ok(())
    }

    pub fn record(&self, mut record: UsageRecord) {
        // 后端结合 pricing 内存缓存补算 cost_usd
        if record.cost_usd.is_none() {
            if let Some(pricing) = &self.pricing {
                let billable_input = if record.billable_input_tokens > 0 {
                    record.billable_input_tokens
                } else {
                    record.compat_input_tokens
                };
                let usage = crate::pricing::PricingUsage {
                    input_tokens: billable_input as i64,
                    output_tokens: record.output_tokens as i64,
                    cache_read_input_tokens: record.cache_read_input_tokens as i64,
                    cache_creation_input_tokens: record.cache_creation_input_tokens as i64,
                };
                record.cost_usd = pricing.compute_cost_sync(&record.model, usage);
            }
        }

        // 异步写 PG(若 attach_storage 过)
        if let Some(db) = self.db.clone() {
            let row = record.clone();
            tokio::spawn(async move {
                if let Err(err) = insert_usage_record_to_pg(&db, &row).await {
                    tracing::warn!("PG usage_records 写入失败(非阻塞): {:#}", err);
                }
            });
        }

        {
            let mut records = self.records.lock();
            records.push_back(record.clone());
            while records.len() > self.limit {
                records.pop_front();
            }
        }

        if let Some(path) = &self.persist_path {
            if let Some(parent) = path.parent() {
                if let Err(err) = std::fs::create_dir_all(parent) {
                    tracing::warn!("创建 usage record 目录失败: {}", err);
                    return;
                }
            }

            match OpenOptions::new().create(true).append(true).open(path) {
                Ok(mut file) => match serde_json::to_string(&record) {
                    Ok(line) => {
                        if let Err(err) = writeln!(file, "{}", line) {
                            tracing::warn!("写入 usage record 失败: {}", err);
                        }
                    }
                    Err(err) => tracing::warn!("序列化 usage record 失败: {}", err),
                },
                Err(err) => tracing::warn!("打开 usage record 文件失败: {}", err),
            }
        }
    }

    pub fn query(&self, query: UsageRecordQuery) -> UsageRecordsResult {
        let limit = normalize_limit(query.limit);
        let mut matched: Vec<UsageRecord> = self
            .records
            .lock()
            .iter()
            .rev()
            .filter(|record| record_matches(record, &query))
            .cloned()
            .collect();
        let total = matched.len();
        matched.truncate(limit);
        UsageRecordsResult {
            total,
            records: matched,
        }
    }

    pub fn query_page(
        &self,
        query: UsageRecordQuery,
        page: usize,
        limit: usize,
    ) -> UsageRecordsPageResult {
        let page = normalize_page(page);
        let limit = normalize_limit(limit);
        let matched: Vec<UsageRecord> = self
            .records
            .lock()
            .iter()
            .rev()
            .filter(|record| record_matches(record, &query))
            .cloned()
            .collect();
        let total = matched.len();
        let total_pages = total_pages(total, limit);
        let start = page.saturating_sub(1).saturating_mul(limit);
        let records = matched.into_iter().skip(start).take(limit).collect();

        UsageRecordsPageResult {
            total,
            page,
            limit,
            total_pages,
            records,
        }
    }

    /// **优先 PG**:从 PG `usage_records` 表分页查询,数据是权威值(含 `cost_usd`)。
    /// 没接 PG 时回退到内存 ringbuffer。
    pub async fn query_page_async(
        &self,
        query: UsageRecordQuery,
        page: usize,
        limit: usize,
    ) -> UsageRecordsPageResult {
        if let Some(db) = &self.db {
            match query_records_from_pg(db, &query, page, limit).await {
                Ok(result) => return result,
                Err(err) => {
                    tracing::warn!("PG 查询 usage_records 失败,回退内存 ringbuffer: {:#}", err);
                }
            }
        }
        self.query_page(query, page, limit)
    }

    /// **优先 PG**:与 query_page_async 同源,只取前 N 条不分页。
    pub async fn query_async(&self, query: UsageRecordQuery) -> UsageRecordsResult {
        if let Some(db) = &self.db {
            let limit = normalize_limit(query.limit);
            match query_records_from_pg(db, &query, 1, limit).await {
                Ok(page) => {
                    return UsageRecordsResult {
                        total: page.total,
                        records: page.records,
                    };
                }
                Err(err) => {
                    tracing::warn!("PG 查询 usage_records 失败,回退内存 ringbuffer: {:#}", err);
                }
            }
        }
        self.query(query)
    }

    pub fn summary(&self, high_cache_threshold: i32) -> UsageSummary {
        let records = self.records.lock();
        let mut summary = UsageSummary {
            total_requests: records.len(),
            success_requests: 0,
            error_requests: 0,
            high_cache_requests: 0,
            total_input_tokens: 0,
            total_output_tokens: 0,
            total_cache_read_input_tokens: 0,
            total_cache_creation_input_tokens: 0,
            local_prompt_cache_requests: 0,
            local_prompt_cache_input_tokens: 0,
            local_prompt_cache_read_input_tokens: 0,
            local_prompt_cache_creation_input_tokens: 0,
            simulated_requests: 0,
            upstream_metadata_requests: 0,
            top_credentials: Vec::new(),
            top_conversations: Vec::new(),
        };
        let mut credentials: HashMap<String, UsageAggregate> = HashMap::new();
        let mut conversations: HashMap<String, UsageAggregate> = HashMap::new();

        for record in records.iter() {
            if record.status == UsageRecordStatus::Success {
                summary.success_requests += 1;
            } else {
                summary.error_requests += 1;
            }
            if record.cache_read_input_tokens >= high_cache_threshold {
                summary.high_cache_requests += 1;
            }
            if record.simulated {
                summary.simulated_requests += 1;
            }
            if record.usage_source == UsageSource::UpstreamMetadata {
                summary.upstream_metadata_requests += 1;
            }
            summary.total_input_tokens += record.total_input_tokens as i64;
            summary.total_output_tokens += record.output_tokens as i64;
            summary.total_cache_read_input_tokens += record.cache_read_input_tokens as i64;
            summary.total_cache_creation_input_tokens += record.cache_creation_input_tokens as i64;
            if record.usage_source == UsageSource::LocalPromptCache {
                summary.local_prompt_cache_requests += 1;
                summary.local_prompt_cache_input_tokens += record.total_input_tokens as i64;
                summary.local_prompt_cache_read_input_tokens +=
                    record.cache_read_input_tokens as i64;
                summary.local_prompt_cache_creation_input_tokens +=
                    record.cache_creation_input_tokens as i64;
            }

            if let Some(id) = record.credential_id {
                let key = id.to_string();
                let entry = credentials.entry(key.clone()).or_insert(UsageAggregate {
                    key,
                    label: record.credential_label.clone(),
                    requests: 0,
                    cache_read_input_tokens: 0,
                    cache_creation_input_tokens: 0,
                });
                entry.requests += 1;
                entry.cache_read_input_tokens += record.cache_read_input_tokens as i64;
                entry.cache_creation_input_tokens += record.cache_creation_input_tokens as i64;
                if entry.label.is_none() {
                    entry.label = record.credential_label.clone();
                }
            }

            if let Some(conversation_id) = &record.conversation_id {
                let entry =
                    conversations
                        .entry(conversation_id.clone())
                        .or_insert(UsageAggregate {
                            key: conversation_id.clone(),
                            label: None,
                            requests: 0,
                            cache_read_input_tokens: 0,
                            cache_creation_input_tokens: 0,
                        });
                entry.requests += 1;
                entry.cache_read_input_tokens += record.cache_read_input_tokens as i64;
                entry.cache_creation_input_tokens += record.cache_creation_input_tokens as i64;
            }
        }

        summary.top_credentials = top_aggregates(credentials);
        summary.top_conversations = top_aggregates(conversations);
        summary
    }

    pub fn clear(&self) {
        self.records.lock().clear();
        if let Some(path) = &self.persist_path {
            if let Err(err) = File::create(path) {
                tracing::warn!("清空 usage record 文件失败: {}", err);
            }
        }
    }

    pub async fn clear_all(&self) -> anyhow::Result<u64> {
        let deleted = if let Some(db) = &self.db {
            sqlx::query("DELETE FROM usage_records")
                .execute(db)
                .await?
                .rows_affected()
        } else {
            0
        };
        self.clear();
        Ok(deleted)
    }

    fn load_recent_records(&self) {
        let Some(path) = &self.persist_path else {
            return;
        };
        let Ok(file) = File::open(path) else {
            return;
        };

        let reader = BufReader::new(file);
        let mut loaded = VecDeque::new();
        for line in reader.lines() {
            match line {
                Ok(line) if line.trim().is_empty() => {}
                Ok(line) => match serde_json::from_str::<UsageRecord>(&line) {
                    Ok(record) => {
                        loaded.push_back(record);
                        while loaded.len() > self.limit {
                            loaded.pop_front();
                        }
                    }
                    Err(err) => tracing::warn!("跳过损坏的 usage record 行: {}", err),
                },
                Err(err) => tracing::warn!("读取 usage record 行失败: {}", err),
            }
        }

        *self.records.lock() = loaded;
    }
}

fn normalize_limit(limit: usize) -> usize {
    if limit == 0 {
        DEFAULT_QUERY_LIMIT
    } else {
        limit.min(MAX_QUERY_LIMIT)
    }
}

fn normalize_page(page: usize) -> usize {
    page.max(1)
}

fn total_pages(total: usize, limit: usize) -> usize {
    if total == 0 { 0 } else { total.div_ceil(limit) }
}

fn record_matches(record: &UsageRecord, query: &UsageRecordQuery) -> bool {
    if let Some(q) = &query.q {
        if !record_matches_search(record, q) {
            return false;
        }
    }
    if let Some(conversation_id) = &query.conversation_id {
        if record.conversation_id.as_ref() != Some(conversation_id) {
            return false;
        }
    }
    if let Some(credential_id) = query.credential_id {
        if record.credential_id != Some(credential_id)
            && record.last_attempted_credential_id != Some(credential_id)
            && !record.attempted_credential_ids.contains(&credential_id)
            && !record.rate_limited_credential_ids.contains(&credential_id)
        {
            return false;
        }
    }
    if let Some(model) = &query.model {
        if &record.model != model {
            return false;
        }
    }
    if let Some(status) = query.status {
        if record.status != status {
            return false;
        }
    }
    if let Some(source) = query.source {
        if record.usage_source != source {
            return false;
        }
    }
    if let Some(stream) = query.stream {
        if record.stream != stream {
            return false;
        }
    }
    if let Some(min_cache_read) = query.min_cache_read {
        if record.cache_read_input_tokens < min_cache_read {
            return false;
        }
    }
    if let Some(since) = query.since {
        let Some(created_at) = parse_record_time(&record.created_at) else {
            return false;
        };
        if created_at < since {
            return false;
        }
    }
    if let Some(until) = query.until {
        let Some(created_at) = parse_record_time(&record.created_at) else {
            return false;
        };
        if created_at > until {
            return false;
        }
    }
    true
}

fn record_matches_search(record: &UsageRecord, q: &str) -> bool {
    let q = q.trim().to_ascii_lowercase();
    if q.is_empty() {
        return true;
    }

    let status = usage_status_value(record.status);
    let source = usage_source_value(record.usage_source);
    let credential_id = record.credential_id.map(|id| id.to_string());
    let last_attempted_credential_id = record.last_attempted_credential_id.map(|id| id.to_string());
    let attempted_credential_ids = join_credential_ids(&record.attempted_credential_ids);
    let rate_limited_credential_ids = join_credential_ids(&record.rate_limited_credential_ids);

    [
        Some(record.id.as_str()),
        Some(record.created_at.as_str()),
        Some(record.endpoint.as_str()),
        Some(record.model.as_str()),
        record.conversation_id.as_deref(),
        record.credential_label.as_deref(),
        Some(status),
        Some(source),
        record.error_type.as_deref(),
        record.error_message.as_deref(),
        credential_id.as_deref(),
        last_attempted_credential_id.as_deref(),
        attempted_credential_ids.as_deref(),
        rate_limited_credential_ids.as_deref(),
    ]
    .into_iter()
    .flatten()
    .any(|value| value.to_ascii_lowercase().contains(&q))
}

fn join_credential_ids(ids: &[u64]) -> Option<String> {
    if ids.is_empty() {
        return None;
    }
    Some(
        ids.iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(","),
    )
}

fn usage_status_value(status: UsageRecordStatus) -> &'static str {
    match status {
        UsageRecordStatus::Success => "success",
        UsageRecordStatus::Error => "error",
        UsageRecordStatus::StreamError => "stream_error",
        UsageRecordStatus::UpstreamTimeout => "upstream_timeout",
        UsageRecordStatus::ClientDropped => "client_dropped",
    }
}

fn usage_source_value(source: UsageSource) -> &'static str {
    match source {
        UsageSource::UpstreamMetadata => "upstream_metadata",
        UsageSource::LocalPromptCache => "local_prompt_cache",
        UsageSource::ContextEstimate => "context_estimate",
        UsageSource::RequestEstimate => "request_estimate",
        UsageSource::None => "none",
    }
}

fn parse_record_time(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

fn top_aggregates(map: HashMap<String, UsageAggregate>) -> Vec<UsageAggregate> {
    let mut values: Vec<_> = map.into_values().collect();
    values.sort_by_key(|item| {
        (
            std::cmp::Reverse(item.cache_read_input_tokens),
            std::cmp::Reverse(item.requests),
        )
    });
    values.truncate(10);
    values
}

// ============================================================================
// PG 持久化与聚合(自 v2026.4)
// ============================================================================

async fn insert_usage_record_to_pg(
    db: &crate::storage::Db,
    record: &UsageRecord,
) -> anyhow::Result<()> {
    let created_at = DateTime::parse_from_rfc3339(&record.created_at)
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now());
    let id = uuid::Uuid::parse_str(&record.id)
        .or_else(|_| {
            // record.id 是 anthropic 风格的 req_xxx,直接生成新 uuid 但保留 request_id
            Ok::<uuid::Uuid, uuid::Error>(uuid::Uuid::new_v4())
        })
        .unwrap_or_else(|_| uuid::Uuid::new_v4());

    sqlx::query(
        "INSERT INTO usage_records (id, created_at, request_id, endpoint, stream, model, \
            model_provider, conversation_id, credential_id, credential_label, \
            attempted_credential_ids, rate_limited_credential_ids, last_attempted_credential_id, scheduler_blocked, \
            status, usage_source, error_type, error_message, \
            total_input_tokens, compat_input_tokens, billable_input_tokens, \
            output_tokens, cache_read_input_tokens, cache_creation_input_tokens, \
            cache_creation_5m_input_tokens, cache_creation_1h_input_tokens, \
            cost_usd, client_user_agent, client_ip, duration_ms, \
            simulated, sticky_bound, fallback_from_sticky) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22,$23,$24,$25,$26,$27,$28,$29::inet,$30,$31,$32,$33) \
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(id)
    .bind(created_at)
    .bind(record.request_id.clone().or(Some(record.id.clone())))
    .bind(&record.endpoint)
    .bind(record.stream)
    .bind(&record.model)
    .bind("anthropic")
    .bind(&record.conversation_id)
    .bind(record.credential_id.map(|x| x as i64))
    .bind(&record.credential_label)
    .bind(
        record
            .attempted_credential_ids
            .iter()
            .map(|x| *x as i64)
            .collect::<Vec<_>>(),
    )
    .bind(
        record
            .rate_limited_credential_ids
            .iter()
            .map(|x| *x as i64)
            .collect::<Vec<_>>(),
    )
    .bind(record.last_attempted_credential_id.map(|x| x as i64))
    .bind(record.scheduler_blocked)
    .bind(usage_status_value(record.status))
    .bind(usage_source_value(record.usage_source))
    .bind(&record.error_type)
    .bind(&record.error_message)
    .bind(record.total_input_tokens)
    .bind(record.compat_input_tokens)
    .bind(record.billable_input_tokens)
    .bind(record.output_tokens)
    .bind(record.cache_read_input_tokens)
    .bind(record.cache_creation_input_tokens)
    .bind(record.cache_creation_5m_input_tokens)
    .bind(record.cache_creation_1h_input_tokens)
    .bind(record.cost_usd)
    .bind(&record.client_user_agent)
    .bind(&record.client_ip)
    .bind(record.duration_ms as i64)
    .bind(record.simulated)
    .bind(record.sticky_bound)
    .bind(record.fallback_from_sticky)
    .execute(db)
    .await?;
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageStats {
    /// today = CURRENT_DATE 起的"今日"指标(无视 since/until)
    pub today_requests: i64,
    pub today_tokens: i64,
    pub today_output_tokens: i64,
    pub today_cost_usd: f64,
    /// total = 不限时间的累计
    pub total_requests: i64,
    pub total_tokens: i64,
    pub total_output_tokens: i64,
    pub total_cost_usd: f64,
    /// range = since~until 时间范围内的指标(默认 = today)
    pub range_requests: i64,
    pub range_tokens: i64,
    pub range_output_tokens: i64,
    pub range_cost_usd: f64,
    pub range_since: chrono::DateTime<chrono::Utc>,
    pub range_until: chrono::DateTime<chrono::Utc>,
    /// 时间序列(按 bucket 聚合,bucket=hour/day),按时间升序
    pub timeline: Vec<UsageStatsBucket>,
    /// bucket 单位:"hour" / "day"
    pub bucket: String,
    /// range 内按模型聚合
    pub by_model: Vec<UsageStatsByModel>,
    /// range 内按凭据聚合
    pub by_credential: Vec<UsageStatsByCredential>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageStatsBucket {
    pub bucket: chrono::DateTime<chrono::Utc>,
    pub requests: i64,
    pub tokens: i64,
    pub output_tokens: i64,
    pub cost_usd: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageStatsByModel {
    pub model: String,
    pub requests: i64,
    pub tokens: i64,
    pub output_tokens: i64,
    pub cost_usd: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageStatsByCredential {
    pub credential_id: i64,
    pub requests: i64,
    pub tokens: i64,
    pub output_tokens: i64,
    pub cost_usd: f64,
}

/// 从 PG 计算今日 / 累计 / 范围内 / 时间序列 / 按模型 / 按凭据 聚合。
/// 支持可选过滤条件,与 usage 列表的过滤参数一致,确保前端"统计"与"列表"在同一筛选下保持口径。
///
/// 时间范围(`since` / `until`)默认为今日 0 点 ~ 现在;`bucket` 默认根据范围长度自动选(<= 48h 用 hour,否则用 day)。
pub async fn query_usage_stats(
    db: &crate::storage::Db,
    filter: &UsageStatsFilter,
) -> anyhow::Result<UsageStats> {
    use sqlx::Row;

    let now = chrono::Utc::now();
    let today_start = now.date_naive().and_hms_opt(0, 0, 0).unwrap().and_utc();
    let range_since = filter.since.unwrap_or(today_start);
    let range_until = filter.until.unwrap_or(now);
    let range_secs = (range_until - range_since).num_seconds().max(1);
    let bucket = match &filter.bucket {
        Some(b) if b == "hour" || b == "day" => b.clone(),
        _ => {
            if range_secs <= 48 * 3600 {
                "hour".to_string()
            } else {
                "day".to_string()
            }
        }
    };

    // 用同一组绑定参数构造 WHERE 子句
    // $1=q (LIKE %q%), $2=conversation_id, $3=credential_id, $4=model,
    // $5=status, $6=usage_source, $7=stream, $8=min_cache_read
    let base_where = "(\
        $1::text IS NULL OR \
        model ILIKE '%' || $1 || '%' OR \
        COALESCE(credential_label, '') ILIKE '%' || $1 || '%' OR \
        COALESCE(last_attempted_credential_id::text, '') ILIKE '%' || $1 || '%' OR \
        COALESCE(array_to_string(attempted_credential_ids, ','), '') ILIKE '%' || $1 || '%' OR \
        COALESCE(array_to_string(rate_limited_credential_ids, ','), '') ILIKE '%' || $1 || '%' OR \
        COALESCE(conversation_id, '') ILIKE '%' || $1 || '%' OR \
        COALESCE(error_message, '') ILIKE '%' || $1 || '%' \
    ) \
    AND ($2::text IS NULL OR conversation_id = $2) \
    AND ($3::bigint IS NULL OR credential_id = $3 OR last_attempted_credential_id = $3 OR $3 = ANY(attempted_credential_ids) OR $3 = ANY(rate_limited_credential_ids)) \
    AND ($4::text IS NULL OR model = $4) \
    AND ($5::text IS NULL OR status = $5) \
    AND ($6::text IS NULL OR usage_source = $6) \
    AND ($7::bool IS NULL OR stream = $7) \
    AND ($8::int IS NULL OR cache_read_input_tokens >= $8)";

    // ----- totals: today / total / range 一次查完 -----
    let totals_sql = format!(
        "SELECT \
            COUNT(*) FILTER (WHERE created_at >= CURRENT_DATE)::bigint AS today_requests, \
            COALESCE(SUM(total_input_tokens) FILTER (WHERE created_at >= CURRENT_DATE), 0)::bigint AS today_input, \
            COALESCE(SUM(output_tokens) FILTER (WHERE created_at >= CURRENT_DATE), 0)::bigint AS today_output, \
            COALESCE(SUM(cost_usd) FILTER (WHERE created_at >= CURRENT_DATE), 0)::float8 AS today_cost, \
            COUNT(*)::bigint AS total_requests, \
            COALESCE(SUM(total_input_tokens), 0)::bigint AS total_input, \
            COALESCE(SUM(output_tokens), 0)::bigint AS total_output, \
            COALESCE(SUM(cost_usd), 0)::float8 AS total_cost, \
            COUNT(*) FILTER (WHERE created_at >= $9 AND created_at <= $10)::bigint AS range_requests, \
            COALESCE(SUM(total_input_tokens) FILTER (WHERE created_at >= $9 AND created_at <= $10), 0)::bigint AS range_input, \
            COALESCE(SUM(output_tokens) FILTER (WHERE created_at >= $9 AND created_at <= $10), 0)::bigint AS range_output, \
            COALESCE(SUM(cost_usd) FILTER (WHERE created_at >= $9 AND created_at <= $10), 0)::float8 AS range_cost \
         FROM usage_records WHERE {}",
        base_where
    );
    let totals_row = sqlx::query(&totals_sql)
        .bind(&filter.q)
        .bind(&filter.conversation_id)
        .bind(filter.credential_id.map(|v| v as i64))
        .bind(&filter.model)
        .bind(&filter.status)
        .bind(&filter.source)
        .bind(filter.stream)
        .bind(filter.min_cache_read)
        .bind(range_since)
        .bind(range_until)
        .fetch_one(db)
        .await?;

    // ----- timeline: 时间序列(限定在 range 内,按 bucket 聚合)-----
    let timeline_sql = format!(
        "SELECT \
            date_trunc('{trunc}', created_at) AS bucket, \
            COUNT(*)::bigint AS requests, \
            COALESCE(SUM(total_input_tokens), 0)::bigint AS tokens, \
            COALESCE(SUM(output_tokens), 0)::bigint AS output_tokens, \
            COALESCE(SUM(cost_usd), 0)::float8 AS cost_usd \
         FROM usage_records \
         WHERE {where_clause} AND created_at >= $9 AND created_at <= $10 \
         GROUP BY bucket ORDER BY bucket ASC",
        trunc = if bucket == "hour" { "hour" } else { "day" },
        where_clause = base_where,
    );
    let timeline_rows = sqlx::query(&timeline_sql)
        .bind(&filter.q)
        .bind(&filter.conversation_id)
        .bind(filter.credential_id.map(|v| v as i64))
        .bind(&filter.model)
        .bind(&filter.status)
        .bind(&filter.source)
        .bind(filter.stream)
        .bind(filter.min_cache_read)
        .bind(range_since)
        .bind(range_until)
        .fetch_all(db)
        .await?;

    // ----- by_model / by_credential 都改为 range 内聚合 -----
    let by_model_sql = format!(
        "SELECT model, \
            COUNT(*)::bigint AS requests, \
            COALESCE(SUM(total_input_tokens), 0)::bigint AS tokens, \
            COALESCE(SUM(output_tokens), 0)::bigint AS output_tokens, \
            COALESCE(SUM(cost_usd), 0)::float8 AS cost_usd \
         FROM usage_records \
         WHERE {} AND created_at >= $9 AND created_at <= $10 \
         GROUP BY model ORDER BY cost_usd DESC, requests DESC LIMIT 32",
        base_where
    );
    let by_model_rows = sqlx::query(&by_model_sql)
        .bind(&filter.q)
        .bind(&filter.conversation_id)
        .bind(filter.credential_id.map(|v| v as i64))
        .bind(&filter.model)
        .bind(&filter.status)
        .bind(&filter.source)
        .bind(filter.stream)
        .bind(filter.min_cache_read)
        .bind(range_since)
        .bind(range_until)
        .fetch_all(db)
        .await?;

    let by_cred_sql = format!(
        "SELECT credential_id, \
            COUNT(*)::bigint AS requests, \
            COALESCE(SUM(total_input_tokens), 0)::bigint AS tokens, \
            COALESCE(SUM(output_tokens), 0)::bigint AS output_tokens, \
            COALESCE(SUM(cost_usd), 0)::float8 AS cost_usd \
         FROM usage_records \
         WHERE {} AND credential_id IS NOT NULL AND created_at >= $9 AND created_at <= $10 \
         GROUP BY credential_id ORDER BY cost_usd DESC, requests DESC",
        base_where
    );
    let by_cred_rows = sqlx::query(&by_cred_sql)
        .bind(&filter.q)
        .bind(&filter.conversation_id)
        .bind(filter.credential_id.map(|v| v as i64))
        .bind(&filter.model)
        .bind(&filter.status)
        .bind(&filter.source)
        .bind(filter.stream)
        .bind(filter.min_cache_read)
        .bind(range_since)
        .bind(range_until)
        .fetch_all(db)
        .await?;

    let mut timeline = Vec::with_capacity(timeline_rows.len());
    for r in timeline_rows {
        timeline.push(UsageStatsBucket {
            bucket: r.try_get("bucket")?,
            requests: r.try_get("requests")?,
            tokens: r.try_get("tokens")?,
            output_tokens: r.try_get("output_tokens")?,
            cost_usd: r.try_get("cost_usd")?,
        });
    }

    let mut by_model = Vec::with_capacity(by_model_rows.len());
    for r in by_model_rows {
        by_model.push(UsageStatsByModel {
            model: r.try_get::<String, _>("model")?,
            requests: r.try_get("requests")?,
            tokens: r.try_get("tokens")?,
            output_tokens: r.try_get("output_tokens")?,
            cost_usd: r.try_get("cost_usd")?,
        });
    }
    let mut by_credential = Vec::with_capacity(by_cred_rows.len());
    for r in by_cred_rows {
        by_credential.push(UsageStatsByCredential {
            credential_id: r.try_get("credential_id")?,
            requests: r.try_get("requests")?,
            tokens: r.try_get("tokens")?,
            output_tokens: r.try_get("output_tokens")?,
            cost_usd: r.try_get("cost_usd")?,
        });
    }

    Ok(UsageStats {
        today_requests: totals_row.try_get("today_requests")?,
        today_tokens: totals_row.try_get("today_input")?,
        today_output_tokens: totals_row.try_get("today_output")?,
        today_cost_usd: totals_row.try_get("today_cost")?,
        total_requests: totals_row.try_get("total_requests")?,
        total_tokens: totals_row.try_get("total_input")?,
        total_output_tokens: totals_row.try_get("total_output")?,
        total_cost_usd: totals_row.try_get("total_cost")?,
        range_requests: totals_row.try_get("range_requests")?,
        range_tokens: totals_row.try_get("range_input")?,
        range_output_tokens: totals_row.try_get("range_output")?,
        range_cost_usd: totals_row.try_get("range_cost")?,
        range_since,
        range_until,
        timeline,
        bucket,
        by_model,
        by_credential,
    })
}

/// SQL 聚合的过滤条件,与 usage 列表筛选保持一致
#[derive(Debug, Clone, Default)]
pub struct UsageStatsFilter {
    pub q: Option<String>,
    pub conversation_id: Option<String>,
    pub credential_id: Option<u64>,
    pub model: Option<String>,
    pub status: Option<String>,
    pub source: Option<String>,
    pub stream: Option<bool>,
    pub min_cache_read: Option<i32>,
    pub since: Option<chrono::DateTime<chrono::Utc>>,
    pub until: Option<chrono::DateTime<chrono::Utc>>,
    pub bucket: Option<String>,
}

/// 从 PG 查询 usage_records,返回与内存版 query_page 一致的分页结果。
/// **数据权威**:cost_usd / client_user_agent / client_ip 等字段都从 PG 读出。
pub async fn query_records_from_pg(
    db: &crate::storage::Db,
    query: &UsageRecordQuery,
    page: usize,
    limit: usize,
) -> anyhow::Result<UsageRecordsPageResult> {
    use sqlx::Row;

    let page = normalize_page(page);
    let limit = normalize_limit(limit);
    let offset = (page.saturating_sub(1) as i64) * limit as i64;

    let q = query.q.clone();
    let conversation_id = query.conversation_id.clone();
    let credential_id = query.credential_id.map(|v| v as i64);
    let model = query.model.clone();
    let status_str = query.status.map(|s| {
        match s {
            UsageRecordStatus::Success => "success",
            UsageRecordStatus::Error => "error",
            UsageRecordStatus::StreamError => "stream_error",
            UsageRecordStatus::UpstreamTimeout => "upstream_timeout",
            UsageRecordStatus::ClientDropped => "client_dropped",
        }
        .to_string()
    });
    let source_str = query.source.map(|s| {
        match s {
            UsageSource::UpstreamMetadata => "upstream_metadata",
            UsageSource::LocalPromptCache => "local_prompt_cache",
            UsageSource::ContextEstimate => "context_estimate",
            UsageSource::RequestEstimate => "request_estimate",
            UsageSource::None => "none",
        }
        .to_string()
    });
    let stream = query.stream;
    let min_cache_read = query.min_cache_read;
    let since = query.since;
    let until = query.until;

    let where_sql = "(\
        $1::text IS NULL OR \
        model ILIKE '%' || $1 || '%' OR \
        COALESCE(credential_label, '') ILIKE '%' || $1 || '%' OR \
        COALESCE(last_attempted_credential_id::text, '') ILIKE '%' || $1 || '%' OR \
        COALESCE(array_to_string(attempted_credential_ids, ','), '') ILIKE '%' || $1 || '%' OR \
        COALESCE(array_to_string(rate_limited_credential_ids, ','), '') ILIKE '%' || $1 || '%' OR \
        COALESCE(conversation_id, '') ILIKE '%' || $1 || '%' OR \
        COALESCE(error_message, '') ILIKE '%' || $1 || '%' \
    ) \
    AND ($2::text IS NULL OR conversation_id = $2) \
    AND ($3::bigint IS NULL OR credential_id = $3 OR last_attempted_credential_id = $3 OR $3 = ANY(attempted_credential_ids) OR $3 = ANY(rate_limited_credential_ids)) \
    AND ($4::text IS NULL OR model = $4) \
    AND ($5::text IS NULL OR status = $5) \
    AND ($6::text IS NULL OR usage_source = $6) \
    AND ($7::bool IS NULL OR stream = $7) \
    AND ($8::int IS NULL OR cache_read_input_tokens >= $8) \
    AND ($9::timestamptz IS NULL OR created_at >= $9) \
    AND ($10::timestamptz IS NULL OR created_at <= $10)";

    // 1) total
    let count_sql = format!(
        "SELECT COUNT(*)::bigint AS total FROM usage_records WHERE {}",
        where_sql
    );
    let total: i64 = sqlx::query(&count_sql)
        .bind(&q)
        .bind(&conversation_id)
        .bind(credential_id)
        .bind(&model)
        .bind(&status_str)
        .bind(&source_str)
        .bind(stream)
        .bind(min_cache_read)
        .bind(since)
        .bind(until)
        .fetch_one(db)
        .await?
        .try_get("total")?;

    // 2) records(按时间倒序)
    let select_sql = format!(
        "SELECT id, request_id, created_at, endpoint, stream, model, conversation_id, \
                credential_id, credential_label, attempted_credential_ids, rate_limited_credential_ids, \
                last_attempted_credential_id, scheduler_blocked, status, usage_source, \
                error_type, error_message, \
                total_input_tokens, compat_input_tokens, billable_input_tokens, output_tokens, \
                cache_read_input_tokens, cache_creation_input_tokens, \
                cache_creation_5m_input_tokens, cache_creation_1h_input_tokens, \
                cost_usd::float8 AS cost_usd, \
                client_user_agent, host(client_ip) AS client_ip, \
                duration_ms, simulated, sticky_bound, fallback_from_sticky \
         FROM usage_records WHERE {} \
         ORDER BY created_at DESC LIMIT $11 OFFSET $12",
        where_sql
    );
    let rows = sqlx::query(&select_sql)
        .bind(&q)
        .bind(&conversation_id)
        .bind(credential_id)
        .bind(&model)
        .bind(&status_str)
        .bind(&source_str)
        .bind(stream)
        .bind(min_cache_read)
        .bind(since)
        .bind(until)
        .bind(limit as i64)
        .bind(offset)
        .fetch_all(db)
        .await?;

    let mut records = Vec::with_capacity(rows.len());
    for r in rows {
        let id: uuid::Uuid = r.try_get("id")?;
        let created_at: chrono::DateTime<chrono::Utc> = r.try_get("created_at")?;
        let request_id: Option<String> = r.try_get("request_id").ok();
        let status_text: String = r.try_get("status")?;
        let source_text: String = r.try_get("usage_source")?;
        let credential_id: Option<i64> = r.try_get("credential_id").ok();
        let attempted_credential_ids: Vec<i64> = r
            .try_get("attempted_credential_ids")
            .unwrap_or_else(|_| Vec::new());
        let rate_limited_credential_ids: Vec<i64> = r
            .try_get("rate_limited_credential_ids")
            .unwrap_or_else(|_| Vec::new());
        let last_attempted_credential_id: Option<i64> =
            r.try_get("last_attempted_credential_id").ok();

        records.push(UsageRecord {
            // 显示用 request_id(更接近 anthropic 习惯),fallback uuid
            id: request_id.clone().unwrap_or_else(|| id.to_string()),
            created_at: created_at.to_rfc3339(),
            endpoint: r.try_get("endpoint")?,
            stream: r.try_get("stream")?,
            model: r.try_get("model")?,
            conversation_id: r.try_get("conversation_id").ok(),
            credential_id: credential_id.map(|v| v as u64),
            credential_label: r.try_get("credential_label").ok(),
            attempted_credential_ids: attempted_credential_ids
                .into_iter()
                .filter_map(|v| u64::try_from(v).ok())
                .collect(),
            rate_limited_credential_ids: rate_limited_credential_ids
                .into_iter()
                .filter_map(|v| u64::try_from(v).ok())
                .collect(),
            last_attempted_credential_id: last_attempted_credential_id
                .and_then(|v| u64::try_from(v).ok()),
            scheduler_blocked: r.try_get("scheduler_blocked").unwrap_or(false),
            status: UsageRecordStatus::parse(&status_text).unwrap_or(UsageRecordStatus::Success),
            usage_source: UsageSource::parse(&source_text).unwrap_or(UsageSource::None),
            total_input_tokens: r.try_get::<i32, _>("total_input_tokens")?,
            compat_input_tokens: r.try_get::<i32, _>("compat_input_tokens")?,
            billable_input_tokens: r.try_get::<i32, _>("billable_input_tokens")?,
            output_tokens: r.try_get::<i32, _>("output_tokens")?,
            cache_read_input_tokens: r.try_get::<i32, _>("cache_read_input_tokens")?,
            cache_creation_input_tokens: r.try_get::<i32, _>("cache_creation_input_tokens")?,
            cache_creation_5m_input_tokens: r
                .try_get::<i32, _>("cache_creation_5m_input_tokens")?,
            cache_creation_1h_input_tokens: r
                .try_get::<i32, _>("cache_creation_1h_input_tokens")?,
            duration_ms: r.try_get::<i64, _>("duration_ms")? as u64,
            simulated: r.try_get("simulated")?,
            sticky_bound: r.try_get("sticky_bound")?,
            fallback_from_sticky: r.try_get("fallback_from_sticky")?,
            error_type: r.try_get("error_type").ok(),
            error_message: r.try_get("error_message").ok(),
            client_user_agent: r.try_get("client_user_agent").ok(),
            client_ip: r.try_get("client_ip").ok(),
            request_id,
            cost_usd: r.try_get("cost_usd").ok(),
        });
    }

    Ok(UsageRecordsPageResult {
        total: total.max(0) as usize,
        page,
        limit,
        total_pages: total_pages(total.max(0) as usize, limit),
        records,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record_with_time(
        id: &str,
        cache_read: i32,
        source: UsageSource,
        created_at: String,
    ) -> UsageRecord {
        UsageRecord {
            id: id.to_string(),
            created_at,
            endpoint: "/v1/messages".to_string(),
            stream: true,
            model: "claude-sonnet-4-5".to_string(),
            conversation_id: Some("session-a".to_string()),
            credential_id: Some(1),
            credential_label: Some("test@example.com".to_string()),
            attempted_credential_ids: vec![1],
            rate_limited_credential_ids: Vec::new(),
            last_attempted_credential_id: Some(1),
            scheduler_blocked: false,
            status: UsageRecordStatus::Success,
            usage_source: source,
            total_input_tokens: 100,
            compat_input_tokens: 50,
            billable_input_tokens: 50,
            output_tokens: 10,
            cache_read_input_tokens: cache_read,
            cache_creation_input_tokens: 5,
            cache_creation_5m_input_tokens: 5,
            cache_creation_1h_input_tokens: 0,
            duration_ms: 10,
            simulated: source.is_simulated(),
            sticky_bound: false,
            fallback_from_sticky: false,
            error_type: None,
            error_message: None,
            client_user_agent: None,
            client_ip: None,
            request_id: None,
            cost_usd: None,
        }
    }

    fn record(id: &str, cache_read: i32, source: UsageSource) -> UsageRecord {
        record_with_time(id, cache_read, source, Utc::now().to_rfc3339())
    }

    #[test]
    fn recorder_respects_limit_and_filters() {
        let recorder = UsageRecorder::new(2, None);
        recorder.record(record("1", 10, UsageSource::UpstreamMetadata));
        recorder.record(record("2", 20, UsageSource::LocalPromptCache));
        recorder.record(record("3", 30, UsageSource::LocalPromptCache));

        let all = recorder.query(UsageRecordQuery::default());
        assert_eq!(all.total, 2);
        assert_eq!(all.records[0].id, "3");

        let query = UsageRecordQuery {
            source: Some(UsageSource::LocalPromptCache),
            min_cache_read: Some(25),
            ..Default::default()
        };
        let filtered = recorder.query(query);
        assert_eq!(filtered.total, 1);
        assert_eq!(filtered.records[0].id, "3");
    }

    #[test]
    fn recorder_query_page_paginates_filtered_records() {
        let recorder = UsageRecorder::new(10, None);
        recorder.record(record("1", 10, UsageSource::LocalPromptCache));
        recorder.record(record("2", 20, UsageSource::LocalPromptCache));
        recorder.record(record("3", 30, UsageSource::LocalPromptCache));
        recorder.record(record("4", 40, UsageSource::UpstreamMetadata));

        let result = recorder.query_page(
            UsageRecordQuery {
                source: Some(UsageSource::LocalPromptCache),
                ..Default::default()
            },
            2,
            2,
        );

        assert_eq!(result.total, 3);
        assert_eq!(result.page, 2);
        assert_eq!(result.limit, 2);
        assert_eq!(result.total_pages, 2);
        assert_eq!(result.records.len(), 1);
        assert_eq!(result.records[0].id, "1");
    }

    #[test]
    fn recorder_search_matches_model_account_session_and_error_text() {
        let recorder = UsageRecorder::new(10, None);
        let mut first = record("1", 10, UsageSource::LocalPromptCache);
        first.model = "claude-sonnet-4-5".to_string();
        first.credential_label = Some("alpha@example.com".to_string());
        first.conversation_id = Some("session-alpha".to_string());
        recorder.record(first);

        let mut second = record("2", 20, UsageSource::UpstreamMetadata);
        second.model = "claude-opus-4-5".to_string();
        second.credential_id = Some(424_242);
        second.credential_label = Some("beta@example.com".to_string());
        second.conversation_id = Some("session-beta".to_string());
        second.error_message = Some("upstream quota exceeded".to_string());
        recorder.record(second);

        let by_model = recorder.query(UsageRecordQuery {
            q: Some("opus".to_string()),
            ..Default::default()
        });
        assert_eq!(by_model.total, 1);
        assert_eq!(by_model.records[0].id, "2");

        let by_account = recorder.query(UsageRecordQuery {
            q: Some("alpha@example".to_string()),
            ..Default::default()
        });
        assert_eq!(by_account.total, 1);
        assert_eq!(by_account.records[0].id, "1");

        let by_credential_id = recorder.query(UsageRecordQuery {
            q: Some("424242".to_string()),
            ..Default::default()
        });
        assert_eq!(by_credential_id.total, 1);
        assert_eq!(by_credential_id.records[0].id, "2");

        let by_error = recorder.query(UsageRecordQuery {
            q: Some("quota".to_string()),
            ..Default::default()
        });
        assert_eq!(by_error.total, 1);
        assert_eq!(by_error.records[0].id, "2");
    }

    #[test]
    fn summary_counts_high_cache_and_sources() {
        let recorder = UsageRecorder::new(10, None);
        recorder.record(record("1", 5, UsageSource::UpstreamMetadata));
        recorder.record(record("2", 20_000, UsageSource::LocalPromptCache));

        let summary = recorder.summary(10_000);
        assert_eq!(summary.total_requests, 2);
        assert_eq!(summary.success_requests, 2);
        assert_eq!(summary.high_cache_requests, 1);
        assert_eq!(summary.simulated_requests, 1);
        assert_eq!(summary.upstream_metadata_requests, 1);
        assert_eq!(summary.local_prompt_cache_requests, 1);
        assert_eq!(summary.local_prompt_cache_input_tokens, 100);
        assert_eq!(summary.local_prompt_cache_read_input_tokens, 20_000);
        assert_eq!(summary.local_prompt_cache_creation_input_tokens, 5);
        assert_eq!(summary.top_credentials[0].key, "1");
    }

    #[test]
    fn recorder_persists_recent_records_and_clear_truncates_file() {
        let path = std::env::temp_dir().join(format!(
            "kiro-rs-usage-test-{}.jsonl",
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));

        let recorder = UsageRecorder::new(2, Some(path.clone()));
        recorder.record(record("1", 10, UsageSource::UpstreamMetadata));
        recorder.record(record("2", 20, UsageSource::LocalPromptCache));
        recorder.record(record("3", 30, UsageSource::RequestEstimate));

        let reloaded = UsageRecorder::new(2, Some(path.clone()));
        let result = reloaded.query(UsageRecordQuery::default());
        assert_eq!(result.total, 2);
        assert_eq!(result.records[0].id, "3");
        assert_eq!(result.records[1].id, "2");

        reloaded.clear();
        let cleared = UsageRecorder::new(2, Some(path.clone()));
        assert_eq!(cleared.query(UsageRecordQuery::default()).total, 0);

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn recorder_filters_by_time_window_and_invalid_times_do_not_match() {
        let recorder = UsageRecorder::new(10, None);
        recorder.record(record_with_time(
            "old",
            10,
            UsageSource::UpstreamMetadata,
            "2026-01-01T00:00:00Z".to_string(),
        ));
        recorder.record(record_with_time(
            "new",
            20,
            UsageSource::LocalPromptCache,
            "2026-01-02T00:00:00Z".to_string(),
        ));
        recorder.record(record_with_time(
            "bad-time",
            30,
            UsageSource::RequestEstimate,
            "not-a-time".to_string(),
        ));

        let since = DateTime::parse_from_rfc3339("2026-01-01T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let filtered = recorder.query(UsageRecordQuery {
            since: Some(since),
            ..Default::default()
        });

        assert_eq!(filtered.total, 1);
        assert_eq!(filtered.records[0].id, "new");
    }
}
