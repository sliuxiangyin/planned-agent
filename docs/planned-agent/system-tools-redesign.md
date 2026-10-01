# `system_tools.rs` 重新设计（评审稿）

> **状态：评审稿，未动任何代码。**
>
> **输入**：用户提供的《`builtin_execute_command` 工具设计文档 v1.0》（Windows / Linux 双平台，369 行）。
>
> **上游文档**：`command-execution.md`（执行契约的历史结论，含一条**硬约定**）、`gui-host-environment.md`（环境事实怎么送到 UI 与执行链）。
>
> **本文的定位**：把那份外来的设计文档**翻译进本仓库的既有约束**，逐条说明「采纳 / 不采纳 / 需拍板」，
> 并给出可实施的契约、实现要点与测试计划。**不拍板不动代码。**
>
> ⚠️ **勘误（2026-09-30）**：文中 `builtin_read_file` / `file_tools.rs` 的引用已随文件工具改造失效。
> 特别是 §1.5「一致性上形同虚设」（`:195-197`）称「file 工具没有沙箱」—— 现在 `filesystem` 工具族
> **有** cap-std 沙箱（允许目录由宿主在启动时注入，见 `filesystem-tools-rewrite.md` §6）。
> 本文关于 `builtin_execute_command` 的 `working_dir` 结论不受影响。

---

## 0. 一页结论

### 0.1 直接采纳（无争议，多为修 bug 或纯增强）

| # | 改动 | 性质 |
|---|---|---|
| A1 | 执行改为**异步** `tokio::process::Command` | 修 bug —— 现状在 async executor 里同步阻塞 |
| A2 | 新增 `timeout_ms`（默认 30s / 上限 300s）+ 超时强制回收子进程 | 修 bug —— 现状**没有超时**，一条挂死的命令会永久阻塞 |
| A3 | `kill_on_drop(true)` + 落空回滚，杜绝孤儿进程 | 健壮性 |
| A4 | 输出**按字节截断**（`max_output_bytes`）+ `*_truncated` 标记 | 现状把整份 stdout 全读进内存 |
| A5 | `\r\n` → `\n` 归一化 | 减负（**改写了原始输出，须写进契约**，见 §4.2） |
| A6 | 返回结构补 `exit_code` / `duration_ms` / `timed_out` / `command_line` | 现状只有 `{stdout,stderr,status}` |
| A7 | 新增 `env` / `stdin` 参数 | 现状给不了环境变量与标准输入 |
| A8 | Windows 进程创建加 `CREATE_NO_WINDOW` | 修 bug（GUI 下每次执行闪黑框）—— `command-execution.md` §5 已列为「未做」 |
| A9 | 审计日志走 `tracing` 结构化字段 | 现状只有 registry 的 `info!("Routing to builtin executor")`，无调用级审计 |
| A10 | `command` 含空白且非真实路径 → 返回**指导性错误** | 修 `command-execution.md` §2.5 记录过的坑（**代码里其实没有**，见 §1.2） |
| A11 | `list_processes` 加 `limit`；`kill_process` 拒绝自杀 | 现状可返回上千进程；可杀死宿主自身 |
| A12 | 环境变量工具**默认屏蔽敏感键**（见 §5） | 现状 `list_env` 把 `*_API_KEY` / `*_TOKEN` 全量回灌给 LLM 并落库 |
| A13 | 删除 `builtin_set_env` | 它改的是**宿主进程**全局状态，对「每次调用都是新进程」无意义，却留下跨调用污染通道 |
| A14 | 补 description 定稿文案（不套 shell 的语义） | 补 `command-execution.md` §2.4 的**文档漂移** |

### 0.2 不采纳（与仓库硬约定冲突，理由见 §2）

| # | 不采纳 | 一句话理由 |
|---|---|---|
| N1 | description **动态注入**「当前平台 + 推荐命令集」 | 直接违反 §3.1 ③「每台机器不一样的值不进静态契约」，且历史上正是这条把模型带进坑（`command-execution.md` §1.5） |
| N2 | 把 `shell: true` 开关**暴露给 LLM** | 粘贴文档自己 §5.3 就写了「建议只对内部受信调用开放，不暴露给 LLM」——自相矛盾（见 §2.2） |
| N3 | description 里的平台专属举例（`dir` / `tasklist` / `powershell`） | 同上，且与 `command-execution.md` §2.5 定稿文案冲突 |
| N4 | 「工具声明 `additionalProperties: false` 即结构上杜绝注入」 | 本仓库 `ToolValidator` **不读** `additionalProperties`（`core/validator.rs:11-56`），它只是给模型的提示 |

### 0.3 拍板结果（2026-09-28 已定稿）

| # | 决策 | 结论 |
|---|---|---|
| P1 | `shell` 参数 | **不暴露** —— 守 §3.1，`command` 语义唯一 |
| P2 | `exit_code ≠ 0` 的 `is_error` | **改成非工具层失败**（`is_error = timed_out \|\| spawn 失败 \|\| 护栏拒绝`） |
| P3 | 工作区隔离 / 命令名单 / 并发上限 | **不做** —— 只做审计 + 局部护栏（拒绝自杀）；隔离留作仓库级独立设计 |
| P4 | 环境变量工具 | **敏感键默认打码 + `allow_sensitive` 逃生阀**，并**删除 `builtin_set_env`** |

据此实施：见 §8（本节以下内容保持评审时的原貌，便于回溯取舍理由）。

### 0.4 第二轮拍板（Q1–Q3）与我越界项的处置

第一轮实施后，用户指出三处**我单方面决定、没让他拍板**的问题（越界）。第一轮只问了 P1–P4，
把下面这些当成了「结论」而不是「待拍板项」—— 这是流程错误，此处补记。

| # | 问题 | 结论 |
|---|---|---|
| Q1 | description 动态注入平台信息 | ~~折中注入极简平台事实~~ → **终审：不注入**（平台事实归执行期运行环境段）→ §3.4 / §7 第四轮 |
| Q2 | Windows `.cmd` / `.bat` 不兼容 | **spawn 前用 `which` 解析裸名**，消除与 `command_exists` 的自相矛盾 → §C8 |
| Q3 | 中文 / 非 UTF-8 输出编码 | **保持现状**：工具只声明「按 UTF-8 解码」，乱码由调用方自己切码（守 `command-execution.md` §3.3） |

其余我越界自定项的处置：

| 我原先擅自决定 | 处置 |
|---|---|
| description 不注入平台 | **即终审结论**：Q1 复议后维持「不注入」，折中注入稿作废（§7 第四轮） |
| Linux `process_group` + `SIGTERM` 宽限期 + 进程树回收未做 | **已定（Q4）**：不做 `libc`/`process_group`，改为 **Windows 侧用 `taskkill /F /T` 杀进程树**（零新依赖）；Unix 侧保持 `kill_on_drop` + `start_kill`（只保证直接子进程） |
| Windows 路径 `PathBuf` 规范化未做 | **无需额外改动**：`command` / `working_dir` 本就经 `Path` / `current_dir` 交给 OS，`/` 与 `\` 都由 OS 接受；不引入字符串拼接式的路径改写 |
| 会话内累计命令数上限未做 | **不做**（属资源政策，P3 已覆盖）；改为审计可统计 |
| 审计日志不落盘、改走 `tracing` | **保持**（理由见 §C7）；若你要落盘文件，这是一条独立小改动 |

---

## 1. 现状盘点（代码事实，全部核对过）

### 1.1 八个工具

`crates/tool-manager/src/builtin/system_tools.rs`（唯一实现，全仓无其它命令执行通道）：

| 工具 | 入参 | 现状返回 | 已知问题 |
|---|---|---|---|
| `builtin_execute_command` | `command`(必填) / `args[]` / `working_dir` | `{stdout, stderr, status}`，`is_error = !status.success()` | 同步阻塞、无超时、无截断、无 `command` 语义校验、状态码语义被当成失败 |
| `builtin_command_exists` | `command` | `{exists}` | 正常（`which`）。可顺带回传解析到的绝对路径 |
| `builtin_list_processes` | `filter?` | `{processes[], count}` | `refresh_all()` 全量收集，无上限 |
| `builtin_get_process_info` | `pid` | 进程详情 | 正常 |
| `builtin_kill_process` | `pid` | 终止结果 | **可杀死宿主自身 / 任意系统进程**，无任何护栏 |
| `builtin_get_env` | `name` | `{name,value,exists}` | 敏感键无遮蔽 |
| `builtin_set_env` | `name,value` | 结果 | 改**宿主进程**全局环境（全仓唯一 `std::env::set_var`），见 A13 |
| `builtin_list_env` | `filter?` | 全量键值 | **无遮蔽**，敏感值直接进 LLM 上下文 / trace / 落库 |

调用面：注册点是 `crates/agent-gui/src/context/tools/mod.rs:51`；`flexible` 执行器默认 `allowed_tools: None`
（全部工具可见），所以 `builtin_execute_command` 是模型**唯一**可用的命令执行工具。除注册与路由外，
全仓没有对这几个工具名的特化引用（无 UI 分支、无代码依赖）→ **改返回结构的影响面很小**。

### 1.2 一个新发现：文档漂移

`command-execution.md` §2.4 / §2.5 声称「唯一保留下来的是工具 **description**」以及
「`command` 含空白时返回指导性错误」，但：

- 代码里 description 仍是旧文案 `"执行系统命令并返回输出（内置工具）"`（`:19`）；
- 全仓 grep `不经过 shell` / `只能是可执行文件名` / `见运行环境段` **零命中**；
- `git status` 显示 `system_tools.rs` **无任何改动**。

结论：那两处改动**从未（或已被回滚）落到代码**。本稿把 A10 / A14 纳入实施清单，顺带回填那份文档（§6.4）。

### 1.3 基线

```
cargo test -p planned-agent-tool-manager --lib   →  14 passed（本次实跑）
system_tools.rs 内无任何 #[cfg(test)]
```

---

## 2. 粘贴文档 vs 本仓库硬约定：逐条冲突

### C1 —— description 动态注入平台信息（粘贴 §2.2 + §2.4）

粘贴文档要求 description 按平台拼接 `当前平台：windows` + 推荐命令集（Windows 给 `dir` / `type` / `findstr`）。

**冲突**。`command-execution.md` §3.1 的硬约定是：

> tool 只执行，不做平台决策。平台差异属于「调用方该知道的事实」，走**环境段**（运行时事实 → LLM 决策）；
> tool 的契约必须对所有调用一致、对所有平台一致。

而 §1.5 记录的实测事故正是这条建议造成的：**工具不过 shell，所以 `dir` 必然 spawn 失败**，
模型照 description 做就失败，只能自己试出 `cmd /c dir` —— 白烧轮次。§2.5 结尾进一步总结：

> 凡是「每台机器不一样」的值，都不该出现在静态 schema 的举例里。

平台事实在本仓库有**专职载体**：执行期「运行环境段」（`crates/planned-agent/src/flexible/prompt.rs`，已含
OS / 路径分隔符 / 行尾 / **本机 shell** / 已探测可用命令 / 编码风险）与 `core::host::RuntimeEnvironment`。

**结论 N1/N3：不动态注入。** description 只回答「**怎么填**」（静态、跨平台），「本机是什么」由环境段给。

### C2 —— `shell: bool`（粘贴 §2.3 / §4.1 / §7.4）

粘贴文档 §2.3 把 `shell` 写成 schema 字段（默认 `false`，`true` 时 Windows `cmd /C` / Linux `sh -c`），
但 §5.3 又写「**建议 `shell = true` 只对内部受信调用开放，不暴露给 LLM**」——两处直接矛盾。

同时它与 C3 打架：`shell=true` 时 `command` 变成「脚本字符串」，与 `command` 是「可执行文件名」是**同一个字段的两种语义**
——正是 §2.5 踩过并已修正的坑（模型把 `powershell -Command "…"` 整条塞进 `command`）。

本仓库现有替代路径（`command-execution.md` §2.4 定稿）：**要套 shell 就把 shell 名填在 `command`、脚本放 `args`**，
本机是哪个 shell 由环境段告知。这条路对所有调用一致、无隐藏语义、不需要 tool 决策。

**结论 N2：默认不暴露（→ P1 拍板）。** 若最终要暴露，必须同时接受：① `command` 字段语义分叉；
② 与 `command-execution.md` §3.1 的关系需要重写（它反对的是「auto 决策」，显式开关严格说不违反，
但会重新引入「同一 schema 两种语义」）。

### C3 —— `command` 的语义，粘贴文档内部自相矛盾

- §2.3：`command` 是「可执行文件名，不含路径、不含参数」；
- §7.4 示例：`{"command": "ps aux | grep node", "shell": true}`。

后者必然失败（`CreateProcess` / `execve` 会去找一个叫 `ps aux | grep node` 的文件，报 `os error 3`，
信息量极低 —— §2.5 有完整的两轮实测记录）。

**结论 A10：** 保留「`command` 只能是可执行文件名」，并实现 §2.5 的指导性错误。
**精确化**（避免误伤合法绝对路径）：仅当 `command` 含空白 **且** `Path::new(command).exists() == false` 时报错；
`"C:\Program Files\nodejs\node.exe"`、`git status` 的判定因此不会互相误伤。

### C4 —— `exit_code ≠ 0` 是否算工具失败（粘贴 §3）

粘贴 §3：「`exit_code` 非 0 **不视为工具调用失败**，仍原样返回给 LLM 判断」。

现状是反的：`is_error = !status.success()`。而这个 `is_error` 在链路上**有三处实际后果**：

| 位置 | 后果 |
|---|---|
| `crates/planned-agent/src/chat/driver/round/handlers.rs:169` | `ErrorType::ExecutionError` → 写进历史、UI 工具卡片标红 |
| `crates/planned-agent/src/flexible/step.rs:277,284` | `ok = !is_error` → `PlanRunEvent::StepToolCall.ok`，`flexible` 事件流 |
| `crates/planned-agent/src/planner/trace/recorder.rs:584` | `is_complete: !is_error` → trace 导出的步骤完成态 |

按粘贴文档的理由：`grep` 没匹配返回 1、`git diff --exit-code` 返回 1、`cargo test` 有失败用例返回 101
—— 这些都是**工具调用成功、命令自身失败**，标成 `ExecutionError` 会误导 LLM（以为工具坏了，重试或换命令）
与用户（卡片标红）。语义上粘贴文档是对的。

**但这会改变三处既有行为** → **P2 拍板**。推荐值见 §3.5。

### C5 —— 工作区隔离（粘贴 §5.2）

粘贴要求：`working_dir` 必须 `canonicalize` 后位于允许根之下，否则拒绝；未提供则用「默认工作区」。

**两个障碍**：

1. **架构上没地方放配置**。`SystemToolsProvider` 是无状态 unit struct，`BuiltinToolProvider` trait
   只有 `tools()` / `executor()`，`ToolRegistry` 也没有任何注入式配置（`tool-manager/src/core/registry.rs:45-69`
   全是 `RwLock<HashMap>`）→ 必须新增 `SystemToolsProvider::new(policy)` 并在 GUI 注册点构造（改动落到
   `agent-gui`，跨 crate）。
2. **一致性上形同虚设**。`builtin_read_file` / `builtin_write_file` / `web_tools` / `data_tools` **统统没有沙箱**，
   且 `file_tools.rs:145-149` 的测试注释显示「读用户桌面路径」是**被支持的正常用法**。
   只锁 `execute_command` 的 `working_dir`，模型可以用 `builtin_read_file` 读任意文件、
   用 `command="node", args=["-e","..."]` 绕过。

**结论 P3。** 若要做，正确落点是**仓库级**（ToolRegistry 路径策略层 / `core` 里的 policy 抽象），
而不是 `system_tools.rs` 单点；单点做只会制造「有安全感的假象」。默认建议：**不做隔离**，只做
「指导性错误 + 审计 + 显式拒绝自杀/敏感值」这类**不需要全局一致性的局部护栏**。

### C6 —— 命令白名单 / 危险命令永久拉黑（粘贴 §5.1）

同 C5：名单极易绕过（`node -e "fs.rmSync(...)"`、`python -c`、`cargo` 的 `build.rs`…），
且「按需加白名单」需要运维面 —— 本仓库没有工具策略的配置面。
`rm` / `del` 黑名单能防**误伤幻觉**（如模型幻觉出删库命令），有一定价值，但价值 < 维护成本 + 被绕过的自欺。

**结论 P3（同项拍板）。** 若只取最小价值，建议**只记审计不拦截**：把 `command` / `args` / `working_dir` /
`exit_code` 落进 `tracing`，出问题时能复现与追溯（粘贴 §5.5 的意图），但不假装能拦住。

### C7 —— 审计日志形态（粘贴 §5.5）

粘贴要求「审计日志**落盘**」，格式 `时间戳 | 会话ID | command | args | working_dir | exit_code | ...`。

`tool-manager` 是 L1 无状态 crate，没有日志目录的概念；本仓库的日志在家门口：
`agent-gui` 的 `logs/` + `tracing`（`command-execution.md` §1.1 就是直接引 `gui.log.*`）。
自建落盘需要新增路径配置 + 目录管理 + 轮转，重复造轮子。

**结论 A9：** 用 `tracing::info!` 结构化字段（`target: "tool_audit"`），落进既有日志设施。
**隐私注意**：`gui-host-environment.md` 记录了「不写 `working_dir`」的隐私约定（针对**送给 LLM 的环境段**）；
审计日志是本机日志、且粘贴 §5.5 明确要求回显 `working_dir`，两者不冲突 —— 但要写清边界：
**审计日志可以记，送进 LLM 上下文的不加工作目录。**

### C8 —— Windows `.cmd` / `.bat` 兼容缺口（**漏项，非冲突** —— 粘贴文档与本稿初版都没覆盖）

**实测 + 源码核实：**

| 事实 | 出处 |
|---|---|
| `Command::new("npm")` 在 Windows 上**只给无扩展名的名字补 `.exe`，不做 PATHEXT 展开** → 找不到 `npm.cmd` | 官方文档「For executable files, the .exe extension may be omitted. Files with other extensions must include the extension」+ `rust-lang/rust#37519`（2016 至今 open） |
| `tokio::process::Command` 只是包 std（`from(StdCommand::new(program))`），**行为完全一致** | tokio 源码；tokio 文档引用同一个 issue |
| `which::which("npm")` **会**做 PATHEXT 展开 → 返回 `npm.cmd` | which-rs `finder.rs` 读 `PATHEXT` 逐扩展名尝试 |
| 本机现场：`Get-Command npm` → `D:\Program Files\nodejs\npm.ps1`（同目录有 `npm.cmd`）；`cargo` / `node` 是真 `.exe`，所以此前掩盖了问题 | 本机实测 |
| **带 `.cmd` / `.bat` 扩展名解析到的完整路径**，std 会自动改用 `cmd.exe` 执行（OS 层找文件，不是我们「套 shell」）；`.ps1` 完全不能直接 spawn | std `has_bat_extension` 分支 |

**后果（为什么必须修）**：`builtin_command_exists("npm")` 经 `which` 说「存在，路径 `…\npm.cmd`」，
而 `builtin_execute_command` 传裸名 `npm` **失败** —— **同一工具族内部自相矛盾，主动误导模型**。
受影响面：`npm` / `pnpm` / `yarn` / `tsc` / `prettier` 等整条 Node 工具链。

**Q2 拍板：spawn 前用 `which` 解析裸名。**

```
解析规则（只在「名字不含路径分隔符」时介入，丝不改调用方给的显式路径）：
  command 含 / 或 \  → 原样交给 OS（调用方的显式意图优先，且真实路径含空格也照旧放行）
  否则               → which::which(command)
                        命中 → 用解析到的完整路径 spawn（.cmd / .bat 由 std 交给 cmd.exe）
                        未命中 → 仍按原名 spawn；失败则报 executable_not_found
                                （回显命令名 + 「本机 PATH / PATHEXT 里找不到」提示）
```

为什么这不违反「tool 不做平台决策」：它**不改变语义、不解释命令、不改写调用方输入**，
只是把「这个裸名在本机对应哪个文件」这一个 OS 本就该回答的问题问了一次 ——
与 `CREATE_NO_WINDOW` 属同一类「例外」（对所有调用一致生效，且结果等价于调用方自己写完整名）。

**仍不处理的（记录在案）**：`command` 带引号（`"C:\...\node.exe"`）· 相对路径 `./x`（原样交给 OS）·
UNC / 长路径 · `.ps1`（必须走 `command="powershell", args=["-File", …]`）。

---

## 3. 优化后的契约

### 3.1 `builtin_execute_command` 的 JSON Schema

```json
{
  "type": "object",
  "properties": {
    "command": {
      "type": "string", "minLength": 1, "maxLength": 256,
      "description": "可执行文件名（不是命令行字符串）：git、cargo、node。要套 shell 就把 shell 名填在这里、脚本放 args。裸名会先按本机 PATH / PATHEXT 解析（含 .cmd / .bat），解析不到才报错。"
    },
    "args": {
      "type": "array", "items": { "type": "string" }, "default": [],
      "description": "参数列表，逐项传递，不做字符串拼接。"
    },
    "working_dir": {
      "type": "string",
      "description": "工作目录绝对路径，省略则用宿主当前目录。"
    },
    "timeout_ms": {
      "type": "integer", "minimum": 100, "maximum": 300000, "default": 30000,
      "description": "超时毫秒数。默认 30000（30 秒）；编译、测试、安装依赖、下载等可能超过 30 秒的任务（如 cargo build、cargo test、npm install、pip install、make、docker build）请显式提高。"
    },
    "env": {
      "type": "object", "additionalProperties": { "type": "string" },
      "description": "追加/覆盖的子进程环境变量，不清空继承的环境。"
    },
    "stdin": {
      "type": "string",
      "description": "写入子进程标准输入的内容（UTF-8）。省略则不写入。"
    },
    "max_output_bytes": {
      "type": "integer", "minimum": 1024, "maximum": 10485760, "default": 262144,
      "description": "stdout / stderr 各自保留的最大字节数，超出截断并标记。默认 262144（256 KiB）。"
    }
  },
  "required": ["command"],
  "additionalProperties": false
}
```

与粘贴 §2.3 的差异：**去掉 `shell`**（→ P1）。其余字段保留。

> ⚠️ `default` / `minLength` / `maximum` / `additionalProperties` 在本仓库**运行时不生效**：
> `ToolValidator::validate_arguments`（`core/validator.rs:11-56`）只检查 `required` 与字段类型，
> 且类型不匹配也只 `warn` 不报错。所以：**实现必须自己做默认值与上下限钳制**，
> schema 里的约束只是给模型的提示。（同时注意：某些 provider 的 strict-schema 模式要求所有
> `properties` 都出现在 `required` 里 —— 本仓库现状未启用，保持 `required: ["command"]`。）

### 3.2 成功返回

```json
{
  "exit_code": 0,
  "stdout": "…",
  "stderr": "…",
  "stdout_truncated": false,
  "stderr_truncated": false,
  "duration_ms": 128,
  "timed_out": false,
  "command_line": "git status"
}
```

- `exit_code`：进程退出码；被信号杀死 / 超时后回收时为 `-1`。
- `command_line`：`command` + 空格连接的 `args`，**仅展示与审计用**（不参与执行）。
- 去掉现状的 `status` 字段。**影响面已核实**：全仓无代码/UI 依赖该字段。

### 3.3 失败返回与错误码

统一形态：`ToolResult { is_error: true, content: { "error": "<code>", "message": "<人话>" } }`。

**不再用 `Err(anyhow!)` 承载可预期的失败**，理由：上层把 `Err` 拍平成字符串
（chat `handlers.rs:161` → `"Error: {e}"`；flexible `step.rs:268` → `"工具执行失败：{err}"`），
**错误码与结构全部丢失**；而返回 `Ok` + 结构化 content 时，上层行为等价（都是 `is_error=true` 回灌），
却保住了机器可读的 code。

| `error` | 触发 | LLM 该怎么办 |
|---|---|---|
| `invalid_arguments` | `command` 缺失 / 类型错 / 含空白且非真实路径 | 重填参数（A10 的指导性错误走这里） |
| `executable_not_found` | spawn 报 `NotFound` | 换命令，或提示用户安装 |
| `permission_denied` | spawn 报 `PermissionDenied` | 提示用户 |
| `working_dir_invalid` | `working_dir` 不存在 / 不是目录 | 换目录 |
| `process_not_found` | `kill_process` / `get_process_info` 的 pid 不存在 | 换 pid |
| `operation_refused` | 护栏拒绝（自杀、敏感值） | 换目标 |
| `spawn_failed` / `internal_error` | 其它 | 记录并上报 |
| `timeout` | 超过 `timeout_ms`，子进程已被终止 | 决定是否重试或加长超时（输出里常有线索） |

`timeout` 与粘贴文档 §6 的处理不同：它**既在错误码表里，也保留完整的执行结构** ——
content 是 `{error:"timeout", message:…, exit_code:-1, timed_out:true, stdout, stderr, …}`。
错误码让上层能机械判断（失败形态只有一种），而输出是排查「为什么这么慢」的唯一现场证据，必须回灌给 LLM。
（评审阶段曾把它排除在错误码表外，那会让「失败统一为 `{error,message}`」的约定多出一个例外，反而更容易误判。）

### 3.4 description（定稿：**不注入平台，保持静态**）

```
执行系统命令并返回 exit_code、stdout、stderr。

调用规则：
1. command 是可执行文件名（不是命令行字符串）：git、cargo、node。要套 shell 就把 shell 名填在这里、
   脚本放 args（本机是哪个 shell 见运行环境段）。
2. args 是参数数组，逐项传递，不做字符串拼接，也不解析引号 —— 每个元素就是一个完整参数。
3. 本工具不经过 shell：管道、重定向与 shell 内建命令（cd / dir / type）都不可直接用。
4. 裸名会先按本机 PATH / PATHEXT 解析（Windows 上 npm / pnpm / tsc 这类 .cmd 脚本也能解析到），
   解析不到才报 executable_not_found。
5. 输出按 UTF-8 解码（非 UTF-8 字节会损坏），换行已归一化为 \n。
6. 默认 30 秒超时，长任务请显式提高 timeout_ms。
7. exit_code 非 0 表示命令自身失败、工具调用本身仍然成功，请据 stdout / stderr 判断原因。
```

**不注入任何平台事实**（连 `当前平台` 也不注入），与粘贴 §2.2/§2.4 的关键差别：

| 粘贴要求 | 本稿 | 理由 |
|---|---|---|
| `当前平台：{PLATFORM}` | ❌ 不注入 | ① 平台是「每台机器不一样」的值，按 §3.1 ③ 不进静态契约（与不采纳项 N1 同源）；② 平台事实已有**专职载体**——执行期运行环境段（`flexible/prompt.rs`）与 `core::host::RuntimeEnvironment`，都已含 OS；③ 不注入让 description 对所有平台**逐字一致**，无需运行时拼装，便于缓存与比对 |
| `推荐命令集：{RECOMMENDED}`（`dir`/`type`/`findstr`…） | ❌ 不注入 | ① 那些是 shell 内建/别名，本工具不过 shell，列出来必然失败（`command-execution.md` §1.5 的实测事故就是这段造成的）；② 「本机有哪些命令」是**每台机器不同**的事实，专职载体是执行期运行环境段（已实现） |

> 注：本稿曾按 Q1「折中注入」只注入 `当前平台` 一行，后经复议改为**完全不注入**，理由如上（另见 §7 第四轮）。

另一处补充：规则 2 明确了「args 不做引号解析」—— 原稿只说「不拼接」，但 LLM 常写
`args: ["-m \"hello world\""]`，那会把引号当成参数的一部分。

### 3.5 `is_error` 语义（P2）

| 方案 | `is_error` | 优点 | 代价 |
|---|---|---|---|
| **B（推荐）** | `timed_out \|\| spawn 失败 \|\| 护栏拒绝` | 符合粘贴 §3 的语义；`exit_code` 字段承担「命令自身失败」的信息 | 改 3 处既有行为（§C4 表）；`cargo test` 失败不再标红 |
| **A（保守）** | `!success()`（现状） | 零回归，三处链路行为不变 | `grep` 无匹配 / `git diff --exit-code` 被误报成 `ExecutionError` |

---

## 4. 执行层设计（实现要点与陷阱）

### 4.1 异步、超时、回收

```
tokio::process::Command  →  spawn
  ├─ creation_flags(CREATE_NO_WINDOW)   [cfg(windows)]
  ├─ kill_on_drop(true)
  ├─ stdin/stdout/stderr = piped
  ├─ envs(env) / current_dir(working_dir)
  └─ child
        ├─ stdout / stderr 各自 spawn 一个「限字节读取」task（见 4.2）
        ├─ stdin 内容 spawn 一个写入 task（避免「子进程不读 → 写满管道 → 双向死锁」）
        └─ tokio::time::timeout(timeout_ms, child.wait())
              ├─ Ok(status)  → 正常路径
              └─ Err(_)      → timed_out = true；child.start_kill()；再 wait() 收割
```

**三个必须避开的陷阱**：

1. **`child.wait_with_output()` 会把 `Child` 的所有权吃掉** —— 超时后拿不回句柄，`kill` 无从下手。
   所以要先 `child.stdout.take()` / `stderr.take()` 手动接管管道，再对 `child.wait()` 做 `timeout`。
2. **截断**后仍必须**继续把管道读干（drain）并丢弃**，否则子进程写满管道会阻塞、永不退出 →
   每次截断都变成一次超时。
3. **Windows 上 `Child::kill` 只杀直接子进程**，不杀进程树。粘贴 §4.3 承诺的「一次性 kill 整个进程树」
   在 Windows 需要 Job Object（新增 `windows-sys` 依赖 + 一段 unsafe 平台代码）。
   **建议**：本轮不做，在文档与 description 里**诚实标注**为「尽力而为」；
   Linux 侧可用 `process_group` + `killpg`（`CommandExt`）覆盖。

### 4.2 输出处理

- 分块读（如 8 KiB/块）累加到 `max_output_bytes`，到顶后置 `*_truncated = true` 并**继续 drain**（4.1 陷阱 2）。
- `String::from_utf8_lossy`（非法字节 `�`）。
- `\r\n` → `\n`（**A5**）。⚠️ 这是**改写调用方的原始输出**，与 `command-execution.md` §3.1 ②
  「不篡改调用方的东西」有张力。判定：它只做「平台差异归一化」，对所有平台一致、不改变命令语义，
  与 `CREATE_NO_WINDOW` 同属「例外」；但**必须写进 description/契约**（已写），否则调用方会以为拿到的是原始字节。

### 4.3 进程创建方式

- `CREATE_NO_WINDOW`（`0x08000000`）仅 Windows，且属于 `command-execution.md` §3.1 认定的**例外**
  （纯进程创建方式，不参与「用什么命令、怎么解释命令」的决策）→ 可以直接做，不必拍板。
- `working_dir` 存在性校验：不存在 / 非目录 → `working_dir_invalid`（**指导性错误，回显路径**，
  与 `file_tools.rs:132` 的 `fs_error` 同一套路 —— 有实测教训：路径拼错会白烧 10 轮重试）。

### 4.4 并发上限

粘贴 §4.6 要求「全局最多 4 个并发子进程」。实现：`static SEM: OnceLock<tokio::sync::Semaphore>`（crate 内）。
注意：它是**进程级全局状态**，测试里跨用例共享 → 用 `limit` 上限而非硬编码，并让测试不依赖并发度。
是否要做 → 并入 P3（属「资源政策」，非 bug）。

### 4.5 审计

`tracing::info!(target: "tool_audit", command, args, working_dir, exit_code, duration_ms, timed_out, truncated)`。
不落自定义文件（C7）。

---

## 5. 其余七个工具的改动

| 工具 | 改动 | 理由 |
|---|---|---|
| `builtin_command_exists` | `which` 命中时**额外回传绝对路径** `{exists, path}` | 让模型少一步 `where`/`which` 试探 |
| `builtin_list_processes` | 新增 `limit`（默认 200）+ 回传 `total` | 现状可能把上千进程（每条 5 字段）塞进上下文 |
| `builtin_get_process_info` | 不存在时改为 `Ok` + `{error:"process_not_found"}` | 与 §3.3 统一（现状是 `is_error:true` 但无 `error` 码） |
| `builtin_kill_process` | ① 拒绝 `pid == std::process::id()`；② 拒绝 `pid <= 1`；③ 统一错误码 | 现状可自杀 / 杀 init；护栏不需要全局一致性 → 安全做 |
| `builtin_get_env` | 敏感键 → P4 | — |
| `builtin_list_env` | ① 默认屏蔽敏感键的值；② 加 `limit`（默认 200） | 现状把 `OPENAI_API_KEY` 等直接灌进 LLM 上下文 / trace / 落库（下游可能有网） |
| `builtin_set_env` | **删除**（A13） | 改宿主全局状态：① 对「每次调用新进程」的语义无用；② 留下跨调用污染通道（可覆盖 `PATH`、可伪造后续所有子进程的环境）；③ 全仓唯一 `set_var`，与「工具不改宿主」的价值观冲突。`env` 参数已覆盖真实用例 |

敏感键判定（实现于 `is_sensitive_env`）：名称 ASCII 大写后包含以下任一片段即视为敏感 ——
`_KEY` / `APIKEY` / `TOKEN` / `SECRET` / `PASSWORD` / `PASSWD` / `PASS` / `BEARER` / `JWT` /
`CREDENTIAL` / `SESSION` / `COOKIE` / `PRIVATE_` / `AWS_` / `OPENAI_` / `ANTHROPIC_` / `GITHUB_`。
默认**打码**为 `"***"`（保留键名，屏蔽值）；逃生阀 `allow_sensitive: true` 显式放行。
`get_env` 与 `list_env` 共用同一个判定与遮蔽函数，不允许两者不一致。

刻意**不放裸 `AUTH`**：那会把 `GIT_AUTHOR_NAME` 一起误伤。判定偏向多打码 —— 误遮一个不敏感的值无害，
漏遮一个敏感值有害。

`builtin_set_env` 删除后，工具总数 8 → 7。

### ⚠️ 脱敏的边界（评审发现，必须写清楚，否则是自欺）

脱敏**只能阻止敏感值进入对话上下文 / trace / 落库**，**不能阻止有命令执行权的模型主动去读**：
子进程继承宿主完整环境，模型完全可以 `command="cmd", args=["/C","set OPENAI_API_KEY"]`
或 `command="sh", args=["-c","env"]` 拿到明文。

**为什么不在 spawn 前剔除敏感环境变量**：那会打断大量正当用法（`GITHUB_TOKEN` 拉私有依赖、
`AWS_*` 部署、`CARGO_REGISTRY_TOKEN` 发版），而用户让 agent 干这些事是常态 ——
它属于「tool 替调用方做决策」，与 §3.1 的硬约定相背。脱敏的定位因此是**防泄漏到下游**（模型上下文、
trace、落库、第三方 API），不是**防本地读取**。这条边界必须同时成立在文档与代码注释里。

同理，审计日志会原样记录 `args`（`--token=…` 这类会落进本机日志文件）：审计的本意就是记录
**实际执行了什么**，删掉关键参数会让日志失去价值；而 `env` 参数不记 —— 差异是有意的，
两个入口都能拿到子进程环境，记一个就够，而 `args` 是唯一能重建命令行现场的地方。

---

## 6. 影响面

### 6.1 代码

- `crates/tool-manager/src/builtin/system_tools.rs` —— 主战场（重写）。
- `crates/tool-manager/Cargo.toml` —— 若做并发上限无需新增依赖；`tokio` 已是 `full`（含 `process`/`time`）。
  `which` / `sysinfo 0.30` 已有。**无需新增依赖**（除非做 Windows Job Object）。
- 注册点 `crates/agent-gui/src/context/tools/mod.rs:51` —— 仅当 P3 选「做隔离/名单」时才需改
  （`SystemToolsProvider::new(policy)`）。
- **上层零改动**：`flexible/step.rs` / `chat/handlers.rs` / `trace/recorder.rs` 都只读 `is_error` 与 `content`。

### 6.2 UI

agent-gui 对这几个工具名**无特化渲染**（走通用工具卡片），返回结构变化由通用 JSON 展示兜住。
唯一可见变化：P2 选 B 时「命令返回非 0 不再标红」。

### 6.3 测试

- 现状 `system_tools.rs` 内**无测试**，基线 14 passed（`--lib`）。新增测试全部放同文件 `#[cfg(test)] mod tests`
  （符合 `file_tools.rs` 的既有习惯）。
- 覆盖清单：正常命令 / `exit_code` 非 0 / 超时被杀 / 命令不存在 / `command` 含空白且非路径（A10）/
  `working_dir` 不存在 / 超长输出截断 + drain 不卡死 / `env` 生效 / `stdin` 生效 / 换行归一化 /
  自杀拒绝 / 敏感值打码 / `list_env` 上限。
- 注意：走真实进程的用例要用**跨平台**命令（`git --version` 太重，优先 `rustc --version` 或
  `cargo --version` —— 已经在跑 cargo，可判定为必然可用，参考 `command-execution.md` §4 的
  `probe_real_command_reports_available_with_version` 的做法）。

### 6.4 文档回填（重要，否则仓库文档自相矛盾）

`docs/planned-agent/command-execution.md` 必须同步：

| 位置 | 现状 | 回填后 |
|---|---|---|
| §2.2 标题 | 「**不改**」 | 改为「本轮重写」，并指向本文 |
| §2.4 | 声称 description 已定稿 | 标注当时**未落到代码**，本轮实装（A14） |
| §5「后续（未做）」第 2、3 条 | 「黑框未加」「仍是同步 `std::process`」 | 两条均已完成（A8 / A1） |
| §4 验证表 | 旧测试数 | 更新为实施后的实测数字 |

---

## 7. 拍板记录（4 项，已定稿）

- **P1 `shell` 参数** → **不暴露**。`command` 永远是可执行文件名，语义唯一。
- **P2 `exit_code ≠ 0` 的 `is_error`** → **方案 B**：`is_error = timed_out || spawn 失败 || 护栏拒绝`。
  三处既有链路行为随之改变（§C4 表），这是**有意为之**：`grep` 无匹配、`git diff --exit-code` 返回 1
  都是「命令自身失败、工具调用成功」，不应报 `ExecutionError`。
- **P3 隔离 / 名单 / 并发上限** → **不做**。只保留「审计 + 拒绝自杀 + 指导性错误」。
  理由：一致性（file/web/data 工具无沙箱，单点锁 `working_dir` 可用 `builtin_read_file` 或
  `node -e` 绕过）与架构（无配置注入面）。真要沙箱，落点应是 `ToolRegistry` 的路径策略层，
  作为独立设计，并覆盖全部工具。
- **P4 环境变量工具** → **敏感键默认打码**（`***`）+ `allow_sensitive: true` 逃生阀；
  **删除 `builtin_set_env`**（工具数 8 → 7）。

### 第二轮（2026-09-28，Q1–Q3）

- **Q1 description 平台信息** → ~~**折中注入极简平台事实**：只注入 `当前平台`~~ → **终审改为不注入**（见下方第四轮）。
- **Q2 Windows `.cmd` / `.bat`** → **spawn 前用 `which` 解析裸名**（§C8），消除
  「`command_exists` 说存在、`execute_command` 失败」的自相矛盾。
- **Q3 输出编码** → **保持现状**（工具声明 UTF-8 契约，调用方自己切码）。

### 第三轮（2026-09-28，Q4）

- **Q4 进程树回收** → **Windows 侧用 `taskkill /F /T /PID <pid>` 杀整棵树**（超时分支里，带短超时兜底），
  再 `start_kill` + `wait`。理由：`npm` / `pnpm` / `yarn` 是「启动器 + 子进程」模型，
  只杀启动器会把 `node` 留成孤儿，而这是最常用的命令形态；`taskkill /T` 系统自带、零新依赖、无需 unsafe。
- **明确不做**：Unix `process_group` + `killpg`（需新增 `libc`）、Windows Job Object（需 `windows-sys` + unsafe）。
  Unix 侧只保证直接子进程，边界已写进文档与代码注释。
- **与粘文档 §4.3/§4.4 的差异**：粘文档要求「优先 `SIGTERM`、宽限后 `SIGKILL`」。本稿统一用**立即强杀**
  （`taskkill /F` / `start_kill`）：agent 场景里「宽限期」的价值低（没人会响应优雅退出信号），
  而多等 500ms 会拖慢每一次超时；真要优雅退出，调用方自己写 `command="..."` 里的信号处理。

### 第四轮（Q1 复议，终审）

- **Q1 description 平台信息** → **改为「不注入」**，推翻第二轮「折中注入 `当前平台`」。
  理由：① 平台是「每台机器不一样」的事实，按 §3.1 ③ 不进静态契约（与不采纳项 N1 同源）；
  ② 平台事实已有**专职载体**——执行期运行环境段（`flexible/prompt.rs`）与 `core::host::RuntimeEnvironment`，
  都已含 OS，description 无需重复；③ 不注入让 description 对所有平台逐字一致、无需运行时拼装，便于缓存与比对。
  落地：`SystemToolsProvider::tools()` 不再拼 `当前平台`；测试断言 description **不含**平台名与
  `{PLATFORM}`/`{RECOMMENDED}` 占位符（`system_tools.rs::description_stays_static_without_platform_injection`）。

---

## 8. 实施步骤

**第一轮（已完成）**

1. ✅ 重写 `system_tools.rs`（异步 / 超时 / 截断 + drain / 结构化错误码 / 局部护栏 / 审计）。
2. ✅ 补 `#[cfg(test)] mod tests`；`cargo test -p planned-agent-tool-manager --lib` → **38 passed**（基线 14）。
3. ✅ 回填 `command-execution.md`（§6.4）。
4. ✅ 全仓回归：`cargo test --workspace` → 仅 3 个既有 `planner::coarse::llm_planner` 失败（prompt 漂移，与本轮无关）。

**第二轮（待确认后执行）**

5. `which` 解析裸名（§C8）：新增 `resolve_command()` helper + 测试
   （`.cmd` 可解析 / 含路径分隔符不介入 / 未命中仍按原名 spawn / 解析结果进 `command_line` 与审计）。
6. description **保持静态**（§3.4，Q1 终审不注入）：`tools()` 不按 `std::env::consts::OS` 拼 `当前平台`；
   测试断言 description **不含**平台名与 `{PLATFORM}`/`{RECOMMENDED}` 占位符、也**不含**平台专属命令举例
   （`dir` / `tasklist` / `findstr`）—— 已由 `system_tools.rs::description_stays_static_without_platform_injection` 覆盖。
7. Windows 进程树回收（§7 Q4）：超时分支改用 `kill_process_tree()` —— `taskkill /F /T /PID` + `start_kill` + `wait`；
   补一条 Windows-only 测试（`cfg(windows)`，断言超时后子进程的孙进程也消失，或至少断言 `taskkill` 路径被走到）。
8. 重跑 `cargo test -p planned-agent-tool-manager --lib` 与 `cargo test -p planned-agent-gui --bins`。
