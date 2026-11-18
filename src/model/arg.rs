use clap::{Parser, Subcommand};

/// Anthropic <-> 上游 API 客户端
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
    /// 直连旧库，原样导出迁移包
    ExportLegacy {
        #[arg(long)]
        from_postgres: String,
        #[arg(long)]
        out: std::path::PathBuf,
        #[arg(long)]
        redact_secrets: bool,
    },
    /// 预览旧配置迁移差异；--apply 单事务全量覆盖目标库
    ImportLegacy {
        file: std::path::PathBuf,
        #[arg(long)]
        apply: bool,
    },
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
    /// 从明细重建总览的路由维度（本地路由、走外部原因）；必须先停止并排空所有网关实例
    UsageRouteDimensionBackfill,
    /// 从明细回填总览合计列（账号成本、原始 token）；启动时只对小表自动做，大表在低峰期执行这里
    UsageOverviewTotalsBackfill,
    /// 向隔离的 overview demo 数据库写入可重复的本地/外部用量样本
    UsageDemoSeed {
        /// 生成最近多少天的数据
        #[arg(long, default_value_t = 8)]
        days: u32,
        /// 固定随机种子，保证每次演示结果可复现
        #[arg(long, default_value_t = 20261002)]
        seed: u64,
        /// 清空 demo 库中的 usage 表后重新造数
        #[arg(long)]
        reset: bool,
    },
}
