use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration as StdDuration;

use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::kiro::call_trace::{KiroCredentialAttempt, summarize_attempts};
use crate::storage::postgres::PostgresUsageStore;

const DEFAULT_QUERY_LIMIT: usize = 100;
const DEFAULT_PAGE_QUERY_LIMIT: usize = 20;
const MAX_QUERY_LIMIT: usize = 1000;
const USAGE_WRITER_QUEUE_CAPACITY: usize = 4096;
const USAGE_WRITER_MAX_ATTEMPTS: u32 = 3;
pub const REALTIME_USAGE_WINDOW_SECS: u32 = 60;

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
    #[serde(default)]
    pub estimated_cost_usd: f64,
    #[serde(default)]
    pub pricing_available: bool,
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pricing_model: Option<String>,
    pub duration_ms: u64,
    pub simulated: bool,
    pub sticky_bound: bool,
    pub fallback_from_sticky: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub credential_attempts: Vec<KiroCredentialAttempt>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_detail: Option<String>,
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
    pub page: usize,
    pub limit: usize,
    pub has_next: bool,
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
    pub estimated_cost_usd: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageRealtimeStats {
    pub window_seconds: u32,
    pub requests: usize,
    pub rpm: f64,
    pub input_tpm: f64,
    pub output_tpm: f64,
    pub total_tpm: f64,
    pub billable_tpm: f64,
}

impl UsageRealtimeStats {
    pub fn empty(window_seconds: u32) -> Self {
        Self {
            window_seconds,
            requests: 0,
            rpm: 0.0,
            input_tpm: 0.0,
            output_tpm: 0.0,
            total_tpm: 0.0,
            billable_tpm: 0.0,
        }
    }

    pub fn from_totals(
        window_seconds: u32,
        requests: usize,
        input_tokens: i64,
        output_tokens: i64,
        billable_input_tokens: i64,
    ) -> Self {
        let scale = if window_seconds == 0 {
            0.0
        } else {
            60.0 / window_seconds as f64
        };
        let input_tpm = input_tokens.max(0) as f64 * scale;
        let output_tpm = output_tokens.max(0) as f64 * scale;
        Self {
            window_seconds,
            requests,
            rpm: requests as f64 * scale,
            input_tpm,
            output_tpm,
            total_tpm: input_tpm + output_tpm,
            billable_tpm: (billable_input_tokens.max(0) + output_tokens.max(0)) as f64 * scale,
        }
    }
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
    pub total_estimated_cost_usd: f64,
    pub priced_requests: usize,
    pub unpriced_requests: usize,
    pub local_prompt_cache_requests: usize,
    pub local_prompt_cache_input_tokens: i64,
    pub local_prompt_cache_read_input_tokens: i64,
    pub local_prompt_cache_creation_input_tokens: i64,
    pub simulated_requests: usize,
    pub upstream_metadata_requests: usize,
    pub realtime: UsageRealtimeStats,
    pub top_credentials: Vec<UsageAggregate>,
    pub top_conversations: Vec<UsageAggregate>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageRecorderStats {
    pub in_memory_limit: usize,
    pub in_memory_records: usize,
    pub postgres_enabled: bool,
    pub writer_queue_enabled: bool,
    pub writer_queue_capacity: usize,
    pub writer_queue_available: usize,
    pub dropped_persist_records: u64,
}

pub struct UsageRecorder {
    records: Mutex<VecDeque<UsageRecord>>,
    limit: usize,
    postgres_store: Option<Arc<PostgresUsageStore>>,
    writer_tx: Option<mpsc::Sender<UsageRecord>>,
    dropped_persist_records: AtomicU64,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct CredentialCostSummary {
    pub estimated_cost_usd: f64,
    pub priced_requests: usize,
    pub unpriced_requests: usize,
}

impl UsageRecorder {
    #[cfg(test)]
    pub fn new(limit: usize) -> Self {
        let limit = limit.max(1);
        Self {
            records: Mutex::new(VecDeque::with_capacity(limit.min(1024))),
            limit,
            postgres_store: None,
            writer_tx: None,
            dropped_persist_records: AtomicU64::new(0),
        }
    }

    pub fn with_postgres(limit: usize, postgres_store: Arc<PostgresUsageStore>) -> Self {
        let writer_tx = if tokio::runtime::Handle::try_current().is_ok() {
            let (tx, rx) = mpsc::channel(USAGE_WRITER_QUEUE_CAPACITY);
            tokio::spawn(usage_writer_loop(postgres_store.clone(), rx));
            Some(tx)
        } else {
            tracing::warn!(
                "创建 UsageRecorder 时没有运行中的 Tokio runtime，将同步写入 PgSQL usage"
            );
            None
        };
        Self {
            records: Mutex::new(VecDeque::with_capacity(limit.max(1).min(1024))),
            limit: limit.max(1),
            postgres_store: Some(postgres_store),
            writer_tx,
            dropped_persist_records: AtomicU64::new(0),
        }
    }

    pub fn record(&self, record: UsageRecord) {
        {
            let mut records = self.records.lock();
            records.push_back(record.clone());
            while records.len() > self.limit {
                records.pop_front();
            }
        }

        if let Some(tx) = &self.writer_tx {
            match tx.try_send(record.clone()) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => {
                    let dropped = self.dropped_persist_records.fetch_add(1, Ordering::Relaxed) + 1;
                    tracing::warn!(dropped, "PgSQL usage 写入队列已满，本条 usage 持久化被丢弃");
                }
                Err(mpsc::error::TrySendError::Closed(record)) => {
                    self.persist_usage_sync(record);
                }
            }
        } else {
            self.persist_usage_sync(record);
        }
    }

    pub fn writer_stats(&self) -> UsageRecorderStats {
        let in_memory_records = self.records.lock().len();
        let (writer_queue_enabled, writer_queue_capacity, writer_queue_available) =
            if let Some(tx) = &self.writer_tx {
                (true, tx.max_capacity(), tx.capacity())
            } else {
                (false, 0, 0)
            };
        UsageRecorderStats {
            in_memory_limit: self.limit,
            in_memory_records,
            postgres_enabled: self.postgres_store.is_some(),
            writer_queue_enabled,
            writer_queue_capacity,
            writer_queue_available,
            dropped_persist_records: self.dropped_persist_records.load(Ordering::Relaxed),
        }
    }

    fn persist_usage_sync(&self, record: UsageRecord) {
        if let Some(store) = &self.postgres_store {
            let store = store.clone();
            if let Err(err) = block_on_usage_store(async move { store.record(record).await }) {
                tracing::warn!("写入 PgSQL usage record 失败: {}", err);
            }
        }
    }

    pub fn query(&self, query: UsageRecordQuery) -> UsageRecordsResult {
        if let Some(store) = &self.postgres_store {
            let store = store.clone();
            let db_query = query.clone();
            return block_on_usage_store(async move { store.query(db_query).await })
                .unwrap_or_else(|err| {
                    tracing::warn!("查询 PgSQL usage records 失败，回退内存记录: {}", err);
                    self.query_memory(query)
                });
        }
        self.query_memory(query)
    }

    fn query_memory(&self, query: UsageRecordQuery) -> UsageRecordsResult {
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
        if let Some(store) = &self.postgres_store {
            let store = store.clone();
            let query_for_fallback = query.clone();
            return block_on_usage_store(async move { store.query_page(query, page, limit).await })
                .unwrap_or_else(|err| {
                    tracing::warn!("分页查询 PgSQL usage records 失败，回退内存记录: {}", err);
                    self.query_page_memory(query_for_fallback, page, limit)
                });
        }
        self.query_page_memory(query, page, limit)
    }

    fn query_page_memory(
        &self,
        query: UsageRecordQuery,
        page: usize,
        limit: usize,
    ) -> UsageRecordsPageResult {
        let page = normalize_page(page);
        let limit = normalize_page_limit(limit);
        let start = page.saturating_sub(1).saturating_mul(limit);
        let mut records: Vec<UsageRecord> = self
            .records
            .lock()
            .iter()
            .rev()
            .filter(|record| record_matches(record, &query))
            .skip(start)
            .take(limit.saturating_add(1))
            .cloned()
            .collect();
        let has_next = records.len() > limit;
        if has_next {
            records.truncate(limit);
        }

        UsageRecordsPageResult {
            page,
            limit,
            has_next,
            records,
        }
    }

    pub fn summary(&self, high_cache_threshold: i32) -> UsageSummary {
        if let Some(store) = &self.postgres_store {
            let store = store.clone();
            return block_on_usage_store(async move { store.summary(high_cache_threshold).await })
                .unwrap_or_else(|err| {
                    tracing::warn!("汇总 PgSQL usage records 失败，回退内存记录: {}", err);
                    self.summary_memory(high_cache_threshold)
                });
        }
        self.summary_memory(high_cache_threshold)
    }

    fn summary_memory(&self, high_cache_threshold: i32) -> UsageSummary {
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
            total_estimated_cost_usd: 0.0,
            priced_requests: 0,
            unpriced_requests: 0,
            local_prompt_cache_requests: 0,
            local_prompt_cache_input_tokens: 0,
            local_prompt_cache_read_input_tokens: 0,
            local_prompt_cache_creation_input_tokens: 0,
            simulated_requests: 0,
            upstream_metadata_requests: 0,
            realtime: UsageRealtimeStats::empty(REALTIME_USAGE_WINDOW_SECS),
            top_credentials: Vec::new(),
            top_conversations: Vec::new(),
        };
        let mut credentials: HashMap<String, UsageAggregate> = HashMap::new();
        let mut conversations: HashMap<String, UsageAggregate> = HashMap::new();
        let realtime_cutoff =
            Utc::now() - chrono::Duration::seconds(REALTIME_USAGE_WINDOW_SECS as i64);
        let mut realtime_requests = 0usize;
        let mut realtime_input_tokens = 0i64;
        let mut realtime_output_tokens = 0i64;
        let mut realtime_billable_input_tokens = 0i64;

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
            summary.total_estimated_cost_usd += record.estimated_cost_usd;
            if record.pricing_available {
                summary.priced_requests += 1;
            } else {
                summary.unpriced_requests += 1;
            }
            if record.usage_source == UsageSource::LocalPromptCache {
                summary.local_prompt_cache_requests += 1;
                summary.local_prompt_cache_input_tokens += record.total_input_tokens as i64;
                summary.local_prompt_cache_read_input_tokens +=
                    record.cache_read_input_tokens as i64;
                summary.local_prompt_cache_creation_input_tokens +=
                    record.cache_creation_input_tokens as i64;
            }
            if DateTime::parse_from_rfc3339(&record.created_at)
                .map(|created_at| created_at.with_timezone(&Utc) >= realtime_cutoff)
                .unwrap_or(false)
            {
                realtime_requests += 1;
                realtime_input_tokens += record.total_input_tokens as i64;
                realtime_output_tokens += record.output_tokens as i64;
                realtime_billable_input_tokens += record.billable_input_tokens as i64;
            }

            if let Some(id) = record.credential_id {
                let key = id.to_string();
                let entry = credentials.entry(key.clone()).or_insert(UsageAggregate {
                    key,
                    label: record.credential_label.clone(),
                    requests: 0,
                    cache_read_input_tokens: 0,
                    cache_creation_input_tokens: 0,
                    estimated_cost_usd: 0.0,
                });
                entry.requests += 1;
                entry.cache_read_input_tokens += record.cache_read_input_tokens as i64;
                entry.cache_creation_input_tokens += record.cache_creation_input_tokens as i64;
                entry.estimated_cost_usd += record.estimated_cost_usd;
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
                            estimated_cost_usd: 0.0,
                        });
                entry.requests += 1;
                entry.cache_read_input_tokens += record.cache_read_input_tokens as i64;
                entry.cache_creation_input_tokens += record.cache_creation_input_tokens as i64;
                entry.estimated_cost_usd += record.estimated_cost_usd;
            }
        }

        summary.top_credentials = top_aggregates(credentials);
        summary.top_conversations = top_aggregates(conversations);
        summary.realtime = UsageRealtimeStats::from_totals(
            REALTIME_USAGE_WINDOW_SECS,
            realtime_requests,
            realtime_input_tokens,
            realtime_output_tokens,
            realtime_billable_input_tokens,
        );
        summary
    }

    pub fn credential_cost_summary(&self) -> HashMap<u64, CredentialCostSummary> {
        if let Some(store) = &self.postgres_store {
            let store = store.clone();
            return block_on_usage_store(async move { store.credential_cost_summary().await })
                .unwrap_or_else(|err| {
                    tracing::warn!("汇总 PgSQL 凭据费用失败，回退内存记录: {}", err);
                    self.credential_cost_summary_memory()
                });
        }
        self.credential_cost_summary_memory()
    }

    fn credential_cost_summary_memory(&self) -> HashMap<u64, CredentialCostSummary> {
        let mut summaries: HashMap<u64, CredentialCostSummary> = HashMap::new();
        for record in self.records.lock().iter() {
            let Some(credential_id) = record.credential_id else {
                continue;
            };
            let entry = summaries.entry(credential_id).or_default();
            entry.estimated_cost_usd += record.estimated_cost_usd;
            if record.pricing_available {
                entry.priced_requests += 1;
            } else {
                entry.unpriced_requests += 1;
            }
        }
        summaries
    }

    pub fn clear(&self) {
        self.records.lock().clear();
        if let Some(store) = &self.postgres_store {
            let store = store.clone();
            if let Err(err) = block_on_usage_store(async move { store.clear().await }) {
                tracing::warn!("清空 PgSQL usage records 失败: {}", err);
            }
        }
    }
}

async fn usage_writer_loop(store: Arc<PostgresUsageStore>, mut rx: mpsc::Receiver<UsageRecord>) {
    while let Some(record) = rx.recv().await {
        let mut attempt = 1;
        loop {
            match store.record(record.clone()).await {
                Ok(()) => break,
                Err(err) if attempt < USAGE_WRITER_MAX_ATTEMPTS => {
                    let delay_ms = 100u64.saturating_mul(2u64.saturating_pow(attempt - 1));
                    tracing::warn!(
                        request_id = %record.id,
                        attempt,
                        "写入 PgSQL usage record 失败，准备重试: {}",
                        err
                    );
                    tokio::time::sleep(StdDuration::from_millis(delay_ms)).await;
                    attempt += 1;
                }
                Err(err) => {
                    tracing::warn!(
                        request_id = %record.id,
                        attempt,
                        "写入 PgSQL usage record 最终失败，已放弃本条持久化: {}",
                        err
                    );
                    break;
                }
            }
        }
    }
}

fn block_on_usage_store<T>(
    future: impl std::future::Future<Output = anyhow::Result<T>>,
) -> anyhow::Result<T> {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        tokio::task::block_in_place(|| handle.block_on(future))
    } else {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(future)
    }
}

fn normalize_limit(limit: usize) -> usize {
    if limit == 0 {
        DEFAULT_QUERY_LIMIT
    } else {
        limit.min(MAX_QUERY_LIMIT)
    }
}

fn normalize_page_limit(limit: usize) -> usize {
    if limit == 0 {
        DEFAULT_PAGE_QUERY_LIMIT
    } else {
        limit.min(MAX_QUERY_LIMIT)
    }
}

fn normalize_page(page: usize) -> usize {
    page.max(1)
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
        if record.credential_id != Some(credential_id) {
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
    let estimated_cost = record.estimated_cost_usd.to_string();
    let attempt_chain = summarize_attempts(&record.credential_attempts);

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
        record.error_detail.as_deref(),
        record.pricing_model.as_deref(),
        Some(estimated_cost.as_str()),
        Some(attempt_chain.as_str()),
        credential_id.as_deref(),
    ]
    .into_iter()
    .flatten()
    .any(|value| value.to_ascii_lowercase().contains(&q))
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
            std::cmp::Reverse((item.estimated_cost_usd * 1_000_000.0).round() as i64),
            std::cmp::Reverse(item.requests),
            std::cmp::Reverse(item.cache_read_input_tokens),
        )
    });
    values.truncate(10);
    values
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
            estimated_cost_usd: 0.001,
            pricing_available: true,
            pricing_model: Some("claude-sonnet-4-5".to_string()),
            duration_ms: 10,
            simulated: source.is_simulated(),
            sticky_bound: false,
            fallback_from_sticky: false,
            credential_attempts: Vec::new(),
            error_type: None,
            error_message: None,
            error_detail: None,
        }
    }

    fn record(id: &str, cache_read: i32, source: UsageSource) -> UsageRecord {
        record_with_time(id, cache_read, source, Utc::now().to_rfc3339())
    }

    #[test]
    fn recorder_respects_limit_and_filters() {
        let recorder = UsageRecorder::new(2);
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
        let recorder = UsageRecorder::new(10);
        recorder.record(record("1", 10, UsageSource::LocalPromptCache));
        recorder.record(record("2", 20, UsageSource::LocalPromptCache));
        recorder.record(record("3", 30, UsageSource::LocalPromptCache));
        recorder.record(record("4", 40, UsageSource::UpstreamMetadata));

        let first_page = recorder.query_page(
            UsageRecordQuery {
                source: Some(UsageSource::LocalPromptCache),
                ..Default::default()
            },
            1,
            2,
        );

        assert_eq!(first_page.page, 1);
        assert_eq!(first_page.limit, 2);
        assert!(first_page.has_next);
        assert_eq!(first_page.records.len(), 2);
        assert_eq!(first_page.records[0].id, "3");
        assert_eq!(first_page.records[1].id, "2");

        let second_page = recorder.query_page(
            UsageRecordQuery {
                source: Some(UsageSource::LocalPromptCache),
                ..Default::default()
            },
            2,
            2,
        );

        assert_eq!(second_page.page, 2);
        assert_eq!(second_page.limit, 2);
        assert!(!second_page.has_next);
        assert_eq!(second_page.records.len(), 1);
        assert_eq!(second_page.records[0].id, "1");
    }

    #[test]
    fn recorder_query_page_defaults_to_twenty_and_uses_has_next() {
        let recorder = UsageRecorder::new(25);
        for index in 1..=21 {
            recorder.record(record(
                &index.to_string(),
                index,
                UsageSource::LocalPromptCache,
            ));
        }

        let first_page = recorder.query_page(UsageRecordQuery::default(), 1, 0);

        assert_eq!(first_page.page, 1);
        assert_eq!(first_page.limit, 20);
        assert!(first_page.has_next);
        assert_eq!(first_page.records.len(), 20);
        assert_eq!(first_page.records[0].id, "21");
        assert_eq!(first_page.records[19].id, "2");

        let second_page = recorder.query_page(UsageRecordQuery::default(), 2, 0);

        assert_eq!(second_page.limit, 20);
        assert!(!second_page.has_next);
        assert_eq!(second_page.records.len(), 1);
        assert_eq!(second_page.records[0].id, "1");
    }

    #[test]
    fn recorder_search_matches_model_account_session_and_error_text() {
        let recorder = UsageRecorder::new(10);
        let mut first = record("1", 10, UsageSource::LocalPromptCache);
        first.model = "claude-sonnet-4-5".to_string();
        first.credential_label = Some("alpha@example.com".to_string());
        first.conversation_id = Some("session-alpha".to_string());
        recorder.record(first);

        let mut second = record("2", 20, UsageSource::UpstreamMetadata);
        second.model = "claude-opus-4-5".to_string();
        second.credential_id = Some(42);
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
            q: Some("beta@example".to_string()),
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

        let mut chained = record("3", 30, UsageSource::LocalPromptCache);
        chained.credential_attempts = vec![
            KiroCredentialAttempt::new(
                0,
                6,
                Some("first@example.com".to_string()),
                Some(reqwest::StatusCode::TOO_MANY_REQUESTS),
                "transient_retry",
                Some("transient_error"),
                Some("429 Too Many Requests".to_string()),
                10,
            ),
            KiroCredentialAttempt::new(
                1,
                9,
                Some("second@example.com".to_string()),
                Some(reqwest::StatusCode::OK),
                "success",
                None::<&str>,
                None::<String>,
                20,
            ),
        ];
        recorder.record(chained);

        let by_chain = recorder.query(UsageRecordQuery {
            q: Some("#6(429)>#9(200)".to_string()),
            ..Default::default()
        });
        assert_eq!(by_chain.total, 1);
        assert_eq!(by_chain.records[0].id, "3");
    }

    #[test]
    fn summary_counts_high_cache_and_sources() {
        let recorder = UsageRecorder::new(10);
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
        assert_eq!(summary.realtime.window_seconds, REALTIME_USAGE_WINDOW_SECS);
        assert_eq!(summary.realtime.requests, 2);
        assert_eq!(summary.realtime.rpm, 2.0);
        assert_eq!(summary.realtime.total_tpm, 220.0);
        assert_eq!(summary.realtime.billable_tpm, 120.0);
        assert_eq!(summary.top_credentials[0].key, "1");
    }

    #[test]
    fn recorder_filters_by_time_window_and_invalid_times_do_not_match() {
        let recorder = UsageRecorder::new(10);
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
