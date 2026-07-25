use clap::{Parser, Subcommand};

/// 命令行参数
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
pub struct Cli {
    /// 配置文件路径
    #[arg(short, long, default_value = "config.toml")]
    pub config: String,
    
    /// 默认 AI 提供商名称
    #[arg(long)]
    pub ai_provider: Option<String>,
    
    /// 默认 MCP 服务器名称
    #[arg(long)]
    pub mcp_server: Option<String>,
    
    /// 是否使用流式输出
    #[arg(long)]
    pub stream: bool,
    
    /// 交互模式
    #[arg(long)]
    pub interactive: bool,
    
    /// 日志级别
    #[arg(long, default_value = "info")]
    pub log_level: String,
    
    /// 子命令
    #[command(subcommand)]
    pub command: Option<Commands>,
}

/// 子命令
#[derive(Subcommand, Debug)]
pub enum Commands {
    /// 测试执行器：运行完整的 Plan-and-Execute 流程
    TestExecute {
        /// 用户输入
        #[arg(help = "用户输入的任务描述")]
        input: String,
    },
}

impl Cli {
    /// 解析命令行参数
    pub fn parse() -> Self {
        Parser::parse()
    }
    
    /// 合并配置
    pub fn merge_with_config(&self, mut config: crate::config::AppConfig) -> crate::config::AppConfig {
        // 设置默认 AI 提供商
        if let Some(provider_name) = &self.ai_provider {
            for provider in &mut config.ai_providers {
                if provider.name == *provider_name {
                    provider.is_default = true;
                } else {
                    provider.is_default = false;
                }
            }
        }
        
        // 设置默认 MCP 服务器
        if let Some(server_name) = &self.mcp_server {
            for server in &mut config.mcp_servers {
                if server.name == *server_name {
                    server.is_default = true;
                } else {
                    server.is_default = false;
                }
            }
        }
        
        config.logging.level = self.log_level.clone();
        
        config
    }
}
