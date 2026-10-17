# Concurrent message processing

Carbon receives WhatsApp events independently from model execution. The application has separate Tokio lanes for bounded processing, durable outgoing delivery, scheduler ticks and transport/cleanup maintenance. A slow provider request no longer holds the delivery/unlink barrier or prevents unrelated users and due timers from progressing. This uses async tasks on Tokio's existing thread pool, not one OS thread or process per conversation.

`model_concurrency` in the protected application JSON config defaults to **4** and accepts **1–16**. The bound includes private turns, scheduled task turns, profile-editor calls and legacy pair processing; nested model calls remain within their parent turn. It is a bound on Carbon turns, not on the proxy's internal attempts. Existing request/storage/fan-out limits still apply.

Production claims are atomic SQLite transactions. At most one private turn per stable participant UUID is processed at a time. Legacy pair work reserves both participants: a private turn and pair turn sharing a participant cannot overlap, nor can overlapping pairs. This deliberately favors safe sequencing over parallelism within one participant. Separate participants can progress concurrently. Private messages precede scheduled events when choosing inbox work, and the dispatcher alternates private and legacy pair claims to avoid starving either lane. Shared task updates retain ownership, authority checks and optimistic revision checks; concurrency never unions participants' access.

Completed replies use the existing durable queues. Replies for a participant follow their sequential turn completions. Task outreach retains its bounded batch of three different recipients and one in-flight task send per recipient. Ordinary replies, files, approval notifications and legacy pair transport sends use the delivery/unlink barrier. A legacy pair reserves a processing slot until its bounded transport send completes. Network delivery can still be delayed by another bounded send/upload; it is no longer delayed by an unrelated model call. There is no claim of unlimited parallel transport delivery.

Disconnect, STOP, permission changes and deadlines continue to use the existing progress and dispatch checks. Dropping the processing pool aborts its child futures, fences unfinished inbox work by deleting its work-authority record, and cancels staged outputs/schedules. No late response can finish that interrupted turn. A legacy send already in flight is recorded as uncertain, never replayed as a known unsent message. Shutdown/restart preserves the existing recovery rules: pending work remains durable, interrupted private turns are not silently replayed, and uncertain sends are not retried automatically. Offline WhatsApp ingestion recovery remains a separate TODO.

## Regression coverage

`tests/app/concurrency_e2e.rs` uses the real WhatsApp parser/intake, production dispatcher, HTTP model loop, SQLite permissions, scheduler and durable reply claims. Only transport delivery and model responses/latency are fixtures.

- A provider response for participant A is held behind a gate. B finishes and its reply is delivered while A is still blocked.
- A second message from A cannot start until the first finishes; outgoing replies remain ordered.
- B's model deliberately requests A's private resource. The read fails; B can read B's own resource, and neither participant's private canary appears in the other's model input.
- The actual scheduler fires while A's model request is held.
- Four independent turns can be active; a fifth participant and another turn for an active participant remain queued.
- Pool cancellation rejects late completion, stages no reply, and survives reopening the database while pending work remains claimable.
- Private and legacy pair claims sharing participants exclude one another.

The suite runs on Linux with `cargo test --locked concurrency_e2e`; it needs no live WhatsApp account or paid model credentials. Full deterministic checks also cover permissions, task revisions, delivery ordering, cancellation and progress feedback.

## Validation and development deployment

The complete deterministic suite passed **142 tests** (three paid/live lanes remain opt-in). After the test-only iterator style correction, all four concurrency E2E cases passed again, formatting checks passed and Clippy passed with warnings denied. Container checks covered required/optional authentication, CSRF, non-root startup, SIGTERM and persisted configuration.