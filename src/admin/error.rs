//! Admin API 错误类型定义

use std::fmt;

use axum::http::StatusCode;

use super::types::AdminErrorResponse;

/// Admin 服务错误类型
#[derive(Debug)]
pub enum AdminServiceError {
    /// 目标不存在（账号、路由场景等）
    NotFound { id: u64 },

    /// 内部状态错误
    InternalError(String),

    /// 参数无效（历史上叫凭据无效，现用于各类对象的校验失败）
    InvalidCredential(String),

    /// 请求参数无效（路由、运行配置等非凭据对象的校验失败）
    InvalidRequest(String),

    /// 资源仍被使用，不能直接删除或变更
    Conflict(String),
}

impl fmt::Display for AdminServiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AdminServiceError::NotFound { id } => {
                write!(f, "目标不存在: {}", id)
            }
            AdminServiceError::InternalError(msg) => write!(f, "内部错误: {}", msg),
            AdminServiceError::InvalidCredential(msg) => write!(f, "参数无效: {}", msg),
            AdminServiceError::InvalidRequest(msg) => write!(f, "{}", msg),
            AdminServiceError::Conflict(msg) => write!(f, "资源冲突: {}", msg),
        }
    }
}

impl std::error::Error for AdminServiceError {}

impl AdminServiceError {
    /// 获取对应的 HTTP 状态码
    pub fn status_code(&self) -> StatusCode {
        match self {
            AdminServiceError::NotFound { .. } => StatusCode::NOT_FOUND,
            AdminServiceError::InternalError(_) => StatusCode::INTERNAL_SERVER_ERROR,
            AdminServiceError::InvalidCredential(_) => StatusCode::BAD_REQUEST,
            AdminServiceError::InvalidRequest(_) => StatusCode::BAD_REQUEST,
            AdminServiceError::Conflict(_) => StatusCode::CONFLICT,
        }
    }

    /// 转换为 API 错误响应
    pub fn into_response(self) -> AdminErrorResponse {
        let error = self;
        match &error {
            AdminServiceError::NotFound { .. } => AdminErrorResponse::not_found(error.to_string()),
            AdminServiceError::InternalError(_) => {
                AdminErrorResponse::internal_error(error.to_string())
            }
            AdminServiceError::InvalidCredential(_) | AdminServiceError::InvalidRequest(_) => {
                AdminErrorResponse::invalid_request(error.to_string())
            }
            AdminServiceError::Conflict(_) => {
                AdminErrorResponse::invalid_request(error.to_string())
            }
        }
    }
}
