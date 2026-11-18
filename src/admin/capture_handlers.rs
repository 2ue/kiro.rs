//! LLM 明文采集 Admin API
//!
//! - `GET  /diagnostics/capture`                 当前会话状态与历史
//! - `POST /diagnostics/capture/start`           启动采集（可选 limits）
//! - `POST /diagnostics/capture/stop`            停止并打包
//! - `GET  /diagnostics/capture/{id}/download`   下载已完成会话的 ZIP

use axum::{
    Json,
    body::Body,
    extract::{Path, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use serde_json::json;

use super::middleware::AdminState;
use super::types::AdminErrorResponse;
use crate::diagnostics::capture::{self, CaptureError, CaptureLimits, CaptureManager};

fn manager() -> Option<&'static std::sync::Arc<CaptureManager>> {
    capture::global()
}

fn not_initialized() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(AdminErrorResponse::api_error(
            "LLM capture is not initialized",
        )),
    )
        .into_response()
}

fn capture_error_response(error: CaptureError) -> Response {
    let status = match error {
        CaptureError::AlreadyRunning | CaptureError::NotRunning => StatusCode::CONFLICT,
        CaptureError::Io(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (
        status,
        Json(AdminErrorResponse::new("capture_error", error.to_string())),
    )
        .into_response()
}

pub async fn get_capture_overview() -> Response {
    match manager() {
        Some(manager) => Json(manager.overview()).into_response(),
        None => not_initialized(),
    }
}

/// 明文采集会落盘真实请求内容，启动/停止/下载都要留审计。
fn audit_capture(
    state: &AdminState,
    action: &'static str,
    session_id: Option<String>,
    result: Result<(), String>,
    detail: serde_json::Value,
) {
    let (success, error_message) = match result {
        Ok(()) => (true, None),
        Err(message) => (false, Some(message)),
    };
    state.service.audit(
        action,
        "capture",
        session_id,
        success,
        error_message,
        detail,
    );
}

pub async fn start_capture(
    State(state): State<AdminState>,
    payload: Option<Json<CaptureLimits>>,
) -> Response {
    let manager = match manager() {
        Some(manager) => manager,
        None => {
            audit_capture(
                &state,
                "start_capture",
                None,
                Err("LLM capture is not initialized".to_string()),
                json!({}),
            );
            return not_initialized();
        }
    };
    let limits = payload.map(|Json(limits)| limits).unwrap_or_default();
    let limits_detail = serde_json::to_value(limits).unwrap_or_else(|_| json!({}));
    match manager.start(limits) {
        Ok(status) => {
            audit_capture(
                &state,
                "start_capture",
                status.session_id.clone(),
                Ok(()),
                json!({ "limits": limits_detail }),
            );
            Json(status).into_response()
        }
        Err(error) => {
            audit_capture(
                &state,
                "start_capture",
                None,
                Err(error.to_string()),
                json!({ "limits": limits_detail }),
            );
            capture_error_response(error)
        }
    }
}

pub async fn stop_capture(State(state): State<AdminState>) -> Response {
    let manager = match manager() {
        Some(manager) => std::sync::Arc::clone(manager),
        None => {
            audit_capture(
                &state,
                "stop_capture",
                None,
                Err("LLM capture is not initialized".to_string()),
                json!({}),
            );
            return not_initialized();
        }
    };
    // 停止需要等待写线程落盘并打包 ZIP，放到阻塞线程池。
    match tokio::task::spawn_blocking(move || manager.stop("admin_stop")).await {
        Ok(Ok(status)) => {
            audit_capture(
                &state,
                "stop_capture",
                status.session_id.clone(),
                Ok(()),
                json!({ "reason": "admin_stop" }),
            );
            Json(status).into_response()
        }
        Ok(Err(error)) => {
            audit_capture(
                &state,
                "stop_capture",
                None,
                Err(error.to_string()),
                json!({}),
            );
            capture_error_response(error)
        }
        Err(error) => {
            audit_capture(
                &state,
                "stop_capture",
                None,
                Err(error.to_string()),
                json!({}),
            );
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(AdminErrorResponse::internal_error(error.to_string())),
            )
                .into_response()
        }
    }
}

pub async fn download_capture(
    State(state): State<AdminState>,
    Path(session_id): Path<String>,
) -> Response {
    let manager = match manager() {
        Some(manager) => manager,
        None => {
            audit_capture(
                &state,
                "download_capture",
                Some(session_id),
                Err("LLM capture is not initialized".to_string()),
                json!({}),
            );
            return not_initialized();
        }
    };
    let Some(path) = manager.archive_path(&session_id) else {
        audit_capture(
            &state,
            "download_capture",
            Some(session_id.clone()),
            Err("capture archive not found".to_string()),
            json!({}),
        );
        return (
            StatusCode::NOT_FOUND,
            Json(AdminErrorResponse::not_found("capture archive not found")),
        )
            .into_response();
    };
    let file = match tokio::fs::File::open(&path).await {
        Ok(file) => file,
        Err(error) => {
            audit_capture(
                &state,
                "download_capture",
                Some(session_id.clone()),
                Err(error.to_string()),
                json!({}),
            );
            return (
                StatusCode::NOT_FOUND,
                Json(AdminErrorResponse::not_found(error.to_string())),
            )
                .into_response();
        }
    };
    let length = file.metadata().await.map(|meta| meta.len()).ok();
    audit_capture(
        &state,
        "download_capture",
        Some(session_id.clone()),
        Ok(()),
        json!({ "archiveBytes": length }),
    );
    let stream = file_stream(file);
    let mut builder = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/zip")
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{session_id}.zip\""),
        )
        .header(header::CACHE_CONTROL, "no-store");
    if let Some(length) = length {
        builder = builder.header(header::CONTENT_LENGTH, length);
    }
    builder
        .body(Body::from_stream(stream))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// 以 64KiB 分块读取文件，避免大归档整体载入内存。
fn file_stream(
    file: tokio::fs::File,
) -> impl futures::Stream<Item = std::io::Result<bytes::Bytes>> + Send + 'static {
    use tokio::io::AsyncReadExt;
    futures::stream::try_unfold(file, |mut file| async move {
        let mut buffer = vec![0u8; 64 * 1024];
        let read = file.read(&mut buffer).await?;
        if read == 0 {
            return Ok(None);
        }
        buffer.truncate(read);
        Ok(Some((bytes::Bytes::from(buffer), file)))
    })
}
