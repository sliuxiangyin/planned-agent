use std::sync::Arc;
use async_trait::async_trait;
use anyhow::Result;
use serde_json::{json, Value};
use sysinfo::System;
use planned_agent_core::mcp::types::{Tool, ToolResult};
use planned_agent_core::tool_registry::{ToolExecutor, ToolCategory, BuiltinToolProvider};

/// 内置系统工具提供者（跨平台）
pub struct SystemToolsProvider;

impl BuiltinToolProvider for SystemToolsProvider {
    fn tools(&self) -> Vec<(Tool, Vec<ToolCategory>)> {
        vec![
            // 系统命令工具
            (
                Tool {
                    name: "builtin_execute_command".to_string(),
                    description: "执行系统命令并返回输出（内置工具）".to_string(),
                    input_schema: json!({
                        "type": "object",
                        "properties": {
                            "command": { "type": "string", "description": "要执行的命令" },
                            "args": { 
                                "type": "array",
                                "items": { "type": "string" },
                                "description": "命令参数"
                            },
                            "working_dir": { "type": "string", "description": "工作目录（可选）" }
                        },
                        "required": ["command"]
                    }),
                },
                vec![ToolCategory::System],
            ),
            (
                Tool {
                    name: "builtin_command_exists".to_string(),
                    description: "检查命令是否存在（内置工具）".to_string(),
                    input_schema: json!({
                        "type": "object",
                        "properties": {
                            "command": { "type": "string", "description": "要检查的命令" }
                        },
                        "required": ["command"]
                    }),
                },
                vec![ToolCategory::System],
            ),
            // 进程管理工具
            (
                Tool {
                    name: "builtin_list_processes".to_string(),
                    description: "列出系统进程（跨平台）（内置工具）".to_string(),
                    input_schema: json!({
                        "type": "object",
                        "properties": {
                            "filter": { "type": "string", "description": "过滤条件（可选）" }
                        }
                    }),
                },
                vec![ToolCategory::System],
            ),
            (
                Tool {
                    name: "builtin_get_process_info".to_string(),
                    description: "获取进程详细信息（跨平台）（内置工具）".to_string(),
                    input_schema: json!({
                        "type": "object",
                        "properties": {
                            "pid": { "type": "integer", "description": "进程ID" }
                        },
                        "required": ["pid"]
                    }),
                },
                vec![ToolCategory::System],
            ),
            (
                Tool {
                    name: "builtin_kill_process".to_string(),
                    description: "终止进程（跨平台）（内置工具）".to_string(),
                    input_schema: json!({
                        "type": "object",
                        "properties": {
                            "pid": { "type": "integer", "description": "进程ID" }
                        },
                        "required": ["pid"]
                    }),
                },
                vec![ToolCategory::System],
            ),
            // 环境变量工具
            (
                Tool {
                    name: "builtin_get_env".to_string(),
                    description: "获取环境变量（内置工具）".to_string(),
                    input_schema: json!({
                        "type": "object",
                        "properties": {
                            "name": { "type": "string", "description": "环境变量名" }
                        },
                        "required": ["name"]
                    }),
                },
                vec![ToolCategory::System],
            ),
            (
                Tool {
                    name: "builtin_set_env".to_string(),
                    description: "设置环境变量（仅限当前进程）（内置工具）".to_string(),
                    input_schema: json!({
                        "type": "object",
                        "properties": {
                            "name": { "type": "string", "description": "环境变量名" },
                            "value": { "type": "string", "description": "环境变量值" }
                        },
                        "required": ["name", "value"]
                    }),
                },
                vec![ToolCategory::System],
            ),
            (
                Tool {
                    name: "builtin_list_env".to_string(),
                    description: "列出所有环境变量（内置工具）".to_string(),
                    input_schema: json!({
                        "type": "object",
                        "properties": {
                            "filter": { "type": "string", "description": "过滤条件（可选）" }
                        }
                    }),
                },
                vec![ToolCategory::System],
            ),
        ]
    }
    
    fn executor(&self) -> Arc<dyn ToolExecutor> {
        Arc::new(SystemToolsExecutor)
    }
}

/// 系统工具执行器
struct SystemToolsExecutor;

#[async_trait]
impl ToolExecutor for SystemToolsExecutor {
    async fn execute(&self, tool_name: &str, arguments: Value) -> Result<ToolResult> {
        match tool_name {
            "builtin_execute_command" => {
                let command = arguments["command"].as_str()
                    .ok_or_else(|| anyhow::anyhow!("Missing command"))?;
                
                let mut cmd = std::process::Command::new(command);
                
                // 添加参数
                if let Some(args) = arguments["args"].as_array() {
                    for arg in args {
                        if let Some(arg_str) = arg.as_str() {
                            cmd.arg(arg_str);
                        }
                    }
                }
                
                // 设置工作目录
                if let Some(working_dir) = arguments["working_dir"].as_str() {
                    cmd.current_dir(working_dir);
                }
                
                let output = cmd.output()?;
                
                Ok(ToolResult {
                    call_id: uuid::Uuid::new_v4().to_string(),
                    content: json!({
                        "stdout": String::from_utf8_lossy(&output.stdout).to_string(),
                        "stderr": String::from_utf8_lossy(&output.stderr).to_string(),
                        "status": output.status.code().unwrap_or(-1)
                    }),
                    is_error: !output.status.success(),
                })
            }
            "builtin_command_exists" => {
                let command = arguments["command"].as_str()
                    .ok_or_else(|| anyhow::anyhow!("Missing command"))?;
                
                let exists = which::which(command).is_ok();
                
                Ok(ToolResult {
                    call_id: uuid::Uuid::new_v4().to_string(),
                    content: json!({ "exists": exists }),
                    is_error: false,
                })
            }
            "builtin_list_processes" => {
                let filter = arguments["filter"].as_str().unwrap_or("");
                
                let mut sys = System::new();
                sys.refresh_all();
                
                let processes: Vec<Value> = sys.processes()
                    .iter()
                    .filter(|(_, process)| {
                        filter.is_empty() || 
                        process.name().to_lowercase().contains(&filter.to_lowercase())
                    })
                    .map(|(pid, process)| {
                        json!({
                            "pid": pid.as_u32(),
                            "name": process.name(),
                            "status": format!("{:?}", process.status()),
                            "cpu_usage": process.cpu_usage(),
                            "memory": process.memory(),
                        })
                    })
                    .collect();
                
                Ok(ToolResult {
                    call_id: uuid::Uuid::new_v4().to_string(),
                    content: json!({ 
                        "processes": processes,
                        "count": processes.len()
                    }),
                    is_error: false,
                })
            }
            "builtin_get_process_info" => {
                let pid = arguments["pid"].as_i64()
                    .ok_or_else(|| anyhow::anyhow!("Missing pid"))?;
                
                let mut sys = System::new();
                sys.refresh_all();
                
                let process_pid = sysinfo::Pid::from_u32(pid as u32);
                
                if let Some(process) = sys.process(process_pid) {
                    Ok(ToolResult {
                        call_id: uuid::Uuid::new_v4().to_string(),
                        content: json!({
                            "pid": pid,
                            "name": process.name(),
                            "exe": process.exe().map(|p| p.to_string_lossy().to_string()),
                            "cwd": process.cwd().map(|p| p.to_string_lossy().to_string()),
                            "status": format!("{:?}", process.status()),
                            "cpu_usage": process.cpu_usage(),
                            "memory": process.memory(),
                            "parent_pid": process.parent().map(|p| p.as_u32()),
                        }),
                        is_error: false,
                    })
                } else {
                    Ok(ToolResult {
                        call_id: uuid::Uuid::new_v4().to_string(),
                        content: json!({ "error": "Process not found" }),
                        is_error: true,
                    })
                }
            }
            "builtin_kill_process" => {
                let pid = arguments["pid"].as_i64()
                    .ok_or_else(|| anyhow::anyhow!("Missing pid"))?;
                
                let mut sys = System::new();
                sys.refresh_all();
                
                let process_pid = sysinfo::Pid::from_u32(pid as u32);
                
                if let Some(process) = sys.process(process_pid) {
                    let success = process.kill();
                    Ok(ToolResult {
                        call_id: uuid::Uuid::new_v4().to_string(),
                        content: json!({
                            "pid": pid,
                            "success": success,
                            "message": if success { "Process terminated" } else { "Failed to terminate process" }
                        }),
                        is_error: !success,
                    })
                } else {
                    Ok(ToolResult {
                        call_id: uuid::Uuid::new_v4().to_string(),
                        content: json!({ "error": "Process not found" }),
                        is_error: true,
                    })
                }
            }
            "builtin_get_env" => {
                let name = arguments["name"].as_str()
                    .ok_or_else(|| anyhow::anyhow!("Missing name"))?;
                
                let value = std::env::var(name).ok();
                
                Ok(ToolResult {
                    call_id: uuid::Uuid::new_v4().to_string(),
                    content: json!({
                        "name": name,
                        "value": value,
                        "exists": value.is_some()
                    }),
                    is_error: false,
                })
            }
            "builtin_set_env" => {
                let name = arguments["name"].as_str()
                    .ok_or_else(|| anyhow::anyhow!("Missing name"))?;
                let value = arguments["value"].as_str()
                    .ok_or_else(|| anyhow::anyhow!("Missing value"))?;
                
                std::env::set_var(name, value);
                
                Ok(ToolResult {
                    call_id: uuid::Uuid::new_v4().to_string(),
                    content: json!({
                        "name": name,
                        "value": value,
                        "success": true
                    }),
                    is_error: false,
                })
            }
            "builtin_list_env" => {
                let filter = arguments["filter"].as_str().unwrap_or("");
                
                let env_vars: Vec<(String, String)> = std::env::vars()
                    .filter(|(name, _)| filter.is_empty() || name.contains(filter))
                    .collect();
                
                Ok(ToolResult {
                    call_id: uuid::Uuid::new_v4().to_string(),
                    content: json!({
                        "variables": env_vars.iter().map(|(name, value)| {
                            json!({ "name": name, "value": value })
                        }).collect::<Vec<_>>(),
                        "count": env_vars.len()
                    }),
                    is_error: false,
                })
            }
            _ => Err(anyhow::anyhow!("Unknown tool: {}", tool_name))
        }
    }
    
    fn name(&self) -> &str {
        "builtin_system_tools"
    }
    
    fn supported_tools(&self) -> Vec<String> {
        vec![
            "builtin_execute_command".to_string(),
            "builtin_command_exists".to_string(),
            "builtin_list_processes".to_string(),
            "builtin_get_process_info".to_string(),
            "builtin_kill_process".to_string(),
            "builtin_get_env".to_string(),
            "builtin_set_env".to_string(),
            "builtin_list_env".to_string(),
        ]
    }
}
