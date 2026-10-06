# builtin_grep_file：单文件内容搜索（「落盘句柄」的配套读法）

> **状态：已实施（2026-10-05）。** 实施中的三处偏离见文末「落地记录」；§1/§2 的现状事实都带 `file:line`，可直接核对；
> §9 是全部决策（Q1/Q5 的结论已写入）。
>
> **已拍板**
> 1. **做法**：**新增**一个单文件 grep 工具；**不动**已上线的 `builtin_search_files_content`
>    （那份要改成支持单文件会牵动 chat 子 agent 等既有调用方）。
> 2. **给谁用**：**任务步 + 输出整理步**。整理步现在只有 `builtin_read_file_lines` 一个工具
>    （`executor/mod.rs:435` 白名单）。
> 3. **命名**：`builtin_grep_file`（动词开头，与 `builtin_read_file_lines` 配对；不叫
>    `builtin_search_file_content` —— 与既有的 `builtin_search_files_content` 只差一个 `s`，太难分辨）。
> 4. **行号基准**：输出 **1-based**（与兄弟工具一致），并把「喂 `read_file_lines` 时
>    `offset = 行号 - 1`」**写死在给模型的文本里**（§3.1）。
> 5. **连带改一个既有工具**：给 `builtin_read_file_lines` 加 **默认关闭**的 `with_line_numbers`
>    → 精读时能核对行号（§3.2、§9.Q5）。这是本稿**唯一**动既有工具的点，默认值保证对
>    既有调用方**逐字零变化**。
> 6. 其余细节（`context_lines=2` / `max_matches=100` / `ignore_case` 开关 / 命中行 `>` 标记）按 §4 实施。

## 1. 为什么需要

`docs/planned-agent/flexible-step-tool-output-spill.md` 把超阈值产出落盘，并把**文件路径**当句柄交给下游
（跨步产出与步内工具输出都已生效）。但**取回手段只有顺序读**：

| 手段 | 能力 | 缺口 |
|---|---|---|
| `builtin_read_file_lines` | 按行范围读**单文件** | 定位靠 `offset` —— **猜**；想找「某关键词在哪」无能为力 |
| `builtin_head_file` / `builtin_tail_file` | 头 / 尾预览 | 同上 |
| `builtin_search_files` | 按**文件名** glob | 与内容无关 |

结果：「按需回读」退化成「按行扫」—— 前面省下的 token，又被来回扫描吃回去。
**缺的正是「按内容定位」**，即 grep 的语义。

## 2. 现状：grep 能力**存在**，但粒度不对口

`builtin_search_files_content`（`tool-manager/src/builtin/filesystem/search/content.rs`）就是内容搜索，
但它的粒度是**目录**，不是文件：

| 环节 | 事实 |
|---|---|
| 入口 | `content.rs:117` `resolved.open_dir()` → `:126` `walk_all(&root_dir, …)`；全程**遍历目录树**，无「单文件」分支 |
| `open_dir()` 语义 | `core.rs:44-50` 走 cap-std `Dir::open_dir(&self.rel)` —— **只接受目录**，传文件返回 `NotADirectory` |
| 失败表现 | `content.rs:121` 转成 `path_error("打开目录", …)` → 错误码 `not_a_directory` |
| 描述自己就这么写 | `contract.rs:396`「`path` 是搜索**根目录**」；schema `:410` 同 |
| 错误码清单也列了它 | `contract.rs:400` 含 `not_a_directory` |
| 测试全传目录 | `tests.rs:597` / `:615` / `:632` 都是 `path = dir.path()` |

即使退一步用「传目录」的形式，它在**本场景**下仍有 4 个不合用点：

1. **最要命**：落盘文件全都堆在**同一个 `run_dir`** 里（`<cache_dir>/<session_id>/run-*/`）。
   想搜 `tool-s2-r1-0.txt`，只能传该目录 —— 结果会把**同一次执行里所有落盘文件**的命中混在一起。
2. 结果封顶 `MAX_MATCHES = 500`（`content.rs:29`、`:202`），**无分页**。
3. **无上下文行** —— 只给命中行本身。
4. 输出前缀是**完整路径**（`content.rs:194`）。

> 结论：**不是「没有 grep」，是「grep 的形态不对口」** —— 缺「针对一个文件、可续读、带上下文」。

## 3. 闭环的两处摩擦（本稿新发现，必须一并处理）

落盘方案的原始取舍（`docs/planned-agent/filesystem-tools-rewrite.md:27`）是：旧 `builtin_read_file`
的 JSON `has_more`/`next_offset` 被照抄成裸文本后**消失**，改为「给总行数让模型自算下一页」。
这条心法要延续，但还有两个**现存**摩擦：

### 3.1 行号基准不一致

- `builtin_read_file_lines` 的 `offset` 是 **0-based**（`contract.rs:53`）。
- `builtin_search_files_content` 的行号是 **1-based**（`content.rs:199` `line_index + 1`；`contract.rs:399` 明说）。

若新工具沿用 1-based（与人直觉一致、与兄弟工具一致），则模型拿到「命中在第 42 行」后，
喂给 `read_file_lines` 要**减 1** —— 少这一步就整体错一行。

→ **决定（Q1）**：用 **1-based**，并把转换**写死在工具给模型的文本里**：
「行号从 1 开始；用 `builtin_read_file_lines` 精读时 `offset = 行号 - 1`」。
不指望模型自己推这层换算。

### 3.2 `read_file_lines` **不返回行号**

`contract.rs:54` 明文：「返回选中的行，行间用 `\n` 连接，**不带行号前缀**」；实现是
`read.rs:156` `lines[start..end].join("\n")`。

后果：精读时模型**无法核对自己读到第几行**，续读只能靠「上一次 limit」硬算 —— 而
`limit` 省略时是读到末尾（同一处文案已在 `flexible-step-tool-output-spill.md` 修正过）。
这让「grep 定位 → read 精读 → 再定位」的往返变得笨拙。

→ **决定（Q5）**：给 `builtin_read_file_lines` 加 `with_line_numbers`（默认 `false`）。
开启时输出「右对齐行号 + 空格 + `|` + 空格」（**与 `builtin_read_text_file` 既有格式一致**，不另造第二种行号写法）；
关闭时行为与现在**逐字相同** —— 既有调用方（含 `spill.rs` 的引用文案引导、整理步、chat）零变化。
已在 §6 列出改动点。

## 4. 契约

### 4.1 入参

| 参数 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `path` | string | 必填 | **单个文件**（传目录 → `is_a_directory`，与搜目录的工具分工明确） |
| `query` | string | 必填 | 要找的内容 |
| `is_regex` | boolean | `false` | `true` 时按正则解释（与兄弟工具同名同义） |
| `ignore_case` | boolean | `true` | 字面量搜索是否忽略大小写。**注意**：兄弟工具的字面量搜索**恒忽略大小写、无开关**（`content.rs:108`），本工具补上开关（`false` 要能精确匹配） |
| `context_lines` | integer | `2` | 每处命中前后各带 N 行（`0` = 只要命中行） |
| `max_matches` | integer | `100` | 本次最多返回几处命中（硬上限 `500`，与兄弟工具对齐） |
| `match_offset` | integer | `0` | **跳过前 N 处命中**（分页用）。**刻意不叫 `offset`** —— 那个名字在 `read_file_lines` 里是「行号」，同名不同义会诱发 off-by-one |

### 4.2 出参（文本，非 JSON —— 与工具族一致）

```
<absolute path>（共 1234 行）
>    42 | 命中行内容
     43 | 上下文行
     44 | 上下文行
---
> L118: 另一处命中
...
共 7 处匹配（已返回 7，续读：match_offset=7）
```

设计要点：

- **命中行用 `>` 标记、上下文行用空格标记；行号部分与 `builtin_read_text_file` 同格式（`{:>6} | `）** —— 行号始终给，便于核对与续读（§3.2）。
- **末尾必给「共 N 处 + 续读 `match_offset=`」** —— 延续 `docs/planned-agent/filesystem-tools-rewrite.md:27`
  的心法（给总数让模型自算下一页），不给总数模型就不知道还有没有。
- **首行给文件总行数** —— 精读时模型可以自算 `offset` 是否到头。
- 无命中时输出 `没有匹配的内容。` + 文件总行数（与兄弟工具的措辞对齐，`content.rs:212`）。
- 行号是 **1-based**；输出与描述里都要写清「用 `builtin_read_file_lines` 精读时 `offset = 行号 - 1`」（§3.1）。

### 4.3 上限与安全

| 项 | 值 | 依据 |
|---|---|---|
| 单文件大小 | `MAX_FILE_BYTES = 16 MiB` | 复用兄弟工具取值（`content.rs:31`），超限 → `content_too_large` |
| 命中数 | `max_matches` 默认 100、硬上限 500 | 与 `MAX_MATCHES = 500` 对齐（`content.rs:29`） |
| 二进制 | 用 `looks_binary` 跳过 → `binary_file` | 复用 `support.rs` |
| 编码 | 与 `read_text_file` 同一套（BOM sniff + `decode_text`） | 复用 `read.rs:178-198` 的既有函数 |
| 沙箱 | cap-std（`FilesystemService::resolve` + `dir.read`） | 与全族同源，无新面 |
| ReDoS | **不存在** —— `regex` crate 是线性时间引擎、无回溯 | 非 PCRE，无需额外超时 |

## 5. 与既有工具的分工（描述措辞，防模型混用）

两者必须让模型一眼分清**「一个文件」还是「一个目录」**：

| 工具 | 一句话定位 |
|---|---|
| `builtin_grep_file` | 「**在一个文件里**找内容，结果带行号与上下文，可续读；**读回大文件/落盘产出时用它定位，再用 `builtin_read_file_lines` 精读**」 |
| `builtin_search_files_content` | 「**在一个目录（含子目录）里**找内容，返回每个命中文件的 `路径:行号:列号`」 |

并在 `builtin_grep_file` 的描述里显式写上那句**两步读法**（定位 → 精读），因为它就是为落盘句柄设计的。

## 6. 接线点

| # | 位置 | 改动 |
|---|---|---|
| 1 | `builtin/filesystem/search/content.rs`（或新增 `search/grep.rs`） | 新增 `grep_file(service, arguments)`；`Matcher` 可复用（但要支持大小写敏感模式） |
| 2 | `builtin/filesystem/contract.rs` | 加 `GREP_FILE_DESCRIPTION` + `grep_file_schema()`；**并给 `READ_FILE_LINES_DESCRIPTION` 补 `with_line_numbers` 说明**（`:48-58`） |
| 3 | `builtin/filesystem/io/read.rs:118-171` | `read_file_lines` 读 `with_line_numbers`（默认 false），开启时按 `{:>6} | ` 前缀输出（复用 `support.rs` 的 `LINE_NUMBER_WIDTH`） |
| 3 | `builtin/filesystem/mod.rs` | **三处**：`tools()` 列表（`:55-234` 风格）、名称清单（`:263-285`）、分发 `match`（`:292+`） |
| 4 | `builtin/filesystem/tests.rs:805` | `assert_eq!(names.len(), 23, …)` → **24** |
| 5 | `flexible/exec/executor/mod.rs:435` | 整理步白名单 `["builtin_read_file_lines"]` → 加 `builtin_grep_file` |
| 6 | `flexible/exec/prompt.rs`（第 3 条） | 加「先搜定位、再按行精读」的两步读法 |
| 7 | 文档同步 | `docs/planned-agent/filesystem-tools-rewrite.md`（`:10`/`:58`/`:95`/`:126` 的「23 个」与清单）、`docs/tool-manager.md:403`「共 23 个」+ 工具表 |

> ⚠️ 顺手发现的**文档漂移**（既有问题，本次可一并修）：
> `docs/tool-manager.md:363` 写「`builtin_read_file_lines`（`offset` **0-based**、`limit` 默认 2000）」——
> 实现是**省略 `limit` 则读到文件末尾**（已在 `flexible-step-tool-output-spill.md` 修正代码侧文案，
> 但这份 doc 没同步）。

## 7. 提示词同步

`STEP_SYSTEM_PROMPT` 第 3 条（落盘引用读法）补「先定位后精读」：

```
需要它的内容时，先用 `builtin_grep_file` 按关键词定位（或直接
用 `builtin_read_file_lines` 分批读取，`offset` 从 0 开始，**显式传 `limit`**（如 2000）—— …）
```

**连带**：`exec/spill.rs` 的 `render_spill_reference` 里那句「读取方式」也要提一下 grep
（否则模型的默认读法仍是「从头按行翻」）。这是**回灌文案**与**提示词**两处同步——与上一轮同样
的「一处改、两处同步」教训。

## 8. 测试计划

**tool-manager 侧**（`builtin/filesystem/tests.rs`，风格对齐 `:591-638` 既有 3 例）：

| 用例 | 断言 |
|---|---|
| 基本命中 | 单文件命中 → 输出含 1-based 行号、命中行带 `>` 标记、末尾给「共 N 处 + `match_offset=`」 |
| 上下文行 | `context_lines=2` → 命中前后各 2 行且行号正确 |
| 分页 | `max_matches` 小于命中数 → 截断提示 + `match_offset` 续读能拿到**后续**命中（且不重不漏） |
| 传目录 | `is_a_directory` 错误码（与 `search_files_content` 的 `not_a_directory` 区分开，方向相反） |
| 正则 | `is_regex=true` 命中；非法正则 → `regex_error` |
| 大小写 | `ignore_case=false` 时不匹配不同大小写；`true` 时匹配 |
| 二进制 | 含 NUL 的文件 → `binary_file` |
| 大文件 | 超 `MAX_FILE_BYTES` → `content_too_large` |
| 无命中 | 输出「没有匹配的内容」+ 总行数，且**不是** error |
| 工具数 | `names.len() == 24`（`:805` 同步） |
| `with_line_numbers` 开 | 输出每行带 `{:>6} | ` 前缀（同 `read_text_file`），行号与 `grep_file` 对得上 |
| `with_line_numbers` 关 | 输出与改动前**逐字相同**（不传 / 传 `false` 两条路径都测） |

**flexible 侧**：

| 用例 | 断言 |
|---|---|
| 整理步拿到 grep | 整理步请求的 `tools` 里同时有 `builtin_read_file_lines` 与 `builtin_grep_file`（对齐 `executor/tests.rs:804` 的既有断言风格） |
| 第 3 条文案 | 提示词含「先…定位」两步读法（回归锁，防止后续被改回去） |

## 9. 决策（已全部确认）

| # | 问题 | 结论 |
|---|---|---|
| Q1 | 行号基准 | **1-based**；并把「`offset = 行号 - 1`」写进工具输出与描述 |
| Q2 | `context_lines` 默认 | `2`（上限 ≤10） |
| Q3 | `max_matches` 默认 | `100`（硬上限 500） |
| Q4 | `ignore_case` | 给开关，默认 `true`（兄弟工具没有这个开关，补在单文件版上） |
| Q5 | `read_file_lines` 加 `with_line_numbers` | **加**，默认 `false` —— 开启时与 `grep_file` 同格式；关闭时对既有调用方逐字零变化 |
| Q6 | 命中行标记 | `>`（与上下文行的 `  ` 前缀区分） |

**实施顺序建议**：先 `with_line_numbers`（独立、可单独验证零回归）→ 再新增
`builtin_grep_file`（注册三处 + `tests.rs:805` 的 23→24）→ 再接线（整理步白名单 + 提示词两处）→ 最后同步文档。

（**已按此顺序实施完毕**，见下。）

---

## 10. 落地记录（实施中与原稿的偏离）

三处偏离，都是动手时才暴露的事实：

1. **行号格式改为复用既有格式**。原稿让 `read_file_lines` 与 `grep_file` 都用新的 `L<行号>: ` 前缀；
   实施时发现 `builtin_read_text_file` **早就有** `with_line_numbers`（`contract.rs:18-19`），
   格式是「右对齐行号 + ` | `」（`io/read.rs:25` 的 `LINE_NUMBER_WIDTH`、`:78` 的渲染）。
   再引入一种写法会让工具族出现两种行号格式 → **改为复用同一格式**，并把列宽提升为
   `support.rs::LINE_NUMBER_WIDTH` 作为全族唯一来源。

2. **Windows 上「读目录」不是 `is_a_directory`，而是 `permission_denied`**
   （`io::ErrorKind::IsADirectory` 实际只在 Unix 出现）。实测踩到：契约写 `is_a_directory`
   会在 Windows 漂移，还会把模型误导成权限问题。**改为读取前显式判目录**
   （`resolved.dir.metadata(..).is_dir()`），返回平台无关的 `is_a_directory`，
   并在消息里指明「搜目录请用 `builtin_search_files_content`」（顺带做工具分流）。

3. **不复用 `io::read` 的私有 `decode_text`**。它的失败文案写死了「本工具不读取二进制」；
   `grep_file` 改为复用**同一组底层判定**（`sniff_encoding` + `decode` + `looks_binary`）——
   行为一致（BOM / 编码探测 / UTF-16 不被误判为二进制），措辞贴合「搜索」，
   且不改动既有工具的对外文案。

**测试基线**：`cargo test -p planned-agent-tool-manager --lib builtin::filesystem`
→ **52 passed / 0 failed**（含新增 4 例，以及 `every_registered_tool_is_dispatchable` 的 24 断言）。
