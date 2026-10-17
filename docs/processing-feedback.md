# Processing feedback and timeouts

Carbon uses `whatsapp-rust` 0.7.0's native `client.chatstate().send_composing()` / `send_paused()` API. No text such as “typing…” is sent as a substitute.

- Typing starts during active processing, including attachment intake and private or pair model work, and refreshes every 10 seconds. A queued request does not show typing until its worker begins.
- After 25 seconds, Carbon attempts one fixed message: “Still working on it…” The model cannot change its text or recipient.
- Typing stops when processing completes or fails, is cancelled, loses access, or the transport disconnects. Cancellation of the parent future also stops its feedback task. A disconnected socket cannot reliably deliver `paused`; Carbon stops refreshing and WhatsApp expires the indicator. A generation check prevents stale cleanup from clearing a newer operation's typing state.
- Presence I/O is bounded to 2 seconds and notice I/O to 5 seconds. Feedback is concurrent with model work; failures never trigger model retries or fail an otherwise valid reply.

The notice is reserved in SQLite before dispatch, once per incoming operation and destination. Lost acknowledgement, cancellation and restart never replay it automatically. This favors avoiding duplicate updates; an interrupted send may produce no notice. It is transport feedback, excluded from the model's private conversation history. Pair notices retain their exact task/destination binding so WhatsApp Reply and STOP still route to the correct conversation. Quoted STOP is validated at live intake and closes the pair without waiting for the model worker; it cannot retract an already dispatched message.

Native presence and notices use the same current identity/scope/source checks as their processing context. Text is fixed and contains no private resource information. A permission change, identity change, scope closure or unlink stops further feedback. Provider work already dispatched cannot necessarily be cancelled at the provider, but Carbon discards the stale result.

## Timeout budget

The installed Reefy proxy uses a 60-second upstream read timeout and up to three retries after the initial attempt. Those four attempts can take roughly four minutes plus connection/backoff time; Carbon's old 90-second total HTTP deadline could abandon a request while the proxy was still working.

Carbon now allows **300 seconds per model/provider HTTP request**, with a **10-second connection timeout**. Model inventory requests remain bounded to **15 seconds** and provider cleanup to its existing **10 seconds**. Carbon adds no new retries. Typing remains active during the proxy's retries.

A separate **600-second overall active-processing deadline** bounds multi-round model/tool work, including media/provider operations. Existing model-round, tool, file and quota limits still apply. Timeout or cancellation discards unfinished output and cancels staged sends. A connected private participant receives the existing failure reply when it can be safely queued; disconnect/revocation leaves the operation interrupted instead of replaying it automatically. Processing now uses the [bounded concurrency lanes](concurrency.md), with each participant serialized; recovery controls remain separate planned work.

This gives ordinary proxy retries room to finish; it is not a promise to wait forever. Mixed retry categories, long `Retry-After` values or a provider that never completes may still exceed Carbon's bounds. The proxy's own upstream timeout is unchanged, so an individual upstream attempt with no data for 60 seconds can still time out and retry.

## Verification

Synthetic clock, SQLite and mock-HTTP tests cover lifecycle, isolation, uncertain delivery, cancellation and responses beyond the old deadline. The native API compiles against the pinned library. Actual phone rendering of typing and a delayed progress message requires a live user interaction; automated tests do not message real participants.

## Temporary model capacity failures

Carbon retries HTTP 429 and 503 responses from the model proxy, repeating only the failed HTTP request (not the tool loop or previously executed actions). After the first busy response, it allows at most 30 additional seconds and four retries, normally waiting 2, 4, 8 and 8 seconds. Retry requests themselves share that deadline. A numeric `Retry-After` can increase a delay; if it exceeds the remaining budget Carbon returns the busy reply without retrying early. A nonnumeric hint conservatively suppresses retries within this short window. Authentication, invalid requests and other failures are not retried by this policy. The existing initial model-request timeout remains separate.

Typing feedback continues during retry waits, with the existing one-time progress update at 25 seconds. Each attempt is traced when debug tracing is enabled; Activity includes retry events. If capacity errors persist, private-message handling returns: “The AI service is busy. Please try again shortly.” Other failures retain their existing fallback. The offline-message recovery TODO is unchanged and remains unimplemented.