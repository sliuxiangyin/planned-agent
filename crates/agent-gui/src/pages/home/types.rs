//! 指挥中心数据模型：计划元数据、Agent 洞察、模拟数据。

use chrono::{DateTime, Utc};

/// 计划状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanStatus {
    /// 待生成 — 计划已创建，尚未通过 AI 生成具体步骤
    PendingGeneration,
    /// 已生成 — AI 已完成计划生成
    Generated,
}

impl PlanStatus {
    pub fn label(&self) -> &'static str {
        match self {
            PlanStatus::PendingGeneration => "待生成",
            PlanStatus::Generated => "已生成",
        }
    }

    pub fn css_class(&self) -> &'static str {
        match self {
            PlanStatus::PendingGeneration => "pending",
            PlanStatus::Generated => "generated",
        }
    }

    /// 在 AI Core 周围的轨道半径层级
    pub fn orbit_level(&self) -> usize {
        match self {
            PlanStatus::PendingGeneration => 1,
            PlanStatus::Generated => 2,
        }
    }

    /// 从 DB 字符串解析
    pub fn from_str(s: &str) -> Self {
        match s {
            "generated" => PlanStatus::Generated,
            _ => PlanStatus::PendingGeneration,
        }
    }

    /// 转为 DB 字符串
    pub fn to_str(&self) -> &'static str {
        match self {
            PlanStatus::PendingGeneration => "pending_generation",
            PlanStatus::Generated => "generated",
        }
    }
}

/// 定时执行配置（预留）
#[derive(Debug, Clone, PartialEq)]
pub struct ScheduleConfig {
    /// cron 表达式或简单描述
    pub cron: String,
    /// 人类可读的描述，如 "每日 09:00"
    pub description: String,
    /// 是否启用
    pub enabled: bool,
    /// 下次执行时间
    pub next_run: Option<DateTime<Utc>>,
}

/// 多策略关联（预留）
#[derive(Debug, Clone, PartialEq)]
pub struct StrategyRef {
    pub id: String,
    pub name: String,
}

/// 计划元数据（指挥中心展示用）
#[derive(Debug, Clone, PartialEq)]
pub struct PlanMeta {
    pub id: String,
    pub name: String,
    pub description: String,
    pub status: PlanStatus,
    /// 计划模式：flexible / thorough
    pub mode: String,
    pub schedule: Option<ScheduleConfig>,
    pub strategy: Option<StrategyRef>,
    pub tags: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// 在 AI Core 轨道上的角度（0-360），用于计算节点位置
    pub orbit_angle: f64,
}

/// Agent 洞察条目
#[derive(Debug, Clone, PartialEq)]
pub struct AgentInsight {
    pub id: String,
    pub message: String,
    pub action_label: Option<String>,
    pub urgency: InsightUrgency,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsightUrgency {
    Info,
    Warning,
    Suggestion,
}

impl InsightUrgency {
    pub fn css_class(&self) -> &'static str {
        match self {
            InsightUrgency::Info => "info",
            InsightUrgency::Warning => "warning",
            InsightUrgency::Suggestion => "suggestion",
        }
    }
}

/// 时间线上的计划条目
#[derive(Debug, Clone, PartialEq)]
pub struct TimelineEntry {
    pub time: String,        // "09:00"
    pub plan_name: String,
    pub is_active: bool,     // 是否已过/未来
    pub plan_id: Option<String>,
}

// ── 模拟数据 ──────────────────────────────────────────────────────

/// 生成模拟计划列表（DB 未就绪时作 fallback）
pub fn mock_plans() -> Vec<PlanMeta> {
    vec![
        PlanMeta {
            id: "plan-1".into(),
            name: "重构 agent-gui 模块".into(),
            description: "分析当前组件结构，设计指挥中心页面布局，拆分 home/plan 双页面架构".into(),
            status: PlanStatus::Generated,
            mode: "flexible".into(),
            schedule: None,
            strategy: None,
            tags: vec!["前端".into(), "重构".into()],
            created_at: Utc::now(),
            updated_at: Utc::now(),
            orbit_angle: 0.0,
        },
        PlanMeta {
            id: "plan-2".into(),
            name: "添加 MCP 工具注册机制".into(),
            description: "支持 browser-use 等外部 MCP 工具的动态注册与热加载".into(),
            status: PlanStatus::PendingGeneration,
            mode: "thorough".into(),
            schedule: None,
            strategy: Some(StrategyRef {
                id: "strategy-1".into(),
                name: "工具链升级".into(),
            }),
            tags: vec!["MCP".into(), "工具".into()],
            created_at: Utc::now(),
            updated_at: Utc::now(),
            orbit_angle: 72.0,
        },
        PlanMeta {
            id: "plan-3".into(),
            name: "优化 Prompt 模板系统".into(),
            description: "重构 coarse_plan 与 react_system 模板，提升计划生成准确率".into(),
            status: PlanStatus::PendingGeneration,
            mode: "flexible".into(),
            schedule: None,
            strategy: None,
            tags: vec!["Prompt".into(), "优化".into()],
            created_at: Utc::now(),
            updated_at: Utc::now(),
            orbit_angle: 144.0,
        },
        PlanMeta {
            id: "plan-4".into(),
            name: "数据库每日备份".into(),
            description: "每天 09:00 自动备份 PostgreSQL 数据库到远程存储".into(),
            status: PlanStatus::PendingGeneration,
            mode: "thorough".into(),
            schedule: Some(ScheduleConfig {
                cron: "0 9 * * *".into(),
                description: "每日 09:00".into(),
                enabled: true,
                next_run: None,
            }),
            strategy: None,
            tags: vec!["运维".into(), "自动化".into()],
            created_at: Utc::now(),
            updated_at: Utc::now(),
            orbit_angle: 216.0,
        },
        PlanMeta {
            id: "plan-5".into(),
            name: "项目初始化骨架".into(),
            description: "搭建 Rust workspace、配置 Cargo.toml、Dioxus 项目脚手架".into(),
            status: PlanStatus::Generated,
            mode: "flexible".into(),
            schedule: None,
            strategy: None,
            tags: vec!["基建".into()],
            created_at: Utc::now(),
            updated_at: Utc::now(),
            orbit_angle: 288.0,
        },
    ]
}

/// 生成模拟 Agent 洞察
pub fn mock_insights() -> Vec<AgentInsight> {
    vec![
        AgentInsight {
            id: "insight-1".into(),
            message: "发现 3 个计划可以合并为「工具链升级」策略，提升执行效率".into(),
            action_label: Some("查看建议".into()),
            urgency: InsightUrgency::Suggestion,
        },
        AgentInsight {
            id: "insight-2".into(),
            message: "「数据库每日备份」定时计划尚未配置目标存储路径".into(),
            action_label: Some("立即配置".into()),
            urgency: InsightUrgency::Warning,
        },
        AgentInsight {
            id: "insight-3".into(),
            message: "本周已完成 4 项计划，进行中 1 项，节奏良好".into(),
            action_label: None,
            urgency: InsightUrgency::Info,
        },
    ]
}

/// 生成模拟时间线
pub fn mock_timeline() -> Vec<TimelineEntry> {
    vec![
        TimelineEntry { time: "08:00".into(), plan_name: "系统自检".into(), is_active: true, plan_id: None },
        TimelineEntry { time: "09:00".into(), plan_name: "数据库备份".into(), is_active: false, plan_id: Some("plan-4".into()) },
        TimelineEntry { time: "10:00".into(), plan_name: "代码审查".into(), is_active: false, plan_id: None },
        TimelineEntry { time: "14:00".into(), plan_name: "周报生成".into(), is_active: false, plan_id: None },
        TimelineEntry { time: "18:00".into(), plan_name: "日志归档".into(), is_active: false, plan_id: None },
    ]
}
