# 多模态图片输入设计稿（core + ai-openai 两层）

> **状态：已落地（core + ai-openai 两层）。** §2–§6 描述的是**已实现**的方案，§7 是已知限制，§9 是已拍板结论。
> 测试基线：`cargo test -p planned-agent-ai-openai --lib` = **21 passed**（改动前 10）。
>
> - **范围**：只打通 `core`（类型契约）与 `ai-openai`（适配层）两层。
> - **不做**：GUI 的粘贴 / 选图 / 拖拽 / 图片气泡渲染，§7 明确列为后续阶段。
> - **图片形态**：消息里带**本地文件路径**，发请求前由适配层**读盘并编码为 `data:` URL**。
> - **目标读者**：后续实现者。§1 的现状事实都带 `file:line`，可直接核对。
>
> 相关既有文档：`docs/planned-agent/plan-storage-design.md`（消息如何落库）、
> `crates/core/AGENTS.md`（core 只放抽象与类型，实现在上层）。

---

## 0. 一句话

让 `MessageContent` 能表达「图文混排」，并让 `ai-openai` 把它转成 OpenAI 真正的多模态形状
`content: [{type:"text"},{type:"image_url"}]` 发出去 —— **替换掉现在把图片降级成 `"Image: {url}"` 纯文本的占位实现**。

---

## 1. 现状（事实）

### 1.1 core 里有一套**从未被消费**的图片类型

`crates/core/src/ai/types.rs:13-16`、`:33-47`：

```rust
MessageContent::Image { image_url: ImageUrl }      // :13-16
pub struct ImageUrl { url: String, detail: Option<ImageDetail> }   // :33-38
pub enum ImageDetail { Low, High, Auto }           // :41-47
```

- 全仓 grep `MessageContent::Image` / `ImageUrl {` / `ImageDetail`：**只有一处消费**（下面的降级分支），**零个构造点**。
- `crates/core/src/ai/mod.rs:13-15` 只 re-export `Message/MessageContent/MessageRole/ToolCall/ToolType`，
  **没有**导出 `ImageUrl`；外部只能从 `planned_agent_core::ai::types::ImageUrl` 拿到，而没人用。
- 结论：**这套类型是死代码，重命名/删除的破坏面为零**（这是本稿敢改类型契约的前提）。

### 1.2 ai-openai 把图片当文本处理

`crates/ai-openai/src/client.rs:247-251`：

```rust
Some(MessageContent::Image { image_url }) => {
    // 图片内容暂时作为文本处理
    let image_text = format!("Image: {}", image_url.url);
    async_openai::types::chat::ChatCompletionRequestUserMessageContent::Text(image_text)
}
```

即：图片只把 URL 字符串拼进 prompt，**模型看不到像素**。
另：`Image` 变体在 System / Assistant / Tool 分支会各自 `return Err`（`client.rs:233`、`:265`、`:295`）。

### 1.3 转换入口与调用时机

| 事实 | 位置 |
|---|---|
| 消息逐条转换 | `client.rs:228` `fn convert_message(&self, &Message)`（**同步**） |
| 消息列表转换 | `client.rs:321-324` `convert_request`（**同步**） |
| 非流式调用点 | `client.rs:592-595` `async fn chat_completion`，`convert_request` 在**重试循环之外**只调一次 |
| 流式调用点 | `client.rs:621-627` `async fn chat_completion_stream`，同样在循环外 |

两个入口**都是 async**，且 request 按值传入 —— 这意味着「发请求前做一次异步预处理」有天然落点（§4.1）。

### 1.4 底层库已支持多模态

`async-openai 0.41.3`（`crates/ai-openai/Cargo.toml`）内置：

- `ChatCompletionRequestUserMessageContent::{Text(String), Array(Vec<...ContentPart>)}`
  —— `.../async-openai-0.41.3/src/types/chat/chat_.rs:264-269`
- `ChatCompletionRequestUserMessageContentPart::{Text, ImageUrl, InputAudio, File}`
  —— 同文件 `:224-229`
- `ChatCompletionRequestMessageContentPartImage { image_url: ImageUrl }` —— 同文件 `:170-172`
- `ImageUrl { url: String, detail: Option<ImageDetail> }`（shared/image_url.rs，serde `skip_serializing_if` detail）

**System / Assistant / Tool 的 content part 只有 `Text`**（同文件 `:234-251`）—— 所以本期图片**只支持 User 消息**。

### 1.5 周边现状（本期不动的证据）

- 消息持久化：整条 `Message` 序列化成 JSON 存 `chat_messages.message_json`（`crates/agent-gui/src/storage/entities/chat_message.rs:16`）。
  `MessageContent` 自带 `#[serde(tag = "type")]`，**加变体不需要改表结构**。
- GUI 输入：`crates/agent-gui/src/components/chat/chat_panel/component.rs:314-339` 只有一个 `Textarea`，发送走
  `bridge.send(String)`（`chat_flow/bridge.rs:67-82`）→ `ChatService::send_text`（`crates/planned-agent/src/chat/service/service.rs:171`）。
- 全仓 grep `attachment|file_picker|clipboard|粘贴|上传|upload|rfd|FileDialog`：**无任何附件/图片入口**（连占位都没有）。
- 现有测试基线：`cargo test -p planned-agent-ai-openai --lib` → **10 passed, 0 failed**。

---

## 2. 目标与非目标

**目标**

1. core 能表达「一条用户消息 = 若干文本段 + 若干图片段」。
2. 图片可以以**本地文件路径**给出。
3. ai-openai 发出请求时，图片转成 `{"type":"image_url","image_url":{"url":"data:image/png;base64,..."}}` 真正送给模型。
4. 纯文本路径**零回归**：`MessageContent::Text` 仍转成 `content: "..."`（不走 array）。
5. 失败**显式**：读盘失败 / 不支持的扩展名 / 超限 → 返回 `Err`，**不再静默降级成文本**。

**非目标（本期不做，§7 有理由）**

- GUI 粘贴 / 选图 / 拖拽 / 图片缩略图 / 气泡渲染。
- 图片落盘缓存、去重、压缩、裁剪。
- System / Assistant / Tool 消息携带图片（底层也不支持）。
- 工具（如文件读取类）返回图片给模型。

---

## 3. core 类型契约（`crates/core/src/ai/types.rs`）

### 3.1 新增 `ImageSource`，取代 `ImageUrl`

```rust
/// 图片来源：要么是可直接使用的地址，要么是本地文件（发请求前读盘）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ImageSource {
    /// `http(s)://` 或已是 `data:` 的 URL —— 原样透传给 API
    Url {
        url: String,
        detail: Option<ImageDetail>,
    },
    /// 本地文件路径 —— 适配层读盘 + base64 后转成 `data:` URL
    File {
        path: std::path::PathBuf,
        detail: Option<ImageDetail>,
    },
}

/// 图片细节级别（不变）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImageDetail { Low, High, Auto }
```

- **删除** `pub struct ImageUrl`（被 `ImageSource::Url` 取代）。依据 §1.1：无构造点、无 re-export、无外部引用。
- 用 `PathBuf` 而非 `String`：类型更准；serde 下序列化就是平台原生路径字符串（Windows 上是 `D:\pics\a.png`）。

### 3.2 新增 `ContentPart`，`MessageContent` 加 `Parts` 变体

```rust
/// 一条用户消息里的一段内容（与 OpenAI 的 content part 一一对应）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    Text { text: String },
    Image { image: ImageSource },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MessageContent {
    Text { text: String },
    /// 单图、无文字
    Image { image: ImageSource },        // ← 字段名由 image_url 改为 image
    /// 图文混排
    Parts { parts: Vec<ContentPart> },   // ← 新增
    ToolResult { tool_call_id: String, content: String },
}
```

`Default` 保持 `Text { text: String::new() }`（`types.rs:24-31`）不变 —— GUI 构造占位消息依赖它。

### 3.3 落库 / 传输的 JSON 形状

core 自己的 JSON（`chat_messages.message_json`，**不直接发给 API**）：

```json
{"role":"user","content":{"type":"parts","parts":[
  {"type":"text","text":"这张图里有什么？"},
  {"type":"image","image":{"kind":"file","path":"D:\\pics\\a.png","detail":"auto"}}
]}}
```

**注意**：core 的 tag 是 `type: "image"`，OpenAI 要的是 `type: "image_url"` —— 这层差异由 §4 的转换负责，
**不要把 core 的 JSON 直接当请求体**（现状也不是这么用的：请求体由 `convert_request` 生成）。

---

## 4. ai-openai 适配层（`crates/ai-openai/src/client.rs`）

### 4.1 预处理：本地路径 → `data:` URL（异步，一次性）

新增一个**只做 IO、不改结构**的函数，在 `convert_request` **之前**调用：

```rust
/// 把所有 `ImageSource::File` 就地替换为 `ImageSource::Url { url: data_url, .. }`。
/// 只扫 User 消息（其它角色底层不支持图片）。
async fn resolve_local_images(request: &mut ChatCompletionRequest) -> Result<()>;
```

调用点（两处，request 已是 `mut` 或按值）：

```rust
// client.rs:592 chat_completion
async fn chat_completion(&self, mut request: ChatCompletionRequest) -> Result<ChatCompletionResponse> {
    resolve_local_images(&mut request).await?;      // ← 新增，在重试循环外、只跑一次
    let chat_request = self.convert_request(&request)?;
    ...
}

// client.rs:621 chat_completion_stream（已经是 let mut stream_request）
resolve_local_images(&mut stream_request).await?;
```

**为什么放这里而不是 `convert_request` 内部**：`convert_request` / `convert_message` 是同步纯函数，保持它们
无 IO 便于单测；IO 集中在一次预处理里，也让「重试时不再重复读盘」（重试循环在预处理之后）。

### 4.2 转换：`convert_message` 的 User 分支

```rust
MessageRole::User => {
    let content = match &message.content {
        Some(MessageContent::Text { text }) =>
            UserContent::Text(text.clone()),                    // 纯文本：不回归

        Some(MessageContent::Image { image }) =>
            UserContent::Array(vec![
                UserPart::ImageUrl(convert_image_source(image)?),          // 单图
            ]),

        Some(MessageContent::Parts { parts }) =>
            UserContent::Array(parts.iter().map(convert_content_part).collect::<Result<Vec<_>>>()?),

        _ => return Err(anyhow::anyhow!("User message must have text, image or parts content")),
    };
    ...
}

fn convert_content_part(part: &ContentPart) -> Result<UserPart> {
    match part {
        ContentPart::Text { text } => Ok(UserPart::Text(
            ChatCompletionRequestMessageContentPartText { text: text.clone() })),
        ContentPart::Image { image } => Ok(UserPart::ImageUrl(convert_image_source(image)?)),
    }
}

/// `Image` → `{"type":"image_url","image_url":{"url":..,"detail":..}}`
/// `ImageSource::File` 走到这里说明预处理没跑（内部错误），显式报错
fn convert_image_source(src: &ImageSource) -> Result<ChatCompletionRequestMessageContentPartImage> {
    let (url, detail) = match src {
        ImageSource::Url { url, detail } => (url.clone(), detail.as_ref()),
        ImageSource::File { path, .. } => bail!("internal: unresolved local image {}", path.display()),
    };
    Ok(ChatCompletionRequestMessageContentPartImage {
        image_url: async_openai::types::chat::ImageUrl { url, detail: detail.map(Into::into) },
    })
}
```

- `ImageDetail` → `async_openai::…::ImageDetail` 一一映射（`Low/High/Auto`，serde 都是 lowercase）。
- **删除** `client.rs:247-251` 的 `format!("Image: {}", ...)` 分支。
- System / Assistant / Tool 分支**不动**（底层 content part 只有 Text，`chat_.rs:234-251`）。

### 4.3 读盘与编码细则

| 项 | 取值 | 理由 |
|---|---|---|
| MIME 推断 | **零依赖小表**：`png→image/png`、`jpg/jpeg→image/jpeg`、`webp→image/webp`、`gif→image/gif`；其它扩展名 → `Err` | 不引 `mime_guess`；白名单与 OpenAI vision 支持面一致 |
| 大小上限 | `const MAX_IMAGE_BYTES: u64 = 20 * 1024 * 1024`（20 MB）；超限 → `Err` | 与 OpenAI 单图上限一致；提前失败好过 400 |
| 读盘 | `tokio::fs::read(path)`（在 async 预处理里） | 不阻塞 runtime；不需要把 `convert_*` 变 async |
| 编码 | `base64 = "0.22"`（`STANDARD`），拼 `data:{mime};base64,{b64}` | `tool-manager/Cargo.toml` 已用同版本，仓库有先例 |
| 已是 `data:` / `http(s):` | `ImageSource::Url` 原样透传，不校验、不读盘 | 保持灵活 |
| 失败 | **一律 `Err(anyhow!(...))`**，带路径与原因 | 静默丢图会让模型答得莫名其妙，比报错更难查 |
| 长度校验时机 | `metadata` 先快速失败，`read` 后**按实际字节数再校一次** | 消除元数据与正文之间的 TOCTOU |
| 错误定位 | `resolve_local_images` 给错误加 `message[{i}]` / `.parts[{j}]` context | 多图请求能定位是哪张图 |
| 空 `Parts` | `Err("… empty content parts …")` | `Array([])` 发出去只会吃 400 |
| 非 user 角色带图 | `Err`，文案写明「images are only supported on user messages」 | 原本的 `must have text content` 看不出是角色限制 |

依赖变更：`crates/ai-openai/Cargo.toml` 加一行 `base64 = "0.22"`（不进 workspace，与 `tool-manager` 一致）。

---

## 5. 改动点清单

| # | 文件 | 改动 |
|---|---|---|
| 1 | `crates/core/src/ai/types.rs` | 新增 `ImageSource` / `ContentPart`；`MessageContent` 加 `Parts`、`Image` 字段名改 `image`；删除 `ImageUrl` |
| 2 | `crates/ai-openai/src/client.rs` | 新增 `resolve_local_images`（async IO）+ `part_of` / `part_image` / `mime_of` / `check_image_size`；改 `convert_message` 的 User 分支；删降级分支；两处 AiClient 方法加预处理调用 |
| 3 | `crates/ai-openai/Cargo.toml` | 加 `base64 = "0.22"` |
| 4 | `crates/ai-openai/src/client.rs`（`mod tests`） | 见 §6 |
| 5 | `docs/planned-agent/multimodal-image-input.md` | 本文（设计稿） |

**不受影响的点（已核实无需改）**：`crates/core/src/ai/mod.rs`（`MessageContent` 等名字没变）、
所有对 `MessageContent` 做匹配的下游 —— 它们**都带 `_` 兜底**（`chat_flow/reduce.rs:392,408,441`、
`round/stream.rs:115`、`flexible/exec/step/render.rs:39`、`react/tool_executor.rs:106`、
`react/default_react_agent.rs:306,343,452`），新增变体**不会破坏编译**（见 §7 的语义影响）。

---

## 6. 测试（已落地）

- `cargo test -p planned-agent-ai-openai --lib` = **21 passed**（改动前 10，新增 11）
- `cargo test -p planned-agent-core --lib` = **19 passed**（改动前 16，新增 3）

ai-openai 新增 11 例（`crates/ai-openai/src/client.rs` 的 `mod tests`）：

| 用例 | 断言 |
|---|---|
| `user_text_message_still_plain_string` | 纯文本仍是 `content: "…"`，不回归成 array |
| `user_parts_text_plus_local_image_becomes_content_array` | 临时 PNG 读盘 → `Array([Text, ImageUrl(data:image/png;base64,…)])`，base64 解回来等于原字节，detail 映射为 `Auto` |
| `http_image_url_becomes_image_part_not_text` | `http(s)` URL 出 image part，**不再**是 `"Image: {url}"` 文本 |
| `existing_data_url_passes_through` | 已是 `data:` 的原样透传 |
| `image_detail_is_mapped` | core `High` → 库 `High` |
| `empty_parts_is_rejected` | 空 `Parts` → `Err` |
| `image_on_system_role_is_rejected` | System 带图 → `Err`，文案说明「仅支持 user」 |
| `unresolved_file_source_is_internal_error` | 未预处理的 `File` 进转换 → `Err`（防绕过预处理） |
| `missing_local_image_errors` | 文件不存在 → `Err`，错误链含 `cannot read local image` **与** `message[0]` |
| `unsupported_image_extension_errors` | `.txt` → `Err`；白名单映射（含大小写）正确 |
| `oversized_image_errors` | `check_image_size(MAX+1)` → `Err`（不真造 20 MB 文件） |

core 新增 3 例（`crates/core/src/ai/types.rs`）：`Parts` JSON 形状与往返、`ImageSource::Url` 往返（锁 `image` 字段名）、`Default` 仍是空文本。

> 临时文件用 `std::env::temp_dir()` + 进程 id + 时间戳命名；PNG 用最小硬编码字节（不用真图）。

---

## 7. 已知妥协与影响面（本期不解决，但必须说清）

1. **GUI 看不到图片**：`chat_flow/reduce.rs:389` `display_text` 只匹配 `Text`，`Parts` 会返回 `""`
   → 历史回放时该气泡显示为空文本。这是「本期不做 GUI」的直接后果，不是 bug。
   后续 GUI 期要动：`Bubble`（`chat_flow/types.rs:131-142`）、`build_bubbles`（`reduce.rs:273-385`）、
   用户气泡渲染（`chat_panel/component.rs:283-291`）。
2. **路径失效即请求失败**：历史里存的是**原始路径**。换机器、文件被删/改名 → 该会话下次带图请求直接 `Err`。
   这是「本地路径 + 运行时读盘」取舍的必然结果（对照另一选项：存 base64 数据 URL 到 SQLite，代价是库膨胀）。
3. **每轮重复读盘 + 重复 base64**：图片进了历史后，**每一次**请求都会重新读盘编码同一张图（预处理按请求遍历）。
   本期不做缓存（会话级 data URL 缓存是后续优化）。
4. **token 成本**：图片留在历史里 = 每轮请求都携带图片，按 vision token 计费；本期不做裁剪/降采样/摘要。
5. **其它角色的图片仍报错**：System/Assistant/Tool 带图 → `Err`（底层不支持，见 §1.4）。
6. **读取路径的下游只看得到文本**：`flexible/exec/step/render.rs:36` `content_text`、`round/stream.rs:112` 预览日志
   等在 `Parts` 下取到 `""`。当前这些路径读的多是 assistant/tool 消息，影响面小；若将来需要，
   在 core 加 `MessageContent::as_text()` helper 统一收口（**本期不加**）。
7. **MIME 只按扩展名判断**，不 sniff magic bytes：`.png` 实为 JPEG 时 MIME 会写错。
   当前接受该限制（本地路径由调用方保证扩展名正确）。要更稳就加 magic-byte 嗅探。
8. **重试时重复构造请求体**：`chat_completion` 的重试循环里 `req_json.clone()` 是**既有代码**，
   但它会整份复制含 base64 的 body（一张 20 MB 图 ≈ 27 MB 文本 × 最多 3 次）。图片放大了这个既有成本，
   可把 `req_json` 提到循环外构建。**本期未改**（既有行为，不属本次范围）。
9. **无总量预算**：只有单图 20 MB 上限；一条消息塞多张图仍可能撞请求体上限。
10. **历史里的旧 JSON 不兼容**：删了 `ImageUrl` 类型、字段名 `image_url` → `image`，是 serde 破坏性变更。
    但全仓无构造点（§1.1），历史库里不可能存在旧形状；`chat_flexible_message_storage.rs:47` 反序列化失败
    时是 `warn!` + 跳过（有日志，非静默）。

---

## 8. 分期

| 期 | 内容 | 状态 |
|---|---|---|
| **本期** | core 类型 + ai-openai 真实传图 + 预处理 + 测试 | ✅ 已落地（21 passed / 19 passed） |
| 后续 A | GUI 输入（粘贴 / 选图 / 拖拽，需引 `rfd` 或剪贴板方案） | 未排期 |
| 后续 B | GUI 渲染（缩略图、点击放大）+ `Bubble` 模型扩展 | 未排期 |
| 后续 C | 图片缓存 / 降采样 / 历史裁剪策略 | 未排期 |

---

## 9. 已拍板（开发前确认）

| 点 | 结论 |
|---|---|
| core 类型 | 删 `ImageUrl`；`MessageContent::Image` 字段名改 `image`（破坏面为零，见 §1.1） |
| mime 未知扩展名 | 白名单 `png/jpeg/webp/gif`，其它**硬失败** |
| 读盘失败 | **一律 `Err`**（不跳过、不降级） |
| 单图上限 | **20 MB** |

**遗留（本期未做）**：magic-byte 嗅探（§7.7）、请求体总量预算（§7.9）、把 `req_json` 提出重试循环（§7.8）。
GUI 输入 / 渲染见 §8 的后续 A / B。
