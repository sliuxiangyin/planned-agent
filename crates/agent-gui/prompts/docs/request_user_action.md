# request_user_action 工具规范

## 概述

`request_user_action` 用于向用户请求交互——确认、选择、输入或多项勾选。
调用后等待用户操作，不要自行假设用户选择。

调用参数：

| 参数 | 必填 | 说明 |
|------|------|------|
| message | ✅ | 展示给用户的引导文本，清晰说明需要用户做什么决定 |
| actions | ✅ | 用户可选的动作列表（数组） |

---

## Actions 类型

### 1. confirm — 确认按钮

用于"是/否"、确认、跳过等场景。一行可放多个。

| 字段 | 必填 | 说明 |
|------|------|------|
| id | ✅ | 唯一标识（如 `generate`、`skip`） |
| type | ✅ | `"confirm"` |
| label | ✅ | 按钮展示文本 |
| description | ❌ | tooltip 补充说明 |

**用户操作返回值：**

- 无 MultiSelect 伴随：返回按钮 `label` 文本
- 有 MultiSelect 伴随：返回勾选项的 `id=value` 逗号拼接（选项未填 `value` 时仅回传 `id`）

### 2. select — 单选按钮

从多个选项中选择一个。一行可放多个，用户点击即选中。

| 字段 | 必填 | 说明 |
|------|------|------|
| id | ✅ | 唯一标识 |
| type | ✅ | `"select"` |
| label | ✅ | 按钮展示文本 |
| description | ❌ | tooltip 补充说明 |

**用户操作返回值：** 按钮 `label` 文本

### 3. input — 文本输入框

引导用户自由输入文本，如路径、关键词等。独占一行。

| 字段 | 必填 | 说明 |
|------|------|------|
| id | ✅ | 唯一标识 |
| type | ✅ | `"input"` |
| label | ✅ | 输入框标签 |
| description | ❌ | placeholder 占位文本 |

**用户操作返回值：** 用户输入的文本

### 4. multi_select — 多选复选框

逐项勾选场景。不直接返回——需配合 `confirm` 按钮收集勾选结果。

| 字段 | 必填 | 说明 |
|------|------|------|
| id | ✅ | 唯一标识 |
| type | ✅ | `"multi_select"` |
| label | ✅ | 复选框组标签 |
| description | ❌ | 补充说明 |
| options | ✅ | 复选框选项数组 |

### MultiSelect options 字段

| 字段 | 必填 | 说明 |
|------|------|------|
| id | ✅ | 唯一标识，小写蛇形（如 `param_city`） |
| label | ✅ | 展示文本（纯描述即可） |
| value | ❌（强烈建议） | **实际数据负载**。勾选后回传为 `id=value` 格式；未填写时勾选仅回传 `id` |
| default | ❌ | 是否默认勾选，默认 `false` |

**value 字段设计意图：**

- AI 把识别到的原始值填入 `value`，`label` 只做展示
- 用户勾选后，系统直接取 `value` 获得结果，不再需要 AI 二次解析
- 示例：`{ id: "param_city", label: "城市", value: "北京" }` → 勾选后回传 `param_city=北京`
- 兜底：若未填 `value`，勾选后仅回传 `id`（如 `param_city`），系统侧需自行处理

---

## 组合规则

| 组合 | 允许 | 说明 |
|------|------|------|
| Input + Confirm | ✅ | 同一问题不同回答方式（如"输入路径 / 使用默认值"） |
| MultiSelect + Confirm | ✅ | 复选框组 + 确认/跳过按钮，Confirm 自动收集勾选结果 |
| Input + Select | ❌ | 两个不同问题混在一次交互，禁止 |
| 纯 Confirm | ✅ | 简单确认场景 |
| 纯 Select | ✅ | 多选一场景 |

- 纯 MultiSelect 无 confirm 伴随 → 禁止：用户勾选后无提交按钮，交互卡死（前端无兜底入口）

核心原则：**一次 request_user_action 调用只针对一个决策点**。

---

## 参数生成原则

1. **id 命名**：小写蛇形，前缀体现分类
   - `param_` — 可参数化值（`param_city`、`param_version`）
   - `opt_` — 一般选项（`opt_a`、`opt_b`）
   - 语义化动作（`generate`、`skip`、`edit`）

2. **label 写法**：简洁清晰，用户无需额外解释
   - ✅ `"确认生成"` / `"城市"` / `"还需补充"`
   - ❌ `"点击此按钮确认生成执行计划"`

3. **value 填写（multi_select 的 options）**：当选项有对应的实际数据值时（参数识别等），务必填充 `value`
   - ✅ `{ id: "param_city", label: "城市", value: "北京" }`
   - ❌ `{ id: "param_city", label: "城市 = 北京" }` — 把数据塞 label 里，系统无法直接取用
   - 注意：confirm / select 类型当前回传按钮 `label`（`value` 字段支持规划中），不要在它们上面填 `value` 并期望回传

4. **提供退出路径**：至少包含一个允许用户跳过的动作
   - `{ id: "skip", type: "confirm", label: "跳过" }`

5. **value 内容限制**：`value` 内避免包含逗号 `,` 与等号 `=`——多选回传用 `,` 分隔、用 `=` 连接 `id` 与 `value`，包含这两个字符会被解析截断

---

## choice 的语义边界

`request_user_action` 的回传 `choice` 是**短字符串**，只承担两类职责：

1. **确认信号**——告诉 AI 用户点了哪个动作（confirm / select 按钮的 `label`）
2. **短数据负载**——multi_select 勾选结果（`id=value` 拼接）、input 用户输入文本

当用户确认的对象是 **AI 展示的一段内容**（数据格式、JSON、计划文本等）时：

- 内容由 AI 在**对话消息**中产出，**不要**塞进 action 的 `value`，也不要期望 `choice` 携带大段内容
- 调用方在卡片挂起时已持有对话历史快照，可在用户确认后从快照中提取内容，或由 AI 在后续对话中继续产出
- 示例：AI 先输出 `{"name":"test"}` 再弹"可以/取消"卡片——用户点"可以"后，`choice` 只是"可以"，JSON 应从对话快照中提取

---

## 完整示例

### 参数识别（MultiSelect + Confirm × 2）

AI 识别到 "在北京搜索 v2.1.0 版本 Rust 项目" 中的可参数化值：

```json
{
  "message": "识别到以下可参数化的动态值，勾选需要固化的参数：",
  "actions": [
    {
      "id": "multi",
      "type": "multi_select",
      "label": "选择参数",
      "options": [
        { "id": "param_city",    "label": "城市",     "value": "北京" },
        { "id": "param_version", "label": "版本",     "value": "v2.1.0" },
        { "id": "param_keyword", "label": "搜索关键词", "value": "Rust" }
      ]
    },
    { "id": "confirm", "type": "confirm", "label": "确认固化所选" },
    { "id": "skip",    "type": "confirm", "label": "跳过，直接执行" }
  ]
}
```

用户勾选"城市"和"版本" → 点击"确认固化所选" → 回调 `choice = "param_city=北京,param_version=v2.1.0"`

系统本地解析得到：参数 `city=北京`、`version=v2.1.0`，无需 AI 再次参与。

### 清晰度追问（纯 Select）

```json
{
  "message": "你想怎么处理这个数据？",
  "actions": [
    { "id": "opt_csv",  "type": "select", "label": "导出 CSV", "description": "适合 Excel 打开" },
    { "id": "opt_json", "type": "select", "label": "导出 JSON", "description": "适合程序读取" },
    { "id": "opt_screen","type": "select", "label": "屏幕打印", "description": "直接显示结果" }
  ]
}
```

用户点击"导出 CSV" → 回调 `choice = "导出 CSV"`

### 文本输入 + 默认值（Input + Confirm）

```json
{
  "message": "请提供目标文件路径：",
  "actions": [
    { "id": "custom_path", "type": "input", "label": "手动输入", "description": "输入文件完整路径" },
    { "id": "default",     "type": "confirm", "label": "使用当前目录", "description": "采用当前工作目录" }
  ]
}
```

用户输入 `/tmp/output.csv` 回车 → 回调 `choice = "/tmp/output.csv"`
用户点击"使用当前目录" → 回调 `choice = "使用当前目录"`
