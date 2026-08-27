```
[
  // 1. 用户提问
  {
    "role": "user",
    "content": "今天成都天气怎么样？"
  },

  // 2. 模型返回的指令（包含 tool_calls，这里没有结果）
  {
    "role": "assistant",
    "content": null,
    "tool_calls": [
      {
        "id": "call_abc123",       // <-- 关键：这个ID用来关联结果
        "type": "function",
        "function": {
          "name": "get_weather",   // 只有方法名
          "arguments": "{\"city\": \"成都\"}" // 只有参数
        }
      }
    ]
  },

  // 3. ★★★ 工具执行后的结果，放在这里！ ★★★
  {
    "role": "tool",               // <-- 注意这个角色
    "tool_call_id": "call_abc123", // <-- 通过ID告诉模型这是哪个调用的结果
    "content": "{\"temp\": 28, \"weather\": \"晴朗\"}" // 结果放在content里
  },

  // 4. 最终模型根据结果生成的回复
  {
    "role": "assistant",
    "content": "今天成都是晴天，气温28摄氏度。"
  }
]

```
