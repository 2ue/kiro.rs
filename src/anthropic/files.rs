use std::collections::{HashMap, VecDeque};

use axum::{
    body::Body,
    extract::{Multipart, Path, State},
    http::{StatusCode, header},
    response::{IntoResponse, Json, Response},
};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use bytes::Bytes;
use chrono::Utc;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tokio::sync::{Semaphore, SemaphorePermit};
use uuid::Uuid;

use super::{envelope, middleware::AppState, types::Message};

pub(crate) const MAX_FILE_BYTES: usize = 50 * 1024 * 1024;
pub(crate) const MAX_FILE_UPLOAD_BODY_SIZE: usize = MAX_FILE_BYTES + 1024 * 1024;
const MAX_STORED_FILES: usize = 128;
const MAX_TOTAL_BYTES: usize = 256 * 1024 * 1024;
const MAX_CONCURRENT_FILE_UPLOADS: usize = 2;
const MAX_FILE_SOURCES_PER_REQUEST: usize = 20;
const MAX_FILE_IMAGE_SOURCE_BYTES: usize = 5 * 1024 * 1024;
const MAX_FILE_DOCUMENT_SOURCE_BYTES: usize = 20 * 1024 * 1024;
const MAX_FILE_SOURCE_BYTES_PER_REQUEST: usize = 32 * 1024 * 1024;
const MAX_FILE_MATERIALIZED_BYTES_PER_REQUEST: usize = 44 * 1024 * 1024;

#[derive(Debug, Clone, Copy)]
struct FileMaterializationLimits {
    max_sources: usize,
    max_image_bytes: usize,
    max_document_bytes: usize,
    max_source_bytes: usize,
    max_materialized_bytes: usize,
}

const FILE_MATERIALIZATION_LIMITS: FileMaterializationLimits = FileMaterializationLimits {
    max_sources: MAX_FILE_SOURCES_PER_REQUEST,
    max_image_bytes: MAX_FILE_IMAGE_SOURCE_BYTES,
    max_document_bytes: MAX_FILE_DOCUMENT_SOURCE_BYTES,
    max_source_bytes: MAX_FILE_SOURCE_BYTES_PER_REQUEST,
    max_materialized_bytes: MAX_FILE_MATERIALIZED_BYTES_PER_REQUEST,
};

#[derive(Debug, Default)]
struct FileMaterializationBudget {
    sources: usize,
    source_bytes: usize,
    materialized_bytes: usize,
}

impl FileMaterializationBudget {
    fn reserve(
        &mut self,
        block_type: &str,
        source_bytes: usize,
        limits: FileMaterializationLimits,
    ) -> Result<(), String> {
        let source_limit = match block_type {
            "image" => limits.max_image_bytes,
            "document" => limits.max_document_bytes,
            _ => return Err(format!("unsupported file source block: {block_type}")),
        };
        if source_bytes > source_limit {
            return Err(format!(
                "uploaded {block_type} exceeds {source_limit} bytes"
            ));
        }
        let next_sources = self.sources.saturating_add(1);
        if next_sources > limits.max_sources {
            return Err(format!("file source count exceeds {}", limits.max_sources));
        }
        let next_source_bytes = self.source_bytes.saturating_add(source_bytes);
        if next_source_bytes > limits.max_source_bytes {
            return Err(format!(
                "file source bytes exceed {}",
                limits.max_source_bytes
            ));
        }
        let encoded_bytes = source_bytes.saturating_add(2) / 3 * 4;
        let next_materialized_bytes = self.materialized_bytes.saturating_add(encoded_bytes);
        if next_materialized_bytes > limits.max_materialized_bytes {
            return Err(format!(
                "materialized file source bytes exceed {}",
                limits.max_materialized_bytes
            ));
        }
        self.sources = next_sources;
        self.source_bytes = next_source_bytes;
        self.materialized_bytes = next_materialized_bytes;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub(crate) struct StoredFile {
    pub id: String,
    pub filename: String,
    pub media_type: String,
    pub bytes: Bytes,
    pub created_at: i64,
}

impl StoredFile {
    fn size_bytes(&self) -> usize {
        self.bytes.len()
    }

    fn metadata_json(&self) -> Value {
        json!({
            "id": self.id,
            "type": "file",
            "object": "file",
            "filename": self.filename,
            "mime_type": self.media_type,
            "size_bytes": self.size_bytes(),
            "created_at": self.created_at
        })
    }
}

#[derive(Debug, Default)]
struct FileStoreInner {
    files: HashMap<String, StoredFile>,
    order: VecDeque<String>,
    total_bytes: usize,
}

#[derive(Debug)]
pub(crate) struct AnthropicFileStore {
    inner: Mutex<FileStoreInner>,
    upload_permits: Semaphore,
}

impl Default for AnthropicFileStore {
    fn default() -> Self {
        Self {
            inner: Mutex::new(FileStoreInner::default()),
            upload_permits: Semaphore::new(MAX_CONCURRENT_FILE_UPLOADS),
        }
    }
}

impl AnthropicFileStore {
    pub(crate) fn insert(
        &self,
        filename: impl Into<String>,
        media_type: impl Into<String>,
        bytes: Vec<u8>,
    ) -> Result<StoredFile, String> {
        if bytes.is_empty() {
            return Err("uploaded file is empty".to_string());
        }
        if bytes.len() > MAX_FILE_BYTES {
            return Err(format!("uploaded file exceeds {} bytes", MAX_FILE_BYTES));
        }

        let id = format!("file_{}", Uuid::new_v4().simple());
        let file = StoredFile {
            id: id.clone(),
            filename: sanitize_filename(filename.into()),
            media_type: normalize_media_type(&media_type.into()),
            bytes: Bytes::from(bytes),
            created_at: Utc::now().timestamp(),
        };

        let mut inner = self.inner.lock();
        inner.total_bytes = inner.total_bytes.saturating_add(file.size_bytes());
        inner.order.push_back(id.clone());
        inner.files.insert(id.clone(), file.clone());

        while inner.files.len() > MAX_STORED_FILES || inner.total_bytes > MAX_TOTAL_BYTES {
            let Some(oldest) = inner.order.pop_front() else {
                break;
            };
            if oldest == id && inner.files.len() <= 1 {
                break;
            }
            if let Some(removed) = inner.files.remove(&oldest) {
                inner.total_bytes = inner.total_bytes.saturating_sub(removed.size_bytes());
            }
        }

        Ok(file)
    }

    pub(crate) fn get(&self, id: &str) -> Option<StoredFile> {
        self.inner.lock().files.get(id).cloned()
    }

    pub(crate) fn delete(&self, id: &str) -> bool {
        let mut inner = self.inner.lock();
        let Some(file) = inner.files.remove(id) else {
            return false;
        };
        inner.total_bytes = inner.total_bytes.saturating_sub(file.size_bytes());
        inner.order.retain(|queued_id| queued_id != id);
        true
    }

    pub(crate) fn list(&self) -> Vec<StoredFile> {
        let inner = self.inner.lock();
        inner
            .order
            .iter()
            .filter_map(|id| inner.files.get(id).cloned())
            .collect()
    }

    fn try_begin_upload(&self) -> Result<SemaphorePermit<'_>, ()> {
        self.upload_permits.try_acquire().map_err(|_| ())
    }
}

pub(crate) async fn upload_file(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> Response {
    let _upload_permit = match state.file_store.try_begin_upload() {
        Ok(permit) => permit,
        Err(()) => {
            return envelope::error_response(
                StatusCode::TOO_MANY_REQUESTS,
                "rate_limit_error",
                "too many file uploads are already in progress",
            );
        }
    };
    let mut selected: Option<(String, String, Vec<u8>)> = None;

    while let Ok(Some(mut field)) = multipart.next_field().await {
        let field_name = field.name().unwrap_or_default().to_string();
        let filename = field.file_name().map(str::to_string);
        if field_name != "file" && filename.is_none() && selected.is_some() {
            continue;
        }

        let filename = filename.unwrap_or_else(|| "upload.bin".to_string());
        let media_type = field
            .content_type()
            .map(str::to_string)
            .or_else(|| {
                mime_guess::from_path(&filename)
                    .first_raw()
                    .map(str::to_string)
            })
            .unwrap_or_else(|| "application/octet-stream".to_string());
        let mut bytes = Vec::with_capacity(16 * 1024);
        loop {
            match field.chunk().await {
                Ok(Some(chunk)) => {
                    if let Err(message) =
                        append_bounded_upload_chunk(&mut bytes, &chunk, MAX_FILE_BYTES)
                    {
                        return envelope::error_response(
                            StatusCode::PAYLOAD_TOO_LARGE,
                            "invalid_request_error",
                            message,
                        );
                    }
                }
                Ok(None) => break,
                Err(err) => {
                    return envelope::error_response(
                        StatusCode::BAD_REQUEST,
                        "invalid_request_error",
                        format!("failed to read uploaded file: {}", err),
                    );
                }
            }
        }

        selected = Some((filename, media_type, bytes));
        if field_name == "file" {
            break;
        }
    }

    let Some((filename, media_type, bytes)) = selected else {
        return envelope::error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "multipart upload must include a file field",
        );
    };

    match state.file_store.insert(filename, media_type, bytes) {
        Ok(file) => Json(file.metadata_json()).into_response(),
        Err(message) => {
            envelope::error_response(StatusCode::BAD_REQUEST, "invalid_request_error", message)
        }
    }
}

fn append_bounded_upload_chunk(
    buffer: &mut Vec<u8>,
    chunk: &[u8],
    max_bytes: usize,
) -> Result<(), String> {
    if chunk.len() > max_bytes.saturating_sub(buffer.len()) {
        return Err(format!("uploaded file exceeds {} bytes", max_bytes));
    }
    buffer.extend_from_slice(chunk);
    Ok(())
}

pub(crate) async fn list_files(State(state): State<AppState>) -> impl IntoResponse {
    let data = state
        .file_store
        .list()
        .into_iter()
        .map(|file| file.metadata_json())
        .collect::<Vec<_>>();
    let first_id = data.first().and_then(|item| item.get("id")).cloned();
    let last_id = data.last().and_then(|item| item.get("id")).cloned();
    Json(json!({
        "object": "list",
        "data": data,
        "has_more": false,
        "first_id": first_id,
        "last_id": last_id
    }))
}

pub(crate) async fn get_file(
    State(state): State<AppState>,
    Path(file_id): Path<String>,
) -> Response {
    get_file_by_id(state, file_id)
}

pub(crate) async fn get_file_dfcache(
    State(state): State<AppState>,
    Path((_route, file_id)): Path<(String, String)>,
) -> Response {
    get_file_by_id(state, file_id)
}

pub(crate) async fn get_file_content(
    State(state): State<AppState>,
    Path(file_id): Path<String>,
) -> Response {
    get_file_content_by_id(state, file_id)
}

pub(crate) async fn get_file_content_dfcache(
    State(state): State<AppState>,
    Path((_route, file_id)): Path<(String, String)>,
) -> Response {
    get_file_content_by_id(state, file_id)
}

pub(crate) async fn delete_file(
    State(state): State<AppState>,
    Path(file_id): Path<String>,
) -> impl IntoResponse {
    delete_file_by_id(state, file_id)
}

pub(crate) async fn delete_file_dfcache(
    State(state): State<AppState>,
    Path((_route, file_id)): Path<(String, String)>,
) -> impl IntoResponse {
    delete_file_by_id(state, file_id)
}

fn get_file_by_id(state: AppState, file_id: String) -> Response {
    match state.file_store.get(&file_id) {
        Some(file) => Json(file.metadata_json()).into_response(),
        None => {
            envelope::error_response(StatusCode::NOT_FOUND, "not_found_error", "file not found")
        }
    }
}

fn get_file_content_by_id(state: AppState, file_id: String) -> Response {
    let Some(file) = state.file_store.get(&file_id) else {
        return envelope::error_response(
            StatusCode::NOT_FOUND,
            "not_found_error",
            "file not found",
        );
    };

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, file.media_type)
        .body(Body::from(file.bytes))
        .unwrap_or_else(|_| {
            envelope::error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "api_error",
                envelope::PUBLIC_PROCESSING_FAILED_MESSAGE,
            )
        })
}

fn delete_file_by_id(state: AppState, file_id: String) -> impl IntoResponse {
    let deleted = state.file_store.delete(&file_id);
    Json(json!({
        "id": file_id,
        "type": "file_deleted",
        "deleted": deleted
    }))
}

pub(crate) fn materialize_file_sources(
    store: &AnthropicFileStore,
    messages: &mut [Message],
) -> Result<usize, String> {
    let mut budget = FileMaterializationBudget::default();
    let mut materialized = 0usize;
    for message in messages {
        materialized +=
            materialize_file_sources_in_content(store, &mut message.content, &mut budget)?;
    }
    Ok(materialized)
}

fn materialize_file_sources_in_content(
    store: &AnthropicFileStore,
    content: &mut Value,
    budget: &mut FileMaterializationBudget,
) -> Result<usize, String> {
    let Value::Array(items) = content else {
        return Ok(0);
    };

    let mut materialized = 0usize;
    for item in items {
        let Some(obj) = item.as_object_mut() else {
            continue;
        };
        let Some(block_type) = obj.get("type").and_then(Value::as_str).map(str::to_string) else {
            continue;
        };
        if block_type != "image" && block_type != "document" {
            continue;
        }
        let Some(source) = obj.get_mut("source").and_then(Value::as_object_mut) else {
            continue;
        };
        let Some(file_id) = source_file_id(source) else {
            continue;
        };
        let Some(file) = store.get(&file_id) else {
            return Err(format!("file source not found: {}", file_id));
        };
        let media_type = media_type_for_block(&block_type, &file).ok_or_else(|| {
            format!(
                "uploaded file {} has unsupported {} media type: {}",
                file_id, block_type, file.media_type
            )
        })?;
        budget.reserve(&block_type, file.size_bytes(), FILE_MATERIALIZATION_LIMITS)?;
        source.clear();
        source.insert("type".to_string(), Value::String("base64".to_string()));
        source.insert("media_type".to_string(), Value::String(media_type));
        source.insert(
            "data".to_string(),
            Value::String(BASE64_STANDARD.encode(file.bytes.as_ref())),
        );
        materialized += 1;
    }

    Ok(materialized)
}

fn source_file_id(source: &serde_json::Map<String, Value>) -> Option<String> {
    let source_type = source.get("type").and_then(Value::as_str);
    if !matches!(source_type, Some("file") | Some("file_id")) && source_type.is_some() {
        return None;
    }
    source
        .get("file_id")
        .and_then(Value::as_str)
        .or_else(|| source.get("id").and_then(Value::as_str))
        .map(str::to_string)
}

fn media_type_for_block(block_type: &str, file: &StoredFile) -> Option<String> {
    match block_type {
        "image" => infer_image_media_type_from_bytes(file.bytes.as_ref())
            .map(str::to_string)
            .or_else(|| {
                is_supported_image_media_type(&file.media_type).then(|| file.media_type.clone())
            }),
        "document" => {
            if is_supported_document_media_type(&file.media_type) {
                Some(file.media_type.clone())
            } else if file.bytes.starts_with(b"%PDF") {
                Some("application/pdf".to_string())
            } else if std::str::from_utf8(file.bytes.as_ref()).is_ok() {
                Some("text/plain".to_string())
            } else {
                None
            }
        }
        _ => None,
    }
}

fn sanitize_filename(filename: String) -> String {
    let value = filename
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("upload.bin")
        .trim();
    if value.is_empty() {
        "upload.bin".to_string()
    } else {
        value.chars().take(255).collect()
    }
}

fn normalize_media_type(raw: &str) -> String {
    raw.split(';')
        .next()
        .unwrap_or(raw)
        .trim()
        .to_ascii_lowercase()
}

fn is_supported_image_media_type(media_type: &str) -> bool {
    matches!(
        normalize_media_type(media_type).as_str(),
        "image/jpeg" | "image/png" | "image/gif" | "image/webp"
    )
}

fn is_supported_document_media_type(media_type: &str) -> bool {
    matches!(
        normalize_media_type(media_type).as_str(),
        "application/pdf"
            | "text/plain"
            | "text/markdown"
            | "text/html"
            | "text/csv"
            | "application/json"
    )
}

fn infer_image_media_type_from_bytes(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n']) {
        Some("image/png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anthropic::types::Message;

    #[test]
    fn materializes_image_file_source_to_base64() {
        let store = AnthropicFileStore::default();
        let file = store
            .insert(
                "shot.png",
                "application/octet-stream",
                vec![
                    0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n', 0, 0, 0, 0,
                ],
            )
            .unwrap();
        let mut messages = vec![Message {
            role: "user".to_string(),
            content: json!([{
                "type": "image",
                "source": {"type": "file", "file_id": file.id}
            }]),
        }];

        let count = materialize_file_sources(&store, &mut messages).unwrap();

        assert_eq!(count, 1);
        assert_eq!(messages[0].content[0]["source"]["type"], "base64");
        assert_eq!(messages[0].content[0]["source"]["media_type"], "image/png");
        assert!(messages[0].content[0]["source"]["data"].as_str().is_some());
    }

    #[test]
    fn stored_file_clones_share_bytes_and_upload_chunk_limit_is_exact_for_five_rounds() {
        for round in 0..5 {
            let store = AnthropicFileStore::default();
            let file = store
                .insert("fixture.bin", "application/octet-stream", vec![1, 2, 3, 4])
                .unwrap();
            let loaded = store.get(&file.id).expect("stored file");
            assert_eq!(loaded.bytes.as_ptr(), file.bytes.as_ptr(), "round {round}");

            let mut body = Vec::new();
            append_bounded_upload_chunk(&mut body, b"abc", 5).unwrap();
            append_bounded_upload_chunk(&mut body, b"de", 5).unwrap();
            assert_eq!(body, b"abcde", "round {round}");
            let error = append_bounded_upload_chunk(&mut body, b"f", 5)
                .expect_err("one byte above the boundary must fail");
            assert!(error.contains("exceeds 5 bytes"), "round {round}");
            assert_eq!(body, b"abcde", "round {round}");
        }
    }

    #[test]
    fn file_upload_admission_is_fail_fast_and_recovers_for_five_rounds() {
        for round in 0..5 {
            let store = AnthropicFileStore::default();
            let first = store.try_begin_upload().expect("first upload permit");
            let second = store.try_begin_upload().expect("second upload permit");
            assert!(store.try_begin_upload().is_err(), "round {round}");
            drop(first);
            let recovered = store
                .try_begin_upload()
                .expect("permit recovers after drop");
            drop(recovered);
            drop(second);
            assert_eq!(store.upload_permits.available_permits(), 2, "round {round}");
        }
    }

    #[test]
    fn file_delete_churn_keeps_fifo_metadata_bounded_for_five_rounds() {
        for round in 0..5 {
            let store = AnthropicFileStore::default();
            for index in 0..1_000 {
                let file = store
                    .insert(
                        format!("fixture-{round}-{index}.bin"),
                        "application/octet-stream",
                        vec![index as u8],
                    )
                    .expect("insert churn fixture");
                assert!(store.delete(&file.id), "round {round} index {index}");
            }

            let inner = store.inner.lock();
            assert!(inner.files.is_empty(), "round {round}");
            assert!(inner.order.is_empty(), "round {round}");
            assert_eq!(inner.total_bytes, 0, "round {round}");
        }
    }

    #[test]
    fn file_materialization_budget_checks_every_boundary_before_commit_for_five_rounds() {
        let limits = FileMaterializationLimits {
            max_sources: 2,
            max_image_bytes: 5,
            max_document_bytes: 8,
            max_source_bytes: 8,
            max_materialized_bytes: 12,
        };
        for round in 0..5 {
            let mut budget = FileMaterializationBudget::default();
            budget.reserve("image", 5, limits).unwrap();
            assert_eq!(
                (
                    budget.sources,
                    budget.source_bytes,
                    budget.materialized_bytes
                ),
                (1, 5, 8),
                "round {round}"
            );
            let before = (
                budget.sources,
                budget.source_bytes,
                budget.materialized_bytes,
            );
            assert!(budget.reserve("image", 6, limits).is_err(), "round {round}");
            assert_eq!(
                (
                    budget.sources,
                    budget.source_bytes,
                    budget.materialized_bytes,
                ),
                before,
                "round {round}"
            );
            budget.reserve("document", 3, limits).unwrap();
            assert_eq!(
                (
                    budget.sources,
                    budget.source_bytes,
                    budget.materialized_bytes
                ),
                (2, 8, 12),
                "round {round}"
            );
            assert!(
                budget.reserve("document", 1, limits).is_err(),
                "round {round}"
            );
        }
    }
}
