//! Admin UI 静态文件服务模块
//!
//! 使用 rust-embed 嵌入前端构建产物

mod router;

pub use router::{create_admin_ui_router, create_console_ui_router, create_new_ui_router};
