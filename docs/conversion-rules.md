# 转换规则

**中文** · [English](conversion-rules.en.md)

代理内部只有 **Chat Completions** 一种表示，`/v1/responses` 与 `/v1/messages` 各维护一条到它的桥。
下面是字段级的映射规则；界面里日志面板展开一行看到的「转换丢弃」列表，就是按这里的规则产生的。

## 请求方向：OpenAI → Anthropic

- 开头的连续 `system` / `developer` 消息合并成顶层 `system`；中途出现的 `system` 折叠进相邻 user 轮次
- 连续同角色消息合并成一条（Anthropic 要求 user/assistant 交替）
- `role: tool` → `tool_result` 块，并排到该 user 消息的最前面
- `tool_calls` → `tool_use`；`arguments` 非法 JSON 时退化为 `{}`
- 工具名按 `^[A-Za-z0-9_-]{1,128}$` 清洗，超长按前缀 + 哈希截断，映射表保留到响应阶段还原
- `stop` → `stop_sequences`，`max_tokens` / `max_completion_tokens` 缺失时补 `server.default_max_tokens`
- `tool_choice`：`required` → `any`，`{function:name}` → `{tool,name}`；`parallel_tool_calls:false` → `disable_parallel_tool_use:true`
- `reasoning_effort` → `thinking.budget_tokens`，并保证 `max_tokens` 大于预算；此时 `temperature` 被丢弃（Anthropic 强制为 1）
- `response_format` 的 json_schema → 顶层 `output_format`，schema 会被补成 `additionalProperties:false` 且全字段 required
- `n > 1` 直接返回 400，因为 Anthropic 只返回单个结果，静默降级会让客户端拿到错数据
- `presence_penalty`、`frequency_penalty`、`logit_bias`、`seed`、`logprobs`、`stream_options` 无对应字段，丢弃并记进日志
- 顶层未识别的入参默认丢弃，打开 `passthrough_unknown_params` 后原样转发给上游

## 请求方向：Responses ⇄ Chat

- `instructions` ⇄ 开头的 system 消息；`max_output_tokens` ⇄ `max_tokens`
- `function_call` 条目的 `call_id` 就是 Chat 的 `tool_calls[].id`，回程时反向补 `fc_` 前缀作为条目 id
- `output_text` 是 SDK 合成的便利字段，不在 wire 上，所以代理也不会发出它
- `previous_response_id`、`background: true`、非空 `include` 一律返回 400：代理不存响应历史，静默降级会让客户端以为续上了上下文
- Responses 的 `input_tokens` 已包含缓存命中，`output_tokens` 已包含推理，因此只做明细拆分，不像 Anthropic 那样再叠一次
- 流式收尾：`response.completed` 等 usage 到位才发，末尾补 `data: [DONE]`（上游发的 `[DONE]` 在转换模式下会被吞掉后重新补发）

## 响应方向

- `stop_reason` ⇄ `finish_reason`：`end_turn`⇄`stop`、`max_tokens`⇄`length`、`tool_use`⇄`tool_calls`、`refusal`⇄`content_filter`
- Anthropic 的 `input_tokens` 与两个 cache 桶互不重叠，因此 OpenAI 的 `prompt_tokens` 等于三者之和，cache 明细放进 `prompt_tokens_details`；反向做减法且不会减成负数
- `thinking` 块 → `reasoning_content` + `thinking_blocks`（保留 signature 以便回传）
- 流式收尾：Anthropic 的 `message_delta` 会等到 OpenAI 的 usage-only 块再发，`[DONE]` 不会外泄给 Anthropic 客户端
- 上游回的是 SSE 而客户端没要流式（或反过来），会收集后按客户端格式返回，并在日志里标注

## 直通

入站面与上游面属于同一种 API 时按直通处理：只改写模型名，客户端原生参数（`top_k`、`store`、`reasoning.summary` 等）原样保留，不做字段审查。

## 鉴权与错误体

协议族决定这两件事，与 API 面无关：

| 协议族 | 鉴权头 | 错误体 |
| --- | --- | --- |
| `openai` | `Authorization: Bearer <key>` | `{"error":{"message","type","param","code"}}` |
| `anthropic` | `x-api-key: <key>` + `anthropic-version` | `{"type":"error","error":{"type","message"}}` |

客户端侧 `Authorization: Bearer` 与 `x-api-key` 两种都接受，校验的是 `server.master_keys`；上游侧按上游的协议族发出对应形态。错误在上游返回时按**客户端**的协议族重新包装，所以 OpenAI SDK 打 Claude 上游出错时，SDK 仍然能正常解析错误对象。
