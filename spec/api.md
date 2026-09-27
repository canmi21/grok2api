# The HTTP surface

What a client of grok2api sees. How the answers are produced is [bridge.md](bridge.md).

## Two endpoints, OpenAI's shape

- `POST /v1/chat/completions`, both streamed (server-sent events, `stream: true`) and not.
- `GET /v1/models`, listing the models the resident agent offers. The list is read from the
  agent, not written down here, because the subscription decides it and it changes: the same
  account showed two models before signing in and four after.

Nothing else for now. `/v1/responses` and Anthropic's `/v1/messages` were considered and left
out; the chat completions shape is the one nearly every client speaks.

## Reasoning is returned, as `reasoning_content`

The agent streams its reasoning as `agent_thought_chunk`. grok2api returns it rather than dropping
it, in the `reasoning_content` field beside `content` -- on the message when not streaming, on the
delta when streaming. The field is not in OpenAI's schema; it is the convention DeepSeek
established and that most clients which show reasoning already read. A client that does not know
it ignores it.

## Function calling is not supported

The agent executes its own tools; it has no way to hand a tool call back to the caller and wait
for the result. Emulating that through prompting and parsing was judged too fragile to start
with, so the `tools` and `tool_choice` parameters are not supported.

## Every interface, one port, one key

The server listens on all addresses on one configured port, because it runs on a server and is
reached from elsewhere ([deployment.md](deployment.md)). Every request must carry one configured
API key as `Authorization: Bearer <key>`; a request without it, or with another, is refused. One
key, not a key store: this is one person's proxy.
