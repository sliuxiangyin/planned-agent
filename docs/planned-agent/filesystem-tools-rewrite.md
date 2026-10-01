# 内置 filesystem 工具重写（对齐 rust-mcp-filesystem）

> 设计稿 · 状态：**已实施**（2026-09 阶段 0–6 全部落地；见下方「实施记录」）
> 参考实现：`github.com/rust-mcp-stack/rust-mcp-filesystem`（v0.4.5，MIT，纯参考不引依赖）
> 取代：`docs/planned-agent/file-tools-redesign.md`（旧 4 工具契约，已作废）

---

## 实施记录（2026-09）

23 个工具全部落地在 `crates/tool-manager/src/builtin/filesystem/`；旧 `file_tools.rs`（58 KB）已删除，
其 `fs_support.rs` 搬为 `filesystem/support.rs`。

| 命令 | 结果 |
|---|---|
| `cargo test -p planned-agent-tool-manager --lib` | **96 passed / 0 failed** |
| `cargo test -p planned-agent --lib` | 169 passed / 3 failed（**既有失败**：`planner::coarse::llm_planner` 的 prompt 目录漂移，与本改动无关） |
| `cargo check -p planned-agent-gui` | 通过 |

**相对本设计稿的实测偏离**（以代码为准）：

| 项 | 本稿原计划 | 实测落地 | 原因 |
|---|---|---|---|
| 内容搜索 | `grep` crate | `regex` + 自写遍历 | 语义等价、依赖更轻 |
| 重复文件 | `sha2` 哈希 | size 分组 + 逐字节比对 | 零哈希依赖；`content_hash` 用的是 `DefaultHasher`（跨运行不稳定），**不能**用来查重 |
| 归档 | `zip` + `rc-zip` | 只用 `zip`（在内存里读写） | 少一棵依赖树 |
| `tail_file` | 反向 chunk 读 | 读全文后取尾部 | 实现更简单，受 64 MiB 上限保护 |
| 跨步骤读回**落盘产出** | `builtin_read_file`（JSON + `next_offset`） | `builtin_read_file_lines`（裸文本、0-based） | ③ 照抄的代价：`has_more` / `next_offset` 消失；改为在 spill 提示里给出**总行数**，让模型自算下一页。引用点已同步（`flexible/exec/{prompt.rs,executor/{mod,spill,tools}.rs,report.rs}`） |
| 根路径 | — | `resolve` 把空 `rel` 归一为 `.` | cap-std 对空路径做 `read_dir` / `create_dir_all` 会报 NotFound（实测踩到） |
| 子目录遍历 | — | 新增 `Resolved::open_dir()` | search / tree / zip_directory 若从沙箱根遍历，产出的相对路径会带多余前缀（实测踩到） |

测试基线：`filesystem/tests.rs` 31 例，覆盖沙箱边界、读写语义、编辑匹配规则、列举 / 树、搜索、统计、归档往返。

---

## 0. 拍板记录（用户 2026-09-30 决策）

| # | 决策 | 落地含义 |
|---|---|---|
| ① | **保留横切强化** | 你们有、它没有的东西一律保留：`tool_audit` 审计、`{error, message}` 结构化错误码、原子写、`MAX_*` 限额、BOM/编码探测、二进制探测 |
| ② | **引入 cap-std** | 采用它的**能力沙箱内核**（`cap_std::fs::Dir`），所有访问限定在允许目录内。注意：引的是 `cap-std` 这个独立 crate，**不是** rust-mcp-filesystem |
| ③ | **语义照抄，不保留旧语义** | 工具对外契约（工具名/schema/字段名/返回形态/偏移基准）以 rust-mcp-filesystem 为准；旧的 1-based offset、默认带行号、`mode`/`ensure_newline`/`create_parents` 等一律弃用 |
| ④ | 范围默认 | 全纳入（含 zip 归档、media 读取）；**不做** `list_allowed_directories` → 共 **23** 个工具 |
| ⑤ | 改名 | 工具名统一 `builtin_` 前缀并对齐上游名；4 处硬编码引用点一并改；49 个旧测试全部重写 |
| ⑥ | 模块分层默认 | 照搬它的 `fs_service` + `tools` 两层组织；`fs_support.rs` 搬进 `filesystem/` 作为你们自己的横切层 |

**① 与 ③ 的关系（本稿最关键的一条）**：

> **对外契约照抄 ③，实现层与横切能力保留 ①。**
> 即：模型看到的 schema / 返回格式 / 参数名 → 与 rust-mcp-filesystem 一致；
> 工具内部怎么落盘、怎么审计、怎么报错、怎么限额 → 用你们 ① 的成熟做法。

---

## 1. 目标 / 非目标

### 目标
1. `crates/tool-manager/src/builtin/` 下新建 `filesystem/` 目录，**所有**文件与目录操作收拢于此。
2. 以 rust-mcp-filesystem 的 23 个工具为准重写，对外名 `builtin_<上游名>`。
3. 引入 cap-std 沙箱：所有路径先 resolve 到某个允许目录，越界即拒。
4. 保留 ① 的审计 / 错误码 / 原子写 / 限额 / 编码兜底。

### 非目标（明确不做，避免误读为"引入它的库"）
- ❌ 不引入 `rust-mcp-filesystem` crate，不引入 `rust-mcp-sdk` / `rust-mcp-schema`。
- ❌ 不搬它的 `handler.rs` / `server.rs` / `main.rs` / `cli.rs` / `macros.rs` —— 那是 MCP 协议层（`ServerHandler` + `StdioTransport`），本项目已有 `rmcp` 通道（`crates/mcp-rmcp`）。
- ❌ 不引入 MCP 协议类型（`CallToolRequestParams` / `CallToolResult` / `TextContent`）。你们的工具契约是：
  `Tool { name, description, input_schema }` + `ToolResult { call_id, content: Value, is_error }`。
- ❌ 不做 `builtin_list_allowed_directories`。

---

## 2. 现状（改造前）

| 位置 | 内容 |
|---|---|
| `crates/tool-manager/src/builtin/file_tools.rs`（58 KB） | `FileToolsProvider` + `FileToolsExecutor` + 4 个工具（`builtin_read_file` / `builtin_write_file` / `builtin_edit_file` / `builtin_list_dir`） |
| `crates/tool-manager/src/builtin/fs_support.rs`（24 KB） | 20 个 `pub(crate)` 共享件：`tool_result` / `failure` / `clamped_u64` / `enum_arg` / `io_error_code` / `path_error` / `content_hash` / `strip_verbatim` / `resolved_path` / `detect_newline` / `normalize_newlines` / `apply_newline_style` / `TextEncoding` / `sniff_encoding` / `decode` / `BINARY_SNIFF_BYTES` / `looks_binary` / `atomic_write` / `line_number_width` / `render_numbered_line` / `truncate_at_char_boundary` |
| `crates/tool-manager/src/builtin/mod.rs` | 8 个 mod 平铺：`file_tools / text_tools / fs_support / system_tools / data_tools / ai_tools / web_tools / doc_tools` |
| `crates/agent-gui/src/context/tools/mod.rs:49` | 唯一生产注册点：`registry.register_builtin_provider(&FileToolsProvider);` |

**注意**：`fs_support` 只服务文件工具（`file_tools.rs` 引用它）；`system_tools.rs` 有一份**等价的私有拷贝**（`failure` / `clamped_u64`），刻意不复用（见 `fs_support.rs:8-10` 注释）。所以搬迁 `fs_support` 不影响别的工具。

---

## 3. 目标模块结构

照搬上游 `src/fs_service/{core,io,search,archive,utils}` + `src/tools/` 的两层划分，落进一个 `filesystem/` 目录：

```
crates/tool-manager/src/builtin/
├── mod.rs                          # 改：file_tools → filesystem
└── filesystem/
    ├── mod.rs                      # FilesystemProvider + FilesystemExecutor + 工具表（对外壳）
    ├── core.rs                     # FilesystemService：cap-std 沙箱 / 允许目录 / resolve / walk_dir
    │                               #   ← 移植上游 src/fs_service/core.rs
    ├── contract.rs                 # 23 个工具的 schema + description 定稿（唯一生效的契约）
    ├── support.rs                  # ← 现 fs_support.rs 搬入（① 的横切层：审计/错误码/原子写/编码/指纹）
    ├── io/
    │   ├── mod.rs
    │   ├── read.rs                 # read_text_file / read_file_lines / read_multiple_text_files
    │   │                           #   / head_file / tail_file / read_media_file / read_multiple_media_files
    │   ├── write.rs                # write_file / create_directory / move_file
    │   └── edit.rs                 # edit_file（含 git 风格 diff 生成）
    ├── search/
    │   ├── mod.rs
    │   ├── files.rs                # search_files（glob 名称搜索）
    │   ├── content.rs              # search_files_content（内容/正则搜索）
    │   └── tree.rs                 # directory_tree
    ├── list.rs                     # list_directory / list_directory_with_sizes
    ├── info.rs                     # get_file_info / calculate_directory_size
    │                               #   / find_duplicate_files / find_empty_directories
    └── archive/
        ├── mod.rs
        ├── zip.rs                  # zip_files / zip_directory
        └── unzip.rs                # unzip_file
```

**分层规则**（照搬上游）：
- **服务层**（`core.rs` + `io/` + `search/` + `list.rs` + `info.rs` + `archive/`）：纯业务，`impl FilesystemService` 挂方法，**不认识 `Value` / `ToolResult`**，返回 `Result<T, FsError>`。
- **契约层**（`contract.rs`）：schema + description 文本。
- **外壳层**（`mod.rs`）：把 `Value` 解释成调用、把服务层结果组装成 `ToolResult` + 写审计。

> 上游的 `support.rs` 对标 `src/fs_service/utils.rs`（`expand_home` / `parse_file_path` / 格式化等），但你们这里**语义不同**：它是"通用小工具"，你们是"审计+错误码+原子写"。命名取 `support.rs` 以免与上游 `utils` 混淆。

---

## 4. 工具清单：旧 → 新对照（23 个）

| # | 新工具名（`builtin_` 前缀） | 旧对应 | 形态 |
|---|---|---|---|
| 1 | `builtin_read_text_file` | `builtin_read_file` | 改名 + 语义重写 |
| 2 | `builtin_read_file_lines` | （旧 read 的 offset/limit 能力） | 新增（独立工具） |
| 3 | `builtin_read_multiple_text_files` | — | 新增 |
| 4 | `builtin_read_media_file` | — | 新增 |
| 5 | `builtin_read_multiple_media_files` | — | 新增 |
| 6 | `builtin_head_file` | — | 新增 |
| 7 | `builtin_tail_file` | — | 新增 |
| 8 | `builtin_write_file` | `builtin_write_file` | 同名 + 语义重写 |
| 9 | `builtin_edit_file` | `builtin_edit_file` | 同名 + 语义重写 |
| 10 | `builtin_create_directory` | — | 新增 |
| 11 | `builtin_move_file` | — | 新增 |
| 12 | `builtin_list_directory` | `builtin_list_dir` | 改名 + 语义重写 |
| 13 | `builtin_list_directory_with_sizes` | — | 新增 |
| 14 | `builtin_directory_tree` | — | 新增 |
| 15 | `builtin_search_files` | — | 新增 |
| 16 | `builtin_search_files_content` | — | 新增 |
| 17 | `builtin_get_file_info` | — | 新增 |
| 18 | `builtin_calculate_directory_size` | — | 新增 |
| 19 | `builtin_find_duplicate_files` | — | 新增 |
| 20 | `builtin_find_empty_directories` | — | 新增 |
| 21 | `builtin_zip_files` | — | 新增 |
| 22 | `builtin_unzip_file` | — | 新增 |
| 23 | `builtin_zip_directory` | — | 新增 |
| ✗ | ~~`builtin_list_allowed_directories`~~ | — | **不做**（④） |

全部归 `ToolCategory::File`（`crates/core/src/tool_registry/types.rs:22`，已存在，**`core` 零改动**）。

**消失的工具名**：`builtin_read_file`、`builtin_list_dir`（⑤：不留别名）。
**保留但语义变的工具名**：`builtin_write_file`、`builtin_edit_file`。

---

## 5. ①③ 交叉取舍：对外照抄 / 内部保留（核心章节）

> 实施前**逐工具核对上游源码** `src/tools/*.rs` 的 `#[mcp_tool]` schema（本表的参数名以该处为准，上游参数命名**并不统一**，见 §5.4）。

### 5.1 原则

| 层 | 归属 | 例子 |
|---|---|---|
| 工具名 / 参数名 / 参数类型 / 必填性 / 默认值 / 返回形态 | **③ 照抄** | `with_line_numbers`、`edits[].oldText`、裸文本返回 |
| 路径解析 / 越界拒绝 / 原子落盘 / 审计 / 错误码 / 限额 / 编码兜底 | **① 保留** | cap-std `resolve`、`atomic_write`、`tool_audit`、`content_too_large` |

### 5.2 逐工具取舍表

| 工具 | 对外（照抄上游） | 内部（保留 ①） | 相对旧契约的**净损失**（必须知情） |
|---|---|---|---|
| `read_text_file` | `{path, with_line_numbers?}`；返回**裸文本**；行号格式 `{:>6} \| <行>`（宽度 6、右对齐） | cap-std resolve；BOM 自动探测（UTF-8/UTF-16 内化，**不暴露 encoding 参数**）；二进制探测；`MAX_FILE_BYTES` 64 MiB 兜底；审计 | 旧 JSON 外壳全丢：`total_lines` / `has_more` / `next_offset` / `encoding` / `newline` / `bytes_read` / `truncated` / `size` / `modified` / `resolved_path`；行号渲染从「行号→」改为 `{:>6} \| ` |
| `read_file_lines` | `{path, offset(0-based), limit?}`；返回裸文本 | 同上的兜底与审计；上游无 `max_bytes` 参数 → 内部固定上限截断 | 旧 offset 是 **1-based**，改为 **0-based** → 所有依赖它的 prompt 与模型习惯要改 |
| `read_multiple_text_files` | `{paths: []}`；逐文件带路径返回 | 总输出限额（新增常量，如 1 MiB）；单文件走同一套编码/二进制兜底 | 新增 |
| `read_media_file` / `read_multiple_media_files` | `{path \| paths, max_bytes?}`；返回 Base64 + MIME | 审计（记 MIME + 字节数，**不记 Base64 正文**） | 新增；需新增依赖 `base64` + `infer` |
| `head_file` / `tail_file` | `{path, lines}` | 审计；`lines` 钳制上限 | 新增；`tail_file` 需反向读实现（上游 8 KB chunk 反扫，可直接移植算法） |
| `write_file` | `{path, content}`；**纯覆盖**（上游无 `mode`） | **原子写**（`atomic_write`：同目录 temp + rename）；`MAX_WRITE_BYTES` 10 MiB；审计（content 只记 hash） | 旧 `mode(overwrite\|append\|create_new)`、`create_parents`、`ensure_newline` **全部删除**；上游**不自动建父目录** → 需先用 `create_directory` |
| `edit_file` | `{path, edits: [{oldText, newText}], dryRun?, replaceAll?}`；返回 **git 风格 diff** | 原子写；审计（记 diff 大小 + hash，不记正文） | 旧 `{old_string, new_string, replace_all}` 与 `no_match`/`ambiguous_match` 保护**改由上游语义替代**（按整行序列匹配） |
| `create_directory` | `{path}`（含多级嵌套） | 审计 | 新增 |
| `move_file` | `{source, destination}`；**目标已存在则失败** | 审计；跨允许目录时校验边界 | 新增。**好消息**：Rust `std::fs::rename` 在 Windows 上目标已存在即失败，与上游语义天然一致（不静默覆盖） |
| `list_directory` | `{path}`；条目带 `FILE` / `DIR` 前缀 | 审计 | 旧 `entries: Vec<String>`（**连 is_dir 都没有**）变为带类型前缀的字符串数组 |
| `list_directory_with_sizes` | `{path}`；同上 + 大小 | 审计 | 新增 |
| `directory_tree` | `{path, max_depth?}`；返回 JSON 递归树 | **条目上限 + 深度上限**（防爆，①）；审计 | 新增 |
| `search_files` | `{path, pattern, excludePatterns?, min_bytes?, max_bytes?}`；大小写不敏感 glob | 遍历上限；审计 | 新增；需 `glob-match` |
| `search_files_content` | `{path, pattern, query, is_regex?, excludePatterns?, min_bytes?, max_bytes?}`；返回 path/行/列/预览 | 遍历上限 + 结果条数上限；审计 | 新增；需 `grep`（或自写 walk + regex） |
| `get_file_info` | `{path}`；size/mtime/权限/类型 | 审计 | 新增 |
| `calculate_directory_size` | `{root_path, output_format?}` | 遍历上限；审计 | 新增 |
| `find_duplicate_files` | `{root_path, pattern?, exclude_patterns?, min_bytes?, max_bytes?, output_format?}` | 审计 | 新增；**哈希方案见 §5.3** |
| `find_empty_directories` | `{path, exclude_patterns?, output_format?}` | 审计 | 新增 |
| `zip_files` / `unzip_file` / `zip_directory` | 见上游 `src/tools/zip_unzip.rs` | 审计；解压路径越界校验（防 zip-slip，cap-std 天然拦截） | 新增；需 `zip` + `rc-zip` |

### 5.3 `find_duplicate_files` 的哈希方案（待定，见 §12）

你们现有 `content_hash` 用 `DefaultHasher`（**非安全、跨运行不稳定**），不能用于查重。三个选项：
- **A（照抄上游）**：引入 `sha2`，按 SHA-256 分组。
- **B（零新依赖）**：按 `size` 分组 → 组内全字节比对。对大文件慢，但本地场景可接受，且**不需要新依赖**。
- **C**：`size` 分组 + `DefaultHasher` 预筛 + 全字节比对确认（降哈希依赖但仍非密码学）。

本稿**默认 A**（③ 照抄上游依赖），但标注为可替换点。

### 5.4 参数命名不统一（必须照抄，或显式统一）

上游的参数命名**混用 camelCase 与 snake_case**（例如 `edits[].oldText` / `dryRun` / `replaceAll` 是 camel，而 `exclude_patterns` / `min_bytes` / `max_bytes` / `output_format` / `root_path` 是 snake）。③ 照抄 → **连这种不统一一起照抄**。若改为统一 snake_case，属**主动偏离 ③**，需单独确认。

---

## 6. cap-std 沙箱与"允许目录"从哪来（② 的必答项）

### 6.1 服务构造

照搬上游 `FileSystemService::try_new(allowed_directories: &[String])` 的形态：

```rust
pub struct FilesystemService {
    allowed: RwLock<Arc<Vec<AllowedDir>>>,   // AllowedDir { path, unc_root, dir: cap_std::fs::Dir }
}

impl FilesystemService {
    pub fn try_new(allowed_directories: &[PathBuf]) -> Result<Self, FsError>;
    pub async fn resolve(&self, requested: &Path) -> Result<Resolved, FsError>;  // 越界即拒
}
```

`resolve` 返回 `Resolved { dir: Dir, rel: PathBuf, display: PathBuf, unc_root: Option<PathBuf> }`，
实际隔离由 cap-std 的 `Dir` 句柄承担（symlink 逃逸/悬空/被竞态替换都由 OS 层拒绝，而非靠路径字符串比较）。

### 6.2 允许目录来源（**本稿要拍板的第一项**）

你们现在**没有"允许目录"概念**（GUI 直接读写进程工作目录）。引入 cap-std 后必须有根。默认方案：

> **`FilesystemProvider::new(roots: Vec<PathBuf>)`，由 agent-gui 在启动时注入工作区根（或 cwd）。**

这是**现成模式**的直接复用 —— `DocToolsProvider::new(docs_dir)`（`crates/agent-gui/src/context/tools/mod.rs:55`）就是这么注入的。注册点相应变为：

```rust
registry.register_builtin_provider(&FilesystemProvider::new(workspace_roots));
```

- 单根：`vec![workspace_root]`
- 多根：cap-std 支持多个 `AllowedDir`，`resolve` 逐个尝试前缀匹配
- `roots` 为空时的行为：照搬上游 —— **拒绝一切访问**（上游在无根且有 roots 协议时等服务端提供；本项目无该协议，应直接报 `path_outside_allowed` 或启动期 `Err`）

**✅ 已拍板（2026-09-30）：`cwd` + 用户主目录 + 系统临时目录。**

单根 `cwd` 会造成**能力收窄**：旧 `builtin_read_file` 能读任意绝对路径
（`system-tools-redesign.md:196` 的测试注释显示「读用户桌面路径」是受支持用法），
收窄后模型读 `<cwd>` 之外的文件会直接拿到 `path_outside_allowed`。
落地在 `crates/agent-gui/src/context/tools/mod.rs` 的 `filesystem_sandbox_roots()`：

| 根 | 作用 |
|---|---|
| `std::env::current_dir()` | 保住「相对路径基于 cwd」既有语义；默认 `output_cache_dir`（`./data/cache`）的落点 |
| `USERPROFILE` / `HOME` | 覆盖桌面 / 下载 / 文档等既有用法 |
| `std::env::temp_dir()` | spill / 中间产物 |

零新依赖（**不引** `dirs`）；形态沿用本仓 `DirToolsProvider::new(docs_dir)` 的注入模式。

**未做（用户 2026-09-30 决定）**：不把 `output_cache_dir` 解析成绝对路径后并入 roots ——
其默认值 `./data/cache` 是相对路径，本就落在 `cwd` 之内；用户若自行改成绝对路径需自行承担。

### 6.3 Windows 与零 cfg 分支

- upstream 的 Windows 特殊性（verbatim `\\?\` 前缀、UNC `\\server\share`、Docker gateway 占位符）用 **运行时判断**（`is_unc_path()` / `strip_verbatim_prefix()`）而非 `#[cfg(windows)]` —— **沿用这一做法，保持本仓"零 cfg 分支"约定**。
- cap-std 在 Windows 上**不能枚举 UNC 目录** → 需要 `std::fs` fallback + 事后 containment 校验（照搬上游 `Resolved::read_dir_names` / `verify_unc_path` 的做法）。
- ⚠️ **cap-std 的 Windows 完备性未在本机实测**，列入 §11 阶段 0 的 spike。

---

## 7. ① 保留项细则

### 7.1 错误契约（不变）
- 可预期失败一律 `Ok(ToolResult { is_error: true, content: { "error": <code>, "message": <text> } })`；**不用 `Err`**（`Err` 只留给"未知工具名"这类编程错误）。
- 沿用 `io_error_code` 的映射，并**新增**新错误码：

| 新错误码 | 触发 |
|---|---|
| `path_outside_allowed` | 路径不在任何允许目录内 |
| `too_many_entries` | `directory_tree` / `find_*` 超条目上限 |
| `timeout` | 遍历类工具超时 |
| `regex_error` | `search_files_content` 的 `is_regex` 语法错 |
| `unsupported_media_type` | `read_media_file` 的 MIME 不在白名单 |
| `archive_error` | zip 读写失败 |

### 7.2 审计（不变）
每个工具写 `tracing::info!(target: "tool_audit", tool = "builtin_<name>", ..., duration_ms)`。
**铁律**：`content` 正文**不进日志**，只进指纹（`content_hash`）或尺寸；media 记 MIME + 字节数，**不记 Base64**。

### 7.3 限额（保留 + 新增）

| 常量 | 值 | 来源 |
|---|---|---|
| `MAX_FILE_BYTES` | 64 MiB | 保留 |
| `MAX_WRITE_BYTES` | 10 MiB | 保留 |
| `MAX_TREE_ENTRIES` | 新增（建议 10_000） | `directory_tree` / `find_*` / `calculate_directory_size` |
| `MAX_TREE_DEPTH` | 新增（建议 32） | `directory_tree` |
| `MAX_MULTI_READ_BYTES` | 新增（建议 1 MiB） | `read_multiple_text_files` |
| `MAX_SEARCH_RESULTS` | 新增（建议 1_000） | `search_files*` |
| `MAX_MEDIA_BYTES` | 新增（建议 10 MiB） | `read_media_file` |

### 7.4 编码 / 二进制 / 换行
`support.rs` 里的 `TextEncoding` / `sniff_encoding` / `decode` / `looks_binary` 保留为**内部兜底**：对外不暴露 `encoding` 参数（③），但内部按 BOM 自动探测、解码失败仍返回 `invalid_encoding`（①）。
`apply_newline_style` / `detect_newline`：因 `write_file` 的 `ensure_newline` 参数已删（③），这两个函数**降级为内部使用或删除**（§12 待定）。

---

## 8. 依赖变更（`crates/tool-manager/Cargo.toml`）

**现有**：core, tokio, serde, serde_json, anyhow, async-trait, tracing, chrono, uuid, which, sysinfo, readability-rust, htmd, scraper。

**新增（② + ③ 照抄所需）**：

| crate | 版本（上游） | 用途 | 可否省略 |
|---|---|---|---|
| `cap-std` | 4 | 沙箱内核（②） | **不可** |
| `dirs` | 6 | `~` 展开 | 可（自写 `std::env::var("HOME"/"USERPROFILE")`） |
| `glob-match` | 0.2 | glob 匹配 | 可（`search_files` 可退化为 substring） |
| `grep` | 0.3 | 内容搜索 | 可（自写 walk + `regex`） |
| `sha2` | 0.10 | `find_duplicate_files` | 可（见 §5.3 方案 B/C） |
| `base64` | 0.22 | media | 可（仅 media 用） |
| `infer` | 0.19 | MIME 探测 | 可（自写魔数表） |
| `similar` | "=2.7" | `edit_file` 的 git diff | 可（自写简单 diff） |
| `zip` | 2 | 压缩 | 可（去掉 archive 三件套） |
| `rc-zip` / `rc-zip-tokio` | 5 / 4 | 解压 | 可（同上） |
| `rayon` | 1.11 | 并行遍历 | 可（用 `tokio` 并发替代） |
| `futures` | 0.3 | stream 组合 | 可（`tokio` 已有） |
| `tokio-util` | 0.7 | 异步 IO 工具 | 可 |

> ⚠️ **依赖膨胀是本方案最大成本**：从"零文件相关依赖"变为最多 13 个新 crate（含一棵 `cap-std` 子依赖树）。§12 列为待拍板项 —— 特别是 `zip` / `rc-zip` / `rayon` / `sha2` 这几项，有零依赖替代路径。

---

## 9. 引用点改动清单（⑤）

改名后必须同步的点（`grep` 实测）：

**生产代码**
| 文件:行 | 内容 | 改法 |
|---|---|---|
| `crates/planned-agent/src/flexible/exec/executor/mod.rs:429` | `tool_definitions_for_names(&self.tools, &["builtin_read_file"])` | → `"builtin_read_text_file"`（**语义审查**：整理步用裸文本还是带行号？） |
| `crates/planned-agent/src/flexible/exec/prompt.rs:19` | STEP system prompt 文本指引 | → 换新名 + 新语义描述 |
| `crates/planned-agent/src/flexible/exec/executor/spill.rs:89` | 落盘说明指引 `builtin_read_file` | → 换新名 |
| `crates/planned-agent/src/flexible/exec/report.rs:92` | doc comment | → 换新名 |
| `crates/agent-gui/src/context/tools/mod.rs:49` | `register_builtin_provider(&FileToolsProvider)` | → `&FilesystemProvider::new(roots)` |

**测试代码**
| 文件:行 | 内容 |
|---|---|
| `crates/planned-agent/src/chat/trace.rs:208,209,217,317,319,327,334` | `builtin_read_file` / `builtin_write_file` 字符串 |
| `crates/planned-agent/src/chat/sub_agent/runner.rs:239,243` | `builtin_write_file` 字面量 |
| `crates/planned-agent/src/flexible/exec/executor/tests.rs:804` | `assert!(resolve_req.contains("builtin_read_file"))` |

**文档**（非阻塞，顺手更新）
`docs/tool-manager.md:355-358`、`docs/planned-agent/file-tools-redesign.md`、`docs/planned-agent/flexible-step-output-spill.md:220`、`docs/planned-agent/system-tools-redesign.md:195`、`crates/agent-gui/src/components/chat/RENDER_FLOW.md:306,322,324`。

---

## 10. 测试迁移

- **废弃**：`file_tools.rs` 34 个 `#[tokio::test]` + `fs_support.rs` 15 个 `#[test]`（合计 49）。
- **重写**：按新 23 工具重写，服务层方法（`impl FilesystemService`）单测 + 外壳层契约测（`ToolResult` 形态 + 错误码）。
- **新增必测**：
  - 越界路径被拒（`path_outside_allowed`）—— cap-std 沙箱的核心断言（含 symlink 逃逸）
  - 原子写失败不留半截文件
  - 审计字段存在且**不含正文**
  - `edit_file` 的 diff 输出形态
  - `move_file` 目标已存在 → 失败（Windows 语义）
  - 限额触发（`too_many_entries` / `content_too_large`）
- **回归命令**：
  ```powershell
  cargo test -p planned-agent-tool-manager --lib
  cargo test -p planned-agent --lib          # 注意既有 3 个 coarse::llm_planner 失败与本次无关
  cargo test -p planned-agent-gui --bins
  ```

---

## 11. 分期计划

| 阶段 | 内容 | 完成标志 |
|---|---|---|
| **0. spike** | 最小验证 cap-std 在本机（Windows）能否 open/resolve/枚举/rename；确认 UNC 限制 | 一个独立 bin/测试跑通，结论写回本稿 |
| **1. 骨架** | 建 `filesystem/` 目录；搬 `fs_support.rs` → `support.rs`；实现 `core.rs`（cap-std 服务）；`FilesystemProvider::new(roots)`；`mod.rs` 接线；改注册点 | 空工具表能注册；`cargo build` 过 |
| **2. 读写编辑** | `io/{read,write,edit}.rs` + 对应契约；**语义切换**（裸文本 / 0-based / `edits[]`） | 三件套测试绿 |
| **3. 目录类** | `list.rs`（`list_directory` / `_with_sizes`）、`search/tree.rs`、`create_directory`、`move_file`、`get_file_info` | 测试绿 |
| **4. 搜索类** | `search/{files,content}.rs`、`find_duplicate_files`、`find_empty_directories`、`calculate_directory_size` | 测试绿 |
| **5. 归档/媒体** | `archive/{zip,unzip}.rs`、`read_media_file(s)`、`read_multiple_text_files`、`head_file`/`tail_file` | 测试绿 |
| **6. 收口** | 改 §9 全部引用点 + prompt；更新文档；全量回归 | 三处 `cargo test` 与基线一致 |

### 阶段 0 结论（✅ 已完成）

- `cap-std = "4"` 已引入 `crates/tool-manager`，编译通过。
- `crates/tool-manager/tests/cap_std_contract.rs`（4 例全绿；原为阶段 0 的 spike，后正名为契约测试）验证本机 Windows 上：
  - `Dir::open_ambient_dir` / `write` / `read_to_string` / `create_dir_all` / `read_dir` / `rename` / `metadata` / `remove_file` **全部可用**；
  - `..` 逃逸被拒；**symlink 逃逸被拒**（本机有 symlink 权限，用例真实执行、未跳过）。
- ⚠️ **未验证**：UNC 共享路径（本机无可用共享）。上游的 `std::fs` fallback 仍按 §6.3 实现。
- ⚠️ **环境（重要）**：本机 cargo 内嵌 libcurl/schannel 连 `index.crates.io` 稳定握手失败（`curl.exe` / `Invoke-WebRequest` 同域名同 TLS 栈均 200 OK，0.5s），故新增项目级 `.cargo/config.toml` 指向 rsproxy 稀疏镜像绕开。`Cargo.lock` 内的 registry 记录**未改变**（source replacement 不写回 lock）。

---

## 12. 风险与待拍板

| # | 项 | 说明 | 建议 |
|---|---|---|---|
| R1 | **cap-std Windows 完备性** | 未实测；`Dir` 在 Windows 的部分操作受限（UNC 枚举已知不支持，需 `std::fs` fallback） | 阶段 0 必须先 spike |
| R2 | **依赖膨胀** | 最多 13 个新 crate（§8） | 逐项确认：`sha2`/`zip`/`rc-zip`/`rayon`/`similar` 是否接受 |
| R3 | **允许目录语义** | 根从哪来？单根还是多根？工作区根 vs cwd | 默认：GUI 注入工作区根（仿 `DocToolsProvider::new`） |
| R4 | **`read_text_file` 返回裸文本** | 丢 `has_more`/`next_offset`/`encoding` 等字段 → 分页续读信息依赖模型自己算 | 确认接受；或保留一个**极简** JSON 外壳（偏离 ③） |
| R5 | **工具数 4 → 23** | prompt 工具表显著变长 → token 成本上升 | 用 `allowed_tools` 分类裁剪；或按需 `-d` 式禁用（见下） |
| R6 | **工具粒度** | 上游"多而细"，你们原为"少而精" | 已按 ③ 照抄；若后续要折叠（`head`/`tail` 并进 `read_file_lines`），需单独决策 |
| R7 | **`edit_file` 语义替换** | 现有 prompt 教模型用 `old_string`/`new_string` | 阶段 6 一并改 prompt，否则模型会用旧参数名 |
| R8 | **是否需要"工具禁用"开关** | 上游有 `-d/--disable-tools`；你们 `ToolMetadata.enabled` 已存在但**内置工具被 `is_locked` 锁死**（GUI 不可切换） | 若 23 个工具太占 token，考虑放行内置工具的可禁用性 |

---

## 附：与上游的对应关系速查

| 上游 | 本方案 |
|---|---|
| `src/fs_service/core.rs` | `filesystem/core.rs` |
| `src/fs_service/io/{read,write,edit}.rs` | `filesystem/io/{read,write,edit}.rs` |
| `src/fs_service/search/{files,content,tree}.rs` | `filesystem/search/{files,content,tree}.rs` |
| `src/fs_service/archive/{zip,unzip}.rs` | `filesystem/archive/{zip,unzip}.rs` |
| `src/fs_service/utils.rs` | `filesystem/support.rs`（**语义不同**：你们是审计/错误码/原子写层） |
| `src/tools/*.rs`（`#[mcp_tool]`） | `filesystem/contract.rs`（schema + description） |
| `src/tools.rs`（`tool_box!`） | `filesystem/mod.rs`（provider + executor + 分派） |
| `src/handler.rs` / `server.rs` / `main.rs` / `cli.rs` | **不移植**（MCP 协议层，本项目用 `rmcp`） |

---

## 13. 交付检查结论（2026-09-30）

**代码**：`crates/tool-manager/src/builtin/filesystem/`（23 工具族）已取代 `file_tools.rs`（该文件已删除）。
`builtin/mod.rs`、`agent-gui` 注册点、`flexible/exec/*` 与 `chat/trace.rs` 的引用点全部同步。

**验证基线**：

| 命令 | 结果 |
|---|---|
| `cargo test -p planned-agent-tool-manager --lib` | **89 passed / 0 failed** |
| `cargo test -p planned-agent --lib` | 169 passed / 3 failed（3 个均为 `planner::coarse::llm_planner` **既有失败**，与本次无关） |
| `cargo check -p planned-agent-gui` | 通过（仅既有 warnings） |
| `cargo build --workspace` | 通过 |
| `cargo clippy -p planned-agent-tool-manager --lib --tests` | `filesystem/` 模块**零 warning** |

**死代码清理**（交付检查的产出）：`support.rs` 从 24 KB 收到 14 KB —— 删掉旧 `fs_support.rs` 里面向
`std::fs` 的 11 个函数（`atomic_write` / `strip_verbatim` / `resolved_path` / `detect_newline` /
`normalize_newlines` / `apply_newline_style` / `line_number_width` / `render_numbered_line` /
`truncate_at_char_boundary` / `clamped_u64` / `enum_arg`）与 10 个测它们的用例；
`FsError` 从 14 个变体收到**实际构造**的 6 个；同理删掉未被调用的 `FilesystemService::allowed_directories`。

**文档同步**：`docs/tool-manager.md`、`crates/tool-manager/ANALYSIS.md` 工具表已更新；
`file-tools-redesign.md`（加作废标注）、`flexible-step-output-spill.md`、`flexible-execution-hardening.md`、
`system-tools-redesign.md` 已加勘误块（都因引用旧名/旧参数基而失效）。

**遗留（已知，未做）**：

- tool-manager 仍有 7 条 clippy **style** warning（`core/registry.rs` / `web_tools.rs` / `doc_tools.rs` /
  `adapter/custom.rs` / `sub_agent/executor.rs`），均**非本次引入**，未顺手改以控制 diff 范围。
- `.cargo/config.toml`（rsproxy 稀疏镜像）是本机拉包握手失败时的临时手段，**删除即恢复**默认源
  （它被 `.gitignore` 忽略，不进提交）。
- cap-std 的 Unix 权限语义**未在 Windows 本机实测**（本次只有 Windows 侧结论）。
- **`cargo test -p planned-agent-tool-manager`（不带 `--lib`）会 hang** 在既有集成测试
  `tests/sub_agent_stream.rs`（`cargo test --workspace` 因此也跑不通）。与本次改造无关
  （`src/sub_agent/` 未改），已记入根 `AGENTS.md` §5。
- **`reasonix.toml` 被权限系统改写了**：该文件是 `[permissions] allow` 命令白名单，本次会话批准过的
  命令被自动追加（diff 里可见几十条历史命令）。**不是人工编辑**，但会污染仓库 diff ——
  不想要就 `git checkout reasonix.toml`。
