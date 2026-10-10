# 单步内工具输出「落盘 + 句柄」设计稿（flexible）

> **状态：已实施。** 基线实测：`cargo test -p planned-agent --lib` = **186 passed / 3 failed**
> （3 个失败是既有 prompt 目录漂移，与本次无关；改动前 182 / 3）；
> `cargo check --workspace --all-targets` 通过。§7 是已确认结论与遗留项。
>
> **决策（用户已拍板）**
> 1. **范围**：只做 `flexible` 单步内的工具输出回灌；不动 chat 路径、不动跨步 spill 的既有行为。
> 2. **机制**：只做「**不内联**」（超阈值→根本不进上下文），**不做「淘汰」**（先内联、后续再抽走）。两者区别见 §2.2。
> 3. **清理**：跑出来的临时文件**暂不清理**（已知会增长，见 §5.4，留作后续）。
> 4. **阈值/预览**：复用 `spill_threshold_chars = 8000` / `spill_preview_chars = 800`，不新增配置项。
> 5. **落盘失败**：`warn` + **回退内联全文**，**不**判该步 `Failed`（与跨步的严格语义刻意不同，理由见 §2.7）。
>
> 相关既有文档：`docs/planned-agent/flexible-step-output-spill.md`（跨步产出落盘，本稿是它在「步内」的延伸）、
> `crates/planned-agent/src/flexible/AGENTS.md`（模块导航）。

---

## 0. 一句话

把「大输出不占上下文」这条规则，从「步骤**之间**的产出传递」下沉到「单步**内部**的工具结果回灌」：
**工具返回的文本超过阈值就落盘，messages 里只放「文件路径 + 预览 + 读回指引」。**

---

## 1. 现状（事实）

### 1.1 单步内的工具结果是「原文进、每轮全量重发、无任何裁剪」

| 环节 | 位置 | 事实 |
|---|---|---|
| 取工具输出 | `exec/step/mod.rs:298` `tool_content(&outcome.result.content)` | 原样取字符串 |
| `tool_content` 本体 | `exec/step/render.rs:107-112` | `Value::String` 直接 `clone()`，**无截断** |
| 入 messages | `exec/step/mod.rs:349` `messages.push(tool_message(&call.id, &tool_output))` | 全文进该步的 messages，**从不移除** |
| assistant 也入 | `exec/step/mod.rs:274` `messages.push(message)` | 同上 |
| 每轮请求 | `exec/step/mod.rs:137-139` `messages: messages.clone()` | 每轮**全量**发给模型 |
| 轮数上限 | `exec/executor/config.rs:77` `max_rounds_per_step: 50` | 一步最多 50 轮 |

**结论**：一步内连读 10 个网页，第 10 轮请求要带前 9 页全文。

### 1.2 相关的截断函数**都不在这条路上**

`render.rs` 里有 `truncate_chars`(:116) / `summarize`(:123)，但调用点只有：
- `summarize` → `exec/step/mod.rs:363` 的 `output_summary`（记录/展示侧）
- `describe_arguments`(:57, 800) / `describe_tool_args`(:73, 120) → 仅 `tracing` 日志与 `StepToolCall` 事件（`mod.rs:286-294, :346`）

回灌给模型的那一份（`tool_output` → `messages`）**不经过它们中的任何一个**。

### 1.3 唯一的兜底在工具自己身上

| 工具 | 上限 |
|---|---|
| `builtin_clean_html` | 默认 20,000 字符，调用方可指定，`clamp(1, 100_000)`（`tool-manager/src/builtin/web_tools.rs:8-10,94-98`） |
| exec / shell | stdout、stderr 各 256 KiB（`system_tools.rs:30-34`） |
| 文件读 | `MAX_LINES = 10_000` / `MAX_FILE_BYTES`（`filesystem/io/read.rs:205,134`） |

即最坏一次就能塞进十万字符。

### 1.4 跨步 spill 已在做同一件事（本稿的复用对象）

`exec/executor/spill.rs`：
- `spill_output`(:53-76)：`chars().count() <= threshold` 内联，多 1 字符落盘到 `<cache_dir>/<run_dir>/step-<index>.txt`；IO 失败**向上抛**，调用方把该步记 `Failed`（`executor/mod.rs:188-226`）。
- `render_prior_output`（`spill.rs`）：落盘时渲染为「行数/字节数 + 路径 + 读法（先 `builtin_grep_file` 定位、再 `builtin_read_file_lines` 精读）+ 前 N 字符预览」。
- `StoredOutput`(:26-29) / `SpilledOutput`(:33-43)。

### 1.5 沙箱耦合（隐性前提，必须写进契约）

文件工具沙箱根 = `cwd` + 用户主目录 + `temp_dir()`（`crates/agent-gui/src/context/tools/mod.rs:35-49`），
而 `output_cache_dir` 默认 `./data/cache`（相对**进程 cwd**，`exec/executor/config.rs:6-7`）→ 落盘文件**在沙箱内**，
模型能读回。**跨步 spill 能跑通就靠这个**，本方案继承同一前提，但它是隐性依赖（见 §5.2）。

---

## 2. 设计

### 2.1 骨架

```
工具执行 → tool_output = tool_content(result)
            │
            ├─ chars() <= spill_threshold_chars ──→ messages.push(tool 消息, 全文)   ← 现状，不改
            │
            └─ 超过阈值
                 ↓
               写 <cache_dir>/<run_dir>/tool-s<step>-r<round>-<n>.txt
                 ↓
               messages.push(tool 消息, 落盘引用文案)                              ← 唯一行为变化
```

**落点**：`exec/step/mod.rs:349` 那一行的 `tool_output` 改为「可能是引用文案」的版本。

**不需要改的东西**：
- **消息结构不变**：仍是 `role: Tool` + `tool_call_id`，只是 `content` 从全文换成引用（`render.rs:23-33` 的 `tool_message` 签名不变）。
- **ai-openai 不改**：OpenAI 不校验 tool content 长度。
- **日志 / 事件 / 记录不变**：`tracing` 的 `output = %content`（`mod.rs:301-308`）、`StepToolCall` 事件（`:343-348`）、`ToolCallRecord`（`:338-342`）都不含进 messages 的那一份，保持不变。**只替换送给 LLM 的版本**。

### 2.2 为什么是「不内联」而不是「淘汰」

| | 不内联（本稿） | 淘汰（**不做**） |
|---|---|---|
| 决策点 | 工具返回的那一刻 | 后续某一轮 |
| 判据 | 单条字符数 | 累计字符数 / 消息年龄 |
| 需要状态 | 无 | 每条消息「多大 / 是否已落盘 / 文件在哪」书签 |
| 落盘时机 | 当场 | 替换时**补写**（老的当年没落盘） |
| 风险 | 单条不超但累计超时无解 | 模型可能已依赖那条信息，抽走会「失忆」 |

不内联的盲区（**留着，实测触发再做**）：每条都不超阈值但累计爆；assistant 自己产出的长文。

### 2.3 复用与提升：`spill` 模块从 executor 层提到 exec 层

现状 `spill.rs` 的项都是 `pub(super)`（= `exec::executor` 可见），而 step 在兄弟模块 `exec::step`，**访问不到**。改动：

```
crates/planned-agent/src/flexible/exec/
├── spill.rs          ← 从 executor/spill.rs 提升上来（新）
├── executor/
│   ├── mod.rs        （`mod spill;` 改为 `use super::spill;`）
│   └── ...（spill.rs 删除）
└── step/
    └── mod.rs        （新增对 super::spill 的使用）
```

对外 API 拆成两个可复用函数（把「落盘」与「渲染」解耦，`kind` 决定措辞）：

```rust
// exec/spill.rs
pub(in crate::flexible::exec) enum SpillKind { StepOutput, ToolOutput }

/// 通用落盘：超阈值写 `file_name`，返回落盘信息；未超阈值返回 `Ok(None)`。
pub(in crate::flexible::exec) async fn spill_text(
    cache_dir: &Path,
    run_dir: &str,
    file_name: &str,
    threshold_chars: usize,
    content: &str,
) -> Result<Option<SpilledOutput>>;

/// 通用渲染：未落盘给全文；落盘给「说明 + 路径 + 读法 + 预览」。
pub(in crate::flexible::exec) fn render_spill_reference(
    spilled_or_content: ...,       // 由调用方给 SpilledOutput 或全文
    preview_chars: usize,
    kind: SpillKind,
) -> String;
```

`spill_output`(step 专用) 与 `render_prior_output` 退化成薄封装，**行为一字不变**（step 产出仍写 `step-<index>.txt`、文案一致），
这样跨步语义零风险。

### 2.4 文件命名与粒度

- **一条工具输出一个文件**：`tool-s<step>-r<round>-<n>.txt`
  - `step` = 步骤序号（`StepInput::index`，从 1 开始）
  - `round` = 该步内的轮次（`mod.rs:136` 的 `rounds`，从 1 开始）
  - `n` = 同轮内第几个工具调用（`for call in tool_calls` 从 0 开始，`mod.rs:275`）
  - 例：第 3 步的第 2 轮第 1 个工具 → `tool-s3-r2-0.txt`
  - ⚠️ **`step` 这一段是必须的**：`run_dir` 是 **run 级**目录，而 `round`/`nth` 在每一步内都从 0 重新计数 —— 只用后两者，不同步骤的同轮次同序号会**互相覆盖**。（review 时发现的真实缺陷，已修）
- **为什么一工具一份**：回读时定位精确、可单独引用；与跨步的 `step-<i>.txt` 不同前缀，同目录互不冲突。
- **不做内容 hash 去重**：`(round, n)` 天然唯一，同一轮内重复执行同一工具的情况极少。
  何时该加：若将来发现「同一份大内容反复落盘」把磁盘撑大 —— 那时连清理策略一起做（§5.4）。

### 2.5 回灌文案

`SpillKind::ToolOutput` 的文案（与跨步那份刻意区分，避免模型把工具输出当成步骤产出）：

```
⚠️ {产出较大|工具输出较大}（{lines} 行 / {bytes} 字节），已存为临时文件，未全文注入。
content@{path}
读取方式：先用 `builtin_grep_file` 按关键词/正则定位（返回 1-based 行号；命中多时按 `match_offset` 续读），
再用 `builtin_read_file_lines` 读那几行（`offset` = 行号 - 1；**不传 `limit` 会一次读到文件末尾**，
大文件请显式传 `limit`（如 2000）；本文件共 {lines} 行，续读时把 `offset` 加上已读到的行数）。
———— 开头预览（前 {preview_chars} 字符）————
{preview}
```

两个细节：
- **路径给 `path.display()` 的原样，但建议统一正斜杠**：Windows 反斜杠要进 JSON 参数得转义，是真实的摩擦点（跨步那份的 `render_prior_output` 已经遇到同样问题）。
- **已顺手修的现存 bug**：原文案写「`limit` 默认 2000」，但实现是**不传 `limit` 就读到文件末尾**
  （`filesystem/io/read.rs:151-154`）。`exec/spill.rs` 的公共渲染与 `exec/prompt.rs:19-20` 的
  step system prompt **两处都已改成准确表述 + 「显式传 `limit`」的策略引导**（只说事实会让模型
  一次拉回整个大文件，又把自己的工具输出撑到超阈值 —— 反倒触发二次落盘）。

- **2026-10 补上「搜索定位」这一半**：落盘给出的是文件句柄，但取回手段原本只有顺序读，
  「按需回读」实际退化成「按行扫」；工具箱补了 `builtin_grep_file`（单文件内容搜索，
  见 `docs/planned-agent/builtin-grep-file.md`）后，上述文案与 `exec/prompt.rs` 第 3 条
  **同步改为「先 grep 定位、再按行精读」两步读法**（两处由 `executor/tests.rs` 的断言锁住，改一处必须同步另一处）。

### 2.6 接口改动

`StepInput`（`exec/step/mod.rs:33-50`）只需要多拿一个 `run_dir` —— **`cache_dir` 不必传**：
step 的签名里已经有 `cfg: &ExecutorConfig`，而 `ExecutorConfig` 本就带 `cache_dir`。

```rust
pub(crate) struct StepInput<'a> {
    ...
    /// 本次执行的产出目录名（工具大输出落盘用；由执行器生成）
    pub run_dir: &'a str,
}
```

调用点两处：`run_step` 与 `run_output_resolve` 的 `StepInput` 构造（都在 `executor/mod.rs`）。
后者还额外给 `resolve_result` 加了 `run_dir` 参数（它原本拿不到 `run_dir`）。阈值/预览长度从 `cfg` 取。

> **实施偏差（已记录）**：设计稿原写「加 `cache_dir` + `run_dir` **两个**字段」，实施时发现 `cache_dir`
> 可从已有的 `cfg` 参数取 —— 少一个字段，`StepInput` 的 11 处构造点（2 处生产 + 9 处测试）各只补 1 行。语义不变。

### 2.7 失败语义（已确认：`warn` + 回退内联）

跨步 spill 的规矩是「IO 失败 → 该步 `Failed`」（`executor/mod.rs:220-225`，不静默降级）。
**步内反过来（已拍板）**：落盘失败 → `tracing::warn!` + **回退为内联全文**。

理由：两次失败的后果不对称。
- 跨步落盘失败 → 下游步骤拿不到数据 → **必须失败**。
- 步内落盘失败 → 只是这一条工具输出多占上下文（回到现状行为），**信息一点没丢**（全文照样进 messages），把整步判失败是过重的惩罚。

这不是「静默降级」（有 warn 日志、且降级目标是现状行为），但仍与跨步的严格语义不一致 —— 这是刻意选择。

---

## 3. 改动点清单

| # | 文件 | 改动 |
|---|---|---|
| 1 | `exec/spill.rs` | 新建：从 `exec/executor/spill.rs` 提升；新增 `SpillKind` / `spill_text` / `render_spill_reference`；`spill_output` / `render_prior_output` 退化为薄封装；修正 `limit` 文案 |
| 2 | `exec/executor/spill.rs` | 删除（内容提升到 `exec/spill.rs`） |
| 3 | `exec/executor/mod.rs` | `mod spill;` → `use super::spill;`；`StepInput` 构造处（`:172-179`）补 `cache_dir` / `run_dir` |
| 4 | `exec/step/mod.rs` | `StepInput` 加 `cache_dir` / `run_dir`；`:349` 回灌前插入「超阈值→落盘→渲染引用」；新增单测 |
| 5 | `exec/step/render.rs` | 不动（`tool_message` / `tool_content` 签名不变） |

**不改**：`ai-openai`、`chat` 路径、`core`、`tool-manager`、GUI、跨步 spill 的行为。

---

## 4. 不做的事（明确边界）

1. **不做「淘汰」**（§2.2）—— 需要书签 + 补落盘 + 与重试/重发的交互，复杂度高一档；等实测出现「单条都小但累计爆」再开。
2. **不做清理**（用户决定）—— 临时文件会持续增长，写进 §5.4 作为已知限制。
3. **不动 chat 路径** —— 它的问题更严重（整会话累积、无步边界），但落点在 `chat/driver/round/handlers.rs` 另一条路，且工具可能是并发执行、命名要另设计。本稿把公共函数做出来，就是为它下一轮复用。
4. **不动 assistant 消息**（模型自己产出的长文）—— 属于「淘汰」范畴。
5. **不动 system prompt / prior 注入**（`prompt::build_step_task`）—— prior 那条路已经有跨步 spill 管着。
6. **不做内容 hash 去重**（§2.4）。
7. **不给工具加「输出上限」参数**（如统一 `max_chars`）—— 那是工具契约层的事，与「事后兜底」正交；`builtin_clean_html` 已有的 `max_chars` 保留不动。

---

## 5. 已知限制与风险

### 5.1 读回可能触发「再落盘」
模型用 `builtin_read_file_lines` 读回 → 这也是一次工具输出 → 又超阈值 → 又落盘 → 又引用。
**不会 token 爆炸**（messages 里始终是引用），但会多烧轮数。预期靠「模型自己把 `limit` 调小」自然收敛（这正是文案里写清读法的原因）。

### 5.2 沙箱耦合是隐性的
落盘文件能被读回，**只因为 cwd 恰好是文件工具的沙箱根之一**（§1.5）。
若宿主把 `output_cache_dir` 配成沙箱外的绝对路径（如 `D:/tmp/cache`），或启动 cwd 变化，模型就**读不回自己刚落的盘**——失败形式很隐蔽（工具报「路径不在沙箱内」）。
本稿**不引入校验**（跨步 spill 也没有），但要写进契约：**`cache_dir` 必须在沙箱根内**。

### 5.3 预览质量决定收益
预览太短 → 模型盲目回读（多一轮）；太长 → 失去省 token 的意义。800 字符是沿用跨步的既有取值，**先按已有经验走，实测调**。

### 5.4 磁盘只增不减
每步每轮的每个大工具输出都会留一个文件（`tool-s<step>-r<round>-<n>.txt`），且 `run_dir` 级联在 `data/cache/<session_id>/` 下**没有清理**（跨步 spill 现在也一样）。长跑会累积。用户已决定本次不处理，留作后续（与 hash 去重一起做更划算）。

### 5.5 token 不会归零
省掉的是「大工具输出」，但 assistant 消息、system prompt、prior 预览、每轮的 tool_call 参数仍然累积；50 轮上限也没变。本方案是**削峰**，不是封顶。

### 5.6 与 `OUTPUT_MAX_CHARS` 的区别（别混淆）
`exec/step/mod.rs:31` 的 `OUTPUT_MAX_CHARS = 8000` 管的是「步骤产出进**记录**的封顶」；`config.rs:9` 的 `DEFAULT_SPILL_THRESHOLD_CHARS = 8000` 管的是「落盘阈值」。**两个 8000 数值相同是刻意的（注释写明"与记录侧对齐"）**，但语义不同、作用对象不同。

---

## 6. 测试（已落地）

4 个用例都在 `exec/step/tests.rs`：

| 用例 | 断言 |
|---|---|
| `oversized_tool_output_spills_and_request_carries_reference` | 12k 字符输出 → 落盘文件逐字等于原文；**第 2 条请求**里那条 tool 消息是引用（含 `已存为临时文件` 与 `tool-s1-r1-0.txt`，且 < 2000 字符、不含全文） |
| `small_tool_output_stays_inline` | 小输出仍原样内联，且**不建目录、不写文件** |
| `spill_failure_falls_back_to_inline_without_failing_step` | 把**文件**当 `cache_dir` 触发 IO 失败 → 该步仍 `Done`，回灌是全文而非引用 |
| `tool_output_for_llm_switches_at_threshold` | 阈值两侧行为；文件名含步骤/轮次/序号；同轮两个工具各写一份互不覆盖；**跨步骤同轮次同序号也不覆盖**（`tool-s1-r3-0.txt` vs `tool-s2-r3-0.txt`）；**文案回归锁**（`不传 \`limit\` 会一次读到文件末尾`） |

跨步不回归：`exec/executor/tests.rs` 既有的两处「已存为临时文件」断言照旧通过（新文案保留了该片段）。

实测：`cargo test -p planned-agent --lib` = **186 passed / 3 failed**（改动前 182 / 3；3 个失败是既有
`planner::coarse::llm_planner` prompt 目录漂移）；`cargo check --workspace --all-targets` 通过。

---

## 7. 已确认（开发前）

| 点 | 结论 |
|---|---|
| 落盘失败语义 | `warn` + 回退内联全文，不判该步 `Failed`（与跨步的严格语义刻意不同，理由见 §2.7） |
| 预览长度 | 沿用 800 字符，不新增配置项 |

**遗留（本期不做）**：「淘汰」机制（§2.2）、临时文件清理（§5.4）、内容 hash 去重（§2.4）、
chat 路径接入同一套公共函数（§4.3）、`cache_dir` 沙箱校验（§5.2）。
