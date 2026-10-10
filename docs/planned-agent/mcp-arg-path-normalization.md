# MCP 工具参数路径规范化（产出目录）

> 状态：**已实施（2026-10-08）** —— 落地形态与本文草案有差异，见文末 **§9 实际落地形态**
> 关联：`flexible-execution-improvements.md` §5.2.5（产出目录注入链）、`gui-cache-root.md` §3.4/§3.8
> 前置结论：prompt 纪律（写完整路径）已落地；`roots` 上报 / spawn `cwd` 已否决；`_meta.cwd` 仅 playwright 认

## 1. 问题

### 1.1 现象（实测日志）

模型调用浏览器工具时**给的是裸文件名**，产出落到仓库根而不是本次执行的产出目录：

```
工具调用入参 step=3 round=2 tool=browser_take_screenshot
args={"element":"验证码图片","filename":"captcha-1.png","scale":"device","target":"e31"}
```

→ 实际落盘 `D:\code\planned-agent\crates\agent-gui\captcha-1.png`
→ 期望落盘 `<cache_root>/<session_id>/run-<millis>-<seq>/captcha-1.png`

### 1.2 为什么 prompt 纪律治不住（关键）

我们已经在 `STEP_SYSTEM_PROMPT` 里写了「落盘产出…**写出完整路径**」，但**模型仍然给裸文件名**。原因不是模型不听话，而是**工具自己的 description 在与我们的 system prompt 竞争**：

> `browser_take_screenshot.filename` 的 description：*"**File name** to save the screenshot to. … If a relative path is provided, it's resolved relative to the **workspace root**"*

模型读到的是 "**File name**"，而 "workspace root" 是个它不知道在哪的东西 —— 于是**合理地**给了一个名字。**调用点就在眼前，工具 description 赢。**

**结论：这是一个"模型行为"问题，只能在宿主侧用代码兜，prompt 只能作为辅助。**

### 1.3 三层根因（完整）

| # | 层 | 事实 |
|---|---|---|
| 1 | 模型行为 | 给裸文件名 / 相对路径，不带产出目录 |
| 2 | 工具语义 | 相对名的解析基准 = MCP server 进程 cwd（= GUI 进程 cwd = `crates\agent-gui`），不是产出目录 |
| 3 | 环境不一致 | `builtin_read_media_file` 沙箱根含 cwd，`builtin_recognize_image` 只有产出目录 → 同一个文件"一个能读一个不能读" |

## 2. 目标与非目标

**目标**

- MCP 工具收到**相对路径 / 裸文件名**时，把它落到**本次执行的产出目录**。
- 对**任何 MCP server 通用** —— 不依赖某个 server 的私有约定（这是否掉 `_meta.cwd` 单一方案的原因）。
- 误判**不致命**：改错了要能被发现、能自动回退。

**非目标**

- **不追求 100% 正确**。这是启发式，边界有三处（见 §7），配套手段是"回灌 + 回退 + 审计"，不是"消灭误判"。
- 不改变 MCP server 的任何行为、不要求 server 配合。
- v1 只覆盖 **flexible 执行**（落点在 `planned-agent`，见 §4.1）；chat / thorough 不在本期。

## 3. 候选方案对比（为什么选"调用期改写"）

播放器 MCP 把**两个东西绑在同一个值上**，这是所有宿主侧方案的共同约束：

```
相对路径的解析基准 = options.cwd = roots[0] ?? process.cwd()
文件白名单(checkFile) = outputDir + options.cwd      ← 同一个 options.cwd
```

| 方案 | 通用（换 server 有效） | 无损（不动白名单） | per-run 隔离 | 结论 |
|---|---|---|---|---|
| 上报 `roots` | ✅ 认 roots 的 | ❌ 白名单收窄 | ❌ | 否决：上传本地文件会失效 |
| spawn `cwd` = 产出根 | ✅ **所有 server** | ❌ 白名单收窄 | ❌ 进程长驻、启动即定 | 否决：run_dir 执行期才有 |
| `_meta.cwd`（playwright 私有） | ❌ 仅 playwright | ✅ | ✅ | 可作**可选并行项**，不作主方案 |
| 每次执行重启 server | ✅ | ✅ | ✅ | 否决：丢浏览器登录态 |
| **调用期改写参数（本方案）** | ✅ | ✅ | ✅ | **选定** |

**唯一约束**：改写是"宿主猜测"，schema 里没有读写方向（§4.3），所以必须配 §4.5/§4.6。

**本方案相对 `_meta.cwd` 的关键优势**：不依赖任何 server 的私有实现；且它是**无共享状态的**（改写用局部变量，不设"当前产出目录"全局字段）→ 天然并发安全（多个 run 在 `FuturesUnordered` 里并发推进时不会互相污染）。

## 4. 方案设计

### 4.1 落点（关键结论：零跨 crate 签名改动）

调研确认：`planned-agent` 持有的 registry 是**具体类型**（`Arc<ToolRegistry>`，不是 trait 对象），且 `input_schema` 与 `run_dir` 在调用点都拿得到 —— **因此整个方案可以全部落在 `planned-agent` 内**，不必给 `core` / `tool-manager` / `mcp-rmcp` 加 per-call 上下文通道。

| 环节 | 位置 | 依据 |
|---|---|---|
| 取工具 schema | `ToolRegistry::get_tool(name) -> Option<Tool>`，读 `Tool.input_schema`（JSON Schema） | `tool-manager/src/core/registry.rs:521`、`core/src/mcp/types.rs:6` |
| 产出目录 | `cfg.cache_dir.join(input.run_dir)` | 与 spill 落点同源：`flexible/exec/spill.rs:79`、`StepInput.run_dir`（`exec/step/mod.rs:52`） |
| **改写点** | `exec/step/mod.rs:375` 之前（`registry.call_tool` 调用前） | 唯一收口，且此处已 `validate_arguments` 之后 |
| **回灌点** | tool 消息渲染处，与 `join_text_and_image_notes` 同族 | `exec/step/mod.rs:403`、`:458-460` |
| **回退点** | 紧包 `registry.call_tool` 调用 | `exec/step/mod.rs:375` |

**为什么不下沉到 `tool-manager`**：`ToolRegistry::call_tool(name, arguments)` 是 GUI 级共享对象、**拿不到 per-run 的产出目录**；要下沉就必须新增跨 crate 的 per-call ctx 通道（`core` trait 变体 + `tool-manager` + `mcp-rmcp` + `planned-agent` 四处），**改动面反而大得多**。v1 就近落地；若将来 chat / thorough 也要，再把这套逻辑上收（§8）。

### 4.2 判定：哪些参数是"路径"

输入：`get_tool(name)?.input_schema`（原始 JSON Schema）。

1. **递归**遍历 `properties`：数组看 `items`、嵌套对象看 `properties`（`browser_file_upload.paths` 是数组、有些工具是 `{output:{path}}` 形态）。
2. **候选判据**（满足其一）：
   - **字段名**命中模式：`filename` / `file_name` / `filepath` / `file_path` / `path` / `file` / `dir` / `directory` / `folder` / `save_path` / `output` / `out`（后缀 `_path` / `_dir` / `_file` 同样命中）
   - **description** 含关键词：`file name` / `path to` / `save to` / `directory` / `文件名` / `路径` / `保存到`
3. **排除表**（防止把非文件路径当路径）：`selector` / `xpath` / `css` / `url` / `uri` / `query` / `expression` / `glob` / `mime_type` / `element` / `ref`。
4. **schema 缺失 / 为空 / `additionalProperties: true`** → **一律不改**（安全降级）。

### 4.3 方向：读还是写（本方案最大的风险点）

JSON Schema **没有读写语义**，而字段名会自相矛盾：

| 工具 | 字段 | 方向 | 补产出目录的后果 |
|---|---|---|---|
| `browser_take_screenshot` | `filename` | **写** | ✅ 正是目标 |
| `browser_run_code_unsafe` | `filename` | **读** | ❌ 找不到脚本 |
| `browser_file_upload` | `paths` | **读** | ❌ 上传不到文件 |

**三级规则（按优先级）**：

1. **读类黑名单**（最高优先，防误伤）——命中则**永不改写**：
   `browser_file_upload.paths`、`browser_run_code_unsafe.filename`、`browser_drop.paths`、`browser_set_storage_state.filename`
2. **写类白名单**——命中则**强制改写**（不看向存在性）：
   `browser_take_screenshot.filename`、`browser_pdf_save.filename`、`browser_evaluate.filename`、`browser_snapshot.filename`、`browser_console_messages.filename`、`browser_network_requests.filename`、`browser_network_request.filename`、`browser_storage_state.filename`、`browser_start_video.filename`
3. **其余** → **存在性优先**启发式：
   - 值解析到**进程 cwd** 后**文件存在** → 视为"读"，**不改**
   - 反之（目标文件通常还不存在）→ 视为"写"，**改**
   - 已知边界：**"写一个已存在的文件"会被误判成读**（后果是落错位置但不报错）→ 靠白名单覆盖主流写类来规避

> 白名单/黑名单是**可配置项**（§4.7），默认内置上表；未收录的工具一律走第 3 级。

### 4.4 改写规则

对判定为"写"的字符串值：

1. **已是绝对路径** → 不改（用户/模型给了完整位置，尊重它）
2. **归一化后已在 `cache_dir` 之下**（如 `data/cache/<session>/run-x/a.png`）→ 不改（防**双重拼接**）
3. 否则 → 改写为 `产出目录.join(归一化后的相对路径)`，并**记录 `原名 → 新名`**

**归一化**：只需要词法处理（去 `.`、消解可消解 `..`）。
⚠️ **不要用 `canonicalize`**（要求路径已存在、Windows 上加 `\\?\` 前缀）—— 与 `gui-cache-root.md` §3.3 的既有结论一致。
实现上消除重复：把 `agent-gui/src/paths.rs` 的 `normalize` 上收到 `planned-agent-util`（该 crate 定位就是"通用、与领域无关"，目前是孤儿 crate），GUI 与 `planned-agent` 共用。

### 4.5 回灌（治"静默"）

**没有这一步，误判就是静默的**（模型不知道参数被改过）。在 tool 消息里追加一行：

```
[宿主] 参数 `filename` 的 `<captcha-1.png>` 已按本次产出目录解析为 `<...\data\cache\<session>\run-...\captcha-1.png>`。
```

沿用既有回灌纪律（照抄 `join_text_and_image_notes`，`exec/step/mod.rs:114-132`）：

- **只在真的发生了改写时**追加（否则每轮多几十 token，长任务累计可观）
- **不点名工具**（写死工具名在工具改名/未注册时是幻觉源）
- **不注入意图**（本步意图已在 system/user 消息里，重复只会稀释 prompt）

### 4.6 失败回退（治"误判致命"）

把改写当成**一次有校验的尝试**，而不是唯一真理：

```
call_tool(改写后参数)
  ├─ 成功                        → 正常返回
  └─ 失败（is_error / Err）
       ├─ 本次没有发生过改写       → 原样返回
       ├─ 错误不像路径问题         → 原样返回
       └─ 像路径问题（not found / ENOENT / no such file / 不存在 / 拒绝 / denied / outside allowed）
            → 用「原始参数」重试一次
                 ├─ 成功 → 返回该结果 + 回灌「已按你给的原值重试成功」
                 └─ 失败 → 返回**第一次**的错误（不再猜）
```

**纪律**：重试**最多一次**（防循环）；**只认路径类错误**（否则工具因别的原因失败也重试，白烧一次调用）。

### 4.7 生效范围、开关与配置

- **只对 `ToolSource::Mcp` 的工具生效**（builtin / custom / sub-agent 不做，避免动内建语义）。
- **不区分 transport**（v1）：远程 server 的路径语义本就不可知，误改后由 §4.6 兜住。若实测有问题，再加"只对 stdio server"的开关。
- **开关**：`off` / `dry_run` / `on`（默认值待拍板 §7）。
  - `dry_run` = 只判定与记录、**不改写** —— 用于先度量误判率再放行。
- **名单可配置**：读/写参数名单 + 排除表允许用户在配置里增删（GUI `GuiConfig`）。

### 4.8 审计

每次判定/改写/回退都记一条 `tracing`（`target: "tool_path_normalization"`）：

```
tool=browser_take_screenshot param=filename dir=write policy=whitelist
  from="captcha-1.png" to="D:\...\run-...\captcha-1.png"
tool=browser_file_upload param=paths dir=read policy=blacklist action=skip
tool=browser_x param=y dir=write policy=existence action=rewrite retried=true retry_ok=false
```

**这是判断"启发式到底靠不靠谱、要不要开改写"的唯一依据**，不能省。

## 5. 改动清单（文件级）

| # | 文件 | 改动 |
|---|---|---|
| 1 | `crates/planned-agent/src/flexible/exec/step/path_args.rs`（**新增**） | 判定 + 方向 + 改写 + 名单常量 + 单测 |
| 2 | `crates/planned-agent/src/flexible/exec/step/mod.rs` | `:375` 前调用改写；`:375` 外包回退重试；`:460` 前把改写说明拼进 tool 消息 |
| 3 | `crates/planned-agent/src/flexible/exec/step/mod.rs`（`mod` 声明） | 挂上 `path_args` 模块 |
| 4 | `crates/planned-agent-util/src/lib.rs` | 上收 `normalize`（自 `agent-gui/src/paths.rs`） |
| 5 | `crates/agent-gui/src/paths.rs` | 改为复用 `planned-agent-util::normalize`（行为不变） |
| 6 | `crates/agent-gui/src/config/**.rs` | 新增开关 + 名单配置项（`off/dry_run/on`，默认待定） |
| 7 | `docs/planned-agent/flexible-execution-improvements.md` §5.2.5 | 补一句：属"模型行为"层，A（prompt）之外的第二道兜底 |

**不改**：`core` / `tool-manager` / `mcp-rmcp` 的任何 trait 与签名。

## 6. 测试计划

**单元（`path_args.rs`）**

- 判定：名字命中 / description 命中 / 排除表命中 / 递归数组 / 递归嵌套对象 / schema 缺失 → 不改
- 方向：黑名单不改 / 白名单强制改 / 存在性（临时目录造一个存在的文件 → 判定为读）
- 改写：绝对路径跳过 / 已在 `cache_dir` 下跳过 / 双重拼接防护 / 归一化（`./`、`../`）

**集成（`exec/step` 层，用 mock/假 registry）**

- 改写后的参数确实传给了 registry（断言入参）
- 回灌文案只在"真改写"时追加，且**不含工具名**
- 回退：第一次返回路径类错误 → 断言用原参数重试了一次；非路径类错误 → 断言**没有**重试

## 7. 风险与未决项

| # | 风险 | 影响 | 缓解 |
|---|---|---|---|
| 1 | **读写方向猜错**（schema 无此语义） | 读类被误改 → 找不到文件 | 黑名单 + 存在性优先 + §4.6 自动回退 |
| 2 | 字段名启发式**漏判**（`target`/`dest`/`artifacts`） | 该改的没改 → 仍落错位置 | 名单可配置；审计日志暴露 |
| 3 | 字段名启发式**误判**（`uri`/`query`/`xpath`） | 不该改的改了 | 排除表 + 回退兜住 |
| 4 | **写一个已存在的文件** → 被存在性判成"读" | 落错位置且不报错 | 写类白名单覆盖主流；其余接受 |
| 5 | 远程 server 的路径语义 | 误改后报错 | §4.6 兜住；必要时加 transport 开关 |
| 6 | 模型看不见改写（弱模型忽略回灌） | 后续引用用错名字 | 回灌 + 工具自返回路径（如 screenshot）双保险 |

**待拍板**

- **Q1 默认开关**：`on`（立刻生效）/ `dry_run`（先观察一段再放行）
- **Q2 名单策略**：接受"内置读写名单 + 存在性优先"三级规则，还是坚持**纯启发式**（不维护名单，误伤面更大）
- **Q3 归一化上收**：是否同意把 `normalize` 上收到 `planned-agent-util`（会让孤儿 crate 首次被依赖），或 v1 先在 `planned-agent` 内复制一份

## 8. 与既有设计的关系

- **与 §5.2.5（产出目录注入链）**：那条把"产出目录"告诉模型（prompt 侧，软）；本条是**同一个目标的硬兜底**（宿主侧）。两者不冲突：模型写全路径 → 规则 1/2 直接跳过；模型给裸名 → 本方案接管。
- **与 `_meta.cwd`**：如果将来要接 playwright 的 `_meta.cwd`，两者**可以并存**（改写把值变绝对路径后，`_meta.cwd` 自然不参与），但 v1 不引入它（避免两套机制同时解释同一个值）。
- **与 `roots` / spawn cwd**：明确否决，理由是"通用 / 无损 / per-run"三者不可兼得（§3 表的推导）——**根因是 MCP 协议没有 per-call 工作目录字段**，只有 server 自己能提供（playwright 用 `_meta.cwd` 提供了它自己那份）。

## 9. 实际落地形态（与本文草案的差异）

**落地（2026-10-08）**：新增 `crates/planned-agent/src/flexible/exec/step/call_arguments.rs`，并在 `exec/step/mod.rs` 三处接入（调用前改写、工具消息回灌、失败回退）。**未改** `core` / `tool-manager` / `mcp-rmcp` 任何签名 —— `planned-agent` 持有的 registry 是具体类型 `Arc<ToolRegistry>`，`get_tool()` 可拿到 `input_schema`，`run_dir`/`cache_dir` 就在调用点作用域内，所以不需要 per-call 上下文通道。

| 本文草案 | 实际落地 | 为什么 |
|---|---|---|
| §4.3 三级规则（读类**黑名单 → 永不改写**） | **不区分读/写**：路径类参数的裸名**一律**补产出目录 | 用户拍板：读类给裸名也说明它指的就是产出目录；补错由回退兜 |
| §4.4 归一化（需把 `normalize` 上收 `planned-agent-util`，见 Q3） | **不需要**：只处理裸名（去 `./` 后不含 `/` `\` `:`），含路径成分一律不碰 → 天然无双重拼接 | 规则收窄后归一化没有用武之地 |
| §4.2 字段名 + description 双判据 | **只用字段名**（精确名 + `_path`/`_file`/`_dir`/`_filename` 后缀） | 确定判断、可预测；description 判据留作以后 |
| §4.5 回灌 | ✅ 已落地，且**分两种措辞**（用了补全参数 / 回退用了原值） | review 发现：不区分会让文案与实际路径矛盾 |
| §4.6 回退 | ✅ 已落地（只重试一次、只认路径类错误） | 因为不排除读类，这是**必需项**而非可选项 |
| §4.7 开关 `off/dry_run/on` + 名单可配置（Q1/Q2） | **未做**：当前无条件生效、无名单 | 待观察后再定 |

**其他落地细节**

- 审计 `tracing` target = `tool_path_normalization`。
- 日志 / 事件 / 报告仍记**模型给的原参数**；只有进 messages 的那一份带回灌说明。
- `looks_like_path_error` 的关键词刻意收紧：**不用** `not found` / `denied` 这类宽词（它们会命中 `Command not found` / `permission denied`），避免把带副作用的工具白跑一次。
- 生效范围：只在 **flexible 执行**（`flex/exec/step`）；`chat` / 周密模式不走这里。

**未做**：§1 的 builtin 沙箱根统一 —— 图片 / 验证码工具仍只认 `flexible_output_dir()`（单根），filesystem 族认 cwd+home+temp（多根），这正是"一个能读一个不能读"的根因。
