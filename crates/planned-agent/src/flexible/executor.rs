//! 总编排：按顺序跑每步，把前序输出递给后面的步，播事件，响应取消。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use planned_agent_core::ai::types::{FunctionDefinition, ToolDefinition, ToolType};
use planned_agent_core::ai::AiClient;
use planned_agent_core::host::RuntimeEnvironment;
use planned_agent_core::mcp::types::Tool;
use planned_agent_tool_manager::ToolRegistry;
use tokio::sync::watch;

use super::event::{PlanRunEvent, PlanRunSink};
use super::output_schema::OutputSchema;
use super::params::{render_step_expected_output, render_step_intent, PlanRunParams};
use super::prompt;
use super::report::{PlanRunReport, StepRunRecord, StepStatus, ToolCallRecord};
use super::step::{run_output_resolve, run_step, StepInput};
use super::template::{FlexiblePlanTemplate, PlanStep};

// ────────── 步骤产出落盘（见 docs/planned-agent/flexible-step-output-spill.md） ──────────

/// 产出落盘根目录的默认值（相对**进程 cwd**；宿主可覆盖为含会话段的路径）。
pub const DEFAULT_CACHE_DIR: &str = "./data/cache";
/// 产出超过该字符数就落盘 —— 与记录侧 `OUTPUT_MAX_CHARS` 对齐。
pub const DEFAULT_SPILL_THRESHOLD_CHARS: usize = 8_000;
/// 落盘后写进 `prior` 的预览长度（字符）。
pub const DEFAULT_SPILL_PREVIEW_CHARS: usize = 800;
/// 日志里单条产出的上限：产出可能上万字符，不截断会把日志淹掉。
///
/// 完整内容总能从产出文件拿到（`StepRunRecord::output_file`）。
pub const LOG_OUTPUT_MAX_CHARS: usize = 2_000;

// ──────── LLM 请求的超时与重试 ────────

/// 单次 LLM 请求的默认超时（秒）。
pub const DEFAULT_LLM_TIMEOUT_SECS: u64 = 180;
/// 单次请求超时后的默认重试次数。
pub const DEFAULT_LLM_TIMEOUT_RETRIES: usize = 1;

/// `run-*` 子目录的进程内序号：与毫秒时间戳一起保证目录唯一。
static RUN_SEQ: AtomicU64 = AtomicU64::new(0);

/// 本次执行的产出目录名（执行器不认识「会话」概念，会话段由宿主拼进 `cache_dir`）。
fn new_run_dir_name() -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|delta| delta.as_millis())
        .unwrap_or(0);
    let seq = RUN_SEQ.fetch_add(1, Ordering::Relaxed);
    format!("run-{millis}-{seq}")
}

/// 一条前序产出的存放形态。
///
/// 全文**始终**留在内存里 —— 本方案的目的不是省内存，而是不把全文塞进下游 `prompt`；
/// 超阈值时另存一份文件，下游以「文件说明 + 预览」引用、用 `builtin_read_file` 按需读取。
#[derive(Debug, Clone)]
struct StoredOutput {
    content: String,
    spilled: Option<SpilledOutput>,
}

/// 落盘信息（供 `prior` 渲染与 `StepRunRecord::output_file` 使用）。
#[derive(Debug, Clone)]
struct SpilledOutput {
    path: PathBuf,
    lines: usize,
    bytes: usize,
}

impl SpilledOutput {
    fn path_string(&self) -> String {
        self.path.display().to_string()
    }
}

/// 产出超过阈值就写到 `<cache_dir>/<run_dir>/step-<index>.txt`。
///
/// - 返回 `Ok(None)` = 未超阈值（下游照旧内联全文）；
/// - 返回 `Ok(Some(_))` = 已落盘；
/// - IO 失败**向上抛**：由调用方把该步记为 `Failed`（不静默降级，见设计稿 §7）。
///
/// 文件名用**步骤序号**而非 `result_reference` —— 后者来自模板 / LLM，
/// 直接拼进路径有目录穿越风险。
async fn spill_output(
    cache_dir: &Path,
    run_dir: &str,
    index: usize,
    threshold_chars: usize,
    output: &str,
) -> Result<Option<SpilledOutput>> {
    if output.chars().count() <= threshold_chars {
        return Ok(None);
    }
    let dir = cache_dir.join(run_dir);
    tokio::fs::create_dir_all(&dir)
        .await
        .with_context(|| format!("创建产出缓存目录失败：{}", dir.display()))?;
    let path = dir.join(format!("step-{index}.txt"));
    tokio::fs::write(&path, output)
        .await
        .with_context(|| format!("写入产出缓存文件失败：{}", path.display()))?;
    Ok(Some(SpilledOutput {
        path,
        lines: output.lines().count(),
        bytes: output.len(),
    }))
}

/// 一条前序产出渲染进 `prompt` 的文本：未落盘给全文，落盘给「文件说明 + 预览」。
///
/// 预览不是截断兜底 —— 它是给模型判断「要不要读这个文件」的线索。
fn render_prior_output(stored: &StoredOutput, preview_chars: usize) -> String {
    let Some(spilled) = &stored.spilled else {
        return stored.content.clone();
    };
    let preview: String = stored.content.chars().take(preview_chars).collect();
    format!(
        "⚠️ 产出较大（{} 行 / {} 字节），已存为临时文件，未全文注入。\n\
         文件：{}\n\
         读取方式：`builtin_read_file`（offset 从 1 开始，limit 默认 2000；\
         返回含 `next_offset` / `has_more`，可续读）。\n\
         ———— 开头预览（前 {} 字符）————\n{}",
        spilled.lines,
        spilled.bytes,
        spilled.path_string(),
        preview_chars,
        preview
    )
}

/// 把产出压成**单行**并封顶，供日志使用。
///
/// 两个处理都是必要的：多行会糊掉日志行（与 `system_prompt` 同一问题），
/// 不封顶则大产出会把日志淹掉。截断时会标出原始长度，便于判断是否要去读文件。
fn log_output(text: &str) -> String {
    let mut flat = String::with_capacity(text.len());
    for ch in text.chars() {
        if ch.is_whitespace() {
            if !flat.ends_with(' ') {
                flat.push(' ');
            }
        } else {
            flat.push(ch);
        }
    }
    let flat = flat.trim();
    let total = flat.chars().count();
    if total <= LOG_OUTPUT_MAX_CHARS {
        return flat.to_string();
    }
    let head: String = flat.chars().take(LOG_OUTPUT_MAX_CHARS).collect();
    format!("{head}…（共 {total} 字符，已截断）")
}

/// 按工具名精确取定义（输出整理步只需要 `builtin_read_file`）。
fn tool_definitions_for_names(tools: &ToolRegistry, names: &[&str]) -> Vec<ToolDefinition> {
    tools
        .get_enabled_tools_with_categories()
        .into_iter()
        .filter(|(tool, _)| names.contains(&tool.name.as_str()))
        .map(|(tool, _)| to_tool_definition(tool))
        .collect()
}

/// 执行器配置。
#[derive(Debug, Clone)]
pub struct ExecutorConfig {
    /// 每步工具循环的轮数上限。
    pub max_rounds_per_step: usize,
    /// 采样温度（`None` 用 provider 默认）。
    pub temperature: Option<f32>,
    /// 最大生成 token（`None` 用 provider 默认）。
    pub max_tokens: Option<u32>,
    /// 工具白名单，语义与 `ChatConfig::allowed_tools` **完全一致**：
    /// `None` = 全部启用工具（含 Utility / SubAgent，不过滤）；
    /// `Some(tokens)` = 各 token 取并集（`"all"` / 分类名 / 精确工具名）。
    pub allowed_tools: Option<Vec<String>>,
    /// 步骤产出落盘的根目录。
    ///
    /// **必有值** —— 宿主不配置时用 `./data/cache`（相对**进程 cwd**，见设计稿 §13-1）。
    /// 执行器在其下自建 `run-<毫秒>-<序号>` 子目录隔离每次执行：它不认识「会话」概念，
    /// 会话段由宿主拼进这里（`run_service` 传 `./data/cache/<session_id>`）。
    pub cache_dir: PathBuf,
    /// 产出超过该字符数就落盘（下游 `prior` 只拿到「文件说明 + 预览」）。
    pub spill_threshold_chars: usize,
    /// 落盘后 `prior` 里保留的预览长度（字符）—— 给模型判断相关性的线索，不是截断兜底。
    pub spill_preview_chars: usize,
    /// 单次 LLM 请求的超时（`None` = 不限制）。
    ///
    /// ⚠️ 语义是「**一次 `AiClient::chat_completion` 调用**的墙钟上限」：该调用在
    /// `ai-openai` 内部本身有 3 次重试，所以超时**包住的是整次调用**（含内层重试）。
    /// 刻意**不是**「整步 / 整次执行」的超时 —— 工作流可能天然很长，固定总时长会误杀。
    pub llm_timeout: Option<Duration>,
    /// 单次请求**超时**后的重试次数（总尝试次数 = 1 + 这个值）。
    ///
    /// 只重试超时：其它失败（4xx / 5xx / 网络）在 `ai-openai` 内部已按自己的策略重试过，
    /// 这一层再叠加会变成**倍数放大**。
    pub llm_timeout_retries: usize,
}

impl Default for ExecutorConfig {
    fn default() -> Self {
        Self {
            max_rounds_per_step: 50,
            temperature: None,
            max_tokens: None,
            allowed_tools: None,
            cache_dir: PathBuf::from(DEFAULT_CACHE_DIR),
            spill_threshold_chars: DEFAULT_SPILL_THRESHOLD_CHARS,
            spill_preview_chars: DEFAULT_SPILL_PREVIEW_CHARS,
            llm_timeout: Some(Duration::from_secs(DEFAULT_LLM_TIMEOUT_SECS)),
            llm_timeout_retries: DEFAULT_LLM_TIMEOUT_RETRIES,
        }
    }
}

/// 灵活计划执行器。
///
/// 与 `agent-gui` 无耦合：只认 `AiClient` 与 `ToolRegistry`，
/// 模板由调用方传入，报告返回给调用方。
pub struct FlexibleExecutor {
    ai: Arc<dyn AiClient>,
    tools: Arc<ToolRegistry>,
    cfg: ExecutorConfig,
}

impl FlexibleExecutor {
    /// 构造执行器。AI 客户端由宿主提供（如从 `AiManager` 取默认 provider）。
    pub fn new(ai: Arc<dyn AiClient>, tools: Arc<ToolRegistry>, cfg: ExecutorConfig) -> Self {
        Self { ai, tools, cfg }
    }

    /// 顺序执行整个模板，返回执行报告。
    ///
    /// - 单步失败不抛错：该步记 `Failed`，其后步骤记 `Skipped`，报告 `success = false`；
    /// - 取消同理：未执行的步骤记 `Skipped`（保留占位，UI 可显示 `N/M`）；
    /// - 展开失败（缺参数 / 未定义占位符）记为 `Failed` 并放事件，不中断整个流程。
    /// - 展开失败（缺参数 / 未定义占位符）记为 `Failed` 并放事件，不中断整个流程；
    /// - `environment` 为 `None` 时不拼环境段，system prompt 与历史行为**逐字一致**。
    pub async fn run(
        &self,
        template: &FlexiblePlanTemplate,
        params: &PlanRunParams,
        environment: Option<&RuntimeEnvironment>,
        sink: &dyn PlanRunSink,
        cancel: Option<watch::Receiver<bool>>,
    ) -> Result<PlanRunReport> {
        // 依赖校验：**只警告不阻断**（对齐「先观测、不挡路」的既定策略）。
        // 一条规则同时覆盖「引用不存在 / 自依赖 / 依赖后面的步骤 / 环」——
        // 线性执行 + 只允许引用前面 ⇒ 环必然表现为「依赖后面的步骤」。
        for issue in collect_dependency_issues(template) {
            tracing::warn!(issue = %issue, "计划依赖校验未通过（该步会拿不到前序数据）");
        }

        // 整次执行只算一次 system prompt，每步复用：同一次执行内每步字符串完全一致，
        // 使「环境段」成为可命中的 provider 前缀缓存（见 `prompt::step_system_prompt`）。
        let system_prompt = prompt::step_system_prompt(environment);
        let started = Instant::now();
        sink.emit(PlanRunEvent::RunStarted {
            total_steps: template.steps.len(),
        });
        // 执行链路原先一条日志都没有：失败原因只进事件/报告（只有 UI 看得到），
        // 事后排查日志里只能看到「底层在干活」的痕迹。这里把关键节点补齐。
        tracing::info!(
            steps = template.steps.len(),
            model = %self.ai.model_name(),
            max_rounds_per_step = self.cfg.max_rounds_per_step,
            allowed_tools = ?self.cfg.allowed_tools,
            cache_dir = %self.cfg.cache_dir.display(),
            task = %template.task.chars().take(80).collect::<String>(),
            // 整次执行只在这里打一次（每步复用同一个 system prompt，不必逐步重复）。
            // `%` 是 Display：环境段的多行会被原样输出，便于直接看清拼装结果。
            system_prompt = %system_prompt,
            "灵活计划开始执行"
        );

        let tools = self.tool_definitions();
        // 本次执行的产出缓存目录名（懒建：只有真要落盘时才 create_dir_all）。
        let run_dir = new_run_dir_name();
        let mut store: HashMap<String, StoredOutput> = HashMap::new();
        let mut records: Vec<StepRunRecord> = Vec::with_capacity(template.steps.len());

        for (offset, step) in template.steps.iter().enumerate() {
            let index = offset + 1;

            // 已取消 / 前序未全部成功 → 本步跳过（保留占位，UI 好显示 N/M）
            let blocked_earlier = records
                .iter()
                .any(|record| record.status != StepStatus::Done);
            if is_cancelled(&cancel) || blocked_earlier {
                let reason = if is_cancelled(&cancel) {
                    "用户取消"
                } else {
                    "前序步骤失败"
                };
                records.push(placeholder_record(
                    index,
                    step,
                    StepStatus::Skipped,
                    Some(reason),
                ));
                tracing::info!(step = index, reason, "步骤跳过");
                continue;
            }

            // `intent` 与 `expected_output` 都可能含 `${name}`（`placeholder::PLACEHOLDER_FIELDS`），
            // 两者都必须展开后才能交给 LLM —— 否则模型会在「期望产出」段读到未展开的 `${...}`。
            // 任一个失败都走同一条路径（该步 Failed），与「缺参数不带病执行」一致。
            let (intent, expected_output) = match (
                render_step_intent(step, params),
                render_step_expected_output(step, params),
            ) {
                (Ok(intent), Ok(expected_output)) => (intent, expected_output),
                (Err(err), _) | (_, Err(err)) => {
                    let message = err.to_string();
                    tracing::warn!(step = index, error = %message, "步骤参数展开失败，该步失败");
                    sink.emit(PlanRunEvent::Failed {
                        index: Some(index),
                        error: message.clone(),
                    });
                    records.push(placeholder_record(
                        index,
                        step,
                        StepStatus::Failed,
                        Some(message.as_str()),
                    ));
                    continue;
                }
            };

            sink.emit(PlanRunEvent::StepStarted {
                index,
                intent: intent.clone(),
            });
            tracing::info!(
                step = index,
                total = template.steps.len(),
                intent = %intent.chars().take(80).collect::<String>(),
                "步骤开始"
            );

            let prior = collect_prior(step, &store, self.cfg.spill_preview_chars);
            let result = run_step(
                &system_prompt,
                StepInput {
                    step,
                    intent: &intent,
                    expected_output: &expected_output,
                    prior: &prior,
                    tools: &tools,
                    index,
                },
                &self.ai,
                &self.tools,
                &self.cfg,
                sink,
                cancel.as_ref(),
            )
            .await;

            // 产出落盘：超阈值就写文件，下游 `prior` 只拿到「文件说明 + 预览」。
            // 落盘失败 → 该步 `Failed`（不静默降级，见设计稿 §7）。
            let mut record = result.record;
            if let Some(output) = &result.output {
                match spill_output(
                    &self.cfg.cache_dir,
                    &run_dir,
                    index,
                    self.cfg.spill_threshold_chars,
                    output,
                )
                .await
                {
                    Ok(spilled) => {
                        if let Some(spilled) = &spilled {
                            record.output_file = Some(spilled.path_string());
                            tracing::info!(
                                step = index,
                                file = %spilled.path_string(),
                                lines = spilled.lines,
                                bytes = spilled.bytes,
                                "步骤产出已落盘，下游按需读取"
                            );
                        }
                        store.insert(
                            step.result_reference.clone(),
                            StoredOutput {
                                content: output.clone(),
                                spilled,
                            },
                        );
                    }
                    Err(err) => {
                        let message = format!("产出落盘失败：{err}");
                        tracing::warn!(step = index, error = %message, "产出落盘失败，该步记为失败");
                        record.status = StepStatus::Failed;
                        record.error = Some(message);
                    }
                }
            }
            if record.status == StepStatus::Failed {
                tracing::warn!(
                    step = index,
                    rounds = record.rounds,
                    tool_calls = record.tool_calls,
                    tools = %summarize_tools(&record.tool_sequence),
                    duration_ms = record.duration_ms,
                    error = record.error.as_deref().unwrap_or("未记录原因"),
                    output_file = ?record.output_file,
                    output = %result.output.as_deref().map(log_output).unwrap_or_default(),
                    "步骤失败"
                );
            } else {
                tracing::info!(
                    step = index,
                    status = ?record.status,
                    rounds = record.rounds,
                    tool_calls = record.tool_calls,
                    tools = %summarize_tools(&record.tool_sequence),
                    duration_ms = record.duration_ms,
                    output_file = ?record.output_file,
                    output = %result.output.as_deref().map(log_output).unwrap_or_default(),
                    "步骤结束"
                );
            }
            sink.emit(PlanRunEvent::StepFinished {
                index,
                record: record.clone(),
            });
            records.push(record);
        }

        // 只有模板里的每一步都 Done 才算成功（空模板不算成功）。
        // 不用可变标志：跳过分支不改标志会在「一上来就取消」时漏置。
        // 末尾可能追加的输出整理步**不参与**这里 —— 它失败只影响「有没有最终结果」，
        // 不改变任务本身的成败（所以必须在追加它之前先算完）。
        let success = !records.is_empty()
            && records
                .iter()
                .all(|record| record.status == StepStatus::Done);

        // ── 输出整理步 ──
        // 任务步全部成功后，按 `output_schema` 把交付步的输出整理成「最终结果」：
        // 契约缺失（用户跳过输出定义）则退化为交付步原文；任务未成功则不整理。
        let result = if success {
            self.resolve_result(
                template,
                params,
                &store,
                &mut records,
                sink,
                cancel.as_ref(),
            )
            .await
        } else {
            None
        };

        let report = PlanRunReport {
            success,
            total_duration_ms: started.elapsed().as_millis() as u64,
            prompt_tokens: records.iter().map(|record| record.prompt_tokens).sum(),
            completion_tokens: records.iter().map(|record| record.completion_tokens).sum(),
            tool_calls: records.iter().map(|record| record.tool_calls).sum(),
            steps: records,
            result,
        };
        if report.success {
            tracing::info!(
                steps = report.steps.len(),
                duration_ms = report.total_duration_ms,
                prompt_tokens = report.prompt_tokens,
                completion_tokens = report.completion_tokens,
                tool_calls = report.tool_calls,
                "灵活计划执行成功"
            );
        } else {
            let failed = report
                .steps
                .iter()
                .filter(|record| record.status == StepStatus::Failed)
                .count();
            let skipped = report
                .steps
                .iter()
                .filter(|record| record.status == StepStatus::Skipped)
                .count();
            tracing::warn!(
                failed,
                skipped,
                steps = report.steps.len(),
                duration_ms = report.total_duration_ms,
                prompt_tokens = report.prompt_tokens,
                completion_tokens = report.completion_tokens,
                tool_calls = report.tool_calls,
                "灵活计划执行结束：未全部成功"
            );
        }
        sink.emit(PlanRunEvent::RunFinished {
            report: report.clone(),
        });
        Ok(report)
    }

    /// 构建暴露给 LLM 的工具定义。
    ///
    /// 白名单过滤复用 `chat` 的 `select_tools_by_tokens` —— 规则单一来源，不做第二套。
    fn tool_definitions(&self) -> Vec<ToolDefinition> {
        let enabled = self.tools.get_enabled_tools_with_categories();
        let selected: Vec<Tool> = match &self.cfg.allowed_tools {
            None => enabled.into_iter().map(|(tool, _)| tool).collect(),
            Some(tokens) => crate::chat::tools::select_tools_by_tokens(enabled, tokens),
        };
        selected.into_iter().map(to_tool_definition).collect()
    }

    /// 按 `output_schema` 整理最终结果（必要时向 `records` 追加一步）。
    ///
    /// 返回 `None` 的四种情形：契约缺失且交付步无输出、没有可用的交付输出、
    /// 契约非法（同时补一条 `Failed` 的整理步）、整理步自身失败。
    async fn resolve_result(
        &self,
        template: &FlexiblePlanTemplate,
        params: &PlanRunParams,
        store: &HashMap<String, StoredOutput>,
        records: &mut Vec<StepRunRecord>,
        sink: &dyn PlanRunSink,
        cancel: Option<&watch::Receiver<bool>>,
    ) -> Option<String> {
        let Some(raw) = &template.output_schema else {
            // 没有契约（用户跳过输出定义步 / 选「定不了」）：结果退化为交付步原文
            let output = deliverable_output(template, store).map(|(_, output)| output.content.clone());
            tracing::info!(
                has_result = output.is_some(),
                "无输出契约：以交付步输出作为结果"
            );
            return output;
        };

        let schema = match OutputSchema::parse(raw) {
            Ok(schema) => schema,
            Err(reason) => {
                // 契约非法是**数据问题**：记一步 `Failed` 让原因可见，但不改变任务成败
                tracing::warn!(error = %reason, "输出契约非法，跳过结果整理");
                let index = records.len() + 1;
                sink.emit(PlanRunEvent::Failed {
                    index: Some(index),
                    error: reason.clone(),
                });
                records.push(resolve_failed_record(index, reason));
                return None;
            }
        };

        let Some((reference, stored)) = deliverable_output(template, store) else {
            tracing::warn!("有输出契约但没有可用的交付输出，跳过结果整理");
            return None;
        };

        let index = records.len() + 1;
        let intent = "按输出契约整理本次执行的最终结果".to_string();
        // 契约文本里的 `${name}` 换成本次参数的实际值（渲染失败就退回原文，不阻断）
        let contract = params
            .render(&prompt::build_output_contract_text(&schema))
            .unwrap_or_else(|err| {
                tracing::warn!(error = %err, "输出契约渲染失败，用未渲染文本");
                prompt::build_output_contract_text(&schema)
            });

        // 交付步的产出必须给：未落盘时是全文，落盘后是「文件说明 + 预览」
        // （整理步因此需要 `builtin_read_file`，见下方 tools）。
        let preview_chars = self.cfg.spill_preview_chars;
        let mut prior = Vec::with_capacity(template.steps.len() + 1);
        prior.push((
            format!("{reference}（交付步的产出）"),
            render_prior_output(stored, preview_chars),
        ));
        // 各步产出一并给出，让整理步在末步信息不全时能回看 —— 超阈值的同样落盘，
        // 所以这里不会随步数线性膨胀（落盘后每条只剩「文件说明 + 预览」）。
        for step in template.steps.iter() {
            if step.result_reference == reference {
                continue;
            }
            if let Some(stored) = store.get(&step.result_reference) {
                prior.push((
                    format!("{} 的产出", step.result_reference),
                    render_prior_output(stored, preview_chars),
                ));
            }
        }

        let resolve_step = PlanStep {
            result_reference: RESOLVE_RESULT_REFERENCE.to_string(),
            intent: intent.clone(),
            expected_output: contract,
            dependencies: Vec::new(),
        };
        sink.emit(PlanRunEvent::StepStarted {
            index,
            intent: intent.clone(),
        });
        // 交付产出可能已落盘 —— 整理步必须能读回来（「不带工具」在落盘机制下不再成立）。
        // 只给这一个只读工具：整理仍是分析，不需要别的外部数据。
        let resolve_tools = tool_definitions_for_names(&self.tools, &["builtin_read_file"]);
        let outcome = run_output_resolve(
            StepInput {
                step: &resolve_step,
                intent: &intent,
                expected_output: resolve_step.expected_output.as_str(),
                prior: &prior,
                tools: &resolve_tools,
                index,
            },
            &self.ai,
            &self.tools,
            &self.cfg,
            sink,
            cancel,
        )
        .await;
        let result = outcome
            .output
            .clone()
            .filter(|_| outcome.record.status == StepStatus::Done);
        tracing::info!(
            step = index,
            status = ?outcome.record.status,
            result_chars = result.as_deref().map(str::len).unwrap_or(0),
            result = %result.as_deref().map(log_output).unwrap_or_default(),
            "输出整理步结束"
        );
        sink.emit(PlanRunEvent::StepFinished {
            index,
            record: outcome.record.clone(),
        });
        records.push(outcome.record);
        result
    }
}

/// 输出整理步的结果引用标识（它不是模板步骤，标签固定）。
const RESOLVE_RESULT_REFERENCE: &str = "#RESULT";

/// 交付步的输出 —— 即最后一个「拿到了输出」的模板步骤。
///
/// 模板是粗粒度**线性骨架**，最后一步就是交付步（依赖图「出度 0」的判定在该形态下与
/// 「最后一步」等价，所以不引入依赖图分析）。失败 / 跳过的步骤不会进 `store`
/// （只有真拿到 output 才写入），所以这里天然只会取到有产出的那一步。
fn deliverable_output<'t, 's>(
    template: &'t FlexiblePlanTemplate,
    store: &'s HashMap<String, StoredOutput>,
) -> Option<(&'t str, &'s StoredOutput)> {
    template.steps.iter().rev().find_map(|step| {
        store
            .get(&step.result_reference)
            .map(|output| (step.result_reference.as_str(), output))
    })
}

/// 契约非法时补一条 `Failed` 的整理步记录（让「为什么没有结果」在报告里可见）。
fn resolve_failed_record(index: usize, error: String) -> StepRunRecord {
    StepRunRecord {
        index,
        result_reference: RESOLVE_RESULT_REFERENCE.to_string(),
        intent: "按输出契约整理本次执行的最终结果".to_string(),
        expected_output: "（输出契约非法，未执行整理）".to_string(),
        status: StepStatus::Failed,
        duration_ms: 0,
        prompt_tokens: 0,
        completion_tokens: 0,
        tool_calls: 0,
        tool_sequence: vec![],
        rounds: 0,
        call_usages: vec![],
        output_summary: None,
        output: None,
        output_truncated: false,
        output_file: None,
        error: Some(error),
    }
}

/// 校验步骤依赖：每个 `dependencies` 引用的 `result_reference` 必须**已在本步之前出现**。
///
/// 一条规则同时覆盖四种模板错误：引用不存在、自依赖、依赖后面的步骤、依赖环
/// （线性执行 + 只允许引用前面 ⇒ 环必然表现为「依赖后面的步骤」）。
///
/// 返回问题描述（空 = 通过）。**调用方只警告不阻断** —— 运行期 [`collect_prior`]
/// 会再兜底一次（对未命中的引用跳过并告警）。
///
/// 为什么能静态判定：「前序失败导致没产出」不会走到 `collect_prior` ——
/// 一有步骤非 `Done`，其后每步都直接记 `Skipped`（见 `run` 的 `blocked_earlier`）。
fn collect_dependency_issues(template: &FlexiblePlanTemplate) -> Vec<String> {
    let mut seen: HashSet<&str> = HashSet::with_capacity(template.steps.len());
    let mut issues = Vec::new();
    for (offset, step) in template.steps.iter().enumerate() {
        for dependency in &step.dependencies {
            if !seen.contains(dependency.as_str()) {
                issues.push(format!(
                    "步骤 {}（{}）的依赖 {} 未在此之前出现",
                    offset + 1,
                    step.result_reference,
                    dependency
                ));
            }
        }
        seen.insert(step.result_reference.as_str());
    }
    issues
}

/// 收集某步依赖项的实际输出（按 `dependencies` 顺序）。
///
/// 未命中的引用会被跳过 —— 正常情况下不该发生（能执行到本步 ⇒ 前序全 `Done`
/// ⇒ 产出都在 store 里），走到这里只可能是模板依赖写错（`collect_dependency_issues`
/// 只警告未阻断），故留一条 warn 作痕迹。
fn collect_prior(
    step: &PlanStep,
    store: &HashMap<String, StoredOutput>,
    preview_chars: usize,
) -> Vec<(String, String)> {
    let mut prior = Vec::with_capacity(step.dependencies.len());
    for reference in &step.dependencies {
        match store.get(reference) {
            Some(output) => prior.push((
                reference.clone(),
                render_prior_output(output, preview_chars),
            )),
            None => tracing::warn!(
                step = %step.result_reference,
                reference = %reference,
                "依赖结果不在结果表中，已跳过（模板依赖可能写错）"
            ),
        }
    }
    prior
}

/// 未执行步骤的占位记录（`Skipped` / 展开失败）。
fn placeholder_record(
    index: usize,
    step: &PlanStep,
    status: StepStatus,
    error: Option<&str>,
) -> StepRunRecord {
    StepRunRecord {
        index,
        result_reference: step.result_reference.clone(),
        intent: step.intent.clone(),
        expected_output: step.expected_output.clone(),
        status,
        duration_ms: 0,
        prompt_tokens: 0,
        completion_tokens: 0,
        tool_calls: 0,
        tool_sequence: vec![],
        rounds: 0,
        call_usages: vec![],
        output_summary: None,
        output: None,
        output_truncated: false,
        output_file: None,
        error: error.map(str::to_string),
    }
}

/// `Tool` → LLM 侧的 `ToolDefinition`。
fn to_tool_definition(tool: Tool) -> ToolDefinition {
    ToolDefinition {
        r#type: ToolType::Function,
        function: FunctionDefinition {
            name: tool.name,
            description: Some(tool.description),
            parameters: Some(tool.input_schema),
            strict: None,
        },
    }
}

/// 报告里工具序列的**日志摘要**：去重后的工具名，按首次出现顺序，逗号分隔。
///
/// 步骤日志里 `tool_calls=3` 只说「调了几次」，看不出「调了什么」；
/// 补上 `tools=read_file,write_file` 才能一眼判断这一步用没用（用错了）工具。
fn summarize_tools(sequence: &[ToolCallRecord]) -> String {
    let mut seen: Vec<&str> = Vec::new();
    for call in sequence {
        if !seen.contains(&call.tool.as_str()) {
            seen.push(call.tool.as_str());
        }
    }
    seen.join(",")
}

/// 是否已收到取消信号。
fn is_cancelled(cancel: &Option<watch::Receiver<bool>>) -> bool {
    cancel
        .as_ref()
        .map(|receiver| *receiver.borrow())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    use async_trait::async_trait;
    use planned_agent_core::ai::types::{ChatCompletionRequest, ChatCompletionResponse};
    use planned_agent_core::ai::ChatCompletionStream;

    use crate::flexible::testing::{
        fake_tool, text_response, tool_response, FakeAiClient, FakeTool, RecordingSink,
    };
    use planned_agent_core::tool_registry::ToolCategory;
    use serde_json::json;
    use crate::flexible::{PlanInput, PlanStep};

    /// 两步模板：第二步依赖第一步的 `#E1`，且 `intent` 含 `${path}` 占位符。
    fn two_step_template() -> FlexiblePlanTemplate {
        FlexiblePlanTemplate {
            output_schema: None,
            task: "维护文件".to_string(),
            inputs: vec![PlanInput {
                name: "path".to_string(),
                default: Some(serde_json::json!("a.txt")),
                description: None,
            }],
            steps: vec![
                PlanStep {
                    result_reference: "#E1".to_string(),
                    intent: "读取 ${path}".to_string(),
                    expected_output: "文件内容".to_string(),
                    dependencies: vec![],
                },
                PlanStep {
                    result_reference: "#E2".to_string(),
                    intent: "基于 #E1 追加一行".to_string(),
                    expected_output: "追加完成".to_string(),
                    dependencies: vec!["#E1".to_string()],
                },
            ],
        }
    }

    /// 有契约（`bool`）→ 追加一个整理步，结果取它的输出；契约渲染进请求，且**不带工具**。
    #[tokio::test]
    async fn output_contract_appends_resolve_step_and_returns_result() {
        let ai = FakeAiClient::new(vec![
            text_response("第一步产出", 10, 1),
            text_response("第二步产出", 20, 2),
            text_response("文件末尾已新增一行（index=8）", 30, 3),
        ]);
        let template = FlexiblePlanTemplate {
            output_schema: Some(serde_json::json!({
                "kind": "bool",
                "goal": "向 ${path} 末尾追加一行",
                "success": "文件末尾新增一行即视为成功"
            })),
            ..two_step_template()
        };
        let params = PlanRunParams::from_template(&template);
        let sink = RecordingSink::default();

        let report = executor(ai.clone())
            .run(&template, &params, None, &sink, None)
            .await
            .expect("run 不应失败");

        assert!(report.success, "整理步不参与任务成败");
        assert_eq!(report.steps.len(), 3, "两个模板步 + 一个整理步");
        let resolve = &report.steps[2];
        assert_eq!(resolve.result_reference, "#RESULT");
        assert_eq!(resolve.index, 3);
        assert_eq!(resolve.status, StepStatus::Done);
        assert_eq!(
            report.result.as_deref(),
            Some("文件末尾已新增一行（index=8）"),
            "最终结果 = 整理步的输出"
        );

        // 第三次请求是整理步：system 换专用 prompt、契约渲染成实际值、交付步输出带上了
        let requests = ai.requests();
        assert_eq!(requests.len(), 3);
        let messages = serde_json::to_string(&requests[2].messages).unwrap();
        assert!(
            messages.contains("结果整理助手"),
            "应换整理专用 system prompt: {messages}"
        );
        assert!(
            messages.contains("文件末尾新增一行即视为成功"),
            "契约应进请求: {messages}"
        );
        assert!(
            messages.contains("第二步产出"),
            "交付步输出应进请求: {messages}"
        );
        assert!(
            messages.contains("向 a.txt 末尾追加一行"),
            "${{path}} 应被渲染: {messages}"
        );
        assert!(requests[2].tools.is_none(), "整理步不应带工具");

        // 整理步也要有自己的事件，UI 的 pipeline 才能显示它
        let events = sink.events();
        assert!(events.iter().any(|event| matches!(
            event,
            PlanRunEvent::StepStarted { index: 3, intent } if intent.contains("整理")
        )));
        assert!(events
            .iter()
            .any(|event| matches!(event, PlanRunEvent::StepFinished { index: 3, .. })));
    }

    /// 没有契约（用户跳过输出定义）→ 不追加整理步，结果退化为交付步原文，也不多花一次 LLM 调用。
    #[tokio::test]
    async fn missing_contract_falls_back_to_deliverable_output() {
        let ai = FakeAiClient::new(vec![
            text_response("第一步产出", 10, 1),
            text_response("第二步产出", 20, 2),
        ]);
        let template = two_step_template();
        let params = PlanRunParams::from_template(&template);
        let sink = RecordingSink::default();

        let report = executor(ai.clone())
            .run(&template, &params, None, &sink, None)
            .await
            .expect("run 不应失败");

        assert_eq!(report.steps.len(), 2, "无契约不应追加整理步");
        assert_eq!(report.result.as_deref(), Some("第二步产出"));
        assert_eq!(ai.requests().len(), 2, "无契约不应多一次 LLM 调用");
    }

    /// `expected_output` 里的 `${name}` 必须展开后才进 prompt；
    /// 而执行记录里仍保留**模板原文**（UI 展示用）。
    #[tokio::test]
    async fn expands_expected_output_in_prompt_but_keeps_raw_in_record() {
        let ai = FakeAiClient::new(vec![
            text_response("第一步产出", 10, 1),
            text_response("第二步产出", 20, 2),
        ]);
        let template = FlexiblePlanTemplate {
            steps: vec![
                PlanStep {
                    result_reference: "#E1".to_string(),
                    intent: "读取 ${path}".to_string(),
                    // 占位符写在 expected_output 里 —— 它必须展开后才能发给模型
                    expected_output: "${path} 的末尾新增一行".to_string(),
                    dependencies: vec![],
                },
                PlanStep {
                    result_reference: "#E2".to_string(),
                    intent: "收尾".to_string(),
                    expected_output: "完成".to_string(),
                    dependencies: vec!["#E1".to_string()],
                },
            ],
            ..two_step_template()
        };
        let params = PlanRunParams::from_template(&template);
        let sink = RecordingSink::default();

        let report = executor(ai.clone())
            .run(&template, &params, None, &sink, None)
            .await
            .expect("run 不应失败");

        let first = serde_json::to_string(&ai.requests()[0].messages).unwrap();
        assert!(
            first.contains("a.txt 的末尾新增一行"),
            "期望产出应展开为实际值: {first}"
        );
        assert!(
            !first.contains("${path}"),
            "不得把未展开的占位符发给模型: {first}"
        );

        assert_eq!(
            report.steps[0].expected_output, "${path} 的末尾新增一行",
            "执行记录仍应是模板原文"
        );
    }

    /// `expected_output` 的占位符缺值时该步失败（与 `intent` 同一路径），
    /// 且**在发起 LLM 调用之前**就失败。
    #[tokio::test]
    async fn missing_param_in_expected_output_fails_step_before_llm() {
        let ai = FakeAiClient::new(vec![]);
        let template = FlexiblePlanTemplate {
            output_schema: None,
            task: "维护文件".to_string(),
            inputs: vec![PlanInput {
                name: "no_default".to_string(),
                default: None,
                description: None,
            }],
            steps: vec![PlanStep {
                result_reference: "#E1".to_string(),
                intent: "做事".to_string(),
                expected_output: "写入 ${no_default}".to_string(),
                dependencies: vec![],
            }],
        };
        let params = PlanRunParams::from_template(&template);
        let sink = RecordingSink::default();

        let report = executor(ai.clone())
            .run(&template, &params, None, &sink, None)
            .await
            .expect("run 不应失败");

        assert!(!report.success);
        assert_eq!(report.steps[0].status, StepStatus::Failed);
        assert!(
            report.steps[0]
                .error
                .as_deref()
                .unwrap_or("")
                .contains("expected_output"),
            "失败原因应点名 expected_output: {:?}",
            report.steps[0].error
        );
        assert!(ai.requests().is_empty(), "展开失败不该发起 LLM 调用");
    }

    /// 依赖校验：引用必须**已在本步之前出现**，一条规则覆盖不存在 / 自依赖 / 依赖后续步。
    #[test]
    fn dependency_issues_cover_missing_self_and_forward_refs() {
        let template = FlexiblePlanTemplate {
            output_schema: None,
            task: "t".to_string(),
            inputs: vec![],
            steps: vec![
                PlanStep {
                    result_reference: "#E1".to_string(),
                    intent: "一".to_string(),
                    expected_output: "a".to_string(),
                    dependencies: vec!["#E2".to_string()], // 依赖后面的步骤
                },
                PlanStep {
                    result_reference: "#E2".to_string(),
                    intent: "二".to_string(),
                    expected_output: "b".to_string(),
                    dependencies: vec!["#E2".to_string()], // 自依赖
                },
                PlanStep {
                    result_reference: "#E3".to_string(),
                    intent: "三".to_string(),
                    expected_output: "c".to_string(),
                    dependencies: vec!["#E9".to_string()], // 引用不存在
                },
            ],
        };
        let issues = collect_dependency_issues(&template);
        assert_eq!(issues.len(), 3, "三处都该报警: {issues:?}");
        assert!(issues[0].contains("#E2"), "{}", issues[0]);
        assert!(issues[1].contains("#E2"), "{}", issues[1]);
        assert!(issues[2].contains("#E9"), "{}", issues[2]);
    }

    /// 只引用前面步骤的合法依赖不该报警。
    #[test]
    fn dependency_issues_accept_backward_refs() {
        // `two_step_template` 的 #E2 依赖 #E1（在其之前）
        assert!(collect_dependency_issues(&two_step_template()).is_empty());
    }

    /// 依赖写错只警告不阻断：执行照样跑完，只是该步拿不到 prior。
    #[tokio::test]
    async fn bad_dependency_warns_but_does_not_block() {
        let ai = FakeAiClient::new(vec![text_response("产出", 10, 1)]);
        let template = FlexiblePlanTemplate {
            output_schema: None,
            task: "t".to_string(),
            inputs: vec![],
            steps: vec![PlanStep {
                result_reference: "#E1".to_string(),
                intent: "一".to_string(),
                expected_output: "a".to_string(),
                dependencies: vec!["#E9".to_string()],
            }],
        };
        let params = PlanRunParams::from_template(&template);
        let sink = RecordingSink::default();

        let report = executor(ai.clone())
            .run(&template, &params, None, &sink, None)
            .await
            .expect("run 不应失败");

        assert!(report.success, "依赖写错不该阻断执行");
        assert_eq!(ai.requests().len(), 1, "仍应正常发起一次 LLM 调用");
    }

    /// 契约非法是数据问题：记一条 `Failed` 的整理步让原因可见，但**不**把任务判失败。
    #[tokio::test]
    async fn invalid_contract_records_failed_resolve_step_without_failing_run() {
        let ai = FakeAiClient::new(vec![
            text_response("第一步产出", 10, 1),
            text_response("第二步产出", 20, 2),
        ]);
        let template = FlexiblePlanTemplate {
            output_schema: Some(serde_json::json!({ "kind": "success_only", "success": "s" })),
            ..two_step_template()
        };
        let params = PlanRunParams::from_template(&template);
        let sink = RecordingSink::default();

        let report = executor(ai.clone())
            .run(&template, &params, None, &sink, None)
            .await
            .expect("run 不应失败");

        assert!(report.success, "契约非法不该把任务判成失败");
        assert_eq!(report.steps.len(), 3);
        let resolve = &report.steps[2];
        assert_eq!(resolve.status, StepStatus::Failed);
        assert!(resolve
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("success_only"));
        assert!(report.result.is_none());
        assert_eq!(ai.requests().len(), 2, "契约非法时不该再调一次 LLM");
    }

    // ── C1：工具序列进报告 ──

    /// 把假工具注册进 registry。
    fn register(registry: &ToolRegistry, name: &str, tool: Arc<FakeTool>) {
        registry.register_custom_tool(
            planned_agent_core::mcp::types::Tool {
                name: name.to_string(),
                description: "测试工具".to_string(),
                input_schema: json!({"type": "object"}),
            },
            vec![ToolCategory::File],
            tool,
        );
    }

    /// C1：每步记下「调了哪些工具、什么入参、成功没有」。
    ///
    /// 在此之前报告里只有 `tool_calls`（**次数**）—— 事后想知道那几次是什么工具、
    /// 在哪一步、传了什么，只能去翻 UI 轨迹（而且那份入参已被截到 120 字符）。
    #[tokio::test]
    async fn report_records_tool_sequence_per_step() {
        let registry = Arc::new(ToolRegistry::new());
        register(
            &registry,
            "read_file",
            fake_tool("read_file", json!({"content": "甲"}), false),
        );
        register(
            &registry,
            "write_file",
            fake_tool("write_file", json!({"error": "磁盘满"}), true),
        );

        let ai = FakeAiClient::new(vec![
            // 第 1 步第 1 轮：要调两个工具（先成功、后工具层报错）
            tool_response("call-1", "read_file", json!({"path": "a.txt"}), 10, 1),
            tool_response(
                "call-2",
                "write_file",
                json!({"path": "b.txt", "content": "乙"}),
                11,
                2,
            ),
            // 第 1 步第 2 轮：给正文收敛
            text_response("第一步产出", 12, 3),
            // 第 2 步：不调工具
            text_response("第二步产出", 20, 4),
        ]);

        let template = two_step_template();
        let params = PlanRunParams::from_template(&template);
        let sink = RecordingSink::default();
        let executor =
            FlexibleExecutor::new(ai as Arc<dyn AiClient>, registry, ExecutorConfig::default());

        let report = executor
            .run(&template, &params, None, &sink, None)
            .await
            .expect("run 不应失败");

        // 第 1 步：两条记录，**按发生顺序**，工具名/入参/成败都对得上。
        let first = &report.steps[0];
        assert_eq!(first.tool_calls, 2);
        assert_eq!(first.tool_sequence.len(), 2);
        assert_eq!(first.tool_sequence[0].tool, "read_file");
        assert!(
            first.tool_sequence[0].args.contains("a.txt"),
            "入参应带路径：{:?}",
            first.tool_sequence[0].args
        );
        assert!(first.tool_sequence[0].ok);
        assert_eq!(first.tool_sequence[1].tool, "write_file");
        assert!(first.tool_sequence[1].args.contains("b.txt"));
        assert!(
            !first.tool_sequence[1].ok,
            "工具返回错误结果应记 ok=false"
        );

        // 第 2 步：没调工具 → 空序列（而不是缺字段/None）。
        assert_eq!(report.steps[1].tool_calls, 0);
        assert!(report.steps[1].tool_sequence.is_empty());
    }

    fn executor(ai: Arc<FakeAiClient>) -> FlexibleExecutor {
        FlexibleExecutor::new(
            ai as Arc<dyn AiClient>,
            Arc::new(ToolRegistry::new()),
            ExecutorConfig::default(),
        )
    }

    /// 可指定配置的执行器（落盘相关用例需要自定义 `cache_dir` / 阈值）。
    fn executor_with(ai: Arc<FakeAiClient>, cfg: ExecutorConfig) -> FlexibleExecutor {
        FlexibleExecutor::new(ai as Arc<dyn AiClient>, Arc::new(ToolRegistry::new()), cfg)
    }

    /// 任意 `AiClient` 的执行器（B1 的 fake 不是 `FakeAiClient`）。
    fn executor_any(ai: Arc<dyn AiClient>, cfg: ExecutorConfig) -> FlexibleExecutor {
        FlexibleExecutor::new(ai, Arc::new(ToolRegistry::new()), cfg)
    }

    /// 单步模板（B1 用例不需要步骤间的依赖关系）。
    fn one_step_template() -> FlexiblePlanTemplate {
        FlexiblePlanTemplate {
            output_schema: None,
            task: "单步任务".to_string(),
            inputs: vec![],
            steps: vec![PlanStep {
                result_reference: "#E1".to_string(),
                intent: "随便做点事".to_string(),
                expected_output: "做完".to_string(),
                dependencies: vec![],
            }],
        }
    }

    // ── B1：LLM 请求的超时、重试与取消 ──

    /// 永不返回的 AI：用来造「请求挂住」。
    struct HangingAi;

    #[async_trait]
    impl AiClient for HangingAi {
        async fn chat_completion(
            &self,
            _request: ChatCompletionRequest,
        ) -> anyhow::Result<ChatCompletionResponse> {
            std::future::pending::<()>().await;
            unreachable!("永不返回")
        }

        async fn chat_completion_stream(
            &self,
            _request: ChatCompletionRequest,
        ) -> anyhow::Result<ChatCompletionStream> {
            anyhow::bail!("不支持流式")
        }

        fn provider_name(&self) -> &str {
            "hanging"
        }

        fn model_name(&self) -> &str {
            "hanging-model"
        }

        fn default_config(&self) -> ChatCompletionRequest {
            ChatCompletionRequest {
                model: "hanging-model".to_string(),
                messages: vec![],
                tools: None,
                temperature: None,
                max_tokens: None,
                stream: false,
                extra: Default::default(),
            }
        }
    }

    /// 第一次调用永久挂起、之后正常：造「第一次超时、重试成功」。
    struct SlowThenOkAi {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl AiClient for SlowThenOkAi {
        async fn chat_completion(
            &self,
            _request: ChatCompletionRequest,
        ) -> anyhow::Result<ChatCompletionResponse> {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                std::future::pending::<()>().await;
            }
            Ok(text_response("重试后的产出", 7, 3))
        }

        async fn chat_completion_stream(
            &self,
            _request: ChatCompletionRequest,
        ) -> anyhow::Result<ChatCompletionStream> {
            anyhow::bail!("不支持流式")
        }

        fn provider_name(&self) -> &str {
            "slow-then-ok"
        }

        fn model_name(&self) -> &str {
            "slow-then-ok-model"
        }

        fn default_config(&self) -> ChatCompletionRequest {
            ChatCompletionRequest {
                model: "slow-then-ok-model".to_string(),
                messages: vec![],
                tools: None,
                temperature: None,
                max_tokens: None,
                stream: false,
                extra: Default::default(),
            }
        }
    }

    /// B1a：LLM 请求**进行中**收到取消，必须立刻结束（而不是等请求自己返回）。
    #[tokio::test]
    async fn cancel_interrupts_in_flight_llm_request() {
        let template = one_step_template();
        let params = PlanRunParams::from_template(&template);
        let sink = RecordingSink::default();
        let (tx, rx) = watch::channel(false);

        let cancel_after_a_beat = async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let _ = tx.send(true);
        };
        let executor = executor_any(Arc::new(HangingAi), ExecutorConfig::default());
        let run = executor.run(&template, &params, None, &sink, Some(rx));

        let (report, ()) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(run, cancel_after_a_beat)
        })
        .await
        .expect("取消后应迅速结束 —— 请求进行中也必须能被中断（否则会话会卡死）");
        let report = report.expect("run 本身不失败（成败看 report）");

        assert!(!report.success);
        assert_eq!(report.steps[0].status, StepStatus::Failed);
        assert_eq!(
            report.steps[0].error.as_deref(),
            Some("用户取消"),
            "应记为取消而非其它失败"
        );
    }

    /// B1b：第一次请求超时 → 重试 → 成功。
    #[tokio::test]
    async fn llm_timeout_retries_and_then_succeeds() {
        let ai = Arc::new(SlowThenOkAi {
            calls: AtomicUsize::new(0),
        });
        let template = one_step_template();
        let params = PlanRunParams::from_template(&template);
        let sink = RecordingSink::default();
        let cfg = ExecutorConfig {
            llm_timeout: Some(Duration::from_millis(30)),
            llm_timeout_retries: 1,
            ..ExecutorConfig::default()
        };

        let report = executor_any(ai.clone(), cfg)
            .run(&template, &params, None, &sink, None)
            .await
            .expect("run 不应失败");

        assert!(report.success, "第一次超时后重试应成功");
        assert_eq!(ai.calls.load(Ordering::SeqCst), 2, "应恰好两次尝试");
    }

    /// B1b：一直超时 → 用尽重试次数 → 该步失败（且**不会**永久挂住）。
    #[tokio::test]
    async fn llm_timeout_exhausts_retries_then_fails() {
        let template = one_step_template();
        let params = PlanRunParams::from_template(&template);
        let sink = RecordingSink::default();
        let cfg = ExecutorConfig {
            llm_timeout: Some(Duration::from_millis(20)),
            llm_timeout_retries: 1,
            ..ExecutorConfig::default()
        };

        let report = tokio::time::timeout(
            Duration::from_secs(5),
            executor_any(Arc::new(HangingAi), cfg).run(&template, &params, None, &sink, None),
        )
        .await
        .expect("配了超时就不该永久挂起")
        .expect("run 本身不失败（成败看 report）");

        assert!(!report.success);
        let error = report.steps[0].error.as_deref().unwrap_or_default();
        assert!(error.contains("超时"), "错误应说明超时: {error}");
        assert!(error.contains("已尝试 2 次"), "错误应体现重试次数: {error}");
    }

    /// 每个用例一个独立临时目录（并行用例互不干扰）。
    fn temp_cache_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "planned-agent-flexible-spill-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// 产出超过阈值 → 落盘；下游 `prior` 只拿到「文件说明 + 预览」，不含全文。
    #[tokio::test]
    async fn large_output_spills_to_file_and_prior_gives_path() {
        let long = "甲".repeat(9_000); // 超过默认阈值 8000
        let ai = FakeAiClient::new(vec![
            text_response(&long, 10, 1),
            text_response("第二步完成", 20, 2),
        ]);
        let template = two_step_template();
        let params = PlanRunParams::from_template(&template);
        let sink = RecordingSink::default();
        let cache_dir = temp_cache_dir("large");

        let report = executor_with(
            ai.clone(),
            ExecutorConfig {
                cache_dir: cache_dir.clone(),
                ..ExecutorConfig::default()
            },
        )
        .run(&template, &params, None, &sink, None)
        .await
        .expect("run 不应失败");

        assert!(report.success);
        let file = report.steps[0]
            .output_file
            .clone()
            .expect("超出阈值应落盘");
        let path = PathBuf::from(&file);
        assert!(path.starts_with(&cache_dir), "落盘应在 cache_dir 下：{file}");
        let written = std::fs::read_to_string(&path).expect("落盘文件应可读");
        assert!(
            written == long,
            "落盘内容应与产出逐字一致（写入 {} 字符，期望 {}）",
            written.chars().count(),
            long.chars().count()
        );

        let second = serde_json::to_string(&ai.requests()[1].messages).unwrap();
        assert!(second.contains("已存为临时文件"), "应给出文件说明: {second}");
        // JSON 里反斜杠会被转义，所以先按 JSON 片段转一次再比。
        let path_in_json = serde_json::to_string(&path.display().to_string())
            .unwrap()
            .trim_matches('"')
            .to_string();
        assert!(
            second.contains(&path_in_json),
            "应给出落盘路径: {second}"
        );
        assert!(!second.contains(&long), "不该把全文塞进下游 prompt");

        let _ = std::fs::remove_dir_all(&cache_dir);
    }

    /// 落盘失败 → 该步 `Failed`（不静默降级）。
    ///
    /// 制造失败：`cache_dir` 被一个**同名文件**占住 → `create_dir_all` 必然失败。
    #[tokio::test]
    async fn spill_failure_marks_step_failed_and_skips_rest() {
        let long = "乙".repeat(9_000);
        let ai = FakeAiClient::new(vec![
            text_response(&long, 10, 1),
            text_response("第二步不该被调用", 20, 2),
        ]);
        let template = two_step_template();
        let params = PlanRunParams::from_template(&template);
        let sink = RecordingSink::default();

        let blocker = temp_cache_dir("blocked");
        std::fs::write(&blocker, b"x").expect("写占位文件");

        let report = executor_with(
            ai.clone(),
            ExecutorConfig {
                cache_dir: blocker.clone(),
                ..ExecutorConfig::default()
            },
        )
        .run(&template, &params, None, &sink, None)
        .await
        .expect("run 本身不失败（成败看 report）");

        assert!(!report.success);
        assert_eq!(report.steps[0].status, StepStatus::Failed);
        assert!(
            report.steps[0]
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("产出落盘失败"),
            "错误应说明落盘失败: {:?}",
            report.steps[0].error
        );
        assert_eq!(report.steps[1].status, StepStatus::Skipped);
        assert_eq!(ai.requests().len(), 1, "第一步失败后不该再调 LLM");

        let _ = std::fs::remove_file(&blocker);
    }

    /// 阈值边界是「含等号」的：等于阈值仍内联，多一个字符就落盘。
    #[tokio::test]
    async fn spill_threshold_boundary_is_inclusive() {
        let cache_dir = temp_cache_dir("boundary");
        let cfg = ExecutorConfig {
            cache_dir: cache_dir.clone(),
            spill_threshold_chars: 100,
            ..ExecutorConfig::default()
        };
        let template = two_step_template();
        let params = PlanRunParams::from_template(&template);
        let sink = RecordingSink::default();

        let exact = "丙".repeat(100);
        let ai = FakeAiClient::new(vec![text_response(&exact, 1, 1), text_response("完", 1, 1)]);
        let report = executor_with(ai, cfg.clone())
            .run(&template, &params, None, &sink, None)
            .await
            .expect("run 不应失败");
        assert!(report.steps[0].output_file.is_none(), "等于阈值应内联");

        let over = "丙".repeat(101);
        let ai = FakeAiClient::new(vec![text_response(&over, 1, 1), text_response("完", 1, 1)]);
        let report = executor_with(ai, cfg)
            .run(&template, &params, None, &sink, None)
            .await
            .expect("run 不应失败");
        assert!(report.steps[0].output_file.is_some(), "超过阈值应落盘");

        let _ = std::fs::remove_dir_all(&cache_dir);
    }

    /// 交付产出落盘后，**整理步**收到的同样只是「文件说明 + 读取方式」，不是全文。
    #[tokio::test]
    async fn resolve_step_prior_points_at_spilled_file() {
        let long = "丁".repeat(9_000);
        let ai = FakeAiClient::new(vec![
            text_response(&long, 1, 1),
            text_response("整理完成", 1, 1),
        ]);
        let template = FlexiblePlanTemplate {
            output_schema: Some(serde_json::json!({
                "kind": "bool",
                "goal": "整理",
                "success": "完成"
            })),
            task: "单步".to_string(),
            inputs: vec![],
            steps: vec![PlanStep {
                result_reference: "#E1".to_string(),
                intent: "产出大结果".to_string(),
                expected_output: "结果".to_string(),
                dependencies: vec![],
            }],
        };
        let params = PlanRunParams::from_template(&template);
        let sink = RecordingSink::default();
        let cache_dir = temp_cache_dir("resolve");

        let report = executor_with(
            ai.clone(),
            ExecutorConfig {
                cache_dir: cache_dir.clone(),
                ..ExecutorConfig::default()
            },
        )
        .run(&template, &params, None, &sink, None)
        .await
        .expect("run 不应失败");

        assert!(report.success);
        assert_eq!(ai.requests().len(), 2, "一次模板步 + 一次整理步");
        let resolve_req = serde_json::to_string(&ai.requests()[1].messages).unwrap();
        assert!(
            resolve_req.contains("已存为临时文件"),
            "整理步应收到文件说明: {resolve_req}"
        );
        assert!(
            resolve_req.contains("builtin_read_file"),
            "文件说明应给出读取方式: {resolve_req}"
        );
        assert!(!resolve_req.contains(&long), "整理步不该收到全文");

        let _ = std::fs::remove_dir_all(&cache_dir);
    }

    /// 日志用的产出渲染：多行压单行、超长封顶并标出原长。
    #[test]
    fn log_output_flattens_and_caps() {
        assert_eq!(log_output("  a\n\nb\t c  "), "a b c");
        assert_eq!(log_output(""), "");

        let long = "x".repeat(LOG_OUTPUT_MAX_CHARS + 5);
        let rendered = log_output(&long);
        assert!(rendered.starts_with(&"x".repeat(LOG_OUTPUT_MAX_CHARS)));
        assert!(
            rendered.contains(&format!("共 {} 字符", LOG_OUTPUT_MAX_CHARS + 5)),
            "截断时应标出原长: {rendered}"
        );

        let exact = "y".repeat(LOG_OUTPUT_MAX_CHARS);
        assert_eq!(log_output(&exact), exact, "恰好等于上限不截断");
    }

    #[tokio::test]
    async fn passes_prior_output_and_expands_params() {
        let ai = FakeAiClient::new(vec![
            text_response("第一步产出", 10, 1),
            text_response("第二步产出", 20, 2),
        ]);
        let template = two_step_template();
        let params = PlanRunParams::from_template(&template);
        let sink = RecordingSink::default();

        let report = executor(ai.clone())
            .run(&template, &params, None, &sink, None)
            .await
            .expect("run 不应失败");

        assert!(report.success);
        assert_eq!(report.steps_done(), 2);
        assert_eq!(report.prompt_tokens, 30);
        assert_eq!(report.completion_tokens, 3);
        assert_eq!(report.tool_calls, 0);

        // 两个请求：第一个含展开后的参数（${path}），第二个含 #E1 的实际产出
        let requests = ai.requests();
        assert_eq!(requests.len(), 2);
        let first = serde_json::to_string(&requests[0].messages).unwrap();
        assert!(
            first.contains("a.txt"),
            "第一步 intent 应展开 ${{path}}: {first}"
        );
        let second = serde_json::to_string(&requests[1].messages).unwrap();
        assert!(
            second.contains("第一步产出"),
            "第二步应收到 #E1 的产出: {second}"
        );

        let events = sink.events();
        assert!(events
            .iter()
            .any(|event| matches!(event, PlanRunEvent::RunStarted { total_steps: 2 })));
        assert!(events
            .iter()
            .any(|event| matches!(event, PlanRunEvent::RunFinished { .. })));
    }

    #[tokio::test]
    async fn failure_marks_downstream_steps_skipped() {
        // 只给一个响应：第二步需要的响应缺失 → 第一步成功、但整体因缺响应而失败
        let ai = FakeAiClient::new(vec![text_response("只有第一步", 10, 1)]);
        let template = two_step_template();
        let params = PlanRunParams::from_template(&template);
        let sink = RecordingSink::default();

        let report = executor(ai.clone())
            .run(&template, &params, None, &sink, None)
            .await
            .expect("run 不应失败");

        assert!(!report.success, "第二步 LLM 失败应使整体失败");
        assert_eq!(report.steps[0].status, StepStatus::Done);
        assert_eq!(report.steps[1].status, StepStatus::Failed);
        assert_eq!(ai.requests().len(), 2);
    }

    #[tokio::test]
    async fn missing_param_marks_step_failed_and_skips_rest() {
        let template = two_step_template();
        // 空参数表：${path} 无值 → 第一步展开失败
        let params = PlanRunParams::new();
        let sink = RecordingSink::default();
        let ai = FakeAiClient::new(vec![text_response("不该被调用", 1, 1)]);

        let report = executor(ai.clone())
            .run(&template, &params, None, &sink, None)
            .await
            .expect("run 不应失败");

        assert!(!report.success);
        assert_eq!(report.steps[0].status, StepStatus::Failed);
        assert_eq!(report.steps[1].status, StepStatus::Skipped);
        assert!(ai.requests().is_empty(), "展开失败时不应发起 LLM 请求");
    }

    #[tokio::test]
    async fn cancelled_before_start_skips_everything() {
        let template = two_step_template();
        let params = PlanRunParams::from_template(&template);
        let sink = RecordingSink::default();
        let ai = FakeAiClient::new(vec![text_response("不该被调用", 1, 1)]);

        let (tx, rx) = watch::channel(true);
        drop(tx);

        let report = executor(ai.clone())
            .run(&template, &params, None, &sink, Some(rx))
            .await
            .expect("run 不应失败");

        assert!(!report.success);
        assert!(report
            .steps
            .iter()
            .all(|step| step.status == StepStatus::Skipped));
        assert!(ai.requests().is_empty());
    }
}
