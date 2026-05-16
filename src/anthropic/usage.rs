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
}

#[derive(Debug, Clone)]
pub struct UsageRecordQuery {
    pub limit: usize,
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
}

impl UsageRecorder {
    pub fn new(limit: usize, persist_path: Option<PathBuf>) -> Self {
        let limit = limit.max(1);
        let recorder = Self {
            records: Mutex::new(VecDeque::with_capacity(limit.min(1024))),
            limit,
            persist_path,
        };
        recorder.load_recent_records();
        recorder
    }

    pub fn record(&self, record: UsageRecord) {
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

fn record_matches(record: &UsageRecord, query: &UsageRecordQuery) -> bool {
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
            duration_ms: 10,
            simulated: source.is_simulated(),
            sticky_bound: false,
            fallback_from_sticky: false,
            error_type: None,
            error_message: None,
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
