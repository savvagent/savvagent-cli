# Savvagent Provider Protocol (SPP) — v0.3.0

A small layering on top of the [Model Context Protocol] for exposing LLM
providers as MCP servers. Savvagent (the host) talks to provider servers over
**Streamable HTTP** transport and to tool servers over **stdio** transport,
treating both uniformly as MCP clients.

[Model Context Protocol]: https://modelcontextprotocol.io/

## Goals

1. Make adding a new model provider equivalent to writing a small standalone
   binary, with no changes to the Savvagent host.
2. Stay strictly within the MCP spec for transport, framing, and error
   handling. The only thing SPP defines is **the shape of one tool's input,
   output, and progress notifications**.
3. Be expressive enough to carry text, multimodal input, tool use, and
   extended thinking without a lossy translation step.
4. Keep wire size small and parse cost low.

## Non-goals

- Defining a new RPC or transport. Use MCP.
- Specifying authentication. Provider servers configure auth out-of-band
  (env vars, config files, OS keychain).
- Specifying caching, batching, or session semantics. Those live in the host.

## Conformance

A *provider server* is **SPP-conformant** if it:

1. Speaks MCP over Streamable HTTP.
2. Exposes a tool named `complete` whose input matches the
   [`CompleteRequest`](#completerequest) JSON schema.
3. Returns either a [`CompleteResponse`](#completeresponse) JSON object as the
   tool's structured content, or an MCP tool error containing a
   [`ProviderError`](#providererror) JSON object.
4. When the request asks for streaming, emits zero or more
   [`StreamEvent`](#streamevent)s as MCP progress notifications before the
   final tool result.

Optional tools: `list_models`, `count_tokens`. Hosts must not require them.

## CompleteRequest

```jsonc
{
  "model": "claude-sonnet-4-6",
  "messages": [
    { "role": "user",
      "content": [{ "type": "text", "text": "..." }] }
  ],
  "system": "You are helpful.",          // optional
  "tools": [                             // optional
    { "name": "ls",
      "description": "list a directory",
      "input_schema": { "type": "object", "properties": { "path": { "type": "string" } } } }
  ],
  "temperature": 0.7,                    // optional
  "top_p": 0.95,                         // optional
  "max_tokens": 4096,                    // required
  "stop_sequences": ["\nUser:"],         // optional
  "stream": true,                        // optional, default false
  "thinking": { "budget_tokens": 8192 }, // optional
  "metadata": { "session_id": "..." }    // optional, opaque
}
```

`messages[].content[]` blocks share a tagged `type` discriminator. Supported
block types:

- `text` — `{ "type": "text", "text": "..." }`
- `tool_use` — `{ "type": "tool_use", "id": "...", "name": "...", "input": { ... } }`
- `tool_result` — `{ "type": "tool_result", "tool_use_id": "...", "content": [ ... ], "is_error": false }`
- `image` — `{ "type": "image", "source": { ... } }` where `source` is either
  `{ "type": "base64", "media_type": "image/png", "data": "..." }` or
  `{ "type": "url", "url": "https://..." }`
- `thinking` — `{ "type": "thinking", "text": "...", "signature": "..." }`
- `html` — `{ "type": "html", "source": "<complete HTML document>" }`. Used
  by hosts that have an HTML renderer registered (e.g. inline rendering in
  a terminal that supports an image protocol). Hosts without a renderer
  render the source as a code block. The source is parsed fresh on each
  render; providers may emit it via [`StreamEvent::ContentBlockStart`] +
  zero or more [`StreamEvent::ContentBlockDelta`] with `html_source_delta`
  before [`StreamEvent::ContentBlockStop`].

## CompleteResponse

```jsonc
{
  "id": "msg_abc",
  "model": "claude-sonnet-4-6",
  "content": [
    { "type": "text", "text": "I'll list /tmp." },
    { "type": "tool_use", "id": "toolu_1", "name": "ls", "input": { "path": "/tmp" } }
  ],
  "stop_reason": "tool_use",
  "stop_sequence": null,
  "usage": {
    "input_tokens": 412,
    "output_tokens": 27,
    "cache_creation_input_tokens": 0,
    "cache_read_input_tokens": 1024
  }
}
```

`stop_reason` ∈ `end_turn | tool_use | max_tokens | stop_sequence | refusal | other`.

## StreamEvent

Each event is delivered as an MCP `notifications/progress` whose
`params.message` is a JSON object with these tagged variants. Events for a
single response always begin with `message_start` and end with `message_stop`.

```text
message_start
  content_block_start { index: 0, block: text("") }
  content_block_delta { index: 0, delta: text_delta("Hello") }
  content_block_delta { index: 0, delta: text_delta(" world") }
  content_block_stop  { index: 0 }
  content_block_start { index: 1, block: tool_use("ls", input: {}) }
  content_block_delta { index: 1, delta: input_json_delta("{\"pa") }
  content_block_delta { index: 1, delta: input_json_delta("th\":\"/tmp\"}") }
  content_block_stop  { index: 1 }
  content_block_start { index: 2, block: thinking("") }
  content_block_delta { index: 2, delta: thinking_delta("step 1…") }
  content_block_stop  { index: 2 }
  content_block_start { index: 3, block: html("") }
  content_block_delta { index: 3, delta: html_source_delta("<!docty") }
  content_block_delta { index: 3, delta: html_source_delta("pe html><body>...") }
  content_block_stop  { index: 3 }
message_delta { stop_reason: "tool_use", usage_delta: { output_tokens: 27 } }
message_stop
```

Hosts:

- MUST tolerate interleaved `ping` events.
- MUST concatenate `input_json_delta` fragments by `index` and parse once at
  `content_block_stop`.
- MUST treat missing fields as defaults (deserializers use `serde(default)`).
- SHOULD fall back to the final tool-result payload as the source of truth if
  the stream is interrupted.

## ProviderError

Returned as the MCP tool error payload when `complete` fails terminally.

```jsonc
{
  "kind": "rate_limited",
  "message": "Slow down.",
  "retry_after_ms": 30000,
  "provider_code": "rate_limit_error"
}
```

`kind` ∈ `invalid_request | authentication | permission_denied |
model_not_found | context_length_exceeded | rate_limited | overloaded |
refusal | network | internal`.

## Versioning

The protocol version is exposed at `savvagent_protocol::SPP_VERSION`. Breaking
wire changes bump the major component. Provider servers SHOULD advertise the
version they implement via MCP server `instructions` or a future
`provider://info` resource.

### v0.3.0 changes (additive)

- `ContentBlock::Html` gained an optional `state` field (base64 opaque
  interactive-state blob). Omitted when absent (`skip_serializing_if`).
  Providers emitting v0.2.0 `html` blocks (no `state`) remain conformant;
  the field is host-internal (set by the TUI when persisting transcripts,
  not by providers).

### v0.2.0 changes (additive)

- Added `ContentBlock::Html` variant.
- Added `BlockDelta::HtmlSourceDelta` variant.

Providers emitting only v0.1.0 block types remain conformant; the new
types are additive. Hosts that do not handle `Html` blocks SHOULD render
the source as a code block to avoid silently dropping output.

## Open questions (deferred to v0.3)

- Resource catalog (`provider://info`, `provider://models`).
- Cancellation: SPP relies on MCP cancellation today; we may add an
  `abort_reason` field on the final `MessageDelta` when v0.2 lands.
- Caching directives: today, prompt caching is implicit (provider decides).
  v0.2 may add explicit cache-control hints on individual messages, mirroring
  Anthropic's `cache_control` markers.
