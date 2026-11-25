+# Crates

| Crate | Responsibility |
| --- | --- |
| `norte` | Facade that re-exports core types and enabled optional crates. |
| `norte-core` | Messages, parts, capabilities, model requests, stream accumulation, tools, and adapter contracts. |
| `norte-http` | Shared HTTP transport, profiles, authentication, and SSE decoding. |
| `norte-openai` | OpenAI Responses and Chat Completions adapters. |
| `norte-anthropic` | Anthropic Messages adapter. |
| `norte-gemini` | Gemini generateContent adapter. |
| `norte-tools` | Optional ready-made tool implementations. |
| `norte-mcp` | Optional MCP client, server, and tool integration. |
| `norte-testing` | Scripted models, probes, fixtures, and conformance helpers. |
| `norte-macros` | Procedural macros for typed tool declarations. |

## Runtime boundary

Norte is a library of model-call and tool primitives. It does not provide a
session store, turn loop, commit protocol, or conversation persistence layer.
Applications own interaction history, tool policy, retries, cancellation, and
persistence. `BlobStore` remains a separate media-bytes port in core; it does
not store sessions or conversations.
