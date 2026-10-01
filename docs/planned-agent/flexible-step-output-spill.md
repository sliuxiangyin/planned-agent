# Flexible 步骤产出「落文件 + 句柄传递」设计稿

> **本稿性质**：设计稿，**待确认后才动代码**。
> **取代关系**：本稿**取代** `flexible-execution-hardening.md` 的 **A4（prior 无上限）** 与 **A5（整理步只看 200 字符摘要）**
> —— 不再用「硬截断 + 预算」，改为「超阈值落文件 + 下游按需分页读取」。
> 检查日期：2026-09-28。
>
> **实施状态（2026-09-28）：已按本稿实施。**
> 落点：`flexible/executor.rs`（`ExecutorConfig` 三字段 + 落盘 + `prior` 渲染 + 整理步给 `builtin_read_file`）、
> `flexible/prompt.rs`（`STEP_SYSTEM_PROMPT` 加读文件规则）、`flexible/report.rs` 与 `run_service/types.rs`（`output_file`）、
> GUI `config.rs`（`[flexible]` 段）+ `services/run_service.rs`（拼会话段）。
> 验收：`cargo test -p planned-agent --lib flexible::` = 88 passed；`cargo test -p planned-agent-gui --bins` = 63 passed。
>
> ⚠️ **勘误（2026-09-30）**：本文写于 `file_tools` 时代，文件工具此后已被 `filesystem` 工具族取代
> （见 `filesystem-tools-rewrite.md`）。以下内容**已失效**：
> - 文中的 `builtin_read_file`（`offset` 从 **1** 开始、返回 JSON + `next_offset`/`has_more`）
>   → 现为 `builtin_read_file_lines`（`offset` 从 **0** 开始、返回**裸文本**）；
> - 「`builtin_read_file` / `builtin_write_file` **不改**」的结论**作废** —— `builtin_write_file`
>   名字保留但语义按上游重写（返回成功消息、写入走 cap-std 原子替换）；
> - `file_tools.rs:55` / `:181-218` 等**行号引用**已失效（该文件已删除，共享件搬入 `filesystem/support.rs`）。
>
> **落盘机制本身未变**：`ExecutorConfig` 三字段、`prior` 渲染、整理步（`#RESULT`）都还在，
> 变的只是它引用的工具名与参数基。下游读回契约同步见 `filesystem-tools-rewrite.md` §3。

---

## 1. 动机：为什么不用硬截断

| | 硬截断（原 A4/A5） | 本方案 |
|---|---|---|
| 下游看到太多 | 截掉尾巴 → **信息静默丢失**，模型可能基于残缺内容下结论 | 落文件 + 按需读取，**一个字不丢** |
| 整理步看得太少 | 单给它加额度（治标） | 同样读文件，一次解决 |
| 截断点 | 任意（很可能正好切在关键数据中间） | 不截断 |

一句话：**截断会「安静地把上下文弄坏」**，而本方案让模型自己决定读哪一段。

---

## 2. 已拍板的决策（2026-09-28）

| # | 决策 | 说明 |
|---|---|---|
| 1 | **executor 直接 fs 写** | 不走 `builtin_write_file` 工具 —— 这是执行器内部机制，走工具会污染 `tool_sequence` / `tool_audit`（C1 要观测的数据） |
| 2 | **强制使用默认 `./data/cache`** | 宿主不配置也生效，不退回硬截断 |
| 3 | **加 `output_file` 字段** | 记录与 UI 都要能显示「本步产出已落文件」 |
| 4 | **先不做清理** | 文件先留着（已知会累积，见 §9） |
| 5 | **不做降级兜底** | 不设 prior 总量预算、不「写失败退回截断」（理由见 §7） |

---

## 3. 数据流

```
step N 产出 output（StepRunResult.output，当前不截断）
  │
  ├─ len ≤ threshold_chars ──→ prior 里 inline（与今天逐字一致）
  │
  └─ len >  threshold_chars ──→ 写  <cache_dir>/step-<index>.txt
                                  prior 改放「文件说明 + 读取指引 + 预览」
                                        ↓
                                  step N+1 的 LLM 用
                                  builtin_read_file(offset/limit) 分批读
                                  （返回带 next_offset / has_more，天然可续读）
```

**术语转译**：LLM **拿不到真正的「文件句柄」** —— 传递的是**文件路径 + 元信息**，
由 LLM 主动用工具读取。这是「句柄传递」在本架构下的等价物。

---

## 4. prior 的形态

**`prior` 保持纯文本**，因此 `StepInput.prior: &[(String, String)]` 与
`prompt::build_step_task` 的**形状不变** —— executor 在 `collect_prior` 里拼好即可。

超阈值时的渲染形态：

```
### #E1
⚠️ 产出较大（1234 行 / 89210 字节），已存为临时文件，未全文注入。
文件：D:\code\planned-agent\data\cache\<session-id>\<run-id>\step-1.txt
读取方式：builtin_read_file（offset 从 1 开始，limit 默认 2000；
返回含 next_offset / has_more，可续读）。
———— 开头预览（前 800 字符）————
<preview…>
```

### 4.1 必须同时改 system prompt

`prompt::STEP_SYSTEM_PROMPT` 增一条规则，否则模型可能只看预览就下结论：

> - 若「前序步骤结果」给出了文件路径，说明该产出**未全文注入**；
>   请用 `builtin_read_file` 分批读取所需部分，**不要仅凭预览臆断**。

### 4.2 预览不是「兜底」

预览（`preview_chars`）的作用是**给模型判断相关性的线索**（这个文件是不是我要的），
不是截断兜底。它与「按需读取」是互补关系：有预览才知道要不要读。

### 4.3 产出进日志（debug）

「步骤结束 / 步骤失败」与「输出整理步结束」都带上产出（字段 `output` / `result`），即**日志里直接看得到每步产出了什么**：

- 先**压成单行** —— 多行会糊掉日志行（与 `system_prompt` 同一问题）；
- 再封顶 `LOG_OUTPUT_MAX_CHARS = 2000`，截断时**标出原始长度**（便于判断要不要去读文件）；
- 落盘时同一条日志再带 `output_file`，看全文就去那个文件。

因为完整内容总能从落盘文件拿到，所以日志截断**不会丢信息**。

---

## 5. 落点：谁写 / 目录 / 文件名

### 5.1 谁写

`FlexibleExecutor`（`flexible/executor.rs`）在 `collect_prior` 阶段判断并落盘。

### 5.2 目录（executor 不知道「会话」概念）

| 角色 | 提供什么 |
|---|---|
| 宿主（GUI） | `ExecutorConfig.cache_dir` = `./data/cache/<session_id>`（**会话段由宿主给**） |
| executor | 在其下自建 run 级子目录 `run-<时间戳毫秒>`，本次执行所有 step 文件都放这里 |

最终路径：`./data/cache/<session_id>/run-<ts>/step-<index>.txt`

**为什么这么分工**：`FlexibleExecutor::run` 的签名（`template, params, environment, sink, cancel`）
**没有 session_id**，executor 也不该知道「会话」这一宿主概念（守住 `run_service` 的零端口契约）
—— 所以**会话段由宿主拼，run 段由 executor 管**。

### 5.3 文件名

**`step-<index>.txt`**（1-based，与 UI 的 `N/M` 一致）。

⚠️ **不要用 `result_reference`（`#E1`）当文件名** —— 它来自模板 / LLM 产出，
直接拼进路径有**目录穿越**风险。

### 5.4 为什么直接 fs 写而不走工具

| 走 `builtin_write_file` | 直接 fs 写 |
|---|---|
| 复用边界处理（create_parents / 换行 / 大小校验） | 需要自己 `create_dir_all` |
| **副作用混进 `tool_sequence` 与 `tool_audit` 日志** —— 会污染 C1 的观测数据 | 与 LLM 的工具调用完全隔离 |

拍板结论：**直接 fs 写**（用 `tokio::fs` 或同步 `std::fs`；单次 ≤10 MiB，开销可忽略）。

---

## 6. 参数

| 参数 | 默认 | 说明 |
|---|---|---|
| `cache_dir` | `./data/cache` | 宿主可覆盖为 `./data/cache/<session_id>`；**必有值**（拍板 2） |
| `threshold_chars` | 8 000 | 超过则落文件（与记录侧 `OUTPUT_MAX_CHARS` 对齐） |
| `preview_chars` | 800 | prior 里保留的预览长度 |

> 配置落在 GUI `config.rs` 的 `[flexible]` 段，沿用既有模式
> （`Option<T>` + `#[serde(default = "default_xxx")]`，参考 `kv_cache.path` 的 `default_cache_path()`）。

---

## 7. 失败语义（不降级）

拍板 5：**不做「写失败退回截断」**。理由：写失败（磁盘满 / 权限）是**不该发生**的情况，
为它加一条降级路径会同时引入「两种 prior 形态」，复杂度与收益不成正比。

因此：

| 情况 | 行为 |
|---|---|
| 落盘失败（权限 / 磁盘满 / 路径非法） | **该步记 `Failed`**，错误说明「产出落盘失败」—— 不静默、不截断 |
| 输出 > `MAX_WRITE_BYTES`（10 MiB，`file_tools.rs:55`） | 同上（这是同一类失败） |
| 下游读不到文件 | 由 `builtin_read_file` 自己报错（它已有完整错误码：`file_not_found` / `content_too_large` 等） |

**为什么不「悄悄截断」**：与 A1/A2/A3 一致 —— 能明确失败就明确失败，不静默降级。

> 注：单步输出上限受 `max_tokens` 约束，实际远小于 10 MiB，所以这条路径基本不会触发。

---

## 8. 记录与 UI 可见性（拍板 3）

| 位置 | 改动 |
|---|---|
| `flexible/report.rs` | `StepRunRecord` 加 `output_file: Option<String>`（`#[serde(default)]`） |
| `flexible/run_service/types.rs` | `StepSnapshot` 加同名字段（UI 消费） |
| GUI | 步骤详情里显示「产出较大，已落文件：<path>」（可点开/复制路径） |

**`StepRunRecord.output` 保持不变**（仍是 `truncate_chars(text, 8000)`）—— UI 的 inline 展示语义不动，
`output_file` 是**额外**的指针。

---

## 9. 清理（本稿不做，拍板 4）

**不做任何自动删除**：文件留在 `./data/cache/<session_id>/` 下。

已知代价（记录在案，后续可另开一条处理）：

| 风险 | 说明 |
|---|---|
| 磁盘累积 | 长跑 / 多会话会持续堆积，无上限 |
| 敏感数据残留 | 步骤产出可能含文件内容 / 接口返回，会落盘 |

后续可选的清理方式（**本稿不实现**）：会话删除时删整个会话目录（这是选
`<session_id>` 目录结构的主要好处）、启动时按 mtime TTL 清理。

---

## 10. 与其它条目的关系

| 条目 | 处置 |
|---|---|
| `hardening` A4（prior 无长度预算） | **被本稿取代** —— 不再设预算，改为落文件 |
| `hardening` A5（整理步只看 200 字符摘要） | **被本稿取代** —— 整理步的 prior 同样用「文件说明」，可自行读取 |
| `hardening` C1（工具序列进报告） | **互补** —— 本稿是「产出」的观测，C1 是「调用」的观测；两者的数据都不受对方污染（§5.4） |
| `run_service` 零端口契约 | **不破坏** —— executor 只知道一个目录，不知道 session/run 概念（§5.2） |

---

## 11. 改动面

| 位置 | 改动 |
|---|---|
| `flexible/executor.rs` | `ExecutorConfig` 加 3 个字段；`run` 开始时建 run 子目录；`collect_prior` 落盘 + 拼「文件说明」 |
| `flexible/prompt.rs` | `STEP_SYSTEM_PROMPT` 加「按需读文件」规则（§4.1） |
| `flexible/report.rs` | `StepRunRecord.output_file` |
| `flexible/run_service/types.rs` | `StepSnapshot.output_file` |
| `crates/agent-gui/src/config.rs` | `[flexible]` 段 + 默认值 |
| `crates/agent-gui/src/...` 装配处 | 把 `cache_dir`（含 session_id）塞进 `ExecutorConfig` |
| `builtin_read_file` / `builtin_write_file` | **不改**（能力已够：`file_tools.rs:181-218` / `:220-252`） |
| 日志 | `executor.rs` 新增 `log_output()`（单行 + 封顶）与 `LOG_OUTPUT_MAX_CHARS`；三条日志（步骤结束 / 步骤失败 / 整理步结束）带产出 |

---

## 12. 验收

| 项 | 方式 |
|---|---|
| 小产出仍 inline | 断言 user message 含全文、`output_file == None` |
| 大产出落文件 | 造 > 8000 字符输出，断言 ① 文件存在且内容 == 原文；② user message 含路径与预览、**不含**全文；③ `output_file` 有值 |
| 下游能读到 | 断言 user message 里的路径**真的能被 `builtin_read_file` 打开**（round-trip），且 `total_lines` 与文件一致 |
| 落盘失败 → 该步 Failed | 用一个不可写路径（如 `<cache_dir>/step-1.txt` 是目录）断言 `Failed` |
| 阈值边界 | 恰好 8000 字符 → inline；8001 → 落文件 |
| 不破坏既有 | `cargo test -p planned-agent --lib flexible::` 全绿 |

---

## 13. 未决点 / 风险

| # | 项 | 说明 |
|---|---|---|
| 1 | `cache_dir` 是否**绝对化** | 相对路径基于**进程 cwd**；建议启动时把配置路径规范化成绝对路径，避免 cwd 变化后找不到文件 |
| 2 | 下游轮数/token 上升 | 模型要多几轮读文件；`max_rounds_per_step` 现为 50，暂够 |
| 3 | 模型不读 | 靠 §4.1 的 prompt 规则 + 预览诱导 |
| 4 | 与「无持久化」的边界 | 本稿**不落库、不加表、不改 schema**；新增的是**执行期磁盘临时产物**，生命周期 = 会话 |
| 5 | 空产出 | A2 已保证 store 里不会有空串，故本稿不必处理「空文件」 |
