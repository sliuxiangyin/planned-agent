# GUI 全局缓存根目录（需求分析与设计）

> 状态：**已实施**（D1–D4 已确认并落地；验证：`cargo test -p planned-agent-gui --bins` → **103 passed**）
> 相关：`crates/agent-gui/src/config/`（每类配置一个文件）、`context/kv.rs`、`context/storage.rs`、`context/rag.rs`、
> `services/run_service.rs`、`docs/planned-agent/flexible-step-output-spill.md` §5.2

## 1. 需求

| # | 需求 | 结论 |
|---|---|---|
| R1 | 配置里有一个**全局缓存根目录**，所有落盘路径都拼在它后面 | 用户确认 |
| R2 | 覆盖范围：sled KV、SQLite 库、flexible 步骤产出、RAG 向量库；**不含** GUI 日志 | 用户确认 |
| R3 | 各子配置项**保留**，默认值改成「根下的子目录」，仍可单独覆盖 | 用户确认 |
| R4 | **不做旧路径兼容**：只改新默认值，旧数据留在原处，读不到就当全新 | 用户确认 |
| R5 | 先出本设计稿，确认后再实施 | 用户确认 |

## 2. 现状

四个落盘点**各自写一个相对路径**，解析逻辑有三份不同的实现（都在 GUI 侧，都以**进程 cwd** 为基准）：

| 用途 | 配置项（`config/` 目录） | 当前默认值 | 解析处 | env 覆盖 |
|---|---|---|---|---|
| SQLite 库（SeaORM） | `storage.db_path`（`config/storage.rs`） | `./data/agent-gui.db` | `context/storage.rs:92` `resolve_db_path` | `PLANNED_AGENT_DB_PATH` |
| sled KV（MCP config/status 也在其中） | `cache.path`（`config/cache.rs`） | `./data/kv_store` | `context/kv.rs:62` `resolve_cache_path` | `PLANNED_AGENT_CACHE_PATH` |
| RAG 向量库（PolarisDB） | `rag.store.path`（`config/rag.rs`） | `./traces/vector_store` ← **不在 `data/` 下** | 无解析：`context/rag.rs:35` 把字符串直接交给 `PolarisDbStore::open(path, 1024)` | 无 |
| flexible 步骤产出 | `flexible.output_cache_dir`（`config/flexible.rs`） | `./data/cache`（执行时再拼 `/<session_id>`） | 无解析：`services/run_service.rs:61` 直接 `PathBuf::from(...)` → `ExecutorConfig.cache_dir` | 无 |

不在范围内、但**会顺带核到**的两项：

- **GUI 日志**：`main.rs:75` 硬编码 `let log_dir = "logs"`（`logs/gui.log.YYYY-MM-DD`）—— 按 R2 **不动**。
- **Prompt 目录**：`prompt_manager.prompt_dir`（`boot.rs:77` 取 `docs` 子目录）—— 是**只读资源**目录，不是缓存，不动。

解析实现的三份差异（改造时要收敛）：

1. `kv.rs:62-80` / `storage.rs:92-110`：两个函数**逐字重复**（env 覆盖 → 相对则 `cwd.join` → `create_dir_all(parent)`），只差 env 名与注释。
2. `rag.rs` / `run_service.rs`：**完全不解析**，把配置字符串原样交出去，由各自的库/执行器在运行期按 cwd 解析。

## 3. 设计

### 3.1 配置新增一个根目录

`GuiConfig`（`config/mod.rs`）新增字段：

```rust
/// 全局缓存根目录：所有需要落盘的缓存/本地数据文件都拼在它下面。
/// 相对路径以**进程 cwd** 为基准。不含 GUI 日志（`logs/` 独立）。
#[serde(default = "default_cache_root")]
pub cache_root: String,
```

- 默认值 `default_cache_root()` → `"./data"`（与现有 `./data/*` 三项默认值一致，故它们**默认落点不变**）。
- 命名：字段名用 `cache_root`（用户措辞是「全局缓存目录」）。若更希望叫 `data_dir` / `storage_root`，实施时改一处即可 —— 见 §5 D3。
- 放在 `[cache]` 段旁边还是顶层？建议**顶层**（它与 `storage` / `cache` / `rag` / `flexible` 四段都是并列的「根」，塞进任何一段都会造成从属误导）。

### 3.2 子项默认值改成「根下的子目录名」

| 配置项 | 新默认值（子项值） | 含义 |
|---|---|---|
| `storage.db_path` | `agent-gui.db` | 根下的文件 |
| `cache.path` | `kv_store` | 根下的目录 |
| `flexible.output_cache_dir` | `cache` | 根下的目录（仍再拼 `/<session_id>`） |
| `rag.store.path` | `vector_store` | 根下的目录 |

默认值因此变成**单段名**（不含路径分隔符）。逐项都仍可单独覆盖 —— 覆盖语义见 §3.3。

### 3.3 派生规则（关键）

统一按「值」的形态决定怎么解释，规则三条、无歧义：

| 子项值形态 | 解释 | 例（`cache_root = "./data"`） |
|---|---|---|
| 绝对路径 | 原样使用（**逃出根**，等于完全自定义） | `D:\rag-store` → `D:\rag-store` |
| 单段相对名（无路径分隔符） | `cache_root` 下的一级子项 | `kv_store` → `<cwd>/data/kv_store` |
| 多段相对路径（含 `/` 或 `\` 与目录成分） | 视为**相对 cwd** 的完整路径（保持既有语义，**不**再拼根） | `./data/kv_store` → `<cwd>/data/kv_store` |

第三条是**故意**的：只有它能让「用户 `config.toml` 里残留的旧值」不被双重嵌套。
没有它，旧值 `./data/kv_store` 会变成 `<cwd>/data/data/kv_store`，而且解析时还会 `create_dir_all` 把它**静默建出来**，表现成「数据凭空消失、多出一个嵌套目录」。这是一个必须挡掉的坑，详见 §4 R-1。

**出口一律归一化**（2026-10-08 补）：`paths::normalize` 去掉 `.` 组件、按词法消解 `..`
（`PathBuf::join` 不做这件事 —— 不归一化时 `absolutize("./data")` 给出 `<cwd>\./data`，
于是 `flexible_output_dir()` 成了 `...\agent-gui\./data\cache`，与别处算出的同一目录**字符串不等**，
还会被拼进 prompt 让模型照抄）。归一化后四个派生点 + 「外发给模型的产出目录」形态一致。
不用 `canonicalize` 的理由：它要求路径**已存在**（配置目录启动时常常还没建），
且 Windows 上会加上 `\\?\` 前缀，把路径变成另一种形态。

### 3.4 统一解析函数

新增 `crates/agent-gui/src/paths.rs`（替代 `kv.rs` / `storage.rs` 里两份重复实现）：

### 3.4 统一解析：`paths` 给**规则**，`GuiConfig` 给**结果**

分工两层（用户在实施期间追加要求：**拼接要收口到配置结构体，不散在各使用点**）：

**① `crates/agent-gui/src/paths.rs`（新增）—— 只管规则，不懂业务**

```rust
/// 相对 → 以进程 cwd 绝对化。
pub fn absolutize(value: &str) -> PathBuf;
/// §3.3 三条派生规则。
pub fn resolve_under(root: &str, child: &str) -> PathBuf;
/// §3.6：env 存在则整值覆盖，否则走 `resolve_under`。
pub fn resolve_with_env(root: &str, child: &str, env: &str) -> PathBuf;
/// 建**父目录**（不建自身：sled / SQLite / PolarisDB 各自会建自己的目录或文件）。
pub fn ensure_parent(path: &Path) -> anyhow::Result<()>;
```

**② `impl GuiConfig`（`config/layout.rs`）—— 给出已含 `cache_root` 的结果（拼接唯一落点）**

```rust
pub fn kv_path(&self) -> PathBuf;              // env `PLANNED_AGENT_CACHE_PATH` > 根派生
pub fn db_path(&self) -> PathBuf;              // env `PLANNED_AGENT_DB_PATH` > 根派生
pub fn rag_store_path(&self) -> PathBuf;       // 绝对路径（PolarisDB 不解析 cwd）
pub fn flexible_output_dir(&self) -> PathBuf;  // **不含**会话段
```

于是使用点**不认识 `cache_root`**，也不再重复写拼接规则：

| 调用点 | 之前 | 现在 |
|---|---|---|
| `context/kv.rs` | 本地 `resolve_cache_path(&config.path)` | `KvContext::init(&config.cache, config.kv_path())` |
| `context/storage.rs` | 本地 `resolve_db_path(&config.db_path)` | `StorageContext::init(&config.storage, config.db_path())` |
| `context/rag.rs:35` | 把 `config.store.path` 原样交给 PolarisDB | `RagContext::init(&config.rag, config.rag_store_path())` |
| `services/run_service.rs:61` | `PathBuf::from(&flexible.output_cache_dir).join(session_id)` | `app.flexible_output_dir().join(session_id)` |

要点：

- context 层签名改收**已解析好的 `PathBuf`**，自己只负责 `ensure_parent` + 「各自建自己的目录」；
- 解析结果取**绝对路径**（相对值先 `cwd.join`），避免执行期/库内部再按 cwd 二次解析的歧义。
  GUI 不 `chdir`，与现状行为等价，只是更显式；
- env 覆盖后**不再拼根**（`PLANNED_AGENT_CACHE_PATH=D:\x` 就是最终路径）；
- 配置里保留用户原值（`cache.path` 仍是 `kv_store`），日志/展示不失真 —— 这也是没选「加载后归一化」方案的原因。

**产出目录还要「告诉模型」**（2026-10-08 补）：`flexible_output_dir()` 同时是**下游工具的沙箱根**
（`builtin_recognize_image` / `builtin_solve_captcha` 建 `FilesystemService` 用的 root），
而浏览器 MCP 的落盘默认在**它自己的 workspace root** —— 没有 roots 上报时 = MCP server 进程的 cwd
（即 `cargo run` 所在目录）。两者**不是同一个目录**，模型就会用 `Copy-Item` / base64 来回搬运。

对齐办法**不是写死路径**，而是把它作为**环境段的一行**动态外发。路径**分两段拼、各管各的**：

```
宿主（会话段）   services/run_service.rs：snapshot().with_output_dir(config.cache_dir)  // = <根>/<session_id>
执行器（执行段） exec/executor/mod.rs：with_run_dir(env, "run-<millis>-<seq>")           // 与 spill 落点同目录
  → step_system_prompt(env, category) → prompt/environment.rs 渲染「产出目录：<path>」
  → STEP_SYSTEM_PROMPT 的全局纪律：落盘产出 → 产出目录 + 显式命名
```

于是「**纪律在全局基准、路径在环境段**」：改 `cache_root` 只需改配置，不必动任何 prompt 文案。

产出目录**按「会话 → 本次执行」两级隔离**：宿主拼会话段（执行器不认识会话概念），
执行器补 `run-*` 段 —— 与 spill 落点**同一目录**，所以模型写出的产出和
`cache_dir/<run_dir>/tool-s*.txt` 挨在一起，**同一个计划跑多次也不会互相覆盖**。
执行器在拼 prompt 前会 `create_dir_all` 该目录（原先是懒建，模型可能在父目录不存在时写失败）；
builtin 图片工具族的沙箱根是它的**上一层**，所以照样能读。

`output_dir` 是环境段里**唯一刻意外发的路径**（与 `working_dir` 相反），理由见
`flexible-execution-improvements.md` §5.2.5。

### 3.5 默认落点变化

（`cache_root` 取默认 `./data` 时）

| 用途 | 旧 | 新 | 变化 |
|---|---|---|---|
| SQLite | `./data/agent-gui.db` | `./data/agent-gui.db` | 无 |
| sled KV | `./data/kv_store` | `./data/kv_store` | 无 |
| flexible 产出 | `./data/cache/<session_id>` | `./data/cache/<session_id>` | 无 |
| RAG 向量库 | `./traces/vector_store` | `./data/vector_store` | **变**（按 R4 不迁移，旧数据留在 `traces/`） |

附带事实：`traces/` 这个目录名在 agent-gui 里**只有** `rag.store.path` 的默认值引用它（全仓 grep 确认），改完后不再被默认使用。

### 3.6 env 覆盖语义

保留现有两个 env，语义 = 「直接给出最终路径」，**优先级高于 `cache_root` 派生**（存在即跳过拼接）：

- `PLANNED_AGENT_CACHE_PATH` → `cache.path`
- `PLANNED_AGENT_DB_PATH` → `storage.db_path`

RAG / flexible 现状没有 env，本次也**不加**（避免凭空多两套开关）。是否新增 `PLANNED_AGENT_CACHE_ROOT` 见 §5 D2。

### 3.7 与内核常量的关系

`flexible.output_cache_dir` 的 GUI 默认值现在引用内核常量
（`config/flexible.rs` 的 `default_flexible_output_cache_dir` 原先引用 `planned_agent::flexible::DEFAULT_CACHE_DIR = "./data/cache"`，`exec/executor/config.rs:7`）。

改成子目录名 `cache` 后，GUI **不再引用**该常量 —— 因为「根目录」是宿主概念，内核不知道也不该知道。

- `DEFAULT_CACHE_DIR` **保留不删**：`planned-agent` 作为纯库直接使用时（无宿主）仍用它兜底。
- 内核 `ExecutorConfig.cache_dir` 的类型与语义**完全不动**（`PathBuf`，宿主拼好会话段传进来），本设计只是改 GUI 侧这一处的取值来源。

### 3.8 改动清单（实际落地）

| 文件 | 改动 |
|---|---|
| `crates/agent-gui/src/paths.rs` | **新增**：`absolutize` / `resolve_under` / `resolve_with_env` / `ensure_parent` + 单测 |
| `crates/agent-gui/src/config/` | **已按类拆文件**：`mod.rs`（聚合 `GuiConfig` + `pub use`）＋ `ai.rs` / `logging.rs` / `gui.rs` / `storage.rs` / `cache.rs` / `rag.rs` / `flexible.rs`（各一类配置）＋ `layout.rs`（四个 `*_path`，**拼接唯一落点**）＋ `load.rs`（候选路径搜索与反序列化）；原来只有一个 533 行的 `config.rs` |
| `crates/agent-gui/src/config/mod.rs` | `GuiConfig` 新增 `cache_root`；四个子项默认值改为单段名；加载日志带上 `cache_root` |
| `crates/agent-gui/src/main.rs` | `mod paths;` |
| `crates/agent-gui/src/boot.rs` | 三处 `init` 调用改为传解析好的路径 |
| `crates/agent-gui/src/context/kv.rs` | 删本地 `resolve_cache_path`；`init(config, path: PathBuf)`，只留 `ensure_parent` |
| `crates/agent-gui/src/context/storage.rs` | 删本地 `resolve_db_path`；`init(config, path: PathBuf)` |
| `crates/agent-gui/src/context/rag.rs` | `init(config, store_path: PathBuf)` |
| `crates/agent-gui/src/services/run_service.rs` | `cache_dir = app.flexible_output_dir().join(session_id)` |
| `crates/agent-gui/src/context/tools/mod.rs` | 沙箱根注释改为引用 `cache_root`（R-2） |
| `docs/agent-gui-storage.md` | 已同步路径与解析说明（示例里补 `cache_root`） |

## 4. 风险

- **R-1（必须挡）旧配置双重嵌套**：用户 `config.toml` 里已显式写着 `./data/kv_store` 之类的**多段相对值**。
  若规则是「相对值一律拼根」，就会变成 `<root>/data/kv_store`，且解析期 `create_dir_all` 会把它建出来 →
  表现为「数据不见了 + 多出一层嵌套目录」。§3.3 第三条（多段相对路径按 cwd 解释）正是为此。
- **R-2 根目录在 cwd 之外时，`filesystem` 工具可能读不到 flexible 产出**：
  `crates/agent-gui/src/context/tools/mod.rs:29` 注释指出 `cwd` 同时是内置文件工具族沙箱根之一的落点依据。
  若把 `cache_root` 配成 `D:\data`，flexible 写出的产出文件在 cwd 之外，模型用 `builtin_*` 文件工具按提示里的路径读可能被沙箱拒绝。
  默认 `./data` 不触发。**建议**：文档/配置注释里写明「根目录建议留在 cwd 内」，代码不做拦截（本次不扩展到沙箱）。
- **R-3 目录创建责任**：统一函数只建**父目录**（即 `cache_root`）。sled / SQLite / PolarisDB 各自会创建自己的目标目录/文件；
  PolarisDB 的 `open_or_create` 语义（`crates/rag/src/store/polaris.rs:45`）已在，无额外动作。

## 5. 决策记录（已确认）

- **D1（§3.3）**：**按段数区分** —— 单段值拼 `cache_root`；多段相对值按 **cwd** 解释（旧配置零破坏）；绝对路径原样使用。→ §3.3 三条规则即为定稿。
- **D2（§3.6）**：**不加** `PLANNED_AGENT_CACHE_ROOT`，根目录只从配置文件读。
- **D3（§3.1）**：字段名定为 **`cache_root`**。
- **D4（§3.4，实施期间追加）**：拼接**收口到配置结构体** —— `paths` 只放规则，`GuiConfig` 给已含根的结果；
  使用点不再认识 `cache_root`。备选（已在实施中否决）：① 各使用点自行调用 `paths::resolve_under`；
  ② `load()` / `default()` 后一次性归一化把根写进子项值（会让结构体丢失用户原值，而 GUI 无配置回写需求但保留原值更利于排障）。

## 6. 本次不做

- 不动 GUI 日志目录（`logs/`，`main.rs:75`）。
- 不动 `prompt_manager.prompt_dir`（只读资源）。
- 不做旧数据迁移 / 兼容读取（R4）。
- 不改内核 `ExecutorConfig` / `DEFAULT_CACHE_DIR` 的类型与语义。
- 不扩展到 `filesystem` 工具沙箱根（R-2 只用文档提示，不拦截）。
