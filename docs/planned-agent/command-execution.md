# 命令执行与环境事实（黑框 / 探测 / shell / 编码）

> 起因：`dx serve` 启动后 ① 冒一堆 `cmd` 黑框；② 环境段把 node/cargo 全报成「不可用」，
> 而它们明明在 PATH 里；③ 环境段给出的命令建议把模型带进坑（`dir` 根本不是可执行文件）。
>
> 这些现象背后是**三条**不相干的线，各自独立修。本文记录实测证据、改动与取舍，
> 包括一条**走错又退回**的路（§2.2）—— 它的结论比改动本身更重要。
>
> 上游文档：`gui-host-environment.md`（环境事实怎么送到 UI 与执行链）。

---

## 1. 实测证据

### 1.1 探测耗时紧贴超时（来自 `crates/agent-gui/logs/gui.log.2026-09-28`）

```
04:53:58.494 DEBUG prompt_manager::template: Added template: ...
04:54:00.105  INFO planned_agent_gui::context::environment: 环境探测完成 os=windows arch=x86_64 available=0 missing=7
04:54:18.088 DEBUG prompt_manager::template: Added template: ...
04:54:19.709  INFO planned_agent_gui::context::environment: 环境探测完成 os=windows arch=x86_64 available=0 missing=7
```

两次都是 **~1.61s**，而当时 `PROBE_TIMEOUT` 正好是 **1.5s** —— 七个命令**全部压线超时**。

### 1.2 同一份探测逻辑在测试进程里完全正常

`cargo test -p planned-agent-core --lib host` 里 `probe_executables(&["cargo"])` 能正常探到版本。
差别只在**进程有没有控制台**：测试进程有，`dx serve` 起的 GUI 进程没有。

### 1.3 `COMSPEC` 与「该用的 shell」是两回事

```
COMSPEC     = C:\WINDOWS\system32\cmd.exe                                ← 系统默认命令解释器
powershell  = C:\WINDOWS\System32\WindowsPowerShell\v1.0\powershell.exe  ← 用户实际在用的
```

`shell_name()` 读 `COMSPEC`，于是环境段报「默认 shell：cmd」—— 客观是「默认解释器」，
但读起来像是「你该用 cmd」，直接把模型带偏。

### 1.4 版本字符串自带命令名前缀

`cargo --version` 输出 `cargo 1.95.0 (f2d3ce0bd …)`，渲染时又拼了一次名字 →
`cargo cargo 1.95.0`。`node`（输出 `v24.15.0`）没这问题。

### 1.5 环境段那条建议是错的（且是我们自己造的弯路）

原 `command_hint()` 写：「Windows：`dir` / `type` / `where` / `findstr`，不要用 `ls` / `cat`」。

但 `builtin_execute_command` 是：

```rust
let mut cmd = std::process::Command::new(command);   // 直接 spawn，不套 shell
cmd.arg(arg_str);                                     // 参数是独立数组
String::from_utf8_lossy(&output.stdout)              // 输出按 UTF-8 强解
```

**不过 shell**，所以 `dir` 必然 spawn 失败。模型照建议做就会失败，再自己试出 `cmd /c dir`
—— 这正是我们要消灭的走弯路，而且是环境段亲手制造的。

---

## 2. 改动

### 2.1 `crates/core/src/host/environment.rs`（探测层）

| 项 | 改动 |
|---|---|
| 黑框 | 新增 `CREATE_NO_WINDOW` 常量 + `no_window()`，`probe_one` 的版本命令 spawn 走它 |
| 超时 | `PROBE_TIMEOUT` 1.5s → **3s**（`CREATE_NO_WINDOW` 后通常 <100ms，3s 是冷启动余量） |
| shell | `shell_name()`（读 `COMSPEC`）→ `preferred_shell()`：按 `pwsh` → `powershell` → `cmd` 的 **PATH 探测**（Unix：`$SHELL` → `sh`）。只 `which`，不 spawn，故仍在同步的 `detect_host()` 里 |
| `console_encoding` | doc 改为诚实说明「**恒为 `None`**」—— 内核无法可靠探测；输出编码由命令自身决定，工具给不了保证 |

### 2.2 `crates/tool-manager/src/builtin/system_tools.rs`（执行层）—— 当时**不改**，后由重设计接手

`builtin_execute_command` 保持原版：`Command::new(command)` + 独立 `args`，**不经 shell**。

> **2026-09-28 更新**：执行层已按 `system-tools-redesign.md` 重新设计（异步 + 超时 + 输出上限 +
> 结构化错误码 + 局部护栏 + 审计）。**本节下面的结论一字未改，仍然是硬约束** ——
> 仍然不经 shell、`command` 仍然是可执行文件名。变的只是「怎么把这条约定实现好」。

> ⚠️ **曾一度改成「默认套 shell」**：新增 `shell` 参数（`auto`/`none`/`powershell`/`cmd`/`sh`）、
> `build_process()` / `shell_wrapped()`、PowerShell UTF-8 前缀、`host_shell()` 复用 core 探测。
> 这一版**已全部退回**。为什么退，见 §3.1 —— 那是本文最有价值的一段。
>
> 唯一保留下来的是工具 **description**（见 §2.4）：代码语义一字未动。

### 2.3 `crates/planned-agent/src/flexible/prompt.rs`（环境段）

最终形态 —— **只陈述宿主事实**：

```
## 运行环境（事实，直接采用，不要试探确认）
- 操作系统：windows (x86_64)
- 路径分隔符：\，行尾：CRLF
- 执行命令的 shell：powershell
- 本机可用命令（已探测 node/python/python3/php/go/cargo/rustc）：node v24.15.0、cargo 1.95.0 (…)、rustc 1.95.0 (…)
- 命令输出按 UTF-8 解码：本机 shell 默认按控制台代码页输出（中文为 GBK），中文可能变乱码 —— 需要时在那条命令里先切码（chcp 65001 / [Console]::OutputEncoding=[Text.Encoding]::UTF8）。
```

三处决定性变化：

| 变化 | 理由 |
|---|---|
| **删除「不可用（不要尝试）」行** | 可用项本身就是完整信息；把「探测范围」写进标题即可。代价是模型可能仍试一次 `go`，但那只是一次失败的调用，比一段常驻 prompt 便宜 |
| **删除「执行命令」整行** | 那是**工具契约**，不该占常驻 prompt；且它当时与 tool 的实现**矛盾**，在主动误导模型 |
| **新增编码风险行** | 只在**真的可能乱码**时出现：`pwsh`（7+）与 Unix shell 默认 UTF-8，不提醒；`powershell`（5.1）与 `cmd` 按控制台代码页写 stdout，才提醒 |

另外：

- `render_available()` 剥掉版本里与命令名重复的前缀（修 §1.4）。
- 不再渲染 `console_encoding`（该字段恒为 `None`）。
- 隐私约定不变：**不写** `working_dir` / `probed_at`。

### 2.4 工具 description（执行契约的新家）

```
执行系统命令并返回输出（内置工具）。**不经过 shell**：command 是可执行文件名、
args 是参数数组，二者分别传给系统。管道、重定向与 shell 内建命令不可直接用 ——
需要用它们时，把 shell 自己当可执行文件调用（本机 shell 名填在 command、脚本放 args；
本机是哪个 shell 见运行环境段）。命令输出按 UTF-8 解码，非 UTF-8 输出会损坏字符。
```

放这里的两个理由：① **归属正确** —— 「怎么调用这个工具」是工具契约；② **成本更低** ——
description 只在该工具进入上下文时才加载，环境段是每次执行都常驻。

> ⚠️ **2026-09-28 更正：本节与 §2.5 描述的两处改动当时并未落到代码** ——
> `system_tools.rs:19` 的 description 仍是旧文案，全仓 grep「不经过 shell」「只能是可执行文件名」
> **零命中**，且该文件在 git 里无任何改动。真正实装发生在 `system-tools-redesign.md` 的重设计里：
> description 在本节文案基础上补了 `timeout_ms` 与 `exit_code` 语义，空白检查补了「真实路径放行」的兜底。

### 2.5 `command` 只能是可执行文件名（实测踩过）

`command` 会被直接交给 `CreateProcess` / `execve` 当**文件名**用，所以它不能是命令行字符串。
第一版 description 却写了半句反话 —— 「（可执行文件名）……需要管道 / 重定向 / 内建命令时，
把整条 shell 调用写在这里（如 `cmd /c "..."`）」。模型照后半句做：

```
round=1  command = "powershell -Command \"if (Test-Path 'C:\\...\\text.txt') {...}\""
         → 失败：系统找不到指定的路径。 (os error 3)
round=2  command = "powershell -Command \"Test-Path 'C:\\...\\text.txt'\""
         → 失败：文件名、目录名或卷标语法不正确。 (os error 123)
```

它在找一个叫 `powershell -Command "…"` 的可执行文件 —— 必然失败，而 `os error 3` 信息量太低，
模型只能靠试，白烧两轮（占该步 6 轮里的 2 轮）。同一段日志里还有第二处：模型把 shell 名
写了两遍（`command="powershell"` + `args=["powershell", "-NoProfile", …]`），实际执行成
`powershell powershell …` —— 多套一层，碰巧成功。

**修法（不是让 tool 去猜）**：

1. description 写死语义：`command` 是**可执行文件名、不是命令行字符串**；要套 shell 就把 shell 名填在
   `command`、脚本放 `args`。
2. `command` 含空白时直接返回**指导性错误**（`只能是可执行文件名…要套 shell 请拆开传：
   command=<本机 shell 名>、args=["<脚本>"]（本机是哪个 shell 见运行环境段）`），替代裸的 `os error 3`。
   单 token 不触发这个判定，正常调用不受影响。

> 这一类错误与 §3.1 同源：**信息要放在调用方能看见的地方**。区别是这次修的不是「谁做决策」，
> 而是「报错够不够教人」—— 工具仍不做任何平台决策，只是把失败原因讲清楚。

**连举例也不要塞平台专属值。** 第一版 description 写的是
`（command="powershell" / "cmd" / "sh"，脚本放 args）` —— 列举具体 shell 名有三个问题：
① **永远补不全**（`bash` / `zsh` / `pwsh` / `fish` …）；② **暗示这三个就是对的** —— 模型可能写
`command="sh"`，而本机只有 `bash`，照样失败；③ 把「本机是哪个 shell」这个**运行时事实**
塞回了**静态契约**，而环境段里已经有一行 `- 执行命令的 shell：…`。

改成「本机 shell 名填在 command、脚本放 args；本机是哪个 shell 见运行环境段」——
契约只说**怎么填**，具体填什么由环境段给。同理，`command` 参数里的例子也换成跨平台的
`git` / `node` / `python`，反例从 `"powershell -Command ..."` 换成 `"git status"`。

> 这条能推广：**凡是「每台机器不一样」的值，都不该出现在静态 schema 的举例里**。
> 举正例要用跨平台的，给反例要中性 —— 一旦举例里出现平台专属名字，它就成了半个契约。

---

## 3. 取舍与理由

### 3.1 走错的那条路：tool 不该替调用方做平台决策

曾经的「`auto` 默认套 shell」看着方便，实际是三处硬伤：

**① 同一个调用，行为随机器而变。**

```
{"command": "ls"}    Windows：经 PowerShell 跑 → ls 是 Get-ChildItem 的别名，行为不明
{"command": "dir"}   Linux：经 bash 跑 → dir 不存在 → 失败
```

同一份 schema、同一个输入，结果取决于「这台机器装了哪个 shell」—— 接口不可预期。
而且「`auto` 到底做了什么」在 schema 里查不到，要读代码 + 读环境段才知道。

**② 它篡改了调用方的命令。**

调用方写 `Get-Date`，tool 实际执行 `[Console]::OutputEncoding=…; Get-Date`。
tool 的最基本契约是「执行调用方给的命令」，偷偷往里塞东西是越界，且副作用不可见。

**③ 它把「平台差异」这个信息放错了层。**

| 信息 | 性质 | 正确载体 |
|---|---|---|
| 「经不经 shell」 | 对所有机器一样 | **工具 schema**（静态契约） |
| 「本机是哪个 shell」 | 每台机器不同 | **环境段**（运行时事实） |

把第二类塞进第一类，schema 里就必然出现 `powershell` / `pwsh` / `cmd` 这些平台专属值 ——
**而且照样不准**：Linux 机器可能没有 `bash`，Windows 机器可能没有 `pwsh`。静态 enum 永远追不上运行时事实。

> **结论（本仓库的硬约定）：tool 只执行，不做平台决策。**
> 平台差异属于「调用方该知道的事实」，走环境段（信息 → LLM 决策）；
> tool 的契约必须对所有调用一致、对所有平台一致。
>
> 例外：**进程创建方式**（如 `CREATE_NO_WINDOW`）不参与任何「用什么命令、怎么解释命令」的判断，
> 对所有调用一致生效、不改变语义 —— 它属于修 bug，不属于「迎合」。core 探测层保留了它。

### 3.2 为什么 shell 按优先级探测，而不是读 `COMSPEC`？

`COMSPEC` 答的是「系统默认命令解释器」（中文 Windows 恒为 `cmd.exe`），不是「该用什么」。
用户与工具链实际用 PowerShell，环境段照 `COMSPEC` 写会误导。

### 3.3 为什么编码走「陈述风险 + 给出切码写法」，而不强制 UTF-8？

强制 UTF-8 要靠**改写调用方的命令**（前置 `chcp` / `[Console]::OutputEncoding`）—— 见 §3.1 ②，越界。
而精确探测代码页要 Win32 API（`GetConsoleOutputCP`）或跑一次 `chcp`，代价高且探测层刚修好不想再 spawn。
所以：**工具声明契约**（「输出按 UTF-8 解码」）、**环境段陈述风险**（哪家的 shell 会捣乱）、
**调用方自己切码**（它知道自己在输出什么）。分界不是「有没有 PowerShell」——
PowerShell **5.1** 一样按代码页写 stdout，真正的分界是 `pwsh`(7+) 与 `powershell`(5.1)/`cmd`。

---

## 4. 验证

| 命令 | 结果 |
|---|---|
| `cargo test -p planned-agent-core --lib host` | **9 passed** |
| `cargo test -p planned-agent-tool-manager --lib` | **14 passed**（2026-09-28 重设计后 → **38 passed**） |
| `cargo test -p planned-agent --lib` | **148 passed**（3 个既有 `planner::coarse::llm_planner` 失败，prompt 目录漂移，与本轮无关） |
| `cargo test -p planned-agent-gui --bins` | **63 passed** |

关键回归断言：

- `probe_real_command_reports_available_with_version` —— 「探测到可用命令」这条路径此前**从未被测过**
  （只有 missing 分支有测试），断言 `cargo` 必然可用（与环境无关：测试本身就是 cargo 跑起来的）。
- `preferred_shell_prefers_powershell_over_comspec_cmd` —— 有 PowerShell 时不该报 `cmd`。
- `render_available_strips_duplicated_command_name_from_version` —— 锁 §1.4 的重复渲染。
- `encoding_hint_only_for_code_page_shells` —— 只对 `powershell` / `cmd` 提醒编码。
- `step_prompt_appends_environment_facts` —— 环境段含探测范围与编码行、**不含**缺失命令清单、
  **不含** `builtin_execute_command`（执行契约不占常驻 prompt）。

---

## 5. 后续（未做）

- **UI 刷新入口**：`EnvironmentContext::refresh()` 仍无调用点（见 `gui-host-environment.md` §5 P4）。
- ~~**`builtin_execute_command` 的黑框**：core 探测层已加 `CREATE_NO_WINDOW`，**执行层没加** ——
  每执行一条命令仍会闪一个控制台窗口。~~ **已做（2026-09-28）**：执行层已加，见 `system-tools-redesign.md`。
- ~~**`execute_command` 仍是同步 `std::process::Command`**：在 async 上下文里会阻塞 executor。~~
  **已做（2026-09-28）**：改为 `tokio::process::Command` + `timeout_ms` + `kill_on_drop`，
  顺带补上了原本缺失的超时。
