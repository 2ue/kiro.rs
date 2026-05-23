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
}

#[derive(Subcommand, Debug)]
pub enum CredentialsCommand {
    /// 输出凭据调度统计
    Stats,
    /// 输出凭据配置诊断
    Diagnostics,
}
