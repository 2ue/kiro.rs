use clap::{Parser, Subcommand};

/// Claude-compatible account scheduler runtime
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
pub struct Args {
    /// 配置文件路径
    #[arg(short, long)]
    pub config: Option<String>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// 显式运行生产维护任务；不会在普通服务启动时自动执行
    Maintenance {
        #[command(subcommand)]
        command: MaintenanceCommand,
    },
}

#[derive(Subcommand, Debug)]
pub enum MaintenanceCommand {
    /// 只运行默认启动 schema 迁移并退出，不连接 Redis、不导入凭据
    Migrate,
    /// 低峰期并发补齐 usage/rollup 相关索引
    UsageIndexes,
    /// 显式回填旧 usage 成本字段；必须先停止并排空所有网关实例
    UsageLegacyCostBackfill,
    /// 显式压缩历史 usage rollup 小桶到小时桶；必须先停止并排空所有网关实例
    UsageRollupCompression,
}
