use clap::{Parser, Subcommand};

/// Anthropic <-> Kiro API 客户端
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
pub struct Args {
    /// 配置文件路径
    #[arg(short, long)]
    pub config: Option<String>,

    /// 凭证文件路径
    #[arg(long)]
    pub credentials: Option<String>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// 离线查看凭据文件和统计缓存
    Credentials {
        #[command(subcommand)]
        command: CredentialsCommand,
    },
    /// 显式运行生产维护任务；不会在普通服务启动时自动执行
    Maintenance {
        #[command(subcommand)]
        command: MaintenanceCommand,
    },
}

#[derive(Subcommand, Debug)]
pub enum CredentialsCommand {
    /// 输出凭据调度统计
    Stats,
    /// 输出凭据配置诊断
    Diagnostics,
}

#[derive(Subcommand, Debug)]
pub enum MaintenanceCommand {
    /// 只运行默认启动 schema 迁移并退出，不连接 Redis、不导入凭据
    Migrate,
    /// 低峰期并发补齐 usage/rollup 相关索引
    UsageIndexes,
    /// 显式回填旧 usage 成本字段；大表环境可能耗时，应在低峰期运行
    UsageLegacyCostBackfill,
    /// 显式压缩历史 usage rollup 小桶到小时桶；大表环境可能耗时
    UsageRollupCompression,
}
