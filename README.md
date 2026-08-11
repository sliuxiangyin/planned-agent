# Planned Agent

**AI 驱动的工作流自动化引擎** —— 将模糊需求转化为可调度、可重放、可编排的执行计划。

不是聊天机器人。不是一次性 AI 问答。Planned Agent 的核心目标是**让 AI 执行过的工作流固化为可复用的自动化脚本**，支持定时执行、串行编排、条件触发。

---

## 核心理念

```
用户需求 ──→ 粗粒度规划 ──→ ReAct 逐步执行 ──→ 结构化轨迹 ──→ 固化脚本
                (Coarse)        (React探路)        (Trace)        (可调度)
```

大多数 AI Agent 帮你"做完一件事"。Planned Agent 帮你"做完一件事，并记住怎么做，下次可以零成本自动重复"。

---

## 两种执行模式

| | 周密模式 (Thorough) | 灵活模式 (Flexible) |
|---|---|---|
| **入口** | 引导用户澄清需求，防止需求模糊 | 接收任务后直接执行，边做边调整 |
| **路径** | 澄清 → Coarse 规划 → ReAct 探路 → 脚本固化 | 自由执行 → 轨迹提取 → Coarse 提炼 → 保存计划 |
| **产出** | **固化计划**：零或极少 AI 调用，可定时调度 | **灵活计划**：保留少量 AI 调用，适合环境多变的任务 |
| **适合场景** | 已稳定流程、重复性任务、定时任务 | 探索性任务、首次执行、环境不确定 |

### 渐进式固化

灵活计划执行多次后，轨迹趋于稳定。分析 trace 变异、去掉不稳定步骤，可升级为固化计划。AI 成本从每次 N 次调用 → 接近零。

---

## 核心优势

### 1. 不是对话工具，是自动化引擎

与通用 AI 聊天助手的根本区别：Planned Agent 的执行结果不是一段文本回复，而是一个**可存储、可调度、可版本管理的工作流计划**。

### 2. Coarse → ReAct 双层架构

```
Coarse Planner          ReAct Agent
──────────────          ───────────
• 从需求生成步骤图      • 按步骤逐个探路
• 步骤间依赖关系        • Think → Act → Observe 循环
• 工具类别推荐          • 自动处理意外和越界
• 意图路由分流          • 产生结构化 Trace
```

Coarse 提供骨架，ReAct 填充血肉。两者解耦，可独立调优和替换。

### 3. Trace 是核心资产

ReAct 执行时每一步的工具调用（名称 + 参数 + 结果）自动记录为结构化 Trace。这是脚本化的原材料，也是后续分析和优化的数据基础。

### 4. 计划可编排调度

固化计划支持：
- **定时执行**：每天早上 8 点抓取数据
- **串行编排**：多个计划组成 DAG 流水线
- **失败恢复**：固化计划执行失败 → 自动切换灵活模式修正
- **版本管理**：页面改版后重新探路，生成新版本计划

### 5. 工具生态完整

内置 8 大类工具 provider，支持 MCP 协议扩展：
- Browser — 浏览器导航、点击、填充、截图
- File — 文件读写、目录操作
- Text — 文本处理、正则提取
- Data — 数据解析、格式化
- System — 系统命令执行
- AI — AI 处理（语义提取、内容理解）
- Chunk — 大文本分片导航
- MCP — 外部 MCP 服务器工具接入

### 6. Rust 全栈，性能可靠

- tokio 异步运行时，高并发低延迟
- 类型安全的工具注册和执行
- Stream 流式响应，GUI 实时反馈
- 支持多 AI Provider（OpenAI / DeepSeek 等）

---

## 项目结构

```
planned-agent/
├── crates/
│   ├── core/                    # 核心抽象层（AI/Planner/Prompt/Tool 接口定义）
│   ├── ai-manager/              # 多 AI Provider 管理器
│   ├── ai-openai/               # OpenAI/DeepSeek API 适配器
│   ├── mcp-rmcp/                # MCP 协议适配器（rmcp 实现）
│   ├── prompt-manager/          # Prompt 模板管理与渲染
│   ├── tool-manager/            # 工具注册表 + 8 类内置 Provider
│   ├── planned-agent/           # 主程序（CLI + Agent + Coarse/ReAct Planner）
│   ├── agent-gui/               # Dioxus 桌面 GUI
│   ├── rag/                     # RAG 检索增强
│   └── util/                    # 工具函数
├── docs/                        # 设计文档
├── prompts/                     # Prompt 模板
│   ├── chat/                    # 周密/灵活模式 system prompt
│   └── planning/                # Coarse / ReAct / Observe prompt
└── traces/                      # 结构化 Trace 存储
```

---

## 快速开始

### 构建

```bash
cargo build --release
```

### CLI 模式（计划生成 + 执行）

```bash
cargo run -- "打开百度搜索安仁乡，提取前三条新闻"
```

### GUI 模式

```bash
cd crates/agent-gui
cargo run
```

---

## 架构总览

```
┌──────────────────────────────────────────────────────────┐
│                      用户入口                             │
│  ┌──────────────────┐    ┌──────────────────┐            │
│  │  周密模式 (GUI)   │    │ 灵活模式 (GUI)    │            │
│  │ 需求澄清 + 引导   │    │ 自由执行 + 探索   │            │
│  └────────┬─────────┘    └────────┬─────────┘            │
│           │                       │                       │
│  ┌────────▼───────────────────────▼─────────┐            │
│  │            Coarse Planner                 │            │
│  │  需求 → 粗粒度步骤图（含依赖/工具类别）    │            │
│  └────────────────────┬─────────────────────┘            │
│                       │                                   │
│  ┌────────────────────▼─────────────────────┐            │
│  │            ReAct Agent                    │            │
│  │  ┌──────┐  ┌──────┐  ┌──────┐           │            │
│  │  │Think │→ │ Act  │→ │Observe│→ 下一步   │            │
│  │  └──────┘  └──┬───┘  └──────┘           │            │
│  │               │ 工具调用                  │            │
│  │               │ 参数记录                  │            │
│  └───────────────┼──────────────────────────┘            │
│                  │                                        │
│  ┌───────────────▼──────────────────────────┐            │
│  │           结构化 Trace                     │            │
│  │  step-1: browser_navigate({url})          │            │
│  │  step-2: browser_fill({selector,value})   │            │
│  │  step-3: ai_extract({prompt})             │            │
│  │  step-4: file_write({path,content})       │            │
│  └───────────────┬──────────────────────────┘            │
│                  │                                        │
│  ┌───────────────▼──────────────────────────┐            │
│  │          计划存储 + 脚本化                  │            │
│  │  ┌────────────┐  ┌────────────────────┐  │            │
│  │  │ 灵活计划    │  │ 固化计划 (零AI调用) │  │            │
│  │  │ (保留AI步骤) │  │ (纯工具调用脚本)    │  │            │
│  │  └────────────┘  └────────────────────┘  │            │
│  └──────────────────────────────────────────┘            │
│                                                          │
│  ┌──────────────────────────────────────────┐            │
│  │          调度 / 编排层 (远期)              │            │
│  │  定时执行 · 串行DAG · 条件触发 · 失败恢复  │            │
│  └──────────────────────────────────────────┘            │
└──────────────────────────────────────────────────────────┘
```

---

## 设计文档

| 文档 | 说明 |
|------|------|
| [docs/design.md](docs/design.md) | 整体设计 |
| [docs/core.md](docs/core.md) | 核心抽象层 |
| [docs/tool-manager.md](docs/tool-manager.md) | 工具管理系统 |
| [docs/mcp-rmcp.md](docs/mcp-rmcp.md) | MCP 协议集成 |
| [docs/planned-agent.md](docs/planned-agent.md) | Agent 主程序 |
| [docs/ai-manager.md](docs/ai-manager.md) | AI 多 Provider 管理 |

### Prompt 设计

| 文档 | 说明 |
|------|------|
| [docs/prompt-engineering.md](docs/prompt-engineering.md) | Prompt 工程概述 |
| [docs/planned-agent/coarse-planner.md](docs/planned-agent/coarse-planner.md) | Coarse Planner Prompt |
| [docs/planned-agent/react-agent.md](docs/planned-agent/react-agent.md) | ReAct Agent Prompt |
| [docs/planned-agent/trace-rag-design.md](docs/planned-agent/trace-rag-design.md) | Trace RAG 设计 |

step-1

需求：打开百度首页，搜索“达州安仁乡“，在搜索结果中切换到“资讯“分类，从中获取最新（最靠前）的三条新闻
输出期望：三条新闻的标题及摘要内容（附来源、时间、链接）
关键实体：百度（baidu.com）、搜索关键词“达州安仁乡“、“资讯“分类

step-2
{“execution_result”: “1. 标题=达州东部经开区燃气管网安全隐患整治工程预计12月底完工, 摘要=涵盖麻柳场镇、檀木社区至安仁乡、葫芦社区的燃气管道;二标段长25.9公里,涉及安仁场镇、麻柳至东兴社区、檀木社区至花红社区的燃气管道。施工人员正多点位、分段式作业,尽力减少对沿线…, 来源=四川经济网, 时间=2025年8月18日, 链接=https://www.scjjrb.com/2025/08/18/99442418.html；2. 标题=达州职业技术学院实践团队深入安仁乡调研板凳龙制作技艺, 摘要=近日，达州职业技术学院巴渠非遗工坊实践团队走进达川区安仁乡，围绕国家级非物质文化遗产代表性项目龙舞（安仁板凳龙）开展实地调研。青年学子叩访第九代龙舞（安仁板凳龙）非物质文化…, 来源=人民资讯, 时间=7月16日, 链接=https://baijiahao.baidu.com/s?id=1870853011692009423&wfr=spider&for=pc；3. 标题=【产城绿乡绘新篇】达州东部经开区安仁乡严马庙村:蚕桑产业…, 摘要=达州东部经开区安仁乡严马庙村党总支书记谭宏:“全年预计收入可达65万元,实现收益30万元。”…, 来源=中国网中国四川, 时间=2025年5月26日, 链接=http://sc.china.com.cn/2025-05/26/content_43125446.html”, “key_steps”: [“导航到 https://www.baidu.com → 在搜索框输入关键词「达州安仁乡」并回车搜索”, “在搜索结果页点击「资讯」分类链接（https://www.baidu.com/s?rtt=1&bsst=1&cl=2&tn=news&ie=utf-8&word=达州安仁乡）”, “读取百度资讯搜索页快照，确认共15条相关资讯，定位第1页最靠前的3条新闻”, “执行页面JS提取前3条新闻的标题、摘要、来源、时间、链接字段”], “tool_steps”: [“browser_navigate | url=https://www.baidu.com | 成功打开百度首页”, “browser_type | target=搜索框, text=达州安仁乡 | 输入搜索关键词成功”, “browser_press_key | key=Enter | 触发搜索，跳转至达州安仁乡_百度搜索页”, “browser_find | text=资讯 | 定位到「资讯」分类标签”, “browser_click | target=f1e152, element=资讯分类标签 | 成功切换到百度资讯搜索页”, “browser_snapshot | target=当前页 | 获取资讯列表快照，含15条资讯及标题/摘要/来源/时间/链接”, “browser_evaluate | function=提取#content_left下前3个结果块的title/link/abstract/source/time | 成功提取3条新闻完整字段”]}