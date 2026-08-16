//! 请求类型模块
//!
//! 包含本地上游请求相关的类型定义

pub mod conversation;
pub mod tool;
pub mod upstream;

#[allow(unused_imports)]
pub use conversation::{ConversationState, CurrentMessage, UserInputMessage};
#[allow(unused_imports)]
pub use upstream::LocalUpstreamRequest;
