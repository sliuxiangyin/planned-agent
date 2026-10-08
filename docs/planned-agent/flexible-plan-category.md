# flexible 计划分类（plan category）

> 设计稿。状态：**✅ 已实施**（2026-10）。
> 目标：给灵活计划引入一个「这个计划**在干什么**」的分类标签，**按分类拼接不同的规则提示词段**，
> 减少执行期的试探与弯路（与「运行环境段」同一条思路）。
> 相关：`docs/planned-agent/flexible-executor.md`（执行链路）、
> `crates/planned-agent/src/flexible/AGENTS.md`（执行器硬约定）。

---

## 1. 需求与概念澄清

### 1.1 用户原话整理

1. 给计划加一个**分类类型**，参考工具分类那种「大分类」的形态。
2. 例：浏览器自动化计划、手机操作计划、数据处理计划、文件整理计划……
3. 用途：**按类型不同，拼接不同的规则提示词**。

### 1.2 计划分类 ≠ 工具分类（本稿的立足点）

这两件事**正交**，必须先钉死，否则整个设计会跑偏：

| | 工具分类 `ToolCategory` | **计划分类**（本稿） |
|---|---|---|
| 回答的问题 | 我**有哪些能力**（工具） | 这个**计划在干什么** |
| 例子 | File / Browser / Data / System | 浏览器自动化 / 手机操作 / 数据处理 / 文件整理 |
| 定义处 | `crates/core/src/tool_registry/types.rs:16` | 新增（候选见 §2） |
| 现有用途 | 过滤、加载工具（`allowed_tools` 分类 token） | 拼该类型的**规则提示词**（本稿） |
| 数量级 | 9（含 `Utility`/`SubAgent`，封闭枚举） | 少量高频类（6~8）+ 兜底 |

因此本稿**不与**「coarse planner 刻意不注入工具分类」（`planner/coarse/llm_planner.rs:100-102`，
理由是「能力边界不属于『做什么』」）冲突 —— 那条讲的是**工具分类**；计划分类回答的正是「做什么」，
本就在规划职责内。

---

## 2. 计划分类体系（草案）

### 2.1 判据（必须显式写死，否则打标全凭各人理解）

**判据 = 计划操作的「介质 / 界面」**（网页 / 本地文件与数据 / 命令行与代码 / 手机 / 网络信息 / …），
**不是处理环节**（读取、分析、写入、转换都是**环节**）。一条作业链（读目录 → 读文件 → 分析 →
写回）里换的是环节、不变的是介质，它**属于同一个技能**，不该按环节拆成多个分类。

> 由此，「技能」与「域」在这里**收敛**了：一个技能 = 「针对某类介质的一整套作业」。
> 所以真正的选择不是「域 vs 技能」，而是**粒度** —— 介质分得对，两者就是一回事。

- **单值**：一个计划**只属于一个分类**。分类回答「**这个计划属于哪门技能 / 面向哪种介质**」——
  是**归属**，不是标签。
- **单一类目强制定主次**：逼生成端想清楚「这个计划的核心是什么」，避免模糊归类。
- **判不定留空**：纯推理 / 无外部操作的计划分类为空（不加规则段）。

> **为什么单值**（调研结论）：成熟产品里，「给一个 workflow 定领域」在**管理归属层**压倒性用**单值**
> —— Power Automate / Dataverse 的 `Category` 是单值 Picklist、Zapier 用单值 folder、CrewAI 的 agent
> `role` 是单值字符串；**多值只用于「检索用标签」**（n8n tags、Dataverse 的 `taggedprocess` 多对多表）。
> 分工是：**单值 category 做权威归属，多值 tag 做检索**。本仓库自己的
> `planner/react/intent_router.rs:20-100` 也是同一范式（多值打标 → 压成单一意图）。
> **所以**：分类保持单值；将来若要记录「这个计划涉及哪些域」，另开一个**独立的 tag 字段（多值，
> 仅用于检索 / 展示）**，不要把这个分类变多值。

### 2.2 分类表（**技能/场景**口径，**待逐条确认**，见 §8-1）

> **技能 = 一套可复用的做法**；一个**计划**就是这门技能的一次具体实现（带参数）——
> 这与 flexible「计划 = 可复用模板」的定位天然契合。分类即「这个计划属于哪门技能」。
> 命名用**场景 / 动作**口吻（「…自动化」「…处理」「…整理」），粒度仍是**大类技能**，
> 不做细分（「抓取」与「填表」都归「浏览器自动化」）。
>
> 依据是**两侧夹逼**：一侧是内置工具能力域
> （`crates/tool-manager/src/builtin/`：filesystem / system_tools / data_tools / text_tools /
> doc_tools / vision_tools / captcha_tools / web_tools），另一侧是 MCP 接入的外部能力
> （浏览器自动化、移动设备/ADB → `ToolCategory::Browser` / `Device`）。
>
> **采样结论（2026-10，`agent-gui.db` / 8 条会话）**：有效计划只有 6 个（2 条空/重复），
> 且**全落在本地文件与网页两类**（文件续写 ×5、网页登录含验证码 ×1）。样本太小，
> **不足以数据驱动地砍类**。（另注：采样里**验证码出现在浏览器登录计划里**，印证「分类是技能、
> 不是工具域」—— `captcha` 归 `Browser`，不单列。）
>
> **粒度修正（2026-10）**：`File` / `Data` / `Text` 本是**同一介质上的同一套作业**
> （读目录 → 读文件 → 分析 → 写回），按「环节」拆会割裂一条作业链 —— 故**合并为 `File`**
> （文件与数据处理）；`Dev` 与原 `System` 同理合并。分类表收敛为 **5 类 + `Other`**。

| 技能 / 场景 | 标识（拟）| 操作的介质 / 界面 | 主要能力域 |
|---|---|---|---|
| 浏览器自动化 | `Browser` | 网页 | Browser(MCP) + web + captcha |
| 文件与数据处理 | `File` | 本地文件 / 数据（读写、清洗、统计、转换、落盘） | filesystem + data + text + doc |
| 信息检索与研究 | `Research` | 网络信息（多源检索、汇总、对比） | web + Browser |
| 开发与运维 | `Dev` | 命令行 / 代码 / 系统 | system + Dev |
| 移动端自动化 | `Device` | 手机 / App | Device(MCP) |
| 其他 | `Other` | — （不加规则段）；判不定也可留空 | — |

**收敛原则**：宁可少，不要多。每多一类就多一段要维护、要调优的提示词；长尾一律进 `Other`。

### 2.3 值域与命名（已定）

✅ **独立定义 `PlanCategory`**（技能 / 场景口径），**英文标识尽量与 `ToolCategory` 对齐**。

- 大部分技能能对上工具分类（Browser / File / Data / Text / Dev / Device），
  个别对不上（如 `Research`）—— **对得上的刻意同名**，将来要接
  「分类 → 默认工具白名单」时零成本（**本版不做**，见 §9-6）。
- 计划分类保持稳定，不被工具生态的变化牵着走（加新工具类 ≠ 应加新计划类）。

**定义放哪**：与 `OutputKind` 同思路，**就近放 flexible**（`flexible/plan/`），
便于执行器直接消费；不往 `core` 塞（`core` 不认识「计划」这个概念）。

---

## 3. 分类的落点：模板字段

`FlexiblePlanTemplate`（`crates/planned-agent/src/flexible/plan/template.rs:12`）扩一个字段：

```json
{
  "task": "...",
  "inputs": [...],
  "steps": [...],
  "output_schema": { ... },
  "category": "Browser"        // 新增；单值；缺失/null = 不分类
}
```

- **`#[serde(default)] pub category: Option<PlanCategory>`**（强类型单值，落库为字符串）：
  - **缺失 / `null`** = 不分类（不加规则段），二者同义；
  - **未知取值容错**：认不出就按「不分类」处理（参考 `coarse_types.rs:8` 的容错思路），不报错。
- **向后兼容**：旧落库 JSON 没有该字段 → `None` → **不加规则段 → 执行期行为逐字不变**
  （与 `output_schema` 同样的思路，见 `template.rs:20-28`）。

---

## 4. 分类的产生（已定）

✅ **由规划步 LLM 输出**：`flexible_plan` 的输出里多加一个 `category` 字段，与 `steps` 一起定稿。

分类必须是**执行前**就存在的信息（规则段要进 system prompt），所以在生成流程里定稿；
判据（「这个计划属于哪门技能 / 场景」）正是 plan 步的职责，此时 `steps` 刚成型、信息最全。
（用户「可覆盖」作为后续增强；本版不做。）

**兜底**：判不定就给 `Other` / 空，绝不硬猜。

落地要点：
- `prompts/flexible/flexible_plan.toml`：注入分类清单 + 输出字段说明（**单选，选最能概括本计划的
  那一个；判不定留空**）；示例 JSON 加 `"category": "..."`。
- `flexible_plan` 回调 `PRODUCTS`（`plan/plan_callback.rs:31`）加 `"category"`。
- 下游清理：`revise`/`parameterize` 等重做 plan 时，`category` 应随 `steps` 一起失效
  （把 `category` 加入各上游步骤的 `CLEAR`，与 `steps` 同进退）。
- `build_payload`（`commit.rs:116`）读 `category` 拼进 payload。

---

## 5. 规则提示词（本稿的真正价值所在）

### 5.1 拼接位置

复用现有 `step_system_prompt`（`crates/planned-agent/src/flexible/exec/prompt/system.rs`），
从「基准 + 环境段」扩成「**基准 + 环境段 + 分类规则段**」：

```
STEP_SYSTEM_PROMPT
  ├─ 运行环境段（已有，只取决于 env）
  └─ 分类规则段（新增，只取决于 category）   ← 尾部追加
```

- 签名扩为 `step_system_prompt(env: Option<&RuntimeEnvironment>, category: Option<PlanCategory>)`。
- **`category` 为 `None` 时逐字返回现状**（保持 `Cow::Borrowed` 零分配路径），行为不变。
- **位置固定在尾部**、内容只取决于 `env + category` ⇒ 同一次执行内每步字符串一致，
  仍是可命中的 provider 前缀缓存（这条不能破，见 `exec/prompt/system.rs` 的注释）。
  单值下组合数 = 分类数，对缓存最友好。
- 调用点：`executor/mod.rs:83`（整次执行算一次，每步复用）—— 传 `template.category`。

### 5.2 规则段内容纪律（硬约束）

1. **只写该技能的作业纪律与「最常见的坑」，不写具体工具用法** —— 「怎么调用某个工具」属工具
   `description`，不占常驻 prompt（`flexible/AGENTS.md` §4、`builtin_execute_command` 先例）。
2. **不写具体个案**（域名、业务格式、某次会话的细节）—— 只写机制 + 通用判据，防过拟合。
3. **不重复环境段**（跨平台、编码、可用命令已在环境段）。
4. 规则段是「作业规范」，不是「教程」；长度克制（每类 3~4 条）。

### 5.3 每类规则段文案（**技能作业规范**口径，**逐条待审**）

> 技能口径下，规则段写的是**该技能的作业纪律**（一套做法怎么走），而非泛泛的「通用判据」；
> 但仍守住 §5.2 的底线：**不写具体工具用法、不写个案**。单值 ⇒ 每次只拼**一段**，长度可控。

**`Browser`｜浏览器自动化**
- 页面内容与元素状态**以工具实际返回为准**，不得凭 URL、惯例或经验臆测页面结构。
- **采集到的内容先落盘再分析**，不要在一次回答里既抓又分析又输出。
- 涉及登录态 / 分页 / 弹窗时，**先确认当前处于目标页面状态**再执行下一步。
- 需要人工介入（验证码、扫码、短信）时**如实报告并停下**，不伪造成功、不跳过。

**`File`｜文件与数据处理**（原 `File` + `Data` + `Text` 合并）
- 所有路径 / 数据来源**来自本次参数或前序结果**，不臆造。
- **读 / 改之前先看现状**：是否存在、当前内容与结构（表头、编码、行尾、大小）。
- 大文件 / 大表**按行或分块读**，不整读进上下文；中间结果**落盘**。
- **保留原文实体**（人名、编号、术语、格式标记）；非本步目标**不改写、不翻译、不填充缺失值**。
- 统计 / 转换的**口径要显式**（去重、空值、单位、时间格式），并写进产出。
- 写 / 改 / 批量操作（移动、删除、改名）**先确认范围与目标**，操作后**核对结果**。

**`Research`｜信息检索与研究**
- **多来源分别落盘**再汇总，不要在中间状态里混合。
- **标注来源**；冲突信息**如实并列**，不擅自取舍或调和。
- 检索不到时**如实报告「未找到」**，不臆造结论。

**`Dev`｜开发与运维**
- 命令**跨平台**，按「运行环境」段判断 shell / 可用命令，不硬编码。
- **破坏性操作**（覆盖、删除、杀进程、改系统配置、推送）**先确认目标再执行**。
- 改代码 / 配置前**先读现状**，改后核对；**不整文件重写**。
- 执行结果**以命令实际返回为准**，不臆测。

**`Device`｜移动端自动化**
- 设备状态**以工具返回为准**；操作前确认已连接、已解锁、处于目标界面。
- 失败**先查连接 / 权限 / 驱动**，不假设操作成功。
- 需要人工介入时**如实报告并停下**。

**`Other` / 空**：不加段。

---

## 6. 影响面清单（精确落点）

**新建**
- `crates/planned-agent/src/flexible/plan/category.rs`（`PlanCategory` 枚举 + `as_str`/`from_name`/`ALL`
  + 每类的规则段常量表；配单测）—— 枚举与规范段分开：枚举在 `plan/category.rs`、文案在 `exec/prompt/skills.rs`。

**修改**

| 位置 | 改什么 |
|---|---|
| `crates/planned-agent/src/flexible/plan/template.rs:12-29` | `FlexiblePlanTemplate` 加 `#[serde(default)] category`；加「旧 JSON 缺字段仍可解析」单测 |
| `crates/planned-agent/src/flexible/mod.rs:29-53` | 若新建 `category.rs`，re-export 对外类型（**对外路径**：`planned_agent::flexible::PlanCategory`） |
| `crates/planned-agent/src/flexible/exec/prompt/system.rs` | `step_system_prompt` 增 `category` 参数 + 尾部追加规则段；补「None 逐字返回」「同 env+category 逐字一致」单测 |
| `crates/planned-agent/src/flexible/exec/executor/mod.rs:83` | 调用点传 `template.category` |
| `crates/planned-agent/src/flexible/exec/executor/tests.rs` | 需要时补「带分类 → prompt 含规则段」的端到端断言 |
| `crates/agent-gui/src/pages/plan/flexible/step_callback/commit.rs:116-168` | `build_payload` 读 `category` 拼进 payload（缺失/null → `null`，非空但非法 → 报错或忽略，见 §8-决策） |
| `crates/agent-gui/src/pages/plan/flexible/step_callback/plan/plan_callback.rs:31,36` | （方案 A）`PRODUCTS` 加 `category`；`CLEAR` 视需要调整 |
| `crates/agent-gui/prompts/flexible/flexible_plan.toml` | （方案 A）注入分类清单 + 输出字段；示例 JSON 加 `"category"` |
| `crates/agent-gui/src/pages/plan/flexible/tool/flexible_state.rs` | tools 描述里的产物 key 列表加 `category`（若作为 state 产物） |
| `crates/agent-gui/src/pages/plan/flexible/placeholder.rs` | 若分类需参与保存校验（一般不需要），同步镜像实现 |
| `crates/agent-gui/src/pages/plan/left_panel/left_panel.rs` | 左面板展示分类（Bento 块，范式同 `output_schema` 块） |
| `crates/agent-gui/src/storage/entities/flexible_state.rs` | 档位串 / products 键注释（若新增产物 key） |

> 落点的行号会漂移，**以代码为准**（见 `flexible/AGENTS.md` §8 的提醒）。

---

## 7. 测试

- `cargo test -p planned-agent --lib flexible::`：`template.rs` 加「旧 JSON 缺 `category` 仍可解析」；
  `exec/prompt/system.rs` 加「`None` 逐字借用 / 有分类追加规则段 / 同入参逐字一致」；`category.rs` 的 `from_name` 往返。
- `cargo test -p planned-agent-gui --bins`：`build_payload` 的分类分支；`flexible_plan` 回调常量；
  左面板展示（如涉及）。
- ⚠️ 该目录已知 baseline 见 `flexible/AGENTS.md` §6，跑本目录用 `--lib flexible::` 过滤。

---

## 8. 定稿说明

分类表（**5 类 + `Other`**）与每类作业规范段文案（§5.3）已随实施定稿；
实现清单见 §6、验证见 §7。

---

## 9. 已拍板并实施（2026-10，用户确认）

1. **粒度**：**整个计划**（`FlexiblePlanTemplate` 顶层加一个字段）；不按步骤分、不做双层
   （步骤级留作将来的扩展位）。
2. **基数**：**单值** —— 一个计划只属于一个分类（归属，不是标签）；理由见 §2.1。
3. **语义**：按 **技能 / 场景** 分（不是按操作对象 / 域）—— 分类 = 「这个计划属于哪门技能」。
4. **分类来源**：**规划步 LLM 输出**（`flexible_plan` 输出 `category`，与 `steps` 一起定稿）；
   判不定留空 / `Other`。
5. **分类值域**：**独立定义 `PlanCategory`**（技能 / 场景口径），英文标识尽量与
   `ToolCategory` 对齐（§2.3）。
6. **第一版范围**：**只做「分类 → 规则提示词」**；暂不联动工具白名单。
7. **分类表**：**5 类 + `Other`** —— 浏览器自动化 / 文件与数据处理 / 信息检索与研究 /
   开发与运维 / 移动端自动化；`File`+`Data`+`Text` 合并、`Dev`+`System` 合并（§2.2）。
