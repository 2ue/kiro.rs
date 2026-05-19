//! Admin UI 静态文件服务模块
//!
//! 使用 rust-embed 嵌入前端构建产物。
//!
//! - `/admin/` 旧版 admin-ui(保留可用,未来退场)
//! - `/console/` 新版 frontend(本次重构产物)

mod console;
mod router;

pub use console::create_console_router;
pub use router::create_admin_ui_router;
