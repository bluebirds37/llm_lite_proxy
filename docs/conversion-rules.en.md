# Conversion Rules

[中文](conversion-rules.md) · **English**

The proxy has exactly one internal representation, **Chat Completions**; `/v1/responses` and `/v1/messages` each maintain a bridge into it.
What follows are the field-level mapping rules; the "dropped in conversion" list you see when expanding a row in the UI's log panel is produced by these rules.

## Request direction: OpenAI → Anthropic

- Leading consecutive `system` / `developer` messages are merged into the top-level `system`; a `system` appearing mid-conversation is folded into the adjacent user turn
- Consecutive same-role messages are merged into one (Anthropic requires user/assistant to alternate)
- `role: tool` → `tool_result` block, placed at the front of that user message
- `tool_calls` → `tool_use`; an `arguments` value that is not valid JSON degrades to `{}`
- Tool names are sanitized against `^[A-Za-z0-9_-]{1,128}$`, over-long ones are truncated to prefix + hash, and the mapping table is kept so the response stage can restore them
- `stop` → `stop_sequences`; when `max_tokens` / `max_completion_tokens` is missing, `server.default_max_tokens` fills it in
- `tool_choice`: `required` → `any`, `{function:name}` → `{tool,name}`; `parallel_tool_calls:false` → `disable_parallel_tool_use:true`
- `reasoning_effort` → `thinking.budget_tokens`, and `max_tokens` is guaranteed to be larger than the budget; `temperature` is dropped in that case (Anthropic forces it to 1)
- The json_schema of `response_format` → top-level `output_format`; the schema is patched to `additionalProperties:false` with every field required
- `n > 1` returns 400 outright, because Anthropic returns a single result; silent degradation would hand the client wrong data
- `presence_penalty`, `frequency_penalty`, `logit_bias`, `seed`, `logprobs`, `stream_options` have no counterpart field, so they are dropped and recorded in the log
- Unrecognized top-level parameters are dropped by default; with `passthrough_unknown_params` enabled they are forwarded to the upstream unchanged

## Request direction: Responses ⇄ Chat

- `instructions` ⇄ leading system message; `max_output_tokens` ⇄ `max_tokens`
- The `call_id` of a `function_call` item is Chat's `tool_calls[].id`; on the return path the `fc_` prefix is re-added as the item id
- `output_text` is a convenience field synthesized by the SDK and never on the wire, so the proxy does not emit it either
- `previous_response_id`, `background: true` and a non-empty `include` all return 400: the proxy stores no response history, and silent degradation would leave the client believing it had continued the context
- Responses' `input_tokens` already includes cache hits and `output_tokens` already includes reasoning, so only the breakdown is split out, rather than being added up again as with Anthropic
- Streaming wrap-up: `response.completed` waits until usage is available before being sent, and `data: [DONE]` is appended at the end (the `[DONE]` sent by the upstream is swallowed in conversion mode and then re-emitted)

## Response direction

- `stop_reason` ⇄ `finish_reason`: `end_turn`⇄`stop`, `max_tokens`⇄`length`, `tool_use`⇄`tool_calls`, `refusal`⇄`content_filter`
- Anthropic's `input_tokens` and the two cache buckets do not overlap, so OpenAI's `prompt_tokens` equals the sum of all three and the cache breakdown goes into `prompt_tokens_details`; the reverse direction subtracts and never subtracts into a negative number
- `thinking` blocks → `reasoning_content` + `thinking_blocks` (the signature is kept so it can be sent back)
- Streaming wrap-up: Anthropic's `message_delta` waits for OpenAI's usage-only chunk before being sent, and `[DONE]` never leaks out to Anthropic clients
- When the upstream replies with SSE but the client did not ask for streaming (or the other way round), the result is collected and returned in the client's format, and this is flagged in the log

## Pass-through

When the inbound API surface and the upstream API surface are the same kind of API, the request is handled as pass-through: only the model name is rewritten, client-native parameters (`top_k`, `store`, `reasoning.summary`, etc.) are kept as-is, and no field inspection is performed.

## Authentication and error envelope

The protocol family decides both of these, independent of the API surface:

| Protocol family | Auth header | Error body |
| --- | --- | --- |
| `openai` | `Authorization: Bearer <key>` | `{"error":{"message","type","param","code"}}` |
| `anthropic` | `x-api-key: <key>` + `anthropic-version` | `{"type":"error","error":{"type","message"}}` |

On the client side both `Authorization: Bearer` and `x-api-key` are accepted, and what is validated is `server.master_keys`; on the upstream side the matching form is emitted according to the upstream's protocol family. Errors coming back from the upstream are re-wrapped in the **client's** protocol family, so when an OpenAI SDK call against a Claude upstream fails, the SDK can still parse the error object normally.
