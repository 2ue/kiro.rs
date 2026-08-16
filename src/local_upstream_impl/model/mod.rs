//! Local upstream data model.
//!
//! 包含当前本地上游兼容实现的数据类型定义：
//! - `common`: 共享类型（枚举和辅助结构体）
//! - `events`: 响应事件类型
//! - `requests`: 请求类型
//! - `credentials`: 上游凭据
//! - `token_refresh`: Token 刷新
//! - `usage_limits`: 使用额度查询

pub mod available_models;
pub mod common;
pub mod credentials;
pub mod events;
pub mod requests;
pub mod token_refresh;
pub mod usage_limits;
