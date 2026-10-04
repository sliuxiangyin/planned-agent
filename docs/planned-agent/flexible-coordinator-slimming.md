# 灵活模式·协调器 system prompt 精简

> 目标：把 `crates/agent-gui/prompts/flexible/flexible_global_system.toml`
> 从 **186 行 / 10807 字符**（全家最厚的一份）压到 **~110 行**，**信息零损失**；
> 并厘清一个更根本的问题：**流程定义该放在哪**。

状态：**第一阶段已实施**（2026-10-04，`flexible_global_system.toml` 整篇重写）；第二阶段（§4）**未立项**。

---

## 1. 现状与问题

### 1.1 体量

| 文件 | 行 | 字符 |
|---|---|---|
| **flexible_global_system** | **186** | **10807** |
| flexible_revise | 172 | 6946 |
| flexible_clarify | 114 | 2742 |
| flexible_parameterize | 96 | 3121 |
| flexible_output | 117 | 3522 |
| flexible_plan | 88 | 2480 |
| flexible_save | 48 | 956 |

协调器一份 = 其余 6 份之和的 1.6 倍。而它只负责**路由**，本应最薄。

各节行数分布（`flexible_global_system.toml`）：

```
1-10    头部（角色 / 五步 / 只做协调）        10
11-28   §一 入口判定                          18
29-67   §二 入口 A｜需求澄清 → 全流程          39
68-89   §三 入口 B｜重做某一步                 22
90-115  §四 入口 C｜修订某一处                 26
116-184 §五 通用规则                          69
185-186 [variables]
```

### 1.2 问题不是「长」，是「同一件事写了两三遍」

**铁证 ①：§5.5 可用工具（11 行）= 100% 抄 tool schema。**
这 6 条描述的**事实来源**在其他地方，且会随 tool schema 进入协调器上下文：

| prompt §5.5 | 注册处（`crates/agent-gui/src/pages/plan/flexible/page.rs`） |
|---|---|
| 「计划子 Agent，把任务描述展开为粗粒度步骤骨架（`steps`：子目标 + 依赖 + 期望产出）。」 | `:177` **一字不差** |
| 「参数化子 Agent，从步骤骨架中识别可变值，把它们就地替换为 `${name}` 占位符…」 | `:214` 几乎逐字 |
| 其余 4 条 | `:132` / `:255` / `:300` / `:350` |

**铁证 ②：§5.3 第 3 条 = 抄 14 遍参数描述。**
`page.rs` 里「由系统自动注入」出现 **14 次**（每个工具的每个业务参数各一条），prompt 又用一整段把它概括说一遍。

**铁证 ③：`error` 分支写了 4 遍。**
`flexible_global_system.toml` 的 `:48-50` `:54-56` `:60-62` `:66` 四处几乎逐字重复「输出错误信息 → 重试 / 取消」；而 `:150` 另有一条通用条款说「**不要**对同一个工具反复重试」—— 两处口径**打架**。

**为什么必须治**：重复 = 漂移源。已经发生的两处漂移：

- §一 里「入口 A」同时表示「新需求全流程」（`:18`）和「闲聊兜底」（`:21`），一个标签两种语义；
- `:25` 的粗体结论「**已有 `steps` 时绝不用 A 接**」与紧随其后的「只有不含任何新要求才走 A」自相抵消，模型容易只记住粗体 → 把闲聊塞进 C 空跑一次 `flexible_revise`。

---

## 2. 目标 / 非目标

**目标**

- 186 → ~110 行；删掉的每一处都是「模型能自己推导」或「已经在别处定义过」的内容。
- 每条规则**只有一个落点**（single source of truth）。

**非目标**

- 不改 agent 拓扑（仍是 1 个协调器 + 6 个 step 子 agent）。
- 不改路由语义（三个入口 A / B / C 及判定表含义不变）。
- 不动核心 `ChatService` / `SubAgentRunner`。

---

## 3. 第一阶段：去重与重组（prompt 文本层，零架构风险）

### 3.1 三张表合并成一张（吃掉 §5.1 + §5.2 表 + §5.4，33 行 → ~10 行）

现状：`§5.1 档位表`(15) + `§5.2 status 表`(7) + `§5.4 步骤名对照`(11) 三张表信息同源。合成：

```markdown
## 步骤总表

| 步骤 | 工具（用户可能说） | 定稿 status | 定稿后档位 | 产物 |
|---|---|---|---|---|
| 需求澄清 | flexible_clarify（澄清 / 改需求） | task_defined | task_defined | task_definition |
| 计划 | flexible_plan（做计划 / 重做计划） | planned | planned | steps |
| 参数化 | flexible_parameterize（抽参数） | success | parameterized | inputs + steps |
| 输出定义 | flexible_output（定输出） | success | output_defined | output_schema（可空） |
| 保存 | flexible_save（保存 / 落库） | saved | saved | 落库 |
| 修订 | flexible_revise（改一处） | revised | **不改档位** | 按改动写回 |

档位顺序：none → task_defined → planned → parameterized → output_defined → saved。
非首列 status（error / ignored / cancelled / needs_restart）＝ 本次未定稿：不推进档位、不产生任何产物。
needs_restart 不是失败，是「这次改动必须整体重做」的信号（按 §入口 C 处理）。
```

### 3.2 `error` 分支从 4 处收成 1 条（§二 的 12 行 → ~3 行）

```markdown
## 异常处理
- 任何 step 返回 `error`：转述 `error_message`，问「重试 / 取消」。重试＝再调同一工具；取消＝输出「任务已取消。」。
- 工具级错误（`is_error`，如「流程状态登记失败」）：该步未成功、状态未登记 —— 如实转述并**停下**，不要对同一工具反复重试。
```

### 3.3 删 §5.5 工具说明（11 行 → 1 行）

```markdown
## 可用工具
用途见各工具的自身定义；你只能调用上表列出的工具，不得调用业务工具或其它子 agent。
```

### 3.4 §5.3 压成 2 行（7 → 2）

```markdown
## 会话上下文
session_id = `{{ session_id }}`。参数含 `session_id` / `host_session_id` 时原样照抄该值；
其余业务字段由系统按 host_session_id 自动注入 —— 不要传、也不要从对话里转抄。
```

### 3.5 §一 那段重复论述收口（`:23-27`，5 行 → 3 行）

```markdown
分不清 B / C 时按 C（B 会作废下游、重跑整链；C 最坏只多改一版）。
已有 steps 且用户在**提新要求** → 一律 C；只有**不含新要求**（纯闲聊 / 取消 / 提问）才走 A。
⚠️ 任何**依赖上下文**的提问都不得交给 `flexible_clarify` —— 它一被误判定稿就会清掉下游产物（含 steps）。
```

### 3.6 §二 / §四 的参数说明删成一句

`:33-37` 与 `:94-97` 逐字罗列「只需传 `user_message` / `conversation_summary` / `host_session_id`，不要传 `previous_task_definition`…」—— 这些在 `page.rs:136-151` 等处的参数 schema 里**逐字段都有**：

```markdown
调用时只传 `user_message`（首次澄清另加 `conversation_summary`）与 `host_session_id`；其余入参由系统注入。
```

### 3.7 改后结构

```
1-8     头部：角色 + 五步 + 「只做协调」（吸收 §5.6）
9-20    §一 入口判定（表 4 行 + 3 行收口）
21-44   §二 入口 A（删 error 重复、删参数说明）
45-57   §三 入口 B（依赖表压成一句）
58-69   §四 入口 C
70-108  §五 通用规则
          步骤总表（合并三表）
          异常处理（error 一条）
          会话上下文（2 行）
          可用工具（1 行）
109-110 [variables]
```

### 3.8 行数预算

| 阶段 | 行 | 字符 |
|---|---|---|
| 改前 | 186 | 10807 |
| 改后（实测） | **136** | **6870（-36.4%）** |

> 行数没落到预估的 ~110，原因是 §7-Q2 决定**补「入口 D｜讨论」整节**（+13 行），
> 且 §四 的 `needs_restart` 等分支保留了完整细节。按内容量（字符）计为 **-36.4%**。

砍完后它只比 `flexible_revise.toml`（172 行 / 6946 字符）略小 —— 而「路由」本来就该是全家最薄的角色。

---

## 4. 第二阶段：流程定义外置（架构层，**待单独立项，本次不做**）

### 4.1 观察

协调器「承担太多」的**真正病灶**不是「它管了流程」，而是「**流程定义用散文写在了 prompt 里**」。而一个关键事实是：

> **入口 A 的主路径是线性的** —— clarify → plan → parameterize → output → save，没有分支。
> （分支全部是「异常 / 用户打断」：补充、重试、跳过、取消。）

**既然是线性的，就不需要 LLM 来编排。**

### 4.2 方向

- **定稿即自动推进**：现在 `create_*_callback` 只**登记状态**；改为登记后**由宿主自动启动下一步**（clarify 定稿 → 自动调 plan → … → save）。
- **协调器只处理偏离**：`flexible_global_system` 退成「入口判定 + 打断 / 重做 / 修订 / 讨论」，§二 那 39 行整段消失。
- **技术核心**：现在「启动 step 子 agent」是 LLM 发起的 tool call；宿主自动推进需要有**编程式启动子 agent** 的能力（`ChatService::send_text`，或新增 API）。这是本改造的主要工作量。
- **主要风险**：用户中途插话（「等等，先别做计划」）需要**中断宿主流程**的语义 —— 必须先设计这个。

### 4.3 结论

值得做，但**是独立的一次架构改造**，需要单独设计稿。本次先不动。

---

## 5. 明确不做的方案（均已核实）

### 5.1 动态切换 `system_prompt`（如「判完入口就换成该入口的 prompt，走完再切回」）

**不可行**，两道硬伤：

1. **`set_system_prompt` 单独调用没有任何效果。**
   `crates/planned-agent/src/chat/service/service.rs:272` 只写 `config.system_prompt`；注入在
   `chat/driver/prompt.rs:10` 的 `inject_system_prompt`，而它 `:20-22` 有
   `if state.history.first_is_system() { return Ok(()); }` —— 历史首条已是 System 就跳过。
2. **要生效必须 `reset_session()`，代价是清空整个对话历史。**
   `service.rs:284` 发 `Command::Reset`；`chat/tests.rs:650-705` 就是它的规格说明：
   「热切换模板 + 会话重置后，下次 send 注入**新**模板、**且旧历史被清空**」（断言只剩
   `[System, User, Assistant]` 三条）。

   而协调器的 history 正是它的核心资产（入口判定要看上文、要记住用户说过什么）。
   **每切一次失忆一次**，比 prompt 太长严重得多。

   另外：`Template` 分支渲染用**空 `PromptContext`**（`chat/driver/prompt.rs:31`），而
   `flexible_global_system` 含 `{{ session_id }}` —— 直接切成 Template 会把 session_id 渲染成空，
   **所有 step 调用就传不了 `host_session_id`**（这也是 `chat_service_factory.rs:46-48` 用
   `Rendered` 的原因）。

3. **同一轮内根本切不了。** `inject_system_prompt` 在 `run_conversation` **开头只调一次**
   （`chat/driver/round/mod.rs:310`），之后那个 tool-round `loop` 不再注入；而「判入口 →
   调 step 工具 → 收结果 → 转述」全在**同一次 send** 里。

### 5.2 把「入口 A 全流程」组装成一个子 agent

**不推荐**，两道墙 + 收益被高估：

- **墙 1：子 agent 没有跨轮生命周期。** `chat/sub_agent/runner.rs:128-148`：每次调用
  **新建 `ChatService`、完成即 drop**。而入口 A 的流程是跨多轮的（clarify 定稿 → 回用户确认 →
  **用户下一条消息** → plan → …）。
- **墙 2：子 agent 的 UI 语义是「挂起返回」而非「阻塞确认」。**
  `chat/service/config.rs:66-72`：主 agent（`run_id=None`）→ `BlockAndConfirm`；
  子 agent（`run_id=Some`）→ `EmitAndSuspend`。入口 A 至少要跟用户来回两次，变成子 agent 后
  全变「挂起」，还得由协调器做 resume 转发 —— **复杂度是涨不是降**。
- **收益量化**：§二 39 行的构成 ——

  | 内容 | 行数 | 能否搬进新 agent |
  |---|---|---|
  | 纯流程定义 | ~8 | ✅ 只有这部分能搬 |
  | 用户交互 | ~8 | ❌ 跨轮，必须留协调器 |
  | `error` 分支（写了 4 遍） | ~9 | ❌ 搬不走，但 §3.2 能直接删 |
  | 参数说明（抄 schema） | ~5 | ❌ 搬不走，但 §3.6 能直接删 |
  | 措辞冗余 | ~9 | ❌ 重组即可 |

  **拆 agent 只能搬走 8 行；剩下 31 行里 14 行是 §3 一删就没的重复，其余是跨轮交互。**

### 5.3 「按需注入」（system 只留路由 + 用只读工具返回入口细节）

技术上可行（与 `flexible_state` 同构），但 §3 做完后剩余体量已不值得为它引入新工具、
并让 history 被注入内容长期占用。**不做。**

---

## 6. 验证与回归

1. 改前 `git status` 确认工作区干净，便于对比 / 回退。
2. 落盘后跑：
   ```powershell
   cargo test -p planned-agent-gui --bins flexible_prompts_load_through_file_manager
   ```
   该测试走与 `PromptContext::init` 同一条加载路径解析 `prompts/flexible` 全部 prompt，
   能抓出 TOML 转义这类**只在运行期才炸**的错误（见
   `crates/agent-gui/src/pages/plan/flexible/step_callback/mod.rs` 的测试）。
   它**只验语法，验不了路由**。
3. ⚠️ **人工过场景**（路由是模型行为，测试覆盖不到）：
   - 有 `steps` + 纯闲聊 / 提问 → 不得进 C、不得进 clarify；
   - 有 `steps` + 「改成…」 → 进 C；
   - 「重做第 2 步」 → 进 B，且步骤序号解析正确（验证 §5.4 步骤名词表的取舍，见 §7-Q1）；
   - 无 `steps` + 任意 → 进 A。

---

## 7. 拍板结果（2026-10-04）

| # | 问题 | 决定 |
|---|---|---|
| Q1 | §5.4 步骤名对照表 | **并入总表**（§6.1 的「用户可能说」列） |
| Q2 | 是否补「讨论档」 | **补** —— 已落地为 §五 入口 D |
| Q3 | 第一阶段落地方式 | **整篇重写** |
| Q4 | 第二阶段是否立项 | **暂不**，先做第一阶段 |

**回归待办**：§6 第 3 条列出的 4 个人工场景尚未执行 ——
路由是模型行为，`flexible_prompts_load_through_file_manager` 只验语法（已过）。
