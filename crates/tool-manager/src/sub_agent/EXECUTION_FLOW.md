# Sub Agent 执行流程

本文档从 `executor.rs` 中提取子 agent 的会话式执行抽象的核心流程。

---

## 整体架构

```
┌─────────────────────────────────────────────────────────────────────┐
│                        SubAgentToolExecutor                         │
│                                                                     │
│   call_tool_streamed / call_tool                                    │
│        │                                                            │
│        ├─ arguments 含 session_id?                                  │
│        │   ├─ Yes → resume 路径（从 store 取会话 → 等用户信号 → 恢复）│
│        │   └─ No  → start 路径（首次启动 runner）                    │
│        │                                                            │
│        └─ 结果                                                      │
│            ├─ Done(ToolResult)             → 正常返回                │
│            └─ AwaitingUserAction           → 会话入 store，返回挂起  │
└─────────────────────────────────────────────────────────────────────┘
```

## 生命周期

子 agent 以 **会话** 为单位执行，支持挂起-恢复：

| 组件 | 职责 |
|------|------|
| `SubAgentSessionRunner` | 首次启动（`start`） |
| `SubAgentRunOutcome` | 单次执行结果：`Done` 或 `AwaitingUserAction` |
| `SubAgentSession` | 挂起时保留子 agent 内部状态（含 history），用户确认后经 `resume` 恢复 |
| `SubAgentSessionStore` | 框架层会话存储：挂起保留 → resume 取出 → 完成/取消/TTL 清理 |

过程事件经 `ToolStreamSender` 实时送达 UI，最终结果走 `ToolResult → history → LLM` 闭环。
挂起信息通过 `ToolResult.content` 结构化传递（不走流）：

```json
{
  "status": "awaiting_user_action",
  "session_id": "...",
  "message": "...",
  "actions": [...]
}
```

---

## 首次调用路径（start）

```
execute_streamed(arguments, invocation_id, stream)
    │
    ├─ arguments 无 session_id
    │
    ├─ runner.start(arguments, stream)
    │      │
    │      ├─ Ok(Done(result))
    │      │      └─ result.call_id = invocation_id → 返回 Ok(result)
    │      │
    │      └─ Ok(AwaitingUserAction { session, message, actions })
    │             │
    │             ├─ 1. spawn 后台任务（tokio::spawn）
    │             │      ├─ rx.await 阻塞，等待前端 signal_resume
    │             │      └─ 收到用户输入后 → take session → resume → 循环
    │             │
    │             ├─ 2. sleep(20ms) 等后台任务就绪
    │             │
    │             ├─ 3. store.upsert(sid, session, tool_name, tx)
    │             │
    │             └─ 4. 返回挂起 ToolResult
    │                    { status: "awaiting_user_action", session_id, message, actions }
    │
    └─ 返回
```

---

## Resume 路径

```
execute_streamed(arguments, invocation_id, stream)
    │
    ├─ arguments 含 session_id（resume 调用）
    │
    ├─ 1. store.get(sid) 检查会话是否存在
    │      └─ 不存在 → Err("session not found or expired")
    │
    ├─ 2. 创建 oneshot channel → store.upsert_resume_signal(sid, tx)
    │      （注册 resume signal，让 signal_resume 能找到）
    │
    ├─ 3. rx.await 阻塞，等待前端 signal_resume 发送用户输入
    │
    ├─ 4. store.take(sid) 取出会话（取出即移除，防重入）
    │
    ├─ 5. session.resume(user_input, stream) → 首次恢复
    │
    └─ 6. 循环处理多次挂起
           │
           ├─ Done(result) → result.call_id = invocation_id → 返回
           │
           ├─ Err(_) → 返回错误
           │
           └─ AwaitingUserAction { session: next_session, .. }
                  │
                  ├─ store.upsert(sid, next_session, tool_name, tx2)
                  ├─ rx2.await 等待下一次用户操作
                  ├─ store.take(sid) → session.resume(inp, stream)
                  └─ → 回到循环顶部
```

---

## 后台任务：挂起 → 恢复循环

以 4 步演示为例（每步都挂起）：

```
spawn ──rx.await──► take session ──resume──► LLM 处理步骤 1
                                              │
                                         又挂起了（步骤 2）
                                              │
◄── upsert 新 session ── rx2.await ── 等待用户点击 ──►
                                              │
                                  用户点击 → take session → resume
                                              │
                                         LLM 处理步骤 2
                                              │
                                         又挂起了（步骤 3）
                                              │
◄── upsert 新 session ── rx3.await ── 等待用户点击 ──►
                                              │
                                          ……如此循环……
                                              │
                                     LLM 处理步骤 4 → Done
                                              │
                                         退出循环
```

### 详细步骤

```
┌─────────────────────────────────────────────────────────────────────┐
│ 步骤 A: rx.await 阻塞，等待前端用户点击按钮                       │
│         （前端调用 signal_resume → tx 发送 → rx 收到）              │
├─────────────────────────────────────────────────────────────────────┤
│ 步骤 B: 从 store 中取出 session（首次挂起时存入的）                │
│         store.take("call_abc123") → 拿到 session 对象              │
├─────────────────────────────────────────────────────────────────────┤
│ 步骤 C: 调用 session.resume(user_input)                           │
│         → 子 agent driver 收到 Command::Resume                     │
│         → LLM 继续对话（处理用户的确认/选择/输入）                  │
│         → LLM 可能再次调用 request_user_action                     │
├─────────────────────────────────────────────────────────────────────┤
│ 步骤 D: 根据 resume 的返回值决定下一步：                          │
│   ┌─ Done ──────────→ 子 agent 全部完成，退出循环                 │
│   ├─ Err  ──────────→ 出错，退出循环                               │
│   └─ AwaitingUserAction → 子 agent 又挂起了！                     │
│         ↓                                                          │
│ 步骤 E: 把新的 session 放回 store（key 还是 "call_abc123"）       │
│         store.upsert("call_abc123", 新session, tx2)               │
│         这样前端下次 signal_resume 就能找到它                      │
├─────────────────────────────────────────────────────────────────────┤
│ 步骤 F: rx2.await 阻塞，等待前端下一次用户点击                     │
│         （回到步骤 B，形成循环）                                    │
└─────────────────────────────────────────────────────────────────────┘
```

---

## 会话存储（SubAgentSessionStore）

### 操作一览

| 操作 | 说明 | 防重入 |
|------|------|--------|
| `upsert` | 存入新会话 / 更新 resume 后再次挂起的会话，同时登记 resume 信号 | — |
| `signal_resume` | 向挂起会话发送用户选择，唤醒 `execute_streamed` 的 `rx.await` | 取出 `resume_tx` 即移除，重复调用报错 |
| `take` | 取出会话用于 resume（取出即移除） | ✓ 取出后不存在，无法重复取 |
| `get` | 检查会话是否存在（不取出） | — |
| `clear` | 显式清空所有挂起会话 | — |

### TTL 清理

- 默认 TTL：**10 分钟**（`DEFAULT_SESSION_TTL`）
- 惰性清理：每次 `upsert` / `take` 时扫描过期会话
- 会话超时未被 resume → 自动移除

---

## 流式 vs 非流式入口

| 入口 | 行为 |
|------|------|
| `call_tool_streamed`（流式） | 阻塞到子 agent 真正完成（含 resume 后继续），支持多轮挂起-恢复循环 |
| `execute`（非流式） | 传入 no-op 流句柄，挂起时返回结构化 `awaiting_user_action` ToolResult，不阻塞等前端信号 |

---

## 数据流

```
前端用户操作
    │
    ▼
signal_resume(session_id, user_input)
    │
    ▼
SubAgentSessionStore.signal_resume()
    │  取出 resume_tx，发送 user_input
    ▼
execute_streamed 中 rx.await 被唤醒
    │
    ▼
store.take(session_id)  ← 取出即移除，防重入
    │
    ▼
session.resume(user_input, stream)
    │  子 agent driver: Command::Resume → LLM 继续对话
    ▼
SubAgentRunOutcome
    ├─ Done(ToolResult) → ToolResult.call_id 覆写 → 返回 → history → LLM
    └─ AwaitingUserAction → 新 session 入 store → 返回挂起信息 → 等待下次操作
```
