//! Admin UI 静态文件服务模块
//!
//! 生产默认使用 rust-embed 嵌入前端构建产物；开发可通过 Vite 单独运行前端。

mod router;

pub use router::{create_admin_ui_router, create_console_ui_router, create_new_ui_router};
