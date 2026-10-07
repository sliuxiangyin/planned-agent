# 图片通路 + 内置 AI 识别工具

> 状态：🚧 **阶段 2 已实施**（`builtin_recognize_image` 已落地并测试通过）；**阶段 1（MCP 图片通路）后续再接入**（用户 2026-10-07 决定）
> 相关：[`multimodal-image-input.md`](multimodal-image-input.md)（图片输入已通到 core + ai-openai 两层，GUI 无入口）
> 决策来源：2026-10-07 讨论，三点已拍板（见 §6）
> 目标场景：浏览器 MCP 截取验证码 → AI 读出字符 → 回填表单

---

## 1. 问题：图片在到达模型之前就没了

从「工具产物里有图片」到「模型能读到」，链路上有三段断点：

| # | 位置 | 现状 | 后果 |
|---|---|---|---|
| ① | `crates/mcp-rmcp/src/client.rs:99-119` `convert_tool_result` | 只取 `content.first()`；image 块被换成 `Value::String("[Image]")` | **base64 与 mime 直接被丢弃**，且多块结果只活第一块 |
| ② | `crates/planned-agent/src/flexible/exec/step/render.rs:107` `tool_content` | 工具结果一律拍成 `String` | 就算 ① 保住了，图片也没有载体 |
| ③ | `crates/ai-openai/src/client.rs:457-464` | tool 角色只接受 `ToolResult` / `Text`，其余 `Err("... images are only supported on user messages")` | **OpenAI 协议里 tool 消息本就装不下图** |

补充事实：③ 不是缺陷而是规范 —— 图片只能挂在 **user** 消息上（`client.rs:398-428` 是唯一支持 image 的分支），且 `resolve_local_images`（`client.rs:258-282`）**只扫 user 消息**。

结论：**要让模型看到图，必须有额外一条 user 消息来承载它**（或换一种做法，见 §3）。

---

## 2. 目标 / 非目标

**目标**

1. 图片从 MCP 结果里**活下来**，并且有一个可寻址的落点；
2. 上下文（LLM 的 messages）、日志、事件、报告里**永远不出现 base64** —— 只出现短路径；
3. 提供一个**内置 AI 识别工具**，读路径 → 返回文本，让「验证码识别」成为一次普通工具调用。

**非目标（本次不做）**

- 不做「图片进执行器主模型的上下文」（路线 A，见 §3）—— 那要求主 provider 支持视觉；
- 不做 GUI 的图片展示 / 粘贴 / 拖拽（`Bubble` 仍只存 `text: String`）；
- **不动** `crates/tool-manager/src/builtin/ai_tools.rs` 的 `ai_process`（已拍板：不处理）；
- 不改主流程协调器 prompt 的工具清单（新工具靠注册态自动可见，见 §5.3）。

---

## 3. 总览

```text
浏览器 MCP 截图（返回 MCP image content 块）
   │
   ▼ ① mcp-rmcp：保留结构，不再降级成 "[Image]"
   │    content = [ {type:"text",text}, {type:"image",mime_type,data:<base64>} ]
   ▼ ② flexible/exec/step：把 image 块**落盘**，base64 就地丢弃
   │    <cache_dir>/<run-xxx>/img-s1-r1-0.png
   │    → tool 消息文本 = "文本块 + [图片已保存到 <绝对路径>]"
   ▼ ③ LLM 拿到路径（短），决定调用内置工具
   │    builtin_recognize_image(path="…/img-s1-r1-0.png", instruction="读出验证码字符")
   ▼ ④ 内置工具：路径校验 → **经沙箱句柄读字节** → 嗅探 MIME → `ImageSource::Url(data:)` → AiClient
   │    （读盘必须走 cap-std 句柄；只把路径交给适配层会绕过沙箱，详见 §5.5-4）
   ▼ ⑤ 返回 text："4F7K" → LLM 继续调 browser_fill(...)
```

**关键性质**：图片**不进**执行器主模型的上下文，也不进任何日志；它只在 ④ 那一次夹在工具内部的 LLM 调用里出现。所以：

- 主模型（跑 step 的那个 provider）**不需要支持视觉**，纯文本模型也能跑主流程；
- 「看 UI / 看图表」这类通用视觉需求**本次不解决**（那是路线 A，可后续叠加：同一套 ①② 落盘前置 + flexible 把图片挂进 user 消息）。

**粒度定位**：本方案 = 「粒度 ② 的形态」（图片靠文件系统 + 路径交接）。③④ 属于本设计；①② 是它**不可省略的前置**。

---

## 4. 阶段 1（前置）：把图片从 MCP 带到 LLM 面前

### 4.1 接口契约：`ToolResult.content`

`crates/core/src/mcp/types.rs:13-18` 的 `content: Value` 不变，只补约定：

- **纯文本结果保持 `Value::String` 不变** —— 现有 `tool_content()` 对所有文本工具零影响（向后兼容，这条是硬要求）；
- **含图片的结果**才改成数组：
  ```json
  [ {"type":"text","text":"..."},
    {"type":"image","mime_type":"image/png","data":"<base64>"} ]
  ```
- `is_error` / `call_id` 语义不变。

> 不这么做就会踩新坑：数组被 `tool_content()` 的 `other.to_string()`（`render.rs:107-111`）直接序列化成一坨 JSON 塞进上下文。

### 4.2 `mcp-rmcp` 改动

`crates/mcp-rmcp/src/client.rs::convert_tool_result`：

- 遍历**整个** `result.content`（修掉 `first()`）；
- 保留 image 块的 `data` / `mime_type`；
- resource / 未知块保持现有的占位文本（`[Resource]` / `[Unknown content type]`）；
- 只有图片块存在时才返回数组，否则维持现有单值行为。

**安全红线**：只采信服务端返回的 `data` 与 `mime_type`，**绝不采信它返回的 `path`/`uri` 去读盘**。

### 4.3 `flexible` 改动

新增 `crates/planned-agent/src/flexible/exec/step/image.rs`（按 `AGENTS.md` §5：执行期新行为放小模块）：

- `materialize_images(content, cache_dir, run_dir, tag) -> (text_for_llm, Vec<PathBuf>, warnings)`
  - 遍历数组：`text` 块拼文本；`image` 块 → 解码 base64 → **按 mime 反推扩展名**（只 `png/jpg/jpeg/webp/gif`，与 `client.rs:189` 的 `image_mime_of` 对齐，写错扩展名会让整个请求 Err）→ 落盘；
  - 文件名约定 **`img-s<步>-r<轮次>-<序号>.<ext>`**，与既有 spill 的 `tool-s<step>-r<round>-<nth>.txt` 同构（`exec/spill.rs` 的 run 目录约定）；
  - 返回的文本里只放**绝对路径**（`cache_dir` 可能是相对路径 —— `ExecutorConfig.cache_dir` 相对进程 cwd，见 `AGENTS.md` §7-10）。

`exec/step/mod.rs` 的回灌点（`mod.rs:345-401`）：

- `tool_content(&outcome.result.content)` 现在直接出文本 —— 改为先走 `materialize_images`；
- **落盘失败 = 该步 `Failed`**（与跨步产出落盘一致，`AGENTS.md` §7-7）；图片解析失败可以退化为「告警 + 文本占位」，但要进 warning 通道（不静默）。

### 4.4 硬不变量（must-not-break）

1. **base64 不得进入**：`messages` 的任何 tool 消息文本、`tracing` 日志（尤其 `mod.rs:350-357` 那条 `output = %content` 的 WARN、`exec/executor/logging.rs::log_output`）、`PlanRunEvent`、`PlanRunReport`。落盘后**立即丢弃** base64。
2. **纯文本工具行为逐字不变**（回归基线：`cargo test -p planned-agent --lib flexible::` = 112）。
3. **绝对路径**：落盘后给 LLM 的路径必须是绝对路径（两个 MCP server 的 cwd 可能不同）。
4. 图片文件大小与张数需要上限（复用/新增常量），避免多轮循环里无限落盘。

---

## 5. 阶段 2：内置 AI 识别工具

### 5.1 工具定义

放在 `crates/tool-manager/src/builtin/` 新增一族（例如 `vision_tools.rs`），实现 `BuiltinToolProvider`：

```
name:        builtin_recognize_image
description: 用 AI 识别图片内容并返回文本（适合读取验证码、二维码、截图中的文字）。
             图片必须是本机已存在的文件路径（通常来自上一个工具的输出）。
input_schema:
  path        string  (required)  本地图片绝对路径
  instruction string  (optional)  识别要求，缺省为「读取图片中的文字，只输出结果」
```

设计意图（写进 description，不改 prompt 文件）：

- 明确「输入是**路径**不是图片数据」——避免 LLM 尝试传 base64；
- 明确「只输出结果」——工具内部提示词固定，返回短文本。

### 5.2 分类：**`Utility`**（已拍板）

⚠️ `crates/planned-agent/src/chat/tools/mod.rs:60-70` 的 `"all"` token 会**剔除 `Utility` 与 `SubAgent`**。

但已核实：**生产代码里没有任何地方使用 `"all"`** —— 它只出现在文档注释（`chat/service/config.rs:49-62`、`tool-manager/src/core/registry.rs:481`）与测试（`chat/tools/mod.rs:128/138/173`）里。GUI 的实际取值是：`None`（默认 = 全部工具）、`[]`（空）、`["request_user_action"]`（`pages/plan/flexible/page.rs`）、或逐个列名（`chat_service_factory.rs:65-74`）。

**结论：现状下 `Utility` 不会被剔除。** 将来若有人第一次用 `"all"`，可用**精确工具名** token 单独追加（`chat/tools/mod.rs:79-82` 支持），即 `Some(["all", "builtin_recognize_image"])` —— 这正是「只追加图片处理工具名」的做法。

**两处可见性结论（已核实）**：

- **flexible 执行步骤**：`ExecutorConfig.allowed_tools` 在 GUI 侧**未设置**（`services/run_service.rs:59-73` → `None` = 全部）→ 本工具**自动对每个步骤可见，零配置**。
- **协调器 / 子 agent**：走 `ChatConfig.allowed_tools`，逐个列名（`chat_service_factory.rs:65-74` 列了 8 个）。若希望协调器也能直接调用，需把 `builtin_recognize_image` 加进该清单 —— **本次不做**（执行步骤能用即可）。

### 5.3 AiClient 注入与「可以更换」的真实含义

**装配路径**（唯一注册点）：`crates/agent-gui/src/context/tools/mod.rs:75-95` `ToolsContext::init` 逐个 `register_builtin_provider(...)`。新增第 8 个 provider，构造时注入 AI 客户端 —— 与既有 `DocToolsProvider::new(docs_dir)`（`:88`）同样的"构造期注入依赖"模式。

```rust
// boot.rs:78 附近，此时 ai（AiContext）已构造完成（boot.rs:49）
let tools = ToolsContext::init(docs_dir, ai.manager.default() /* Result */)?;
```

**分层约束**：`tool-manager` 是 L1，**不能**依赖 L2 的 `ai-manager`，所以工具只收 core 的 `Arc<dyn AiClient>`（`crates/core/src/ai/mod.rs:10`）。宿主负责解析成具体 provider（`ai.manager.default()`，现有消费点见 `services/run_service.rs:82`、`pages/plan/flexible/chat_service_factory.rs:58`）。

**「可以更换」的落地（关键澄清）**：GUI **目前没有运行期切换 provider 的入口** —— 设置页「模型设置」tab 未启用（`pages/settings/types.rs:37-39`），`AiManager` 也没有 `set_default`。切换 = 改配置里的 `is_default` + **重启**，重启会重建 `AiContext`/`ToolsContext` → 新 client 自然生效。

因此：**装配期注入 `Arc<dyn AiClient>` 已经满足「复用主 provider + 可以更换」**。

> 预留接缝（本次不做）：将来若能运行期切换，把注入物从 `Arc<dyn AiClient>` 换成 core 侧的 resolver trait（`fn current(&self) -> Option<Arc<dyn AiClient>>`），宿主实现它。加这个 trait 属于 core 的「抽象在 core」约定，但要等真需求。
>
> 未配置任何 provider 时：`manager.default()` 返回 `Err`。**实现选择：跳过注册**（工具表里少一个，好过留一个必然报错的工具）；注册失败（目录不可用等）同样只 `warn` 不阻断启动。

### 5.4 路径约束：只允许 cache 产出区

需求：**只限 cache_dir**。落地时有一个必须澄清的点：**会话级 `cache_dir` 在装配期不存在** —— 它是每次运行由宿主拼出来的（`services/run_service.rs:64`：`app.flexible_output_dir().join(session_id)`）。而工具是全局注册的。

**方案**：把沙箱根设为它的**父目录** `GuiConfig::flexible_output_dir()`（`config/layout.rs:36`，由 `cache_root` 派生、装配期即知、涵盖所有会话）。既满足「只限 cache 区」，又允许跨会话回看旧图。

**复用既有沙箱原语**：`FilesystemService::resolve()` + `Resolved.dir` 句柄（`crates/tool-manager/src/builtin/filesystem/core.rs:87-139`，越界返回 `FsError::OutsideAllowed`）。注意它是 `pub(crate)`（`builtin/filesystem/mod.rs:12`），但新工具与它同在 `tool-manager` crate 内 → **可直接用**；用 `flexible_output_dir()` 作为唯一 root 构造一个专用 `FilesystemService::try_new(...)`。

> ⚠️ **校验与读盘必须用同一个句柄**。只 `resolve` 出字符串路径、再让外层按路径 `tokio::fs::read`（`crates/ai-openai/src/client.rs:228`）会**绕过 cap-std**，中间目录被换成指向沙箱外的 symlink/junction 即可逃逸（TOCTOU）。所以工具内用 `resolved.dir.metadata(&rel)` / `resolved.dir.read(&rel)` 完成判大小与读字节。

> 已拍板：**新建一个 root = `flexible_output_dir()` 的专用 `FilesystemService`** —— 它涵盖所有会话/运行目录（都是它的子目录）。注意 `FilesystemService::try_new` 走 `Dir::open_ambient_dir`，**root 目录必须已存在**，否则需先 `create_dir_all`。

### 5.5 工具内部的调用细节

1. **不带 `tools`**：内部这次 `ChatCompletionRequest.tools` 必须为空，否则模型可能回 `tool_calls` 而非文本。
2. **自带超时**：⚠️ flexible 的 `llm_timeout` 是「**一次 `chat_completion`**」的墙钟上限（`exec/executor/config.rs:47-57`）。工具内部的 LLM 调用发生在两次 `chat_completion` **之间**，**不在它覆盖范围内**。工具内必须 `tokio::time::timeout`（建议 60s，可配）。
3. **不可取消**：flexible 的 stop 信号在工具执行期间不生效（所有工具皆然）——识别慢时用户点停止要等超时。属既有行为，此处仅记录。
4. **图片走 `ImageSource::Url`（`data:` URL）**：字节由工具**经 cap-std 沙箱句柄读取**（`resolved.dir.read(rel)`）→ 嗅探 MIME → `data:{mime};base64,…`。
   - 为什么不用 `ImageSource::File`（原计划）：那条路只校验不读盘，真实读盘发生在 ai-openai（`client.rs:228`），**绕过沙箱句柄**，且「不存在 / 是目录 / 超限 / 格式不支持」都会退化成整个 chat 请求 `Err`（`client.rs:216` 不降级）。工具内读盘把这些变成可控的结构化错误码。
   - 代价：工具内存里会短暂持有一份 base64 —— 但**不进日志、不进上下文**（只进请求体），与用户关心的「巨大 base64 撑大上下文」无关。
5. **`detail`**：固定 `ImageDetail::High`（已拍板）—— 验证码这类小图以识别率为先。

### 5.6 失败与降级

| 情况 | 错误码 |
|---|---|
| `path` 缺失 / 空 / 指向目录 | `invalid_arguments` |
| 路径不在 cache 产出区 | `path_outside_allowed`（来自 `FsError::OutsideAllowed`） |
| 文件不存在 / 不可读 | `file_not_found` |
| 超过 20 MB | `image_too_large` |
| 内容嗅探不出图片 / 不在白名单 | `unsupported_image_type` |
| 句柄读取失败 | `read_failed` |
| AI 调用失败 | `ai_request_failed` |
| 超过 60s | `timeout` |
| 模型返回空 | `empty_result` |
| 模型返回超长文本 | 截断到 4000 字符（不报错） |

工具**不 panic**；所有可预期失败走 `Ok(ToolResult { is_error: true, content: {error, message} })`。

---

## 6. 已拍板（2026-10-07）

1. **视觉 provider**：复用主 provider，且要能更换 → 见 §5.3（现状 = 装配期快照，等价满足）。
2. **`ai_process` 占位**：不处理。
3. **路径约束**：只限 cache 产出区 → 见 §5.4（用 `flexible_output_dir()` 作根）。
4. **工具分类**：`Utility` → 见 §5.2（现状下不会被 `"all"` 剔除）。
5. **`detail`**：`ImageDetail::High`。
6. **范围**：先只做「内置识别工具」（阶段 2）；**阶段 1（MCP 图片通路）后续再接入**；不做路线 A。
7. **路径校验实现**：复用 `FilesystemService::resolve` —— 用 `flexible_output_dir()` 作唯一 root 新建专用 service（见 §5.4）。

---

## 7. 待拍板

无 —— 全部决策已定（见 §6）。

---

## 8. 影响面清单

**阶段 1（后续再接入）**

- `crates/mcp-rmcp/src/client.rs`（`convert_tool_result`）
- `crates/planned-agent/src/flexible/exec/step/mod.rs`（回灌点 `:345-401`）
- `crates/planned-agent/src/flexible/exec/step/image.rs`（**新增**）
- `crates/planned-agent/src/flexible/exec/step/render.rs`（`tool_content` 分流）
- 可能：`exec/executor/config.rs`（图片相关上限常量）

**阶段 2（✅ 已实施）**

- `crates/tool-manager/src/builtin/vision_tools.rs`（**新增**，含 8 个单测）+ `builtin/mod.rs`（挂载）
- `crates/core/src/mcp/types.rs`（仅在需要类型化时；`Value` 通常够）
- `crates/agent-gui/src/context/tools/mod.rs`（`init` 增参 + 注册第 8 个 provider）
- `crates/agent-gui/src/boot.rs:78`（调用点传 `Arc<dyn AiClient>`）
- 分类已定 `Utility`（已核实现状下不会被 `"all"` 剔除）→ **不动** `core/src/tool_registry/types.rs` 与 `registry.rs::map_categories`，也不新增 `Vision` 分类

**零改动（已核实）**

- flexible 的步骤工具白名单：`ExecutorConfig.allowed_tools` 在 GUI 侧**未设置**（`services/run_service.rs:59-73` → `None` = 全部工具），所以新工具**自动对所有步骤可见**，不需要改 `left_panel`。
- 协调器 prompt 的工具清单（`chat_service_factory.rs:65-74` 逐个列名）只在**协调器**用；若要协调器也能直接识别图片，需另行加入清单 —— **本次不做**（已拍板）。

---

## 9. 测试计划

| 层 | 用例 |
|---|---|
| `mcp-rmcp` | 单 image 块保留 data/mime；text+image 多块都保留；纯文本仍返回 `String`（回归）；resource/未知块占位不变 |
| `flexible/exec/step` | 图片结果落盘到 `<cache_dir>/<run_dir>/img-s1-r1-0.png`；tool 消息文本**不含 base64**、含绝对路径；mime→扩展名映射覆盖 5 种；落盘失败 → 该步 `Failed`；纯文本路径行为不变（95 例基线不破） |
| `tool-manager` | ✅ **已实施**（10 个用例）：路径越界 → `path_outside_allowed`；不存在 → `file_not_found`；指向目录 → `invalid_arguments`；非图片内容 → `unsupported_image_type`；伪装扩展名也按**内容**判定；成功路径断言 `ImageSource::Url` 的 `data:image/png;base64,` 前缀 + `tools` 为空；空回复 → `empty_result`；超长 → 截断；工具注册态（名字 + `Utility`）。桩 `FakeAiClient` 写在用例模块内 |
| 基线 | `cargo test -p planned-agent --lib flexible::`（112）、`cargo test -p planned-agent-tool-manager --lib`（108）、`cargo test -p planned-agent-gui --bins`（97） |

**实测（阶段 2 实施后）**：`planned-agent-tool-manager --lib` → **108 passed / 0 failed**（新增 10 个 `builtin::vision_tools::tests`）；`planned-agent-gui --bins` → **97 passed / 0 failed**；`planned-agent --lib flexible::` → **112 passed / 0 failed**（无回归）；`planned-agent-tool-manager --test cap_std_contract` → 4 passed。

---

## 10. 风险

1. **日志污染**（最高风险）：任何一条把工具输出原样 `tracing` 的路径只要漏掉，就会把几 MB base64 写进日志 —— §4.4 不变量 1 必须逐点核查。
2. **扩展名错配** → 整个 LLM 请求 `Err`（`client.rs:218-223` 无降级）。
3. **provider 不支持视觉** → 400；错误信息要能指向「这个 provider 不支持图片」。
4. **落盘目录生命周期**：run 目录目前无人清理（与 spill 产物相同），图片会长期堆积，需要后续 GC 策略。
5. **明文落盘**：截图可能含会话/个人信息，落盘即明文 —— 需与用户确认可接受，或后续加密/清理。
