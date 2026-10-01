# 渐进式工具发现（progressive tool discovery）设计稿

> 目标读者：后续实现者。**本文是方案，不是现状描述**——现状部分的事实都带 `file:line`，
> 可以直接核对；方案部分标了「待拍板」的点，需要先定下来再写代码。
>
> 相关既有文档：`docs/planned-agent/flexible-executor.md`（工具表如何进执行器）、
> `docs/planned-agent/system-tools-redesign.md`（内置工具契约）、
> `docs/planned-agent/flexible-output-step.md`（按名精确取单工具的既有先例）。

---

## 0. 一句话

把「一次性把 40 个内置 + 全部 MCP 工具 + 全部子 agent 工具的完整 schema 塞进每一次
LLM 请求」，改成「常驻一小撮必需工具 + 一个搜索元工具，模型按需把工具**发现**进来，
下一轮起它们才出现在请求里」。

---

## 1. 为什么做：现状与诊断

### 1.1 事实：默认是全量注入

| 链路 | 构造点 | 默认注入什么 |
|---|---|---|
| chat 主 agent / 子 agent 循环 | `crates/planned-agent/src/chat/driver/round/mod.rs:344` → `:367` | `allowed_tools=None` 时**全部 enabled 工具** |
| flexible 执行步 | `crates/planned-agent/src/flexible/exec/executor/mod.rs:102` → `step/mod.rs:138` | 同上（`ExecutorConfig.allowed_tools=None`） |
| flexible 输出整理步 | `executor/mod.rs:429` → `executor/tools.rs:8` | 只按名取 `builtin_read_file_lines` 一个 |
| ReAct 探路 | `crates/planned-agent/src/planner/react/tool_executor.rs:34-43` | 有分类时取该类；无分类时 `get_all_tools()` |
| coarse 规划 / replanner | `planner/coarse/llm_planner.rs:170,:260` | `tools: None`（不带工具） |

`allowed_tools=None` 的分支：`chat/tools/mod.rs:26`、`executor/mod.rs:336`、
`tool_executor.rs:38`。

### 1.2 工具规模

- 内置 **39 个** + `request_user_action` **1 个** custom = **40**
  （filesystem 23 有测试断言 `builtin/filesystem/tests.rs:805`；system 7；data 4；text 2；
  ai 1；web 1；doc 1；custom 1 —— 注册在 `crates/agent-gui/src/context/tools/mod.rs:80-93`）
- 再加上**运行时的全部 MCP 工具**（`tool-manager/src/core/registry.rs:89-125` 注入全量同步）
  和**全部已注册子 agent 工具**（flexible 的 5 个 `flexible_*`）。

量级估算（**未实测，阶段 0 用日志实测**）：单个工具定义常在 100–500 字符，
40 个内置约 6–20 KB ≈ 2–6k tokens，**每一次请求、每一轮都付**；MCP server 越多越贵。

### 1.3 诊断

1. **成本与请求数成正比的固定税**：`max_tool_rounds` 默认 10（`chat/service/config.rs:91`），
   一轮对话最多 10 次请求，每次都在重复发送同一张工具表。
2. **无观测**：全仓没有任何「本次请求带了多少个工具」的日志，也没有 tool 数量断言
   （唯一的数量断言是 filesystem 的 23）。
3. **选择机制过粗**：现有手段只有 `allowed_tools` 三态 token（静态、写死），
   解决不了「事先不知道这轮要用哪些工具」的场景。
4. **不是「工具不好用」，是「工具的说明书全塞在门口」**：仓库自己也认这一点——
   `flexible/exec/prompt.rs:36` 明确写「『怎么调用某个工具』属于工具契约（**按需加载**，不占常驻 prompt）」。
   本方案就是把这句话从「对 system prompt 成立」推进到「对 tools 数组也成立」。

---

## 2. 已有积木（别重造）

| 能力 | 位置 | 本方案怎么用 |
|---|---|---|
| 三态工具选择 | `chat/tools/mod.rs:52` `select_tools_by_tokens`（`"all"` / 分类名 / 精确名，5 个单测） | 作为「可见域」的第一道过滤，**逻辑不动** |
| 工具搜索 | `tool-manager/src/core/registry.rs:533` `search_tools`（名称/描述/标签子串匹配，只返回 enabled） | 直接作为搜索后端，阶段 1 先不增强 |
| 分类 / 来源 | `core/src/tool_registry/types.rs:5,:18` `ToolSource` / `ToolCategory` | 搜索结果的元信息、按来源折叠 MCP |
| 启停开关 | `registry.rs:765` `set_tool_enabled` + `ToolMetadata.enabled`（`core/types.rs:38`） | 「disabled 的工具不该被搜出来」，search 已保证 |
| 按名精确取工具 | `flexible/exec/executor/tools.rs:8` `tool_definitions_for_names` | 「已发现集 → 工具定义」的现成写法 |
| 按需加载知识的内置工具 | `tool-manager/src/builtin/doc_tools.rs` `builtin_read_documentation` | **范式参考**（`filesystem-tools-rewrite.md:239` 称之为「现成模式的直接复用」） |
| 每轮重建 tools 的能力 | `round/mod.rs:344` 已在 `loop{}` 内 | chat 侧几乎零结构改动（见 §5.1） |

---

## 3. 方案总览：三层，逐层可选

```
L0 静态裁剪（已存在，零成本）
   allowed_tools = Some([...])            ← 已知场景首选，先做这个
        ↓ 未覆盖的「事先不知道用哪些工具」
L1 渐进式发现（本方案核心，形态 A）
   常驻 = pinned ∪ 元工具；其余按 discovered 集合逐轮展开
   tool_search(query) → 写入 discovered → 下一轮起该工具有完整定义
        ↓ 需要「同一轮内就能调用」或「连 schema 都不想给」
L2 代理调用（形态 B，默认关）
   tool_invoke(name, arguments) → 转发给 registry.call_tool
```

**关键洞见（决定了形态 A 之所以便宜）**：chat 与 flexible 的 `tools` 数组**都是每轮重建**的
（`round/mod.rs:344` 在循环里；flexible 需要改一下，见 §5.2）。所以「发现」不需要
`describe_tool` 也不需要代理调用——**模型调用 `tool_search` 后的下一轮，工具定义自动出现**。
L1 只需要一个元工具。

**另一条重要判断**：L0 是**正确性优先于巧妙**的选择。如果某个 agent 的工具集是已知的
（GUI 里 flexible 的 5 个子 agent 都写死了白名单，见 `agent-gui/src/pages/plan/page.rs:157,195,236,281,325`），
那它根本不需要 L1。L1 的适用面是 **`allowed_tools=None` / `["all"]` 的「业务执行型」agent**
（如 `agent-gui/src/services/run_service.rs:58-70` 的 flexible 执行），这是收益最大的点。

---

## 4. 契约设计

### 4.1 配置

`ChatConfig`（`chat/service/config.rs:63` 之后）与 `ExecutorConfig`
（`flexible/exec/executor/config.rs:36` 之后）**各加一组同名字段**（两处语义必须一致，
沿用 `allowed_tools` 的先例——那两处就是这么对齐的）：

```rust
/// 工具发现模式。
pub enum ToolDiscovery {
    /// 关闭：完全保持现有行为（默认）。
    Off,
    /// 形态 A：注入 `tool_search` 元工具，发现结果下一轮生效。
    Search,
    /// 形态 B：A + `tool_invoke` 代理（同轮可用）。
    SearchAndInvoke,
}

pub tool_discovery: ToolDiscovery,     // Default::default() == Off
pub pinned_tools: Vec<String>,         // 常驻工具名（不进发现的工具）
pub tool_search_limit: usize,          // 单次搜索结果条数上限（默认 10）
```

**与 `allowed_tools` 的组合语义（必须写死，否则会自锁）**：

1. `allowed_tools` 决定 **可见域**（现有语义，一字不改）。
2. `tool_discovery != Off` 时，**元工具（`tool_search` / `tool_invoke`）无条件注入**，
   不参与 `allowed_tools` 过滤。
   ⚠️ 否则 GUI 里那些 `allowed_tools = Some([...])` 的 agent 一开 discovery 就**没有任何
   工具可用**（元工具被白名单滤掉，功能自锁）。
3. **常驻集 = `pinned_tools` ∪ 元工具**，其余可见域内工具**默认不注入**，只能被发现进来。
4. 发现只能拿到**可见域内**的工具：`tool_search` 的搜索范围也只在可见域内，
   防止越权绕过白名单。
5. `ToolDiscovery::Off` 时上述 2/3/4 全部不生效 —— 严格向后兼容。

### 4.2 元工具契约

`tool_search`（合成工具，不进 `ToolRegistry`，见 §5.3 待拍板）：

```jsonc
{
  "name": "tool_search",
  "description": "按意图检索可用工具。返回匹配的工具名 + 一句话用途（不含参数 schema）。\
                  对返回的工具直接调用即可——它们在下一轮会出现在你的工具列表里。\
                  不确定该用哪个工具时，先用本工具检索，不要瞎猜工具名。",
  "parameters": {
    "type": "object",
    "properties": {
      "query": { "type": "string", "description": "能力描述或关键词，中英文均可" },
      "category": { "type": "string", "description": "可选：限定工具分类" },
      "limit": { "type": "integer", "description": "返回条数上限，默认 10" }
    },
    "required": ["query"]
  }
}
```

返回体（**text，不是 JSON 包裹**，与仓库其它工具一致）：

```
已匹配 3 个工具（已在你的工具列表中生效，可直接调用）：
- builtin_read_file_lines（File）：按行范围读取文本文件
- builtin_search_content（File）：在文件内容中正则搜索
...
若都不合适，可换关键词再搜；当前可用分类：File / Data / Text / Web / System / Ai / Browser / Utility
```

**空命中必须给分类概览**（否则模型会陷入「搜不到 → 换个词 → 再搜不到」的循环，
每搜一次烧一轮）。这一条是「发现不到」这一类静默失败的兜底。

`tool_invoke`（仅 `SearchAndInvoke`）：

```jsonc
{ "name": "tool_invoke",
  "parameters": { "properties": {
      "name": { "type": "string" },
      "arguments": { "type": "string", "description": "该工具参数的 JSON 字符串" } },
    "required": ["name", "arguments"] } }
```

**硬约束**：
- `name` 必须落在可见域内且 enabled，否则返回错误（不静默）。
- `name ∈ UI_TOOL_NAMES`（`chat/tools/mod.rs:95`，当前仅 `request_user_action`）
  **一律拒绝**，并在返回里说明「交互类工具必须直接调用」。UI 工具走的是
  `execute_tool_batch` 的 UI 通道（`round/mod.rs:248-295`），从代理调用会绕过前端交互卡，
  是个会卡死会话的坑（正确做法先例见 `round/mod.rs:279-285` 对无效 UI 调用的处理）。
- `arguments` 解析失败 → 返回错误文本让模型重试，不 panic。

### 4.3 提示词

`chat/driver/prompt.rs:10` `inject_system_prompt` 在 `discovery != Off` 时，
在 system prompt **尾部**追加一段（与 `flexible/exec/prompt.rs:38` 追加环境段同一手法）：

```
## 工具使用
你的工具列表只包含常驻工具。更多工具需先用 `tool_search` 检索，
检索结果会在下一轮出现在你的工具列表里，届时直接调用即可。
不确定该用哪个工具时，先 search，不要凭记忆猜工具名。
```

**必须放在尾部且文本在一次会话内恒定**——理由同 `flexible/exec/prompt.rs:29-36`：
尾部追加 + 内容稳定 = 可命中的 provider 前缀缓存。

---

## 5. 接缝清单（改哪里）

### 5.1 新模块 `planned-agent/src/tool_discovery/`（收口）

chat 与 flexible 都要做同样的三件事（选工具 / 合成元工具 / 拦截本地调用），
**抽成一个 crate 内模块，两处只接线**，避免第二套规则（沿用
`executor/mod.rs:332` 注释里「规则单一来源，不做第二套」的既有原则）。

```rust
// tools 选择：可见域 → 常驻 ∪ 已发现
pub(crate) fn select_exposed(
    enabled: Vec<(Tool, Vec<ToolCategory>)>,
    allowed_tokens: Option<&[String]>,
    cfg: &ToolDiscoveryConfig,
    discovered: &HashSet<String>,
) -> Vec<Tool>

// 合成元工具定义（Off 时返回空）
pub(crate) fn synthetic_definitions(cfg: &ToolDiscoveryConfig) -> Vec<ToolDefinition>

// 本地工具尝试执行：命中元工具 → 就地处理并返回 Some(文本)；否则 None（调用方走原路径）
pub(crate) async fn try_local_call(
    cfg: &ToolDiscoveryConfig,
    registry: &ToolRegistry,
    discovered: &Mutex<HashSet<String>>,
    name: &str,
    args: &Value,
) -> Option<Result<String>>
```

模块内自带单测（选择组合 / 空命中 / 越权拒绝）。**不放 core**（core 无工具注册表实现，
且 `tool-manager` 不认识 `planned-agent`，见 `crates/core/AGENTS.md` 的分层约束）。

### 5.2 调用点

**chat 侧（改动最小）**

| 位置 | 改法 |
|---|---|
| `chat/state/state.rs:42-65` `State` | 加 `discovered: Mutex<HashSet<String>>` + `discover()` / `discovered()`；`ChatService::start()` 时重置（子 agent 每次 `start()` 新建服务，天然隔离，见 `config.rs:71-73`） |
| `chat/tools/mod.rs:15-42` `build_tool_definitions` | 改成 `select_exposed(...)` + `synthetic_definitions(...)` 拼接；`select_tools_by_tokens` **保留不改**（被新函数内部复用） |
| `chat/driver/round/mod.rs:344` | **不用改结构**（本来就在循环里）——只加日志 |
| `chat/driver/round/handlers.rs` `execute_backend_tool_call` | 开头插 `try_local_call(...)`，命中就地执行并 `push_tool`；未命中走原 bridge |
| `chat/driver/prompt.rs:10` | 追加 §4.3 提示段 |

**flexible 侧（需要把 tools 从「执行级一次」改成「每轮」）**

| 位置 | 改法 |
|---|---|
| `flexible/exec/executor/mod.rs:333-340` `tool_definitions()` | 接受 `&HashSet<String>`（发现集），逻辑同 chat |
| `executor/mod.rs:102` | `let tools = self.tool_definitions();` 从执行级一次 → 移进 `run_step` 的轮循环 |
| `flexible/exec/step/mod.rs:34-47` `StepInput.tools: &'a [ToolDefinition]` | 换成 `&'a ToolSet`（新结构：持 `&ToolRegistry` + config + `Arc<Mutex<HashSet>>`，暴露 `definitions()`）——这样 `step/mod.rs:135-143` 每轮构造 request 时能拿到最新集合，不必给 `StepInput` 加可变借用 |
| `flexible/exec/step/mod.rs` 工具执行处 | 与 chat handler 同构地调 `try_local_call(...)` |
| `flexible/exec/executor/config.rs:36` 之后 | 加 §4.1 的三个字段（默认 Off，行为不变） |

⚠️ `StepInput` 是 `pub(crate)`（`step/mod.rs:34`），改它的字段会牵动
`flexible/exec/step/tests.rs` 里 8 处 `tools: &[]` 字面量 —— 都是机械改动，但要在同一个 commit 里做完。

**ReAct 侧（阶段 3，可选）**：`planner/react/tool_executor.rs:34-43` `resolve_tools` 是同一形状的
「选工具」函数，可接入同一套 `select_exposed`；但 ReAct 的工具数本来就受
`step.recommended_tool_categories` 约束，收益低于 chat/flexible，**建议放到最后**。

### 5.3 元工具的实现位置（待拍板）

两个选项，取舍真实存在：

- **A. 放 `tool-manager` 做内置 provider**（如 `builtin/discovery_tools.rs`）：
  与「内置工具归 tool-manager」的既有约定一致（`workspace/AGENTS.md` §2），
  GUI 设置页能看见、MCP 侧也能复用。代价：provider 需要访问 `ToolRegistry` 自身，
  得持 `Weak<ToolRegistry>`（`Arc::downgrade`）避免 `Arc` 环，且要处理「注册顺序 / registry 未注入」的时序。
- **B. 在 `planned-agent` 合成（不进注册表）**：无 `Arc` 环、无时序问题、不被设置页与
  MCP 同步污染，`try_local_call` 就地处理。代价：`registry.call_tool("tool_search")` 会失败
  （注册表不认识它），也就是「有两条工具通路」，需要在文档里写清。

**倾向 B**，理由是 §3 的洞见——发现不需要注册表参与，只要每轮的 `tools` 数组能变。
但这条必须用户拍板（见 §11）。

---

## 6. 观测与测试

### 6.1 观测（阶段 0 先做，零行为变化）

- chat：`round/mod.rs:344` 之后加
  `info!(round, tools = %tools.len(), pinned, discovered, domain, "本轮工具表")`
- flexible：每步开始处打同构一行
- 效果：把 §1.2 的「量级估算」变成实测数字，也是本方案收益的唯一量化依据

### 6.2 测试

| 层 | 用例 |
|---|---|
| 单元（`tool_discovery/`） | `Off` 时与现状等价；`Search` 时首轮只含常驻 + 元工具；已发现工具后续轮出现；`allowed_tools` 与 discovery 组合；元工具不被白名单滤掉；搜索范围不越出可见域；空命中返回分类概览；`tool_invoke` 拒绝 UI 工具 / 域外工具 / disabled 工具 |
| chat 集成（`chat/tests.rs`） | 桩 AI：第 1 轮返回 `tool_search` 调用，断言第 2 轮 `request.tools` 含新工具（该文件已有 mock 模式与 41KB 用例规模，可直接加） |
| flexible 集成（`flexible/exec/executor/tests.rs`） | 同构一条；该文件已有请求捕获能力（先例 `tests.rs:104` 断言 `requests[2].tools.is_none()`） |
| 回归 | `cargo test -p planned-agent --lib`、`cargo test -p planned-agent-tool-manager --lib`、`cargo test -p planned-agent-gui --bins` |

**基线警告（不要顺手改）**：
- `cargo test -p planned-agent` 有 **3 个既有的** `planner::coarse::llm_planner` 失败（prompt 目录漂移）；
- `cargo test -p planned-agent-tool-manager`（不带 `--lib`）会**挂住**在
  `tests/sub_agent_stream.rs`；`cargo test --workspace` 因此不可用。
  两件事都与本方案无关，除非任务就是修它们。

---

## 7. 分期落地（每期独立可交付、独立可测）

| 阶段 | 内容 | 交付判据 |
|---|---|---|
| **0** | 配置字段（`Off`）+ 每轮工具数日志 | 零行为变化；日志出现实测工具数与估算 token 量级 |
| **1** | `tool_discovery` 模块 + `tool_search` + chat 接入 + 提示段 | chat 侧首轮工具数下降；集成测试绿 |
| **2** | flexible 接入（`StepInput` 改 `ToolSet`，每轮构建） | 同上；`flexible/exec` 测试绿 |
| **3** | `SearchAndInvoke`（`tool_invoke`）+ 可选 react 接入 | 同轮调用用例绿 |
| **4**（可选） | GUI 开关（settings 工具页）+ 发现事件 → 前端「本轮加载 N 个工具」+ 配置持久化 | GUI 手测 |

GUI 的接线点：`agent-gui/src/services/run_service.rs:58-70`（flexible 业务执行，当前
`ExecutorConfig::default()` → 全量）与 `agent-gui/src/pages/plan/flexible/chat_service_factory.rs:65-73`。

---

## 8. 风险与取舍

1. **前缀缓存会受影响（要如实承认）**。`tools` 数组参与请求前段，发现导致它变化 →
   该轮前缀缓存命中率下降。缓解：**发现集只增不减**、工具顺序按 enabled 遍历顺序稳定输出、
   提示段文本恒定。
   > 顺带发现（**与工期无关，但值得记一笔**）：`registry.rs:429` / `:487` 用
   > `HashMap::values()` 迭代工具，`chat/tools/mod.rs:30` 直接 `map` 输出 ——
   > 同一进程内不增删工具时顺序稳定，但 MCP 同步 / 启停会改变它，
   > 也就是**同一份工具集在不同次执行里顺序可能不同**。这不是本方案引入的，
   > 但它会放大本方案的缓存抖动，建议在阶段 1 顺手把输出按名字排序（小改，独立可测）。
2. **「发现不到」是静默失败**。模型不知道自己不知道什么。兜底三件套：
   空命中返回分类概览、`pinned_tools` 常驻关键工具、提示词明写「不确定先 search」。
3. **一次探索 ≈ 一轮 LLM 请求**。`max_tool_rounds` 默认 10，复杂任务下探索会挤占轮次预算。
   建议阶段 1 的默认 `tool_search_limit=10`，并在 §4.3 提示里要求「一次搜索多关键词，不要逐词试」。
4. **子 agent 的发现不共享**。子 agent 每次 `start()` 新建 `ChatService`（`chat/sub_agent/runner.rs:133`），
   `discovered` 天然隔离 —— 隔离是安全的，但父 agent 已发现的结果子 agent 要重新发现（多花一轮）。
   后续可选：把父的 `discovered` 作为 `ChatConfig` 的种子传入。**本期不做。**
5. **与静态裁剪的关系**：能静态裁剪的一律静态裁剪（零成本、零风险），
   discovery 只用于「事先不知道」的场景。**不要用 discovery 替代 `allowed_tools`。**

---

## 9. 非目标

- 不做 MCP 工具的 **schema 懒拉** —— schema 已随工具缓存一起同步到内存
  （`mcp-rmcp/src/config.rs:105-115` 缓存 name + description + input_schema），
  没有网络成本，懒加载只省 token，收益已被 L1 覆盖。
- 不做 MCP 工具的**懒连接** —— 已有（`mcp-rmcp/src/manager/routing.rs:51` 「懒连接」）。
- 不改 `select_tools_by_tokens` 的既有语义与它的 5 个测试。
- 不改 `builtin_read_documentation`（`doc_tools.rs`）：那是「按需加载**知识**」，
  与「按需加载**工具**」是两件事，不要合并。

---

## 10. 可选的后续形态（本期不做，记下思路）

- **按 MCP server 折叠**：把一台 server 的几十个工具折叠成 `mcp_<server>_search` 一个入口，
  对「装了很多 MCP server」的用户收益最大。等价于把 L1 的粒度从「工具」换成「server」。
- **search 后端增强**：目前 `search_tools`（`registry.rs:533`）是纯子串匹配，
  中文/近义/多词效果一般。增强方向：分词 + 分类/标签加权 + 同义词表。
  **建议先量测再增强**，别一上来就上向量（`crates/rag` 在，但这是过度设计）。
- **`tool_search` 走 RAG**：`crates/rag` 可做语义检索。同上，先不。

---

## 11. 待拍板（写代码前必须定）

| # | 问题 | 选项 |
|---|---|---|
| Q1 | 做哪个形态 | A 只做 `tool_search`（L1） / B L1 + `tool_invoke` 代理（L2） / C 先只做 L0 静态裁剪增强 + 阶段 0 观测 |
| Q2 | 落地范围与顺序 | chat 先行（阶段 0→1） / flexible 先行（阶段 0→2） / 两者同批 |
| Q3 | 元工具实现位置 | B. `planned-agent` 合成 + handler 拦截（倾向） / A. `tool-manager` 内置 provider + `Weak<ToolRegistry>` |
| Q4 | 默认值 | 保持 `Off`（推荐，显式开启） / 让 flexible 业务执行默认开 `Search` |
