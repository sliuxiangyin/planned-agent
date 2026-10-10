# 图片通路 + 内置 AI 识别工具

> 状态：✅ **阶段 1 + 阶段 2 均已实施**（MCP 图片通路 + `builtin_recognize_image` 内置工具，测试全绿）
> 实施日期：2026-10-07
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

`crates/core/src/mcp/types.rs:13-18` 的 `content: Value` **字段类型不变**（理由见下），只补约定 + 一个借用视图：

- **全部是文本块** → 仍是 `Value::String`（多块用 `\n` 拼接）—— `tool_content()` 对所有文本工具零影响，向后兼容是硬要求，已实现并有回归测试；
- **其余（含图片 / resource）** → 数组。注意**文本块是裸字符串**、图片块才是对象：
  ```json
  [ "文本块原文",
    {"type":"image","mime_type":"image/png","data":"<base64>"} ]
  ```
  > 文本块刻意不写成 `{"type":"text","text":…}`：那样多块纯文本结果会从「只取第一块原文」变成「一坨 JSON」，是行为倒退。视图侧两种形态都认（`ContentBlock::Text`）。
- 块语义的解析收敛在 `crates/core/src/mcp/types.rs` 的**借用视图** `ContentBlock` + `parse_content_blocks()`（唯一约定点）：消费方不再手写 `block.get("data").and_then(Value::as_str)`；
- 只需要文本的消费方（chat / ReAct）走 `ToolResult::sanitized_content()`：**图片块降级为 `[图片]` 占位，不含图片的结果逐字不变**。

**为什么 `content` 不直接改成枚举**（2026-10-07 讨论结论）：`content` 的真实语义是「任意工具 payload 的通用容器」—— `ToolResult` 是**所有**工具（MCP + 8 族内置 + 子 agent）的统一返回类型，其中只有 `mcp-rmcp` 一个生产者会产内容块（且已降级），其余全塞自定义 JSON（`{stdout,exit_code}` / `{matches,count}` / `Value::String`…）。字段枚举化就必然带 `Structured(Value)` 逃逸分支 —— 覆盖 90% 生产者、却要改 10+ 个消费点，类型安全只惠及 3 处。所以切法是把**块语义的解析**枚举化，而不是把**字段**枚举化：生产端与线格式零变化，磁盘 / 事件兼容性不受影响（落盘的是 `Message`，其 `MessageContent::ToolResult.content` 本就是 `String`）。

`ContentBlock` 故意**不**标 `#[non_exhaustive]`：同一工作区内部类型，将来加变体（例如适配层改为保留 resource / audio 的对象形态）时让编译器点出全部消费方。
- `is_error` / `call_id` 语义不变。

> 不这么做就会踩新坑：数组被 `tool_content()` 的 `other.to_string()`（`render.rs:107-111`）直接序列化成一坨 JSON 塞进上下文。

### 4.2 `mcp-rmcp` 改动

`crates/mcp-rmcp/src/client.rs::convert_tool_result`：

已实施（`convert_tool_result` → `convert_contents` → `convert_content`）：

- 遍历**整个** `result.content`（修掉 `first()`）；
- 保留 image 块的 `data` / `mime_type`；
- resource / 未知块保持现有的占位文本（`[Resource]` / `[Unknown content type]`）；
- 纯文本单块仍返回 `Value::String`，否则返回数组。

**安全红线**：只采信服务端返回的 `data` 与 `mime_type`，**绝不采信它返回的 `path`/`uri` 去读盘**。

### 4.3 `flexible` 改动

**职责划分**（2026-10-07 两次复核后定型）：**决策在主流程，落盘与校验在 `image.rs`，文案在回灌点**。

`exec/step/mod.rs` 的回灌点自己 `match` —— 「这一步的结果里有没有图片」是编排决策，必须在主流程一眼可见，不能藏在下游函数里：

```rust
let blocks = parse_content_blocks(&outcome.result.content);
let content = match blocks.as_deref() {
    // 含图片：图片落盘换路径，文本块从视图取原文。
    // 文本部分**必须**从块里取 —— 不能走下面的 `tool_content`，
    // 它会把含 base64 的数组序列化成 JSON。
    Some(blocks) if blocks.iter().any(|b| matches!(b, ContentBlock::Image { .. })) => {
        let notes = image::spill_images(blocks, &cfg.cache_dir, input.run_dir, &tag).await;
        let text = blocks.iter().filter_map(ContentBlock::text).collect::<Vec<_>>().join("\n");
        join_text_and_image_notes(&text, &notes)
    }
    // 其余（纯文本 / 自定义 JSON）→ 原渲染路径，语义逐字不变
    _ => tool_content(&outcome.result.content),
};
```

**必须发生在 `output = %content` 与 `messages.push` 之前**（否则 base64 进日志）。

`crates/planned-agent/src/flexible/exec/step/image.rs`（**新增**，按 `AGENTS.md` §5：执行期新行为放小模块）**只做一件事：把图片块落盘、返回说明**：

- `spill_images(blocks: &[ContentBlock<'_>], cache_dir, run_dir, tag) -> Vec<String>`
  - 收的是**类型化视图**（`ContentBlock`），不碰 `Value`；非图片块 `continue` 掉 → 模块内**没有任何类型判断**（它不决定「要不要走这条路」，也不渲染文本块 —— 后者归 `render::tool_content` 与回灌点）；
  - 返回**每张图一条**的说明（绝对路径 + mime + 字节数）；被上限 / 校验拒掉的也各占一条，故 `notes.len()` = 实际处理的图片张数；
  - 落盘路径：`<cache_dir>/<run_dir>/img-<tag>-<nth>.<ext>`，其中 `tag = s<步>-r<轮>-c<第几次工具调用>`；文件名只含数字/字母，**不含 LLM 给的任何字符串** → 无目录穿越；
  - 扩展名按 mime 反推，只认 `png/jpg/jpeg/webp/gif`（与 `ai-openai::image_mime_of` 对齐，写错会让整个请求 Err）；不支持的 mime 直接给「格式不支持」说明，**不落盘**；
  - 三道上限：单张 20 MB（`MAX_IMAGE_BYTES`，与 ai-openai / `builtin_recognize_image` 一致）、单次最多 8 张（`MAX_IMAGES_PER_RESULT`）、单次总字节 64 MB（`MAX_TOTAL_BYTES_PER_RESULT`）；超限的那张只给说明、不落盘；
  - **解码前先按 base64 长度预筛**（`len > 额度/3*4 + 8` 即拒），避免把超大 payload 整份解进内存；
  - 路径用 `std::path::absolute` 绝对化（`ExecutorConfig.cache_dir` 可能是相对路径）。

回灌文案（「已保存为本地文件：…」那段，图片路径带 `file@` 前缀）在 `mod.rs::join_text_and_image_notes` —— 它是**给模型的提示**，属于编排，不进 `image.rs`。

**两条措辞纪律**（2026-10-07 定，理由见 §5.1）：**不点名工具**（工具没注册 / 改名时点名即幻觉源）、**不预设用途、也不指导怎么读**（只声明 `file@<path>`「这是已落盘的文件」，不写用哪个工具打开它 —— 「识别验证码」只是图片用途之一，还可能是看布局、提取表格）。

`ContentBlock::text()`（`core`）：`Text` 给原文、`Image` 给 `None`、`Unknown` 给 JSON 字面量 —— 供回灌点一行取完文本部分（免得到处写三变体 `match`）。

**落盘失败不判该步 `Failed`**（与本节初版设计相反，实施时改的）：工具**已经执行完**，落盘失败只是图片没留下来；失败原因写进回灌文本，模型可以重新调用工具取一张新图 —— 判 `Failed` 反而丢掉这次工具调用的信息。这与「跨步产出落盘失败」不同：那里失败等于后续步骤拿不到输入，这里模型能自适应。

### 4.4 硬不变量（must-not-break）

1. **base64 不得进入**：`messages` 的任何 tool 消息文本、`tracing` 日志（尤其 `mod.rs` 里那条 `output = %content` 的 WARN、`exec/executor/logging.rs::log_output`）、`PlanRunEvent`、`PlanRunReport`。落盘后**立即丢弃** base64（`materialize` 里解码出的 `bytes` 只活在该函数作用域）。
2. **纯文本工具行为逐字不变**（回归基线：`cargo test -p planned-agent --lib flexible::`，实施后 = 122）。
3. **绝对路径**：落盘后给 LLM 的路径必须是绝对路径（沙箱根 `flexible_output_dir()` 是绝对路径）。
4. **同一个 `ToolResult` 的所有消费方都要脱敏**。图片块进入 `Value` 之后，除 `flexible` 走落盘外，还有三条「只要文本」的路径，全部改用 `ToolResult::sanitized_content()`（图片 → `[图片]` 占位）：
   - `flexible/exec/step/mod.rs`（走 `image.rs` 落盘，天然无图片块进上下文）
   - `chat/driver/round/handlers.rs` → 工具历史 + `ChatEvent::ToolExecuted`
   - `planner/react/tool_executor.rs` → `ChunkStore` + `Observation.raw_output`（后者会进 ReAct 提示词）
   > 这三处 `sanitized_content()` 对**不含图片**的结果是逐字不变的克隆，所以 chat / ReAct 的既有行为**零回归**。不做这一步，改完 `mcp-rmcp` 反而会让 chat/ReAct 收到裸 base64 —— 比改之前的 `[Image]` 更糟。
5. 落盘上限三道（单张 20 MB / 单次 8 张 / 单次总 64 MB），超限只给提示不落盘；**但落下来的文件没有清理机制**，与 spill 产物一样依赖后续 GC。

---

## 5. 阶段 2：内置 AI 识别工具

### 5.1 工具定义

放在 `crates/tool-manager/src/builtin/` 新增一族（例如 `vision_tools.rs`），实现 `BuiltinToolProvider`：

```
name:        builtin_recognize_image
description: 用 AI 读取一张本地图片的内容并返回文本。读什么由 instruction 决定
             （读出图中的字符、判断界面状态与布局、提取表格数据）。
             path 必须是本机已存在的图片文件路径，通常来自上一个工具的输出。
input_schema:
  path        string  (required)  本地图片文件路径（png / jpg / jpeg / webp / gif）
  instruction string  (required)  要看什么；按当前步骤的意图填写
```

设计意图（写进 description，不改 prompt 文件）：

- 明确「输入是**路径**不是图片数据」——避免 LLM 尝试传 base64；
- **工具不预设用途**（2026-10-07 定）：`instruction` **必填**，「这张图要看什么」由**当前步骤的意图**决定。
  原先的「缺省读字 + description 写死验证码/二维码」把工具窄化成了 OCR —— 换成「检查登录页布局是否错乱」这类意图时，默认值会把答案压成一段文字（工具其实答得了，是默认值在拽它）。
  漏传 `instruction` → `invalid_arguments`（可自愈：模型看到错误会补），且校验**在读盘之前**，不浪费 IO。
- 工具定位记为「**图 → 文本**」：不返回图片数据，也不做**多图对比**（需未来加 `paths`）。

> 这条链上的三处「预设用途」是一起改的：回灌文案（§4.3）、本工具的 description、`instruction` 默认值。
> 只改一处无效 —— 另两处仍会把模型拽向 OCR。

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

**阶段 1（✅ 已实施）**

- `crates/mcp-rmcp/src/client.rs`（`convert_tool_result` → `convert_contents` / `convert_content`；+6 单测）
- `crates/core/src/mcp/types.rs`（`ToolResult::sanitized_content` + 借用视图 `ContentBlock` / `parse_content_blocks`；+6 单测）
- `crates/planned-agent/src/flexible/exec/step/image.rs`（**新增**；+6 单测）
- `crates/planned-agent/src/flexible/exec/step/mod.rs`（回灌点接线 + `mod image;`）
- `crates/planned-agent/src/flexible/exec/spill.rs`（新增 `spill_bytes`，二进制落盘）
- `crates/planned-agent/src/chat/driver/round/handlers.rs`、`crates/planned-agent/src/planner/react/tool_executor.rs`（改用 `sanitized_content`，防 base64 进历史 / 事件 / ReAct 提示词）
- `crates/planned-agent/Cargo.toml`（`base64` 依赖、`tempfile` dev-dependency）

> `render.rs` 的 `tool_content` **未改** —— 它还被 `describe_tool_args`（入参渲染）复用，改语义会误伤；分流放在 `image.rs`。

**阶段 2（✅ 已实施）**

- `crates/tool-manager/src/builtin/vision_tools.rs`（**新增**，含 8 个单测）+ `builtin/mod.rs`（挂载）
- `crates/core/src/mcp/types.rs`（**阶段 1 才动的** —— 视图 `ContentBlock` / `parse_content_blocks` 与 `sanitized_content`；阶段 2 未改）
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
| `mcp-rmcp` | ✅ 6 例：单 image 块保留 data/mime；text+image 多块都保留；仅图片也是数组；多块纯文本拼接成一个 `String`（不再只活第一块、也不退化成 JSON 字面量）；空 content → `Null`；`is_error` 保留；纯文本仍返回 `String`（回归） |
| `core::mcp::types` | ✅ 6 例：纯文本 / 结构化结果的 `sanitized_content()` 逐字不变；图片块 → `[图片]` 且 base64 不残留；无图片数组不变；`content_blocks()` 解析 Text/Image/Unknown、裸字符串视为单文本块、自定义 JSON → `None` |
| `flexible/exec/step/image` | ✅ 8 例：`spill_images` 把图片落盘并返回说明（**无 base64**、含**绝对路径**）；多图各自成文件；不支持的 mime 不落盘；坏 base64 不 panic；非图片块一概不理（不落盘、不建目录）；张数超限不落盘；超大 base64 解码前即被拒；mime→扩展名映射覆盖 ai-openai 白名单 |
| `flexible/exec/step`（回灌点） | ✅ 2 例：① 集成：假工具返回含图片结果 → `run_step` 全程跑通，tool 消息里只有绝对路径、无 base64，且 **tracing 日志里也无 base64**，run 目录落盘 1 张 png；② `join_text_and_image_notes` 纯函数：无说明时原文一字不动、有说明时拼接、文本为空时无前导空行。纯文本路径行为不变由既有的 `spill_threshold_*` / `spill_failure_*` 用例守着 |
| `tool-manager` | ✅ 11 例：**`instruction` 缺失 / 空白 → `invalid_arguments` 且不发 AI 调用**；路径越界 → `path_outside_allowed`；不存在 → `file_not_found`；指向目录 → `invalid_arguments`；非图片内容 → `unsupported_image_type`；伪装扩展名也按**内容**判定；成功路径断言 `ImageSource::Url` 的 `data:image/png;base64,` 前缀 + `tools` 为空；`instruction` 原样进 user 消息第一段；空回复 → `empty_result`；超长 → 截断；注册态（名字 + `Utility` + schema 里 `instruction` 进 `required`）。桩 `FakeAiClient` 写在用例模块内 |
| 基线 | `cargo test -p planned-agent --lib flexible::`（122）、`cargo test -p planned-agent-tool-manager --lib`（109）、`cargo test -p planned-agent-gui --bins`（97） |

**实测（阶段 1 + 阶段 2 实施后，2026-10-07）**：

| 命令 | 结果 |
|---|---|
| `cargo test -p planned-agent --lib flexible::` | **122 passed / 0 failed** |
| `cargo test -p planned-agent-mcp-rmcp --lib` | **15 passed** |
| `cargo test -p planned-agent-core --lib` | **27 passed** |
| `cargo test -p planned-agent-tool-manager --lib` | **109 passed** |
| `cargo test -p planned-agent-gui --bins` | **97 passed** |
| `cargo test -p planned-agent-tool-manager --test cap_std_contract` | 4 passed |

---

## 10. 风险

1. **日志污染**（最高风险）：任何一条把工具输出原样 `tracing` 的路径只要漏掉，就会把几 MB base64 写进日志 —— §4.4 不变量 1 必须逐点核查。
2. **扩展名错配** → 整个 LLM 请求 `Err`（`client.rs:218-223` 无降级）。
3. **provider 不支持视觉** → 400；错误信息要能指向「这个 provider 不支持图片」。
4. **落盘目录生命周期**：run 目录目前无人清理（与 spill 产物相同），图片会长期堆积，需要后续 GC 策略。
5. **明文落盘**：截图可能含会话/个人信息，落盘即明文 —— 需与用户确认可接受，或后续加密/清理。
6. **模型可能忽略图片**：图片不进主模型上下文，模型只看到路径 —— 若它不主动调用看图工具，图就白落了。缓解：工具 description 里写明「读取一张本地图片」（模型从工具表能对上）+ 回灌文案说明「需要查看时用能读图片的工具」。⚠️ **不可**改用「回灌文本点名工具」——工具没注册 / 将来改名时，点名就是幻觉源（2026-10-07 已把这个旧缓解推翻）。
7. **`instruction` 必填的代价**：弱模型可能漏传 → 多一次往返（`invalid_arguments` 可自愈）。这是刻意的取舍：让「图片用来干嘛」由**步骤意图**决定，而不是由工具的默认值替它决定。
8. **落盘失败不判该步失败**（实施时定的，见 §4.3）：好处是模型能自适应重取，代价是「图真丢了」时该步仍算成功 —— 回灌文本里的「保存失败」是被发现的唯一线索。
