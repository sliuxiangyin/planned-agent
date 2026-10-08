# 验证码求解工具（`builtin_solve_captcha`）设计

> **状态**：✅ **已实施**（2026-10-07；定稿 → 实现 → 装配 → 测试 → review 全过，见 §8 实测）。
> **依赖**：图片通路（`image-recognition-tool.md` §4）**已实施** —— MCP 图片落盘 + 回灌绝对路径。本工具吃这条链产出的路径。
> **前提决策（已拍板）**：`builtin_recognize_image` **不动**；另起**验证码专用**工具；v1 **只做字符型**；后端**以本地视觉模型为主**，先把工具形状与扩展点定下来。
> 与其他文档冲突时以代码为准，并顺手更新本文。

---

## 0. 一页速览

| 项 | v1 的决定 |
|---|---|
| 工具名 | `builtin_solve_captcha`（待拍板，备选 `builtin_recognize_captcha`） |
| 入参 | `path`（必填）、`kind`（可选，缺省 `"text"`） |
| 出参 | tagged union：`{"kind":"text","text":"4F7K","readable":true}` |
| 支持类型 | **只有字符型**（`kind = "text"`） |
| 后端 | **本地视觉模型**（复用主 provider 的 `Arc<dyn AiClient>`） |
| 分类 | `Utility`（与 `builtin_recognize_image` 一致，零配置可见） |
| 落点 | 新文件 `crates/tool-manager/src/builtin/captcha_tools.rs` + 新 provider |
| 不做 | 拖拽型 / 点选型 / token 型（reCAPTCHA·Turnstile）/ 第三方 API 后端 / ddddocr 集成 |

**扩展点的三根柱子**（§4、§5 展开）：

1. **`kind` 分派 + 策略表** —— 加一种验证码 = 加一个 enum 变体 + 一个策略块，主流程零改动；
2. **tagged union 出参** —— 加变体不改已有消费者（`kind` 字段自带判别）；
3. **后端位预留** —— v1 只实现本地视觉模型，第三方 API / ddddocr 接在同一个函数入口后面。

---

## 1. 目标与边界

### 1.1 目标

给模型一个**验证码专用**的内置工具：输入本地图片路径，输出**结构化**的求解结果。v1 覆盖字符型（image captcha / 图形验证码）。

### 1.2 不是目标（现在不做，但要留路）

| 类型 | 为什么现在不做 | 留下的路 |
|---|---|---|
| **拖拽型**（滑块 / 缺口） | 需要**两张图**（背景 + 滑块）+ 返回**坐标**；且成败一半取决于 MCP 能否**拟人拖动**（§6） | `paths` 入参 + `{"kind":"slide","x":…,"y":…}` 出参 |
| **点选型** | 需要**多目标坐标 + 顺序**；且常需"点 A → 看反馈 → 点 B"的多轮 | `targets` 入参 + `{"kind":"click","points":[…]}` 出参（多轮天然由 `run_step` 循环支持） |
| **token 型**（reCAPTCHA / Turnstile / hCaptcha） | **不是图像识别问题** —— 要拿 sitekey 调服务商、往页面注入 token | 不打算自建；将来只可能作为第三方后端 |
| **第三方商业 API 后端** | 要 key、要付费、图片要出境（§9） | §5 的后端位 |
| **本地 ddddocr** | Python 生态，集成成本高（嵌运行时 / 子进程 / ONNX 转换） | §5 的后端位 |

### 1.3 与 `builtin_recognize_image` 的分工（**必须写进两个工具的 description**）

| | `builtin_recognize_image` | `builtin_solve_captcha` |
|---|---|---|
| 定位 | **通用**读图：读什么由调用方用自然语言指定 | **验证码专用**：领域固定，策略内置 |
| 入参 | `path` + `instruction`（**必填**） | `path` + `kind`（可选） |
| 出参 | 纯文本 | **结构化 JSON** |
| 提示词 | 调用方给 | **工具内置**（固化的验证码提示词） |

> ⚠️ 两个工具都能读图，**不写清边界模型会选错**。两边 description 都要加一句互指。

---

## 2. 为什么单开一个工具（而不是让通用工具加参数）

三条真收益 —— 它们**都不是**"更会识别"，而是工程属性：

1. **结构化出口**：验证码的下游（填输入框 / 拖拽 / 点击）需要**精确数据**。让模型从一段自然语言里抠"4F7K"或抠坐标，脆且不可测。
2. **提示词固化**：通用工具把"说什么"交给调用方 → 模型每步即兴编。验证码的提示词**应该写死**（"只输出字符本身，不要解释，无法辨认时输出 UNREADABLE"）。
3. **可测试**：契约固定才能拿固定图片做回归。通用工具（`instruction` 千变万化）**测不了**。

### 2.1 这不违反"工具不预设用途"的纪律

前一阶段刚定的纪律是：**通用工具不能替模型决定「这张图要看什么」**（见 `image-recognition-tool.md` §4.3、§5.1）。

本工具**不冲突**，因为它的**领域从名字起就收窄了** —— 在"验证码"这个领域内给最常见形态（字符型）一个默认值，是合理的领域内约定，而不是替调用方猜通用意图。同理，它的提示词固化也是**领域内的**，不是猜测。

> 反过来说：**不要**把 `builtin_recognize_image` 也"验证码化"，也不要给本工具加通用 `instruction` 参数 —— 那会把两个工具的定位都搞浑。

### 2.2 为什么不直接挂第三方验证码 MCP server

调研（§9）确实查到现成的（`2captcha/2captcha-mcp` 有 **40 个工具**，`aezizhu/mcp-captcha-solver` 有 **30 个**）。但：

- 挂上去 = 工具表瞬间膨胀几十条，**每一步请求都带**（flexible 每步都用完整工具定义表），模型选错工具的概率陡增；
- 出参形态由别人定，**和我们的回灌/落盘链路对不上**（它们回 token / 注入 JS，我们要的是"给模型看的结果"）；
- 密钥与计费策略散在别人的 server 里，我们无法统一。

→ 结论：**要能力不要形态**。将来若接第三方，也是在**我们自己的工具内部调它的 HTTP API**（§5），而不是把它的 MCP 挂进来。

---

## 3. 工具契约（v1）

### 3.1 定义

```
name:        builtin_solve_captcha
description: 求解一张验证码图片。支持类型由 kind 指定（当前仅字符型 "text"），
             返回结构化结果。用于验证码场景；通用读图请用 builtin_recognize_image。
             图片必须是本机已存在的图片文件路径，通常来自上一个工具的输出。
input_schema:
  type: object
  properties:
    path: string   # 本地图片文件路径（png / jpg / jpeg / webp / gif）
    kind: string   # 验证码类型，缺省 "text"；当前仅支持 "text"
  required: ["path"]
```

`kind` 现在**只有一个合法值**，看着像噪音 —— 保留它的理由：它是扩展点的**声明**，且 tool schema 每轮请求都完整带给模型、不落库，**变化对模型零成本**（§4.3）。

### 3.2 出参（tagged union）

```jsonc
// 成功
{ "kind": "text", "text": "4F7K", "readable": true }

// 执行成功但没认出来
{ "kind": "text", "text": null, "readable": false }
```

**为什么"认不出来"不是 `is_error`**：`is_error: true` 会让回灌链路把它当"工具失败"（日志 WARN + 模型看到"错误"），模型容易就此放弃。而"这张图看不清"是**正常结果**，模型该据此决定下一步（重新截图 / 换一张 / 报步骤失败）。用 `readable: false` 表达，语义归结果而不归错误。

**为什么不用魔字符串 `"UNREADABLE"`**：万一某个验证码的内容恰好是那个字符串就分不清了。`text: null` + `readable: false` 无歧义。

### 3.3 归一化（**只做保守的四件事**）

模型输出 → 归一化：

1. `trim`（去首尾空白与换行）；
2. 去掉**包裹的代码块围栏**（```` ``` ```` / `~~~`，允许首行带语言标注；**只成对时去** —— 只有半个围栏说明模型输出本身不规范，去掉反而可能删掉真内容）；
3. 去掉**成对的首尾引号**（`` ` ` ``、`'`、`"`）；
4. 去掉**结尾的句号**（含中文句号）。

`UNREADABLE` 哨兵**大小写不敏感**匹配（模型偶尔回 `Unreadable`；漏判会把这个词当答案交出去）。

**不做**的事（做了就可能给出错误答案）：

- ❌ 大小写转换 —— 验证码**可能区分大小写**；
- ❌ 删除内部空格 —— 有些验证码含空格，且"4 F 7 K"也可能就是答案的形态；
- ❌ 正则抽取"看起来像验证码的部分" —— 猜错就是错答案。

**超长即判失败**：字符验证码的合理长度很短。归一化后超过 `MAX_TEXT_CHARS`（建议 64，待拍板）→ 视为模型跑偏，返回 `readable: false`。**比截断好**：截断会给出一个看似合理但错误的验证码。

原始输出（归一化前）**只进 `tool_audit` 日志**，不进上下文、不进结果 —— 排查"为什么读错"时它是唯一现场证据（它不含 base64，记录是安全的）。

### 3.4 内置提示词（固化，v1 不可覆盖）

```
这是一张验证码图片。请只输出图片中的验证码字符本身：
- 不要解释、不要标点、不要空格、不要引号；
- 如果图中有干扰线、噪点或背景，请忽略它们；
- 如果确实无法辨认，只输出 UNREADABLE。
```

（纯机制描述，不含任何业务/站点个案 —— 违反这条会让工具过拟合到某一种验证码。）

### 3.5 参数校验顺序与错误码

顺序与 `builtin_recognize_image` 对齐（**参数校验先于一切 IO**）：

```
path 非空 → kind 合法 → resolve（沙箱越界即拒）→ metadata（存在 / 是文件 / ≤上限）
  → 经 cap-std 句柄读字节 → mime 内容嗅探 → 调后端
```

| 错误码 | 触发 | `is_error` |
|---|---|---|
| `invalid_arguments` | `path` 缺失/空白、`kind` 不在支持列表 | `true` |
| `path_outside_allowed` | 越界（复用 `FilesystemService::resolve`） | `true` |
| `file_not_found` | 不存在 / 不可读 | `true` |
| `image_too_large` | 超单张上限（20 MB，与 ai-openai 对齐） | `true` |
| `unsupported_image_type` | 内容嗅探不在白名单（png/jpg/jpeg/webp/gif） | `true` |
| `read_failed` | 读盘失败 | `true` |
| `backend_failed` | 内部 LLM 调用 `Err` | `true` |
| `timeout` | 超 60s（**必须自带**：flexible 的 `llm_timeout` 覆盖不到工具内部那次调用） | `true` |
| —（`readable: false`） | 模型没给出可辨认结果 | `false` |

> `kind` 非法要**明确报错**而不是静默回退到 `text` —— 静默回退会让"将来加了 slide 但调用方拼错"变成"静默按字符型解"，错误难查。

---

## 4. 扩展空间：三类验证码的变化轴与落点

### 4.1 变化轴

| 轴 | 字符型 | 拖拽型 | 点选型 |
|---|---|---|---|
| 输入图 | 1 张 | 2 张（背景 + 滑块） | 1 张 |
| 出参 | 文本 | 1 个坐标 | N 个坐标 + 顺序 |
| 提示词 | 只输出字符 | 找缺口 x | 找出并排序 |
| 需多轮 | 否 | 否 | **可能**（点 A 看反馈再点 B） |

最后一行是好消息：**点选型的"迭代"不需要工具持有状态** —— `run_step` 本身就是「LLM ⇄ 工具」多轮循环，模型天然能一轮一轮来。工具保持无状态。

### 4.2 落点：策略表

```rust
/// 一种验证码的求解策略 —— 加类型只加这里，主流程不动。
struct CaptchaStrategy {
    /// 固定提示词
    prompt: &'static str,
    /// 该类型允许的图片张数（字符型 = 1；拖拽型 = 2）
    images: ImageCount,
    /// 把模型输出解析成出参（含 `readable` 判定与归一化）
    parse: fn(&str) -> Value,
}

fn strategy(kind: &str) -> Option<&'static CaptchaStrategy>;
```

`recognize` 主流程：校验 → 读图 → `strategy(kind)` → 调后端 → `parse` → 包装成 `ToolResult`。**全流程对 `kind` 无感知**。

### 4.3 加一种验证码要改哪几处（diff 清单）

1. `CaptchaKind` 加一个变体（或 `strategy()` 表加一行）；
2. 加一个 `CaptchaStrategy` 常量（提示词 + `parse`）；
3. `input_schema` 里为它补需要的入参字段（如拖拽型的 `paths`）—— **可选字段 + 运行时校验**，不破坏既有调用；
4. 出参变体（如 `{"kind":"slide",…}`）—— 消费方按 `kind` 判别，**已有变体不受影响**；
5. 测试：加该类型的 parse 单测 + 一条端到端。

**不在这份清单里的东西都不能改** —— 若发现要动主流程，说明策略抽象不成立，应先修抽象。

### 4.4 关于"单工具 + `kind`"这个取舍

**缺点**：随类型增多，`input_schema` 会变胖（各类型的入参都得塞进同一个 schema）。

**仍然选它**的理由：

- 现在只有一种 `kind`，schema 不胖；
- **加工具容易、改工具难** —— 将来若拆成 `builtin_solve_char_captcha` / `builtin_locate_slide_gap`，模型要重新学；反过来从多工具合成一个基本不可能（工具名一旦被模型记住就不能随便撤）；
- 判据明确：等 `kind` 多到 schema 明显臃肿、或模型**频繁选错 `kind`** 时再拆 —— 那时也更清楚该按什么轴拆。

---

## 5. 后端可插拔（v1 只留位）

### 5.1 形状（**待拍板：现在就抽 trait，还是只留一个函数入口**）

```rust
/// 求解后端。v1 只有 LocalVision；将来第三方 API / ddddocr 接同一个入口。
#[async_trait]
pub trait CaptchaBackend: Send + Sync {
    fn name(&self) -> &str;
    /// 本次实际用的模型名（进审计日志）。默认 `None` —— 不依赖模型的实现不用管。
    fn model(&self) -> Option<String> { None }

    /// 原始模型输出（归一化属于策略层）。
    /// **调用点会用同等时长再做一层外层兜底**，超出时限的 future 会被 cancel。
    async fn solve(&self, req: SolveRequest<'_>) -> Result<String, BackendError>;
}
```

- **v1 唯一实现**：`LocalVisionBackend` —— `Arc<dyn AiClient>` + 图片字节 → 构造 `MessageContent::Parts`（文本 + `ImageSource::Url{data:}`，`detail = High`）→ 调 `chat_completion`（**不带 `tools`**）。
- **不做**：多后端选择逻辑、配置项、密钥管理、余额查询、失败降级到另一个后端。这些都是"有第二个后端时"的事。

### 5.2 将来接第三方的落点

第三方 API 的**入出口与本工具高度吻合**（调研见 §9）：字符型回**文本**，点选/拖拽回**坐标或索引** —— 正好是本工具的出参形状。所以接入点在 `CaptchaBackend` 后面，**不需要改工具契约**。

⚠️ 那时必须一并处理：密钥配置位置（宿主注入）、**图片出境**的告知与开关（登录页截图可能含账号信息）、按次计费的成本可见性。

---

## 6. 坐标系与 MCP 分工（为拖拽型铺路，现在就定约定）

拖拽型 v1 不做，但它的**两个致命细节现在就该写进契约**，否则等做到那步再定会返工：

### 6.1 坐标基准：**图片像素坐标，左上角原点**

工具返回的坐标一律是**所给图片的像素坐标**（原图，不缩放），并**假定调用方负责换算到页面坐标**。

调研证实这个坑是真的（§9）：主流浏览器 MCP 的截图工具**都不报图片像素尺寸** ——

| MCP | 截图接口的尺寸/基准处理 |
|---|---|
| `microsoft/playwright-mcp` | 有独立参数 `scale: 'css'\|'device'`（默认 css）声明基准，**不报具体尺寸** |
| `ChromeDevTools/chrome-devtools-mcp` | 内部按 `devicePixelRatio` downscale，**响应里不给尺寸** |
| `hangwin/mcp-chrome` | 唯一一个返回 `dimensions:{width,height}` 的 |

→ 结论：**"算出的坐标偏 5 像素"在滑块验证码里等于失败**。做拖拽型之前，必须先确认所用 MCP 的截图基准（`scale` 参数或 `dimensions`），必要时让工具同时回 `image_width` / `image_height` 供换算。

### 6.2 拖拽轨迹**不在我们手里**

调研的关键结论：**没有任何一个主流浏览器 MCP 提供带轨迹/多步/拟人化的拖拽参数** —— `playwright-mcp` 的 `browser_mouse_drag_xy` 是**单次直线 move（无 steps）**，`chrome-devtools-mcp` / `mcp-chrome` 也未暴露步数/抖动。

→ 意味着：**拖拽型验证码的通过率瓶颈，很可能不在"我们能不能识别缺口"，而在"MCP 能不能拟人拖动"**。这一点在做拖拽型之前要有预期：识别做对了也可能过不了。

（`withLinda/puppeteer-real-browser-mcp-server` 用了 `ghost-cursor` 做拟人移动，但它**没有拖拽工具**。）

### 6.3 分工

| 环节 | 归谁 |
|---|---|
| 截图 / 取图元 | 浏览器 MCP |
| 落盘 + 回灌路径 | flexible（**已实施**） |
| **识别 / 定位** | **本工具** |
| 填输入框 / 拖拽 / 点击 | 浏览器 MCP |
| 坐标换算（若需要） | **调用方（模型）**，工具只给图片像素坐标 |

---

## 7. 落点清单

| 文件 | 动作 |
|---|---|
| `crates/tool-manager/src/builtin/captcha_tools.rs` | **新增** —— provider + executor + 策略表 + `LocalVision` 后端 |
| `crates/tool-manager/src/builtin/mod.rs` | 加 `pub mod captcha_tools;` |
| `crates/agent-gui/src/context/tools/mod.rs` | 注册 `CaptchaToolsProvider`（与 `VisionToolsProvider` 同一套 `vision_ai` / `vision_root`，**不再新增入参**） |
| `crates/tool-manager/src/builtin/vision_tools.rs` | 只改 **description 一句**（加互指，§1.3），逻辑不动 |
| 本文件 | 实施后回填「实测」数字与状态 |

**为什么新文件而不是塞进 `vision_tools.rs`**：`vision_tools.rs` 已约 600 行；两者领域不同（通用读图 vs 验证码求解），混在一起会让"加验证码类型"这件事去改一个通用读图文件，违反 §4.3 的"改动范围可控"。

**为什么新 provider**：同族的两个工具各自独立演进；装配侧只多一行。

**分类**：`ToolCategory::Utility` —— 与 `builtin_recognize_image` 一致。`allowed_tools` 的 `"all"` token 会剔除 `Utility`，但生产代码零使用 `"all"`（详见 `image-recognition-tool.md` §5.2），故**零配置即可见**。

---

## 8. 测试计划

| 层 | 用例 |
|---|---|
| 参数校验 | `path` 缺失 / 空白 → `invalid_arguments`；`kind` 非法 → `invalid_arguments`（**且不发 AI 调用**）；**校验顺序**：`path` 优先于 `kind` 优先于 IO |
| 图片校验 | 越界 → `path_outside_allowed`；不存在 → `file_not_found`；是目录 → `invalid_arguments`；非图片内容 → `unsupported_image_type`；伪装扩展名也按**内容**判定；超大 → `image_too_large` |
| 归一化（纯函数，重点） | trim；去成对引号；去结尾句号；**保留内部空格**；**不改大小写**；超长 → `readable: false` |
| 提示词 | 进 user 消息**第一段**且与常量逐字一致；图片是第二段 `ImageSource::Url{data:…}`；`tools` 为空；`detail = High` |
| 出参 | 成功 → `{"kind":"text","text":…,"readable":true}`；模型回 `UNREADABLE` → `readable: false` 且 `is_error == false`；模型回空 → `readable: false` |
| 超时 | 后端超 60s → `timeout` |
| 注册态 | 名字 + `Utility` + `supported_tools` + schema（`required` 只含 `path`） |
| 日志 | `tool_audit` 记 path / mime / bytes / model / 原始输出长度 / 归一化是否发生；**不记图片字节、不记 data URL** |
| 桩 | `FakeAiClient`（沿用 `vision_tools.rs` 用例内的写法，**不新增 public 测试模块**） |

**实测（实施后，2026-10-07）**：

| 命令 | 结果 |
|---|---|
| `cargo test -p planned-agent-tool-manager --lib` | **126 passed / 0 failed**（109 基线 + 17 新增） |
| `cargo test -p planned-agent --lib flexible::` | **122 passed** —— 未动 flexible 侧，与基线一致 |
| `cargo test -p planned-agent-gui --bins` | **97 passed** —— 未动 GUI 逻辑，与基线一致 |
| `cargo test -p planned-agent-core --lib` | 27 passed |
| `cargo test -p planned-agent-mcp-rmcp --lib` | 15 passed |

新增 17 例的分布：参数校验 2、图片校验 5、出参 4、归一化 1、后端 2、请求形状 1、日志截断 1、注册态 1。

**复核（review）结论**：minor nits，无阻塞。3 个 should-fix 已收：

1. **超时只包在后端内部**：自定义后端不自带超时就会拖死整个步骤 → 调用点再加**同等时长的外层兜底**，并把 `SOLVE_TIMEOUT_SECS` 公开供外部实现遵守；
2. 审计日志缺 `model` → `CaptchaBackend` 加默认方法 `model()`（默认 `None`，不依赖模型的实现不用管），日志补上；
3. 设计稿列的 `image_too_large` 用例缺失 → 补上（用**稀疏文件** `File::set_len` 造长度，不真占磁盘）。

另收 3 个 nit：`UNREADABLE` 哨兵改**大小写不敏感**（模型偶尔回 `Unreadable`）、`strip_fence` 补进 §3.3、`debug_assert_eq!` 补说明（release 不生效，多类型时要升级为运行时校验）。

**未采纳一条**：`backend_failed` 会把后端错误原文回灌进 messages，若 provider 的错误响应体回显请求，理论上可能带出 data URL。**与既有的 `builtin_recognize_image` 行为一致**，为保持两个工具族一致不改；若要防，应当两处一起改。

---

## 9. 第三方生态调研结论（2026-10-07）

三个只读调研任务的合并结果（GitHub Search API / npm registry / 各仓库 README，**仅报告实际命中的项目**）。

### 9.1 三类现状

| 类别 | 代表 | 能力 | 代价 |
|---|---|---|---|
| **商业 API 的 MCP 封装** | `2captcha/2captcha-mcp`（官方，40 工具）、`capsolver-ai/capsolver-mcp`（官方）、`CapMonsterCloud/capmonster-mcp-captcha-solver`（官方） | **最全**：reCAPTCHA / Turnstile / hCaptcha / DataDome / **GeeTest v3+v4 / 腾讯 / 阿里 / 网易易盾** / 字符型 / Grid 点选 / Canvas 坐标 / 拖拽 | 要 key、付费、**图片上传第三方**；工具表膨胀 |
| **本地自建模型** | `ymeng98/ddddocr-captcha-mcp`、`ilien-dev/svipall`（Rust）、`autokeren/ghostfox`、`libaibaia/CaptchaMCP` | 字符型 OCR / 目标检测 / 滑块匹配；**无需 key** | 覆盖窄（商业级 reCAPTCHA·Turnstile 弱） |
| **浏览器自动化 MCP 的内置 captcha** | 基本**没有** —— `ChromeDevTools/chrome-devtools-mcp`（53k⭐）、`microsoft/playwright-mcp`（37k⭐）**均无**；`BrowserMCP` 靠真实指纹"规避"；`withLinda/...` 的 `solve_captcha` 是**明确占位符** | — | — |

`modelcontextprotocol/servers` 官方参考仓库中**无验证码相关 server**。专用验证码 MCP 的整体 star 极低（多为个位数），维护质量存疑。

### 9.2 四条对本设计有用的事实

1. **字符型的专业解是 `ddddocr`**（离线 ONNX，专为验证码训练），识别率通常显著高于通用视觉模型 —— 但**是 Python 库**，Rust 侧集成成本高（嵌运行时 / 子进程 / 转 ONNX 用 `ort`）。→ 列为将来的后端位（§5），v1 不做。
2. **点选/拖拽的成熟解都是商业 API，且原生返回坐标/索引**（CapSolver `AwsWafClassification` 回坐标、`ReCaptchaV2Classification` 回方格索引；2Captcha Canvas 回多边形顶点、Drag&Drop 回坐标）。→ **与本工具的出参形状天然吻合**，将来接入不需要改契约。
3. **国内验证码（极验/腾讯/阿里/易盾）靠"纯视觉识别"基本做不了** —— 极验类的成败一半是**加密协议逆向**（`w` 参数、轨迹签名），商业 API 能做是因为**持续逆向维护**。→ **若目标是极验类，自建视觉模型这条路走不通**，必须第三方。
4. **坐标系与拖拽轨迹是真坑**，且**不在我们手里**（§6）：主流 MCP 都不报截图尺寸；没有任何 MCP 提供拟人轨迹的拖拽。

### 9.3 结论

生态**存在但不成熟**：能力最全的都要付费+出境，本地免费的覆盖窄、star 低。对本项目的意义是：**"验证码专用工具"这个方向成立**，但**自建路线的天花板由验证码类型决定** —— 字符型可行，极验类不可能。故 v1 选字符型 + 本地视觉模型是合理起点；将来若要做极验类，应当是「加第三方后端」而不是「把视觉模型调得更好」。

---

## 10. 风险与不变量

1. **成功率有上限**：字符验证码的识别率取决于图片难度，工具能做的是"提示词固化 + 输出归一化 + 明确表达认不出"，**不保证认得对**。不要把架构押在识别率上。
2. **工具内部不重试**：读错了该由模型决定（重新截图 / 换图 / 判步骤失败）。工具自己重试会让结果漂移且不可测。
3. **两个工具抢活**：`builtin_recognize_image` 与 `builtin_solve_captcha` 都能读图，**description 必须写清边界**（§1.3）。这是本设计最可能出的"使用侧 bug"。
4. **`kind` 静默回退是禁止的**：非法 `kind` 必须报错（§3.5）。
5. **base64 纪律照旧**：工具内部读盘 → data URL → 发请求，**任何日志 / messages / 事件 / 报告都不得出现 base64**；`tool_audit` 只记元信息。
6. **读盘必须走 cap-std 句柄**（复用 `FilesystemService::resolve`）—— **不要**只校验路径再看字符串路径重新读（TOCTOU 逃逸；且"不存在/超限/格式错"会退化成整次 chat 请求 `Err`）。这条与 `vision_tools.rs` 一致。
7. **内部调用自带 60s 超时、不带 `tools`**。
8. **图片不落盘进主模型上下文**：本工具只是"看图工具"之一，主 provider 仍不必支持视觉（图片只在工具内部那次调用里出现）。

---

## 11. 拍板记录（2026-10-07）

| # | 问题 | 结论 |
|---|---|---|
| Q1 | 工具名 | **`builtin_solve_captcha`**（动词 `solve` —— 将来不只是「认」，还有定位与排序） |
| Q2 | 后端是否抽 trait | **抽** —— `CaptchaBackend`，v1 唯一实现 `LocalVisionBackend` |
| Q3 | 「认不出来」的表达 | **`readable: false` + `text: null`**（不用 `is_error`，不用魔字符串） |
| Q4 | 超长结果的处置 | **判 `readable: false`**（不截断） |
| Q5 | `MAX_TEXT_CHARS` | **64** |
| Q6 | 归一化再加一步 | **去掉包裹的代码块围栏**（```` ``` ```` / `~~~`，只在**成对**时去） |
| Q7 | `kind` 默认值 | **缺省 `"text"`** |
| Q8 | 是否给 `hint` 参数 | **不给** —— 通用说明请用 `builtin_recognize_image`；保持契约最小 |

### 实施时的小补充（实现后回填）

- `tool_audit` 日志里记**截断到 64 字符**的原始输出（`vision_tools.rs` 的审计纪律是「不记完整识别文本」，这里刻意不同：验证码文本极短、不含页面内容，而它是排查「为什么读错」的唯一现场证据）。

拍板完毕，按 §7 落点实施、按 §8 补测试、回填实测数字。
