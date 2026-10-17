# Debug model tracing

Set `"debug_trace": true` in `/data/state/config.json` and restart Carbon. Default is false. The setting is independent of `experimental_tasks`.

Production captures the actual JSON model requests (instructions, context, tool definitions and generation parameters), raw response bodies before parsing (including provider errors), tool results/rejections, and response disposition. Each HTTP round has a call UUID and shares the incoming message's operation UUID. Malformed `tool_calls` now fail explicitly instead of falling through to ordinary content. Requests saved without a response indicate interrupted/in-flight work; trace failures produce `trace_write_failed` activity.

Open Debug, expand an operation and choose **Download model trace**. The management API also exposes `GET /api/traces/{operation UUID}`, under the same authentication boundary as the rest of the UI. On Reefy installations without an app password, Reefy access control is that boundary. Never expose the management port directly. LLM resource tools cannot read traces.

Files are private (directory 0700, files 0600) under `/data/state/traces`, inside the backed-up data volume. Capture retains up to seven days, 1,000 event files or 256 MiB, evicting oldest entries on the next write. An event larger than 8 MiB becomes an explicit omission marker; an export larger than 16 MiB is explicitly marked truncated. Existing provider responses are limited to 1 MiB; traces preserve the bounded prefix and mark oversized responses. Exports indicate missing data and warn that retention can remove earlier entries. Disabling capture does not erase existing traces.

Conversation text and tool data are not redacted. HTTP authorization headers/API credentials are not collected. PDF/audio multipart requests record the immutable resource reference, MIME, size and filename instead of duplicating binary bytes; responses and PDF interpretation requests are captured. Image request JSON includes the image data URI. Model discovery, provider capability probes and temporary-file deletion are not model reasoning traces. Format/build versions accompany entries; exact prompts are present in captured requests.

Raw traces contain private conversation data. Debug capture defaults to disabled. Traces are diagnostic artifacts rather than durable audit records.