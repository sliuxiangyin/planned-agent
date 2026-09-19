# TemplateExecutionSpec 完整文档

## 0. 定位

`TemplateExecutionSpec` 是**模板执行器唯一消费的执行契约**。  
它描述：给定输入变量，如何一步步调用工具/代码/LLM，如何传值、分支、判成功、处理失败、记录指标。

---

## 1. 顶层结构

```ts
interface TemplateExecutionSpec {
  constants?: Record<string, any>;        // 常量/默认变量
  inputs: InputSpec[];                    // 输入契约
  steps: StepSpec[];                      // 可执行步骤（核心）
  edges: EdgeSpec[];                      // 控制流
  outputs: OutputSpec;                    // 输出与整体成功判定
  errorCatalog?: ErrorCatalogItem[];      // 已知错误处理策略
  fixation: FixationSpec;                 // 固化总策略
  runtimeRecordSchema: RuntimeRecordSchema; // 执行器要记录什么
  metricsSchema: MetricsSchema;           // 指标定义
}
```

必填：`inputs`、`steps`、`edges`、`outputs`、`fixation`、`runtimeRecordSchema`、`metricsSchema`。  
可选：`constants`、`errorCatalog`。

---

## 2. constants

| 字段 | 类型 | 必填 | 说明 |
|---|---|---|---|
| constants | Record<string, any> | 否 | 模板级常量，执行时注入变量池，可被 `{{key}}` 引用 |

示例：

```json
{
  "time_format": "yyyy-MM-dd HH:mm:ss",
  "default_dir": "C:\\Users\\woddp\\Desktop\\Downloads"
}
```

---

## 3. inputs：输入契约

描述执行前必须绑定哪些变量，以及如何从用户输入/常量/上一步输出获得。

```ts
interface InputSpec {
  name: string;
  type: InputType;
  required: boolean;
  default?: any;
  source: InputSource;
  extractRule?: ExtractRule;
  validation?: ValidationRule;
  description?: string;
}

type InputType = 'string' | 'number' | 'boolean' | 'object' | 'array' | 'path' | 'datetime';
type InputSource = 'user_input' | 'constant' | 'step_output' | 'system';
```

| 字段 | 类型 | 必填 | 说明 |
|---|---|---|---|
| name | string | 是 | 变量名，后续用 `{{name}}` 引用 |
| type | InputType | 是 | 变量类型，执行器据此做类型校验/转换 |
| required | boolean | 是 | 是否必填 |
| default | any | 否 | 缺省值 |
| source | InputSource | 是 | 变量来源 |
| extractRule | ExtractRule | 否 | 从用户输入抽取的规则 |
| validation | ValidationRule | 否 | 校验规则 |
| description | string | 否 | 说明 |

```ts
interface ExtractRule {
  type: 'regex' | 'llm' | 'jsonpath' | 'literal';
  pattern?: string;    // regex 用
  prompt?: string;     // llm 用
  path?: string;       // jsonpath 用
  value?: any;         // literal 用
}

interface ValidationRule {
  pattern?: string;
  enum?: any[];
  min?: number;
  max?: number;
}
```

示例：

```json
{
  "name": "dir",
  "type": "path",
  "required": true,
  "default": "C:\\Users\\woddp\\Desktop\\Downloads",
  "source": "user_input",
  "extractRule": { "type": "regex", "pattern": "在(.+?)目录下" },
  "description": "目标目录"
}
```

---

## 4. steps：核心可执行步骤

每一步是执行器的最小调度单元。

```ts
interface StepSpec {
  stepId: string;
  name: string;
  kind: StepKind;
  tool?: string;
  code?: CodeSpec;
  llm?: LlmSpec;
  argsTemplate?: Record<string, any>;
  dependsOn: string[];
  condition?: string;
  outputs?: Record<string, string>;
  successCriteria?: string;
  failurePolicy: FailurePolicy;
  fixation: StepFixation;
  timeoutMs?: number;
}

type StepKind = 'tool' | 'code' | 'condition' | 'transform' | 'response' | 'llm' | 'loop';
```

| 字段 | 类型 | 必填 | 说明 |
|---|---|---|---|
| stepId | string | 是 | 步骤唯一 ID，如 `s1` |
| name | string | 是 | 可读名称 |
| kind | StepKind | 是 | 步骤类型 |
| tool | string | kind=tool 时必填 | 工具名 |
| code | CodeSpec | kind=code 时必填 | 代码定义 |
| llm | LlmSpec | kind=llm 时必填 | LLM 调用定义 |
| argsTemplate | object | 否 | 参数模板，支持 `{{var}}` |
| dependsOn | string[] | 是 | 依赖的 stepId 列表 |
| condition | string | 否 | 执行条件表达式 |
| outputs | object | 否 | 输出映射，如 `{ "exists": "$.result.exists" }` |
| successCriteria | string | 否 | 本步成功判定表达式 |
| failurePolicy | FailurePolicy | 是 | 失败处理策略 |
| fixation | StepFixation | 是 | 本步固化信息 |
| timeoutMs | number | 否 | 超时毫秒 |

### 4.1 StepKind 语义

| kind | 作用 | 是否调用外部 |
|---|---|---|
| tool | 调用工具 | 是 |
| code | 执行内联代码 | 否（沙箱） |
| condition | 条件判定，决定走哪条边 | 否 |
| transform | 数据变换（如字符串拼接、JSON 提取） | 否 |
| response | 生成最终响应 | 否 |
| llm | 调用 LLM | 是 |
| loop | 循环子步骤 | 视情况 |

### 4.2 CodeSpec

```ts
interface CodeSpec {
  language: 'javascript' | 'python';
  entry: string;
  body: string;
  inputMapping: Record<string, string>;
  outputMapping: Record<string, string>;
}
```

### 4.3 LlmSpec

```ts
interface LlmSpec {
  promptTemplate: string;
  model?: string;
  temperature?: number;
  inputMapping?: Record<string, string>;
  outputSchema?: Record<string, any>;
  fallbackStepId?: string;
}
```

### 4.4 FailurePolicy

```ts
interface FailurePolicy {
  retry: number;
  retryDelayMs?: number;
  onError: 'abort' | 'skip' | 'fallback' | 'llm' | 'continue';
  fallbackStepId?: string;
  errorCodes?: string[];
}
```

| 字段 | 说明 |
|---|---|
| retry | 重试次数，0 表示不重试 |
| retryDelayMs | 重试间隔 |
| onError | 失败后动作：终止 / 跳过 / 回退到指定步骤 / 转 LLM / 继续 |
| fallbackStepId | `onError=fallback` 时回退到哪步 |
| errorCodes | 触发该策略的错误码白名单，空表示全部 |

### 4.5 StepFixation

```ts
interface StepFixation {
  bypassLlm: boolean;
  canFix: boolean;
  fixType: 'tool_call' | 'code' | 'condition' | 'response' | 'llm_required' | 'semi_fix';
  confidence: number;   // 0~1
  reason?: string;
  risk?: string;
  fallback?: string;
}
```

| 字段 | 说明 |
|---|---|
| bypassLlm | 执行时是否绕过 LLM |
| canFix | 是否可固化 |
| fixType | 固化类型 |
| confidence | 固化置信度 |
| reason | 原因 |
| risk | 风险说明 |
| fallback | 固化失败时的回退描述 |

---

## 5. edges：控制流

```ts
interface EdgeSpec {
  from: string;
  to: string;
  condition?: string;
  kind?: 'normal' | 'then' | 'else' | 'loop' | 'error';
  priority?: number;
}
```

| 字段 | 类型 | 必填 | 说明 |
|---|---|---|---|
| from | string | 是 | 源 stepId |
| to | string | 是 | 目标 stepId |
| condition | string | 否 | 条件表达式，为空表示无条件 |
| kind | EdgeKind | 否 | 边类型，默认 normal |
| priority | number | 否 | 多条边冲突时的优先级，越小越优先 |

执行器按 `edges + steps.dependsOn + condition` 构建 DAG。

---

## 6. outputs：输出与整体成功判定

```ts
interface OutputSpec {
  responseTemplate: string;
  resultMapping?: Record<string, string>;
  postconditions?: Condition[];
  successCriteria?: string;
}

interface Condition {
  expression: string;
  description?: string;
}
```

| 字段 | 类型 | 必填 | 说明 |
|---|---|---|---|
| responseTemplate | string | 是 | 最终响应模板，支持 `{{var}}` |
| resultMapping | object | 否 | 输出变量映射 |
| postconditions | Condition[] | 否 | 后置校验 |
| successCriteria | string | 否 | 整体成功判定表达式 |

---

## 7. errorCatalog：已知错误处理

```ts
interface ErrorCatalogItem {
  stepId: string;
  errorCode: string;
  errorPattern?: string;
  action: 'retry' | 'fallback' | 'llm' | 'abort' | 'human';
  fallbackStepId?: string;
  note?: string;
}
```

从原始失败 tool 调用中提炼，只保留可执行策略。

---

## 8. fixation：固化总策略

```ts
interface FixationSpec {
  llmRequiredSteps: string[];
  fixableSteps: string[];
  fixableRate: number;
  recommendedMode: 'full_fix' | 'semi_fix' | 'llm_planned';
}
```

| 字段 | 类型 | 必填 | 说明 |
|---|---|---|---|
| llmRequiredSteps | string[] | 是 | 必须走 LLM 的步骤 |
| fixableSteps | string[] | 是 | 可固化步骤 |
| fixableRate | number | 是 | 固化比例，0~1 |
| recommendedMode | enum | 是 | 推荐执行模式 |

---

## 9. runtimeRecordSchema：执行器要记录什么

```ts
interface RuntimeRecordSchema {
  perStep: boolean;
  collectTiming: boolean;
  fields: RuntimeField[];
}

interface RuntimeField {
  name: string;
  type: string;
  required: boolean;
  description?: string;
}
```

最小推荐字段：

| 字段 | 类型 | 说明 |
|---|---|---|
| step_id | string | 步骤 ID |
| tool | string | 工具名 |
| args | object | 实际参数 |
| result | any | 工具返回 |
| status | string | success / failed / skipped |
| error_code | string | 错误码 |
| error_message | string | 错误信息 |
| start_time | string | 开始时间 |
| end_time | string | 结束时间 |
| duration_ms | number | 耗时 |
| retry_count | number | 重试次数 |
| llm_used | boolean | 是否走了 LLM |
| fixation_used | boolean | 是否用了固化 |
| source_template_version | number | 来源模板版本 |

---

## 10. metricsSchema：指标定义

```ts
interface MetricsSchema {
  totalTimeMs: boolean;
  llmTimeMs: boolean;
  toolTimeMs: boolean;
  stepTimeMs: boolean;
  retryCount: boolean;
  tokenUsage: boolean;
  cost: boolean;
  successRate: boolean;
  p95: boolean;
}
```

执行器按此采集，回流给分析器用于选最优。

---

## 11. 完整示例

```json
{
  "constants": {
    "time_format": "yyyy-MM-dd HH:mm:ss"
  },
  "inputs": [
    {
      "name": "dir",
      "type": "path",
      "required": true,
      "default": "C:\\Users\\woddp\\Desktop\\Downloads",
      "source": "user_input",
      "extractRule": { "type": "regex", "pattern": "在(.+?)目录下" }
    },
    {
      "name": "file_name",
      "type": "string",
      "required": true,
      "default": "text.txt",
      "source": "user_input"
    }
  ],
  "steps": [
    {
      "stepId": "s1",
      "name": "检查文件是否存在",
      "kind": "tool",
      "tool": "fs.exists",
      "argsTemplate": { "path": "{{dir}}\\{{file_name}}" },
      "dependsOn": [],
      "outputs": { "exists": "$.result.exists" },
      "successCriteria": "$.status == 'success'",
      "failurePolicy": { "retry": 0, "onError": "abort" },
      "fixation": { "bypassLlm": true, "canFix": true, "fixType": "tool_call", "confidence": 1.0 }
    },
    {
      "stepId": "s2",
      "name": "是否存在分支",
      "kind": "condition",
      "dependsOn": ["s1"],
      "condition": "{{s1.exists}} == false",
      "failurePolicy": { "retry": 0, "onError": "abort" },
      "fixation": { "bypassLlm": true, "canFix": true, "fixType": "condition", "confidence": 1.0 }
    },
    {
      "stepId": "s3a",
      "name": "创建并写入序号1和时间",
      "kind": "tool",
      "tool": "fs.write_text",
      "argsTemplate": {
        "path": "{{dir}}\\{{file_name}}",
        "content": "1 {{now:time_format}}\n",
        "mode": "create_new"
      },
      "dependsOn": ["s2"],
      "successCriteria": "$.status == 'success'",
      "failurePolicy": { "retry": 0, "onError": "abort" },
      "fixation": { "bypassLlm": true, "canFix": true, "fixType": "tool_call", "confidence": 1.0 }
    },
    {
      "stepId": "s3b",
      "name": "读取文件内容",
      "kind": "tool",
      "tool": "fs.read_text",
      "argsTemplate": { "path": "{{dir}}\\{{file_name}}" },
      "dependsOn": ["s2"],
      "outputs": { "content": "$.result.content" },
      "failurePolicy": { "retry": 1, "onError": "abort" },
      "fixation": { "bypassLlm": true, "canFix": true, "fixType": "tool_call", "confidence": 1.0 }
    },
    {
      "stepId": "s4",
      "name": "解析最大序号并+1",
      "kind": "code",
      "code": {
        "language": "javascript",
        "entry": "nextSeq",
        "body": "const lines = input.content.trim().split(/\\r?\\n/); const nums = lines.map(l => parseInt(l.match(/^(\\d+)/)?.[1])).filter(Number.isFinite); return { next: Math.max(0, ...nums) + 1 };",
        "inputMapping": { "content": "{{s3b.content}}" },
        "outputMapping": { "next": "$.next" }
      },
      "dependsOn": ["s3b"],
      "outputs": { "next": "$.next" },
      "failurePolicy": { "retry": 0, "onError": "llm" },
      "fixation": { "bypassLlm": true, "canFix": true, "fixType": "code", "confidence": 0.95 }
    },
    {
      "stepId": "s5",
      "name": "追加新序号和时间",
      "kind": "tool",
      "tool": "fs.append_text",
      "argsTemplate": {
        "path": "{{dir}}\\{{file_name}}",
        "content": "{{s4.next}} {{now:time_format}}\n"
      },
      "dependsOn": ["s4"],
      "successCriteria": "$.status == 'success'",
      "failurePolicy": { "retry": 1, "onError": "abort" },
      "fixation": { "bypassLlm": true, "canFix": true, "fixType": "tool_call", "confidence": 1.0 }
    },
    {
      "stepId": "s6",
      "name": "反馈成功",
      "kind": "response",
      "argsTemplate": { "message": "成功" },
      "dependsOn": ["s3a", "s5"],
      "fixation": { "bypassLlm": true, "canFix": true, "fixType": "response", "confidence": 1.0 }
    }
  ],
  "edges": [
    { "from": "s1", "to": "s2" },
    { "from": "s2", "to": "s3a", "condition": "{{s1.exists}} == false", "kind": "then" },
    { "from": "s2", "to": "s3b", "condition": "{{s1.exists}} == true", "kind": "else" },
    { "from": "s3b", "to": "s4" },
    { "from": "s4", "to": "s5" },
    { "from": "s5", "to": "s6" },
    { "from": "s3a", "to": "s6" }
  ],
  "outputs": {
    "responseTemplate": "成功",
    "successCriteria": "{{s6.status}} == 'success'"
  },
  "errorCatalog": [
    {
      "stepId": "s5",
      "errorCode": "permission_denied",
      "action": "fallback",
      "fallbackStepId": "s6",
      "note": "权限不足时回退到 LLM 处理"
    }
  ],
  "fixation": {
    "llmRequiredSteps": [],
    "fixableSteps": ["s1", "s2", "s3a", "s3b", "s4", "s5", "s6"],
    "fixableRate": 1.0,
    "recommendedMode": "full_fix"
  },
  "runtimeRecordSchema": {
    "perStep": true,
    "collectTiming": true,
    "fields": [
      { "name": "step_id", "type": "string", "required": true },
      { "name": "tool", "type": "string", "required": false },
      { "name": "args", "type": "object", "required": false },
      { "name": "result", "type": "any", "required": false },
      { "name": "status", "type": "string", "required": true },
      { "name": "error_code", "type": "string", "required": false },
      { "name": "error_message", "type": "string", "required": false },
      { "name": "start_time", "type": "string", "required": true },
      { "name": "end_time", "type": "string", "required": true },
      { "name": "duration_ms", "type": "number", "required": true },
      { "name": "retry_count", "type": "number", "required": true },
      { "name": "llm_used", "type": "boolean", "required": true },
      { "name": "fixation_used", "type": "boolean", "required": true }
    ]
  },
  "metricsSchema": {
    "totalTimeMs": true,
    "llmTimeMs": true,
    "toolTimeMs": true,
    "stepTimeMs": true,
    "retryCount": true,
    "tokenUsage": true,
    "cost": true,
    "successRate": true,
    "p95": true
  }
}
```

---

## 12. 变量引用语法约定

执行器需要支持以下引用形式：

| 语法 | 含义 |
|---|---|
| `{{dir}}` | 引用输入变量 |
| `{{constants.time_format}}` | 引用常量 |
| `{{s1.exists}}` | 引用步骤 s1 的输出 exists |
| `{{s3b.content}}` | 引用步骤 s3b 的输出 content |
| `{{now:time_format}}` | 系统内置：当前时间，按指定格式 |
| `{{env.XXX}}` | 环境变量（可选） |

---

## 13. 设计原则

1. **spec 只描述执行，不描述身份。** meta / match / lineage / selection 全部外置。
2. **静态值进 argsTemplate，动态值变 `{{var}}`。**
3. **每步必有 successCriteria 与 failurePolicy。** 不能只看工具返回。
4. **固化信息放 fixation，执行器据此决定走不走 LLM。**
5. **未验证分支用 condition + inferred 语义标注**（可通过 fixation.confidence 体现）。
6. **runtimeRecordSchema / metricsSchema 由 spec 定义**，保证回流数据统一。
7. **spec 可独立缓存和 diff**，便于版本对比和选最优。

---

需要的话，我可以把这份文档转成 **JSON Schema 文件** 或 **TypeScript 类型声明文件**，直接放进项目。
