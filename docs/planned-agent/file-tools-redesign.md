# `file_tools.rs` 重新设计（评审稿）

> **缘起**：一份外部粘贴的《文件工具设计优化指南》（通用 LLM Agent 最佳实践清单）。
> 本稿不做「指南的抄录」，只做**指南 vs 本仓库既有硬约定**的对齐：能采纳的采纳、冲突的说明为什么不采纳、
> 指南自身矛盾处修正。形态对齐 `docs/planned-agent/system-tools-redesign.md`（同一套约定：错误码表 /
> description 定稿 / 影响面 / 测试密度）。
>
> **状态**：4 项拍板已定稿（§0.3），实施分阶段见 §6。**代码尚未改动。**

---

## 0. 一页结论

### 0.1 直接采纳（无争议，多为补真实缺口）

| # | 采纳项 | 现状（核对过的代码事实） | 为什么 |
|---|---|---|---|
| A1 | **结构化失败**：可预期失败一律 `Ok(ToolResult { is_error: true, content: {error, message} })`，不用 `Err` | `file_tools.rs` 三个分支全部 `Err(anyhow!)`（`:75` / `:86` / `:99`） | 上层把 `Err` 拍平成字符串，**错误码与结构全丢**（`system_tools.rs:290-292` 已定此约定，见 §3.3） |
| A2 | `read_file` 分块读 `offset` / `limit` | `file_tools.rs:75` 直接 `read_to_string` 全量入内存，无上限 | 大文件 OOM / 爆上下文 |
| A3 | 行号前缀 + `total_lines` / `has_more` | 无 | 让模型能说「第 100–150 行」并知道有没有后续 |
| A4 | 二进制检测（前 8 KiB 含 NUL 字节） | 无（读 `.png` 得到乱码或 `InvalidData`） | 零依赖可做 |
| A5 | 编码**严格失败**，不回退 lossy | `read_to_string` 对非 UTF-8 本来就报错 | 保持现状 + 补结构化错误码；静默替换字符比报错更危险 |
| A6 | `write_file` 的 `mode` 三态（overwrite / append / create_new） | 只有覆盖 | 让「新建」与「覆盖」成为两个不同意图，避免误覆盖 |
| A7 | `write_file` 原子写（同目录 temp + rename） | `file_tools.rs:88` `std::fs::write`（create + truncate + write），崩溃留半截 | 半截文件比不写更糟 |
| A8 | `write_file` 的 `content` 上限 10 MiB | 无 | 与 `max_output_bytes` 上限一致（`system_tools.rs:34`） |
| A9 | `create_parents`（默认 true） | 无（父目录不存在直接失败） | 模型常写新路径 |
| A10 | **新增 `builtin_edit_file`（精确替换原语）** | 全仓不存在（已 grep） | 收益最大：没有它，模型只能 read → write 全量重写 |
| A11 | 审计日志（`target: "tool_audit"`） | file 工具无 | 仓库已有现成 target 约定（`system_tools.rs:667`）；`content` 只记 hash |
| A12 | 参数钳制在实现里做 | `clamped_u64` 是 `system_tools.rs:338` 的**私有函数**，file 工具用不了 | schema 的 default/min/max 运行时不生效（§1.2 修正后的事实） |

### 0.2 不采纳（与仓库硬约定冲突，理由见 §2）

| # | 不采纳 | 一句话理由 |
|---|---|---|
| N1 | **工作区隔离** / `path_denied`（指南 §2.2⑧ / §5.1 / §5.2） | `system-tools-redesign.md:50`（P3）与 `:195-201`（C5）**已拍板不做**：file/web/data 工具全都没沙箱，只锁一处是「有安全感的假象」，正确落点是仓库级 policy |
| N2 | `encoding` 支持 `gbk`（指南 §2.1 / §3.1） | 需新依赖 `encoding_rs`，与仓库「零新依赖」拍板冲突（`system-tools-redesign.md:71` Q4） |
| N3 | `builtin_delete_file`（指南 §4.5） | 破坏性操作，属「资源政策」，与 `kill_process` 护栏同类（P3）；不在首批 |
| N4 | 依赖 `additionalProperties: false` 拦参数 | 本仓库 `ToolValidator` **不读**该字段（`system-tools-redesign.md:42` N4；代码见 `crates/tool-manager/src/core/validator.rs:11-56`）。写上无害，但只是给模型的提示 |
| N5 | `with_line_num` 开关（指南 §2.1） | 与指南 §2.2② 自相矛盾（行号归展示层却给模型开关），模型必误用；形态固定（§0.4-②） |

### 0.3 拍板结果（本轮定稿）

| # | 决策 | 结论 |
|---|---|---|
| **Q1 范围** | 做多大 | **核心 + 地基**：阶段 1 结构化错误码/审计，阶段 2 `read_file`，阶段 3 `write_file`，阶段 4 `edit_file`。`list_dir` 增强 / `search_files` / `file_info` 留待后续（§8） |
| **Q2 路径隔离** | 是否做工作区隔离 | **不做**（沿用 P3）。只保留低成本部分：路径回显（已有 `fs_error`，`file_tools.rs:132`）+ 审计记 `resolved_path`。隔离另立仓库级设计 |
| **Q3 read 返回形态** | `lines:[{no,text}]` vs 文本 | **`content` 放带行号文本 + 结构化元信息**（最省 token）。取舍与后果见 §0.4-③ / §3.1 |
| **Q4 编码支持** | 支持到哪一档 | **零新依赖**：UTF-8 严格 + BOM 检测（UTF-8 / UTF-16LE / UTF-16BE），其余 `invalid_encoding`。`gbk` 不纳入 |

### 0.4 对指南本身的修正（指南有缺陷，不是本仓库的取舍）

| # | 指南的问题 | 修正 |
|---|---|---|
| ① | **行号基准自相矛盾**：§2.1 说 `offset` 是 0-based，§2.3 的返回示例 `lines[].no` 从 **1** 开始 | 统一为 **1-based**：`offset` 语义 = 「从第几行开始读」，`content` 里的行号也是 1-based。同基准才不会 off-by-one（模型的「第 42 行」心智就是 1-based） |
| ② | **`with_line_num` 与 §2.2② 矛盾** | 砍掉该参数（见 N5） |
| ③ | **没估 token 成本**：§2.2② 承认「更省 token 的呈现」更优，§2.3 的 schema 却是完整结构化 | 采拍板 Q3。**代价**：带行号的 `content` 是**派生视图**，不是原文 → 见 §3.1「后果」 |
| ④ | **§5.4 说「你已经有 `clamped_u64`，复用即可」是错的** | 它是 `system_tools.rs:338` 的文件内私有函数；需提升为 `builtin/mod.rs` 级共享 helper（`mod.rs:1-7` 当前只声明 7 个模块） |
| ⑤ | **§2.2⑦ 表述含糊**：把 `max_bytes`（单次返回上限）和「文件总量不限制」混在一句 | 正确语义：**单次返回受 `max_bytes` 限，靠 `offset` 续读**；没有「总量」概念 |
| ⑥ | §7「和 `execute_command` 的关系」写得好，但漏了本仓库的既有邻居 | 必须同时与 `builtin_text_search` / `builtin_text_replace`（`text_tools.rs:16/75`，**内存字符串**操作，不落盘）划清边界，否则模型会混用 |

---

## 1. 现状盘点（代码事实，全部核对过）

### 1.1 三个工具

`crates/tool-manager/src/builtin/file_tools.rs`（`FileToolsProvider`，`file_tools.rs:9`；执行器 `FileToolsExecutor`，`:66`）：

| 工具 | 入参 | 现状返回 | 已知问题 |
|---|---|---|---|
| `builtin_read_file`（`:16`） | `path` | `{content}` | 全量读入内存（`:75`）、无行号、无上限、无二进制/编码处理、失败走 `Err` |
| `builtin_write_file`（`:30`） | `path` / `content` | `{success:true}` | 非原子写（`:88`）、无 mode、无 `create_parents`、无大小上限、失败走 `Err` |
| `builtin_list_dir`（`:45`） | `path` | `{entries:[name]}` | 无 `recursive` / `pattern` / `limit`；失败走 `Err`（`:99`） |

三个 `description` 都是旧文案（`"读取文件内容（内置工具）"` 之类），schema 字段**全部无 description**。

### 1.2 两个新发现

**① 文档漂移（既有，非本次引入）**：`crates/tool-manager/ANALYSIS.md:48` 与 `docs/tool-manager.md:355-357` 对 file 工具的描述停留在「读取/写入/列出」；`ANALYSIS.md:184` 甚至声称含「创建、删除」——实际没有。实施后需回填（§5.4）。

**② 比指南更严重的契约事实（本次新核实，必须写进设计）**：

`system_tools.rs:17-20` 的注释说「schema 的 default/min/max 运行时不生效，实现里自己兜」。核对 `crates/tool-manager/src/core/validator.rs:11-56` 后，实际行为还要更硬：

- **`required` 缺失 → 直接返回 `Err` 并短路**（`validator.rs:18`），**根本到不了 executor**。
  也就是说：模型漏传 `path` 时，得到的是被上层拍平的字符串
  `"Error: Missing required field 'path' for tool 'builtin_read_file'"` —— **没有错误码**。
- 字段**类型**不符 → 只 `warn!`（`validator.rs:44`），继续执行（executor 必须自己再判类型）。
- `default` / `minimum` / `maximum` / `enum` / `minLength` / `additionalProperties` **一个都不看**（`validator.rs:55` 直接 `Ok(())`）。
- 调用点：`crates/tool-manager/src/core/registry.rs:620` 与 `:716`（`ToolRegistry::call_tool` 路径）。

**对本设计的三条硬约束**：

1. 新 schema 的 `required` 必须**最小化**（只放真正必填项），因为凡进 `required` 的字段，其缺失错误都走 `Err` 通道、丢错误码 —— 那是一段我们**不打算**在本次改动里动的公共路径（registry/validator 层）。
2. 每个可选参数的默认值与上下限**必须在实现里自己兜**（含上限钳制）。
3. `edit_file` 的 `new_string` **必须放进 `required`**，同时允许空字符串（表达「删除该片段」）—— 因为「缺失」会被 validator 拦成 `Err`，而「空串」需要能通过。

### 1.3 共享件与注册链路

- **模块声明**：`crates/tool-manager/src/builtin/mod.rs:1-7`（7 个 `pub mod`）+ `:10` re-export `BuiltinToolProvider`。新增共享 helper 模块要在此登记。
- **注册点（全仓唯一）**：`crates/agent-gui/src/context/tools/mod.rs:45-55`，`ToolsContext::init` 逐个 `register_builtin_provider`。若 `edit_file` 拆成独立 provider，需在此加一行。
- **可复用样板**：`system_tools.rs` 的 `tool_result`（`:324`）、`failure`（`:333`）、`clamped_u64`（`:338`）、`tool_audit`（`:667`）。
- **依赖**：`crates/tool-manager/Cargo.toml:23-24` 的 `[dev-dependencies]` **只有 `tokio`** —— 没有 `tempfile`（§5.3）。

### 1.4 基线

```
cargo test -p planned-agent-tool-manager --lib   →  14 passed（system-tools-redesign.md:113 记录）
```

---

## 2. 指南 vs 本仓库：逐条冲突

### C1 —— 工作区隔离（指南 §2.2⑧ / §5.1 / §5.2）→ **不做**

指南把两件事混在一个方案里，本稿必须拆开（拆开之后结论更清楚）：

| | 是什么 | 本仓库现状 | 结论 |
|---|---|---|---|
| **隔离** | 拒绝工作区外的绝对路径 / `..` 逃逸（安全） | 无任何实现 | **不做**（P3 拍板）。做的话是跨 crate 改造：`FileToolsExecutor` 是无状态 unit struct（`file_tools.rs:66`），`ToolRegistry` 无配置注入面，必须改注册点（`agent-gui/src/context/tools/mod.rs:45-55`）注入根目录 |
| **基准** | 相对路径相对谁来解析（可用性） | **进程 cwd**（`std::fs::read_to_string` 直接把路径交给 OS） | **本次不做**，但**必须在 description 里如实说明**（§3.1）：相对路径 = 进程当前工作目录，建议调用方用绝对路径 |

`FileToolsProvider` 是 unit struct，注入根目录 = 注入配置 = 与「隔离」是**同一个改造**（都要求 provider 有状态、都有注册点改动）。既然隔离已拍板不做，基准也一并留待后续，但**不能不说**：否则模型会以为相对路径基于「工作区」，这是静默的错误前提。

### C2 —— `gbk` / 宽编码支持（指南 §2.1 / §3.1）→ **零新依赖**

指南的 `encoding` 枚举含 `gbk`，需要 `encoding_rs`。仓库在 `system-tools-redesign.md:71`（Q4）明确选了零新依赖方案（Windows `.cmd` 兼容用 `which`、进程树回收用 `taskkill` 都是这个思路）。本次：UTF-8 严格 + BOM 检测覆盖 UTF-16LE/BE，够用；`gbk` 列为后续独立小改动（§8）。

### C3 —— `builtin_delete_file`（指南 §4.5）→ **不做**

删文件是破坏性操作，指南自己都说「必须谨慎」。它和 `kill_process` 的护栏问题同类：属「资源政策」，正确落点是仓库级，不在 file 工具改造里夹带。

### C4 —— `clamped_u64`「复用即可」→ **私有函数，需提升**

见 §0.4-④。落点：新建 `crates/tool-manager/src/builtin/fs_support.rs`，把 `failure` / `clamped` / 编码探测 / 二进制探测 / 原子写 / 换行风格 收在一处（§4.1）。

### C5 —— 行号基准与 `with_line_num` → 修正

见 §0.4-①②。

### C6 —— 返回形态的 token 成本与「字节保真」→ 采拍板 Q3，并显式记录后果

见 §3.1「后果」。

---

## 3. 优化后的契约

### 3.1 `builtin_read_file`

**JSON Schema**

```json
{
  "type": "object",
  "properties": {
    "path": {
      "type": "string",
      "description": "文件路径。相对路径基于进程当前工作目录（不是工作区根）；建议传绝对路径。"
    },
    "offset": {
      "type": "integer",
      "minimum": 1,
      "default": 1,
      "description": "起始行号（从 1 开始）。大文件配合 limit 分批读取。"
    },
    "limit": {
      "type": "integer",
      "minimum": 1,
      "maximum": 10000,
      "default": 2000,
      "description": "本次最多读取的行数，默认 2000，上限 10000。"
    },
    "encoding": {
      "type": "string",
      "enum": ["auto", "utf-8", "utf-16le", "utf-16be"],
      "default": "auto",
      "description": "编码。auto 先看 BOM 再按 UTF-8 严格解码；解码失败返回 invalid_encoding，不会静默替换字符。"
    },
    "max_bytes": {
      "type": "integer",
      "minimum": 1024,
      "maximum": 10485760,
      "default": 262144,
      "description": "本次返回的最大字节数（默认 256 KiB，上限 10 MiB）；超出部分截断并置 truncated。"
    }
  },
  "required": ["path"]
}
```

> `required` 只有 `path`（§1.2 硬约束 1）。所有 default/min/max 在实现里自己兜。

**成功返回**

```json
{
  "path": "src/main.rs",
  "resolved_path": "D:/code/planned-agent/src/main.rs",
  "content": "   1→fn main() {\n   2→    println!(\"hi\");\n   3→}",
  "offset": 1,
  "limit": 2000,
  "total_lines": 340,
  "has_more": false,
  "next_offset": 341,
  "encoding": "utf-8",
  "newline": "lf",
  "bytes_read": 4096,
  "truncated": false,
  "size": 4096,
  "modified": "2026-09-28T10:23:00Z"
}
```

- `content`：**派生视图** —— 行号右对齐（宽度 = `total_lines` 十进制位数，至少 4）+ `→`（U+2192）+ 行文本，行间以 `\n` 连接。
- `newline`：`lf` | `crlf` | `mixed`（原文风格）。
- `resolved_path`：`canonicalize` 成功时的真实路径，失败（如文件已删）则为 `null`。**仅审计/展示用**，不做任何准入判断（Q2 不做隔离）。
- `has_more`：`(offset - 1) + 本次返回行数 < total_lines`。
- `next_offset`：续读起点 = `offset + 本次返回行数`。若它等于 `offset`，说明单行本身就超了 `max_bytes`（此时 `truncated: true`），应提高 `max_bytes` 而不是继续 `offset`。

**`content` 是派生视图的三条后果（必须写清，否则是隐患）**

1. 它**不是**原文的字节等同物：加了行号前缀、换行统一成 `\n`、可能被截断。
2. 因此 **`read_file` → `write_file` 的往返会破坏文件**（行号进正文、CRLF 变 LF）。
   —— 这恰好是 `edit_file` 存在的理由，也是 description 里要写明的调用纪律：**改文件用 `edit_file`，不要 read → write 往返**。
3. 若将来确实需要字节保真读（如二进制/base64、或纯原文），加 `raw: true` 参数另开一条路径（§8 留待后续）。

**description（定稿：中文、不注入平台信息）**

```
读取文本文件，按行返回并带行号前缀。

调用规则：
1. path 是文件路径；相对路径基于进程当前工作目录解析（不是工作区根），建议传绝对路径。
   path 指向目录时返回 is_a_directory，请改用 builtin_list_dir。
2. 默认从第 1 行开始、最多读 2000 行。大文件用 offset + limit 分批读；
   has_more 为 true 表示后面还有内容。
3. 返回的 content 是带行号的呈现（每行前缀「行号→」，行号从 1 开始、与 offset 同基准），
   行间用 \n 连接；原文换行风格见 newline 字段（lf / crlf / mixed），content 内不保留原文换行。
4. 本工具返回的是派生视图，不是原文：要修改文件请用 builtin_edit_file，
   不要 read_file + write_file 往返（会把行号写进正文、把 CRLF 改成 LF）。
5. 二进制文件（前 8 KiB 含 NUL 字节）拒绝读取，返回 binary_file 与 size。
6. 非 UTF-8 文件按 encoding 处理：默认 auto（先看 BOM）；无法解码返回 invalid_encoding，
   不会静默替换字符。需要 GBK 等编码请先用其它工具转码。
7. 单次返回受 max_bytes 限制，超出时截断并置 truncated=true，续读起点见 next_offset。
8. 失败返回错误码：file_not_found / permission_denied / is_a_directory / invalid_encoding /
   binary_file / invalid_arguments / internal_error。
```

### 3.2 `builtin_write_file`

**JSON Schema**

```json
{
  "type": "object",
  "properties": {
    "path": {
      "type": "string",
      "description": "文件路径。相对路径基于进程当前工作目录；建议传绝对路径。"
    },
    "content": {
      "type": "string",
      "description": "写入内容（UTF-8 文本），上限 10 MiB。"
    },
    "mode": {
      "type": "string",
      "enum": ["overwrite", "append", "create_new"],
      "default": "overwrite",
      "description": "overwrite 覆盖或创建；append 追加到末尾（不存在则创建）；create_new 仅在文件不存在时创建，已存在则报 already_exists。"
    },
    "create_parents": {
      "type": "boolean",
      "default": true,
      "description": "父目录不存在时是否自动创建。"
    },
    "ensure_newline": {
      "type": "string",
      "enum": ["preserve", "lf", "crlf", "none"],
      "default": "preserve",
      "description": "换行风格。preserve 沿用现有文件风格（新文件用 lf）；lf / crlf 强制；none 原样写入。"
    }
  },
  "required": ["path", "content"]
}
```

**成功返回**

```json
{
  "path": "src/new.rs",
  "resolved_path": "D:/code/planned-agent/src/new.rs",
  "mode": "create_new",
  "bytes_written": 128,
  "created": true,
  "replaced": false,
  "newline": "lf"
}
```

**description（定稿）**

```
写入文本文件。overwrite / append 走原子写入（同目录临时文件 + rename，失败不留半截文件）。

调用规则：
1. path 是文件路径；相对路径基于进程当前工作目录解析（不是工作区根），建议传绝对路径。
2. mode 三选一，请按意图明确选择：
   overwrite（默认）覆盖或创建；append 追加到末尾（不存在则创建）；
   create_new 仅在文件不存在时创建，已存在返回 already_exists。
3. content 按 UTF-8 写入，上限 10 MiB；超限返回 content_too_large，请分块写。
4. 换行：preserve（默认）沿用现有文件风格（新文件、探测不到换行时用 \n）；需要强制 LF / CRLF 请显式指定。
5. create_parents 默认 true，缺失的父目录会自动创建。
6. 只改文件的一处/几处内容时，请用 builtin_edit_file，不要用本工具整份重写
   （整份重写要求你把整个文件内容完整重新生成，容易静默丢内容）。
7. 失败返回错误码：already_exists / permission_denied / content_too_large /
   is_a_directory / invalid_arguments / disk_full / internal_error。
```

### 3.3 `builtin_edit_file`（新增）

**JSON Schema**

```json
{
  "type": "object",
  "properties": {
    "path": {
      "type": "string",
      "description": "文件路径。相对路径基于进程当前工作目录；建议传绝对路径。"
    },
    "old_string": {
      "type": "string",
      "description": "要被替换的原文片段，必须与文件内容逐字节一致（含缩进与换行）。默认要求在文件中唯一出现。"
    },
    "new_string": {
      "type": "string",
      "description": "替换后的内容；空字符串表示删除 old_string。"
    },
    "replace_all": {
      "type": "boolean",
      "default": false,
      "description": "为 true 时替换所有匹配项；默认 false，此时 old_string 必须唯一，否则报 ambiguous_match。"
    }
  },
  "required": ["path", "old_string", "new_string"]
}
```

> `new_string` 必须进 `required`（§1.2 硬约束 3）：缺失会被 validator 拦成 `Err`，而「删除片段」要靠**空串**表达。

**成功返回**

```json
{
  "path": "src/main.rs",
  "resolved_path": "D:/code/planned-agent/src/main.rs",
  "replacements": 1,
  "bytes_written": 4112,
  "newline": "crlf"
}
```

**description（定稿）**

```
在文本文件中精确替换一个片段。只改动匹配到的内容，文件其余字节原样保留 —— 这是修改已有文件的首选方式。

调用规则：
1. path 是文件路径；相对路径基于进程当前工作目录解析（不是工作区根），建议传绝对路径。
2. old_string 必须与文件内容逐字节一致（含缩进、换行、空格）。
   默认要求它在文件中唯一出现：找不到返回 no_match，出现多次返回 ambiguous_match
   （此时请补足上下文让片段唯一，或显式设 replace_all=true）。
3. new_string 为空字符串表示删除 old_string 这段内容。
4. 锚点是内容而不是行号 —— 行号会因其它编辑漂移，内容不会。
5. 写入走原子路径，失败不留半截文件。
6. 失败返回错误码：no_match / ambiguous_match / file_not_found / permission_denied /
   is_a_directory / invalid_arguments / internal_error。
```

### 3.4 失败返回与错误码表

统一形态（与 `system_tools` 一致）：

```json
{ "error": "<code>", "message": "<人话>" }
```

放在 `ToolResult { is_error: true, content }` 里，**不用 `Err`**。

| `error` | 触发 | 模型该怎么办 |
|---|---|---|
| `invalid_arguments` | 参数类型/取值非法（`required` 缺失走不到这里，见 §1.2） | 重填参数 |
| `file_not_found` | 读时文件不存在 | 核对路径（**message 必须回显路径**，见 §4.7） |
| `already_exists` | `create_new` 时文件已存在 | 换 mode 或换路径 |
| `is_a_directory` | 对目录做文件操作 | 改用 `builtin_list_dir` |
| `invalid_encoding` | 编码检测/解码失败 | 换 encoding，或先转码 |
| `binary_file` | 前 8 KiB 含 NUL 字节 | 换工具（本工具不读二进制） |
| `content_too_large` | `content` 超 10 MiB | 分块写 |
| `no_match` | `edit_file` 找不到 `old_string` | 重新读该片段后再改 |
| `ambiguous_match` | `edit_file` 的 `old_string` 出现多次 | 补足上下文，或 `replace_all` |
| `permission_denied` | 无权限 | 提示用户 |
| `disk_full` | 写入时磁盘满 | 提示用户 |
| `internal_error` | 其它 | 记录并上报 |

> `path_denied` 预留给将来的隔离（Q2 不做），本稿**不实现**。

### 3.5 `is_error` 语义

沿用 `system-tools-redesign.md:385`（P2）的思路：**「文件不存在」这类可预期失败 = 工具层失败**（`is_error: true` + 错误码），
而不是 `Err`。`Err` 只留给「未知工具名」这种编程错误（`file_tools.rs` 现状即如此）。

---

## 4. 实现层设计（要点与陷阱）

### 4.1 共享 helper 的落点

新建 `crates/tool-manager/src/builtin/fs_support.rs`，在 `builtin/mod.rs:1-7` 登记。收纳：

- `tool_result` / `failure` / `clamped_u64`（从 `system_tools.rs:324-343` 提取或复制，**注意 `system_tools.rs` 的改动要单独评估**，避免顺手重构扩大 diff）
- 编码探测（BOM + 严格 UTF-8）
- 二进制探测（前 8 KiB 查 NUL）
- 换行风格探测（`lf` / `crlf` / `mixed`）与转换
- 原子写（temp + rename）
- 行号渲染（右对齐 + `→`）

一切 helper 都是纯函数或只碰 FS，便于单测。

### 4.2 编码与 BOM 检测（Q4）

顺序：

1. 有 BOM → 按 BOM 定编码并**剥掉 BOM**：`EF BB BF` → UTF-8；`FF FE` → UTF-16LE；`FE FF` → UTF-16BE。
2. 无 BOM 且 `encoding: auto` → 按 UTF-8 **严格**解码（`std::str::from_utf8`）。
3. 失败 → `invalid_encoding`（**不回退 `from_utf8_lossy`**）。
4. 显式 `encoding` 时跳过探测，按指定编码解（`utf-16le/be` 用 `u16::from_le_bytes/from_be_bytes` 手工拼，零依赖）。

UTF-16 解码需处理**奇数长度**（报 `invalid_encoding`）与**代理对**（`char::decode_utf16`）。

### 4.3 二进制检测

读**前 8 KiB**，含 `0x00` 即判二进制 → `binary_file`（附 `size`）。这与 `system_tools` 的 `CappedBytes`（`system_tools.rs:350-354`）思路一致：先小样本判定，再决定怎么读。

注意顺序：**二进制判定要在编码解码之前**（否则 UTF-16 文本也会因 NUL 被误判 —— 所以有 BOM 的 UTF-16 要先按 BOM 认下来，见 §4.2-1）。

### 4.4 换行风格

- 探测：统计首个 `\n` 前的 `\r` 有无 → `lf` / `crlf`；两种都出现 → `mixed`。
- **`read_file` 不改写原文换行**（`content` 是派生视图，行间固定 `\n`，风格只由 `newline` 字段表达）。
- **`write_file` 的 `ensure_newline`**：
  - `preserve`：目标存在 → 沿用其风格；不存在 → `lf`。
  - `lf` / `crlf`：把 `content` 里的 `\r\n` / 孤立 `\r` 归一后按目标风格重排（避免 `\r\r\n`）。
  - `none`：原样写入。
- **不自动补末尾换行**（指南 §3.2⑤ 的结论）：工具不替调用方做决策。是否补由调用方在 `content` 里自决。

### 4.5 原子写（Windows 陷阱）

1. 在**同目录**建临时文件（`.{name}.tmp.{pid}{rand}`）—— 跨文件系统 rename 不原子，所以必须在同目录。
2. 写入 → `flush` → `sync_all`。
3. `std::fs::rename` 覆盖目标。Rust 的 `fs::rename` 在 Windows 上走 `MoveFileEx(MOVEFILE_REPLACE_EXISTING)`，**可以覆盖已存在文件**。
4. 失败清理临时文件。

**Windows 特有陷阱（必须处理）**：目标文件被其它进程占用（编辑器、杀软、索引器）时 `rename` 会失败 →
必须**回结构化错误**（`permission_denied` 或 `internal_error`，message 说明「目标可能被占用」），
不能静默失败，也不能退回非原子直写。

**`append` 不走原子重写**（指南 §3.2③）：`OpenOptions::new().append(true).create(true)`，语义就是「打开-定位末尾-写」。
**`create_new` 靠 `create_new(true)` 天然原子**。

### 4.6 参数钳制（schema 不生效）

`clamped_u64(arguments.get("limit"), 2000, 1, 10000)` 之类，全部在实现里做（§1.2 硬约束 2）。
`mode` / `encoding` / `ensure_newline` 这些 `enum` 值：**validator 不看 enum**，实现里遇未知值要走 `invalid_arguments`，
不能默默按默认值处理（否则模型拼错枚举值时会得到一个「看起来成功但语义不对」的结果）。

### 4.7 路径回显（保留既有精神）

既有 `fs_error`（`file_tools.rs:132`）已做对了一件事：`std::fs` 的 `io::Error` 只说「系统找不到指定的路径。 (os error 3)」，
**不说是哪个路径** —— 调用方只能靠反复换写法试错（真实案例见 `file_tools.rs:145-147` 的测试注释）。

新实现保留并强化：
- `message` 里**必带路径**；
- `io::ErrorKind::NotFound` → 追加「（该路径不存在，请核对拼写）」；
- 错误码由 `io::ErrorKind` 映射（`NotFound`→`file_not_found`、`PermissionDenied`→`permission_denied`、`AlreadyExists`→`already_exists`、其它→`internal_error`）。

既有测试 `missing_path_error_echoes_the_path`（`file_tools.rs:142-160`，注释在 `:145-146`）**保留**，并扩到新错误码形态（断言 `error` 字段）。

### 4.8 审计

沿用 `system_tools.rs:667` 的 `tracing::info!(target: "tool_audit", …)`：

| 工具 | 记 |
|---|---|
| `read_file` | `path` / `resolved_path` / `offset` / `limit` / `lines` / `bytes_read` / `encoding` / `truncated` / `duration_ms` |
| `write_file` | `path` / `resolved_path` / `mode` / `bytes_written` / `created` / `replaced` / `newline` / **`content_hash`** / `duration_ms` |
| `edit_file` | `path` / `resolved_path` / `replacements` / `bytes_written` / `content_hash` / `duration_ms` |

**`content` 本身不进日志**（可能含敏感信息），只记 `content_hash`。
`content_hash` 用 `std::collections::hash_map::DefaultHasher`（std，**零依赖**）—— 它只用于「同内容可对账」，
不用于安全用途，故不必上 `sha2`。

---

## 5. 影响面

### 5.1 代码

已 grep 核实：**除工具自身外，全仓没有代码依赖这三个工具的返回结构。**

- `crates/planned-agent/src/chat/trace.rs:208-209`、`runner.rs:239-243`：只是把工具名当**字符串**用（`"builtin_write_file"`），不解析返回体 → 不受影响。
- `crates/agent-gui/src/context/tools/mod.rs:49`：注册点。`edit_file` 若并入 `FileToolsProvider` 则**无需改**。
- `system_tools.rs` 的 `failure`/`clamped_u64`：若提取为共享 helper，属于**本次唯一碰到其它工具文件**的改动，建议**复制**而非重构 `system_tools.rs`，把 diff 限制在 file 工具内（见 §4.1）。

### 5.2 UI

`crates/agent-gui/src/components/chat/tool_view/component.rs:40-42` 是通用渲染（`serde_json::to_string_pretty(result)`）。

- **不破坏**：返回结构变了，但 UI 只做 pretty-print，无字段依赖。
- **副作用**：展开 Output 时 JSON 变长（`read_file` 多了元信息）。这正是拍板 Q3 选「文本 content」的原因之一 ——
  若选 `lines:[{no,text}]`，这段 pretty JSON 会膨胀得更厉害。

### 5.3 测试

- 基线：`cargo test -p planned-agent-tool-manager --lib` → 14 passed。
- **缺口**：`crates/tool-manager/Cargo.toml:23-24` **没有 `tempfile`**。原子写 / CRLF / 二进制 / 边界这类测试需要隔离的临时文件 →
  建议加 `tempfile` 到 `[dev-dependencies]`（仅测试依赖，与「生产零新依赖」不冲突，见 Q4 的语义）。
- 风格对齐 `system_tools.rs`：`#[tokio::test]` + `exec()` helper（断言**不该**返回 `Err`）+ 断言 `content["error"]` 错误码。

### 5.4 文档回填（否则仓库文档自相矛盾）

| 文档 | 要改什么 |
|---|---|
| `docs/tool-manager.md:349-364` | file 工具表补充新参数与错误码；新增 `builtin_edit_file` 行 |
| `crates/tool-manager/ANALYSIS.md:48`、`:184` | 修正「创建、删除」的不准确描述；补 `fs_support.rs` |
| `crates/agent-gui/src/components/chat/RENDER_FLOW.md:306-324` | 举例里 `builtin_read_file` 的返回形态 |

---

## 6. 实施步骤

| 阶段 | 内容 | 交付 |
|---|---|---|
| **1 地基** | `fs_support.rs`（`failure`/`clamped`/编码/二进制/换行/原子写/行号渲染）；三个工具的错误路径全改 `Ok + is_error`；错误码映射；审计 | 错误码断言测试 |
| **2 `read_file`** | `offset`/`limit`/`total_lines`/`has_more`/行号/`encoding`+BOM/二进制拒绝/`max_bytes` 截断/元信息（`size`/`modified`/`resolved_path`） | 分块、二进制、非 UTF-8、边界（空文件 / 单行无换行 / CRLF / mixed） |
| **3 `write_file`** | `mode` 三态/原子写/`ensure_newline`/`create_parents`/10 MiB 上限/返回 `bytes_written`·`created`·`replaced` | 原子性（中途失败不留半截）、`create_new` 已存在、CRLF preserve、`disk_full`（可 mock 则做） |
| **4 `edit_file`** | 唯一匹配 / `no_match` / `ambiguous_match` / `replace_all` / 内部走原子写 | 四类结果 + 原子性 |
| **5（后续）** | `list_dir` 增强（`recursive`/`pattern`/`limit`）、`search_files`、`file_info`、`raw: true`、`gbk` | 见 §8 |

每阶段独立可验收；`cargo test -p planned-agent-tool-manager --lib` 保持在起步线以上，
并在阶段 4 后跑一次 `cargo test -p planned-agent-gui --bins` 确认 GUI 侧未破。

---

## 7. 测试清单（按 `system_tools.rs` 密度）

**读**
- 正常：小文件、行号前缀格式、`total_lines`/`has_more` 正确
- 分块：`offset` 越界（> `total_lines`）→ 空 `content` + `has_more:false`，不报错
- 边界：空文件（`total_lines:0`）、单行无末尾换行、CRLF、mixed
- `max_bytes` 截断 → `truncated:true`
- 二进制：PNG 头 → `binary_file`
- 编码：UTF-8 BOM（剥 BOM）、UTF-16LE/BE BOM、非法 UTF-8 → `invalid_encoding`、奇数长 UTF-16 → `invalid_encoding`
- `path` 是目录 → `is_a_directory`
- 不存在 → `file_not_found` 且 **message 含路径**（保留既有测试扩写）
- `limit` 超上限被钳到 10000（验证 §4.6）

**写**
- `overwrite` 创建 / 覆盖；`created`/`replaced` 标志正确
- `append`：不存在则创建、存在则追加（且内容不重排）
- `create_new`：已存在 → `already_exists`
- `create_parents`：false 且父目录缺失 → 结构化错误；true → 成功
- 原子性：写入失败（可用只读目录/占用句柄）后目标文件**保持原内容**
- 换行：`preserve` 在 CRLF 文件上保持 CRLF；新文件用 LF；`lf`/`crlf`/`none` 各一例
- `content` 超 10 MiB → `content_too_large`（用小上限 override 避免真造 10 MiB）

**编辑**
- 唯一匹配替换成功、`replacements:1`
- `no_match`、`ambiguous_match`
- `replace_all` 替换全部
- `new_string` 为空 → 删除片段
- 改 CRLF 文件 → 其余部分字节不变（`git diff` 干净这一性质的测试化）

**通用**
- 未知 `mode`/`encoding`/`ensure_newline` 枚举值 → `invalid_arguments`（§4.6）
- 三个工具都不返回 `Err`（除未知工具名）

---

## 8. 不做 / 留待后续

| 项 | 理由 | 归属 |
|---|---|---|
| 工作区隔离（`path_denied`、`allowed_root`） | P3 已拍板；需跨 crate 注入根目录 | **仓库级 policy 设计**（另立文档） |
| 相对路径基准改为「工作区根」 | 与上面的注入改造是同一件事 | 同上 |
| `builtin_delete_file` / 软删除 | 破坏性 + 资源政策 | 独立评估 |
| `gbk` / 更多编码 | 需 `encoding_rs` | 独立小改动 |
| `list_dir` 增强 / `search_files` / `file_info` | 配套工具，非核心 | 阶段 5 |
| `read_file` 的 `raw: true`（字节保真） | 有需求再加 | 阶段 5 |

---

## 附：与 `execute_command` 的分工（指南 §7 采纳）

| | `builtin_execute_command` | file 工具 |
|---|---|---|
| 用途 | 构建、测试、git 等**外部工具** | 文件**读写编辑** |
| `cat` / `grep` / `sed -i` | 能跑，但：结构化差、无原子性、平台相关（Windows 无 `sed -i`） | `read_file` / `search_files` / `edit_file` 分别替代 |
| 边界 | 不要用 shell 做高频文件操作 | 不要用文件工具跑外部程序 |

---

## 9. 实施记录

**状态：阶段 1–4 已落地**（`fs_support.rs` 新增、`file_tools.rs` 重写、四个工具 + 审计）。测试：`cargo test -p planned-agent-tool-manager --lib` → **87 passed**（新增 44 个用例：32 个工具用例 + 12 个共享件单测）。

### 9.1 实现期对设计稿的三处收紧（记录在案）

| # | 设计稿写法 | 实现 | 理由 |
|---|---|---|---|
| I1 | `read_file`「文件总量不限制、按行分块」（§2.2⑦ / §0.4-⑤） | 加了 **64 MiB 的 `MAX_FILE_BYTES` 硬上限**，超限返回 `content_too_large` | 给出 `total_lines` 与行号必须先读全文；假装不限只会变成 OOM。更大的文件请走其它手段 |
| I2 | （设计稿未明确）`edit_file` 的编码范围 | **只支持 UTF-8**（UTF-8 BOM 会被保留） | 写回时保持 UTF-16 需要重新编码，收益不足以抵消复杂度；UTF-16 文件返回 `invalid_encoding` 并提示转码 |
| I3 | `list_dir` 在阶段 1「只改错误路径」 | 顺带把返回结构统一为 `{ path, resolved_path, entries, count }` | 与其余三个工具风格一致（§3.4 的返回风格统一意图） |

### 9.2 落点

| 文件 | 内容 |
|---|---|
| `crates/tool-manager/src/builtin/fs_support.rs`（新） | 共享件 + 12 个单测 |
| `crates/tool-manager/src/builtin/file_tools.rs`（重写） | 四工具 + schema/description 定稿 + 审计 + 32 个测试 |
| `crates/tool-manager/src/builtin/mod.rs` | 登记 `pub(crate) mod fs_support;` |
| `crates/tool-manager/Cargo.toml` | `[dev-dependencies] tempfile = "3"`（**生产依赖零新增**：编码/哈希/原子写全用 std + 既有 `uuid`/`chrono`） |
| `docs/tool-manager.md`、`crates/tool-manager/ANALYSIS.md` | 按 §5.4 回填 |

### 9.3 §5.4 的一处修正

`crates/agent-gui/src/components/chat/RENDER_FLOW.md:306-324` **实际无需改动** —— 那里的示例只出现工具**名**，没有展开返回结构。保留此条是为了留下判断依据。

### 9.4 仍未做（归属见 §8）

工作区隔离、`builtin_delete_file`、`gbk`、`list_dir` 的 `recursive`/`pattern`/`limit`、`builtin_search_files`、`builtin_file_info`、`read_file` 的 `raw: true`。
阶段 1–4 完成后，file 工具进入「分块读 + 精确编辑 + 原子写 + 统一错误码/审计」的生产级状态。

### 9.5 独立审查后的修复

对这批改动跑了一次独立只读审查（结论无 blocking），据此修掉：

| 严重度 | 问题 | 修复 |
|---|---|---|
| should-fix | `edit_file` 全量读入却无大小上限（`read_file` 有 64 MiB 兜底，它没有）→ 超大文件 OOM | 加同样的 `MAX_FILE_BYTES` 检查，返回 `content_too_large` |
| should-fix | `create_new` 分支：`create_new(true)` 已建出文件，随后写失败**不清理** → 留半截文件，且下次同路径变 `already_exists` | 失败路径先 `drop(file)` 再 `remove_file`（Windows 上占用中的文件删不掉，顺序不能反） |
| nit | 10 MiB 上限在换行重排**之前**按 `content.len()` 判，而 `ensure_newline=crlf` 会把 `\n` 扩成 `\r\n`，实际写入接近两倍 | 校验移到重排后的 `bytes` 上，并挪到**创建父目录之前**（否则超限返回时会留下「建了目录却没写文件」的副作用） |
| nit | 首行截断时 `returned = 0`，模型没有续读依据 | 新增 `next_offset` 字段（见 §3.1）；等于 `offset` 时提示提高 `max_bytes` |
| nit | `preserve` 在探测不到换行时退化为 `lf`，会改写调用方给的 `\r\n` | 在 §3.2 的 `description` 与本节写明该退化行为 |

审查同时确认这些**不是**缺陷：分页边界（`offset > total_lines` / 空文件 / 恰好读到末行）、`truncate_at_char_boundary` 不会切多字节字符、`edit_file` 的 `matches` 与 `replacen` 计数一致、`apply_newline_style` 不会产出 `\r\r\n`、`atomic_write` 两路失败都会清临时文件、`&bytes[bom_len..]` 不会越界、日志不泄漏 `content`。

修复后 `cargo test -p planned-agent-tool-manager --lib` → **89 passed**。

---

## 10. 跨平台行为核对（系统环境对这组工具的影响）

**问题**：这四个工具在不同 OS（Windows / Linux / macOS）上行为是否有差异、是否需要在 system prompt 里区分系统环境？

**代码事实**：`file_tools.rs` 与 `fs_support.rs` 里**没有任何 `#[cfg(windows)]` / `#[cfg(unix)]`**（全仓的 cfg 分支都在 `system_tools.rs`）；唯一的平台相关逻辑是 `strip_verbatim`（**运行期判前缀**，不是编译期分支）。

**结论：不需要在 prompt 层为这四个工具区分系统环境。** 与 `execute_command` 形成对照：

| | 输入本质 | 需要平台事实吗 |
|---|---|---|
| `builtin_execute_command` | 命令名（`dir` / `ls` / `taskkill`）= **OS 属性** | 是 → 由「执行期运行环境段」提供（`system-tools-redesign.md` 第四轮终审） |
| `read/write/edit/list_dir` | 换行 / 编码 / 权限 / 路径 = **文件属性** | 否 |

换行是**文件的属性**、不是 OS 的属性 —— 正确行为取决于「这个文件是什么风格」，与「你在什么系统」无关。这是这四个工具比 `execute_command` 干净得多的根本原因。

### 10.1 已消化的差异（0 个平台分支）

| 差异 | 处理方式 |
|---|---|
| 换行 | `detect_newline` 按**文件内容**判 lf/crlf/mixed，不查 OS |
| 读目录报错 | Linux 报 `IsADirectory`、Windows 报 `PermissionDenied` → 读之前**显式 `metadata.is_dir()`** 前置判断，两端统一 `is_a_directory`（不做这步错误码就会随 OS 漂移） |
| Windows `\\?\` 路径 | `strip_verbatim` 运行期剥前缀，让 `resolved_path` 与其它平台同风格 |
| rename 覆盖 | Windows 走 `MoveFileEx(REPLACE_EXISTING)`；目标被占用则失败 → 映射 `permission_denied` + 「若文件正被其它程序占用」提示 |
| 文件名非法字符 / 大小写敏感 | 交给 OS，错误统一映射 |

### 10.2 有意统一掉的差异（是决策，不是 bug）

| 项 | 行为 | 拍板 |
|---|---|---|
| 新建文件换行 | 一律 **LF**（不看 OS） | **保持统一**：与「file 工具不区分系统环境」一致，已在 description 声明 |
| `write_file` 的 BOM | 不写 BOM（`edit_file` 保留原 BOM） | **维持现状**：`content` 是调用方给的全量内容，工具不替它猜 |
| `edit_file` 编码 | 只支持 UTF-8 | 既定（Q4 零新依赖）。Windows 上 GBK/UTF-16 文件会走 `invalid_encoding`，属已知代价 |

### 10.3 本次补上的缺口：Unix 权限保留

`atomic_write` 原来用 `File::create(&tmp)` 建临时文件 → 权限是 umask 默认（通常 `0644`），`rename` 覆盖后**原文件的 `0600` / `0755` 被静默放宽**。Windows 的 ACL 不随 rename 变化，所以**只在 Unix 上暴露**。

修法（已经落地）：

- 覆盖前取 `std::fs::metadata(path).ok().map(|m| m.permissions())`（不存在则 `None` → 新建文件走默认权限）；
- 写入后 `file.set_permissions(...)` 复制到临时文件；
- **刻意用跨平台 API、不加 `#[cfg(unix)]`** —— `#[cfg]` 掉的代码在非 Unix 平台连语法/类型错都发现不了，而 `Permissions` / `set_permissions` 在 Windows 上也有意义（保留只读标志）；
- 顺序必须放在 `write_all` **之后**：Windows 上先把文件置为只读，后续写入就会失败。

回归：`cargo test -p planned-agent-tool-manager --lib` → **90 passed**。
⚠️ 其中 Unix 权限的两条断言是 `#[cfg(unix)]`，**在 Windows 开发机上被跳过、未在本机实测** —— 需要一次 Linux/macOS 上的 `cargo test -p planned-agent-tool-manager --lib` 确认（测试名 `atomic_write_preserves_existing_permissions` / `atomic_write_creates_new_files_with_default_permissions`）。
