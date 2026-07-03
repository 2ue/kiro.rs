//! Anthropic API 兼容服务模块
//!
//! 提供与 Anthropic Claude API 兼容的 HTTP 服务端点。
//!
//! # 支持的端点
//!
//! ## 标准端点 (/v1)
//! - `GET /v1/models` - 获取可用模型列表
//! - `POST /v1/messages` - 创建消息（对话，默认 high-cache 本地 usage 模拟）
//! - `POST /v1/messages/count_tokens` - 计算 token 数量
//!
//! ## No-cache 端点 (/na/v1)
//! - `GET /na/v1/models` - 获取可用模型列表
//! - `POST /na/v1/messages` - 创建消息（默认不进入本地 prompt-cache 模拟，直接使用原始 usage）
//! - `POST /na/v1/messages/count_tokens` - 计算 token 数量
//!
//! ## 高缓存 input 兼容端点 (/ha/v1)
//! - `GET /ha/v1/models` - 获取可用模型列表
//! - `POST /ha/v1/messages` - 创建消息（high-cache；usage 上报由 `/ha` 覆盖项独立控制）
//! - `POST /ha/v1/messages/count_tokens` - 计算 token 数量（与 /v1 相同）
//!
//! ## Claude Code 兼容端点 (/cc/v1)
//! - `GET /cc/v1/models` - 获取可用模型列表
//! - `POST /cc/v1/messages` - 创建消息（实时流式返回，最终 message_delta.usage 修正用量）
//! - `POST /cc/v1/messages/count_tokens` - 计算 token 数量（与 /v1 相同）
//!
//! # 使用示例
//! ```rust,ignore
//! use kiro_rs::anthropic;
//!
//! let app = anthropic::create_router("your-api-key");
//! let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await?;
//! axum::serve(listener, app).await?;
//! ```

pub(crate) mod cache;
pub(crate) mod converter;
pub(crate) mod envelope;
mod handlers;
mod middleware;
pub(crate) mod model_capabilities;
pub(crate) mod payload_guard;
pub(crate) mod payload_guard_runtime;
pub(crate) mod pricing;
pub(crate) mod prompt_cache;
pub(crate) mod prompt_cache_creation_control;
mod router;
mod stream;
pub(crate) mod tool_format_debug;
pub mod types;
pub(crate) mod usage;
mod websearch;

pub use router::create_router_with_provider;
