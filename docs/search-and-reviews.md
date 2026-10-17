# Search and pending reviews

Ask Carbon “find my bicycle receipt” or “what requests are waiting for review?” No additional permission prompts are introduced. These features improve knowledge lookup and administration; autonomous cross-participant conversations remain planned.

## Search

`search_resources(query)` accepts up to 200 bytes and eight whitespace-separated terms. Terms are quoted as literal FTS terms and combined with AND; case-insensitive Unicode token matching is provided by SQLite FTS5. This is keyword search, not vector/semantic retrieval. The model can try more specific or alternative keywords within its existing tool/round limits.

Names are indexed for all resource types. Bodies are indexed for notes and valid UTF-8 files up to 16 KiB with plain text, Markdown, CSV or JSON MIME. PDFs, images, audio and video are searchable by filename only. To make interpreted contents searchable, ask Carbon to analyze the resource and save a note: existing source dependencies apply to that note. No local document parser or transcription engine is added.

The index lives in the existing `/data/state/carbon.sqlite` backup boundary. It is a derived cache and never an authorization source. Background indexing processes up to 32 immutable resources per minute, including older resources; each search also advances one batch. It uses verified Git/LFS bytes without adding those scanned resources to anyone's model context. Cache rows and completion markers commit atomically. Resource publication remains independent, so a crash before indexing leaves content available and backfill resumes later. Unreadable/non-UTF-8 bodies are indexed by name only; rebuilding after repairing storage requires clearing both `resource_search` and `search_indexed` while Carbon is stopped. Do not modify ACL tables for a rebuild.

Every candidate is checked against current SQLite ownership, direct/group/everyone grants, folder inheritance and recursive source dependencies. Text excerpts are reread from verified storage, never trusted from the cached body. Only returned results enter context-source tracking, so an unrelated private search hit cannot taint a saved note or leak its ID, name or content. Permission changes require no index rebuild. Existing global authority checks discard stale model work and queued replies after an approved change.

Search returns at most ten authorized results from at most 1,000 indexed candidates, ordered by resource creation and UUID rather than global relevance statistics. Excerpts include the first 360 characters; the model should read a matching resource for its full text. The tool always explains its bounds and possible incompleteness, without exposing hidden-match counts. Large collections, pagination, relevance ranking, multilingual semantic retrieval and automatic analysis indexing remain future work. Indexing is eventual, so a newly saved item may require a later search if a legacy backlog exists.

## Pending reviews

`pending_reviews()` returns unexpired pending requests: ordinary users see their own, and sealed reviewers see all. It includes the exact structured operation, requester UUID, expiry and whether the caller is a configured admin. Eligible admins, including the requester, receive instructions to use WhatsApp Reply on the approval notice and send `approve` or `reject`. Internal request IDs are not displayed in human-facing messages. A model cannot approve; the human must send the command, and Rust performs the existing stale-state, authority and replay checks. Listing a request does not guarantee it is still applicable when approved. Reviewed, expired and cancelled requests are excluded; use `proposal_status` for a known historical request. Resource bodies are never included.

Tests cover private-result exclusion, actor spoofing, folder inheritance, source-dependent revocation, restart/cache rebuilding, corrupt backing data, prompt-to-search-to-reply isolation, pending-request visibility and expiry.

Note search/listing returns the newest currently readable revision per note lineage. Private newer revisions never suppress an older version for its authorized readers. Use `note_history` to retrieve earlier readable versions; hidden revision IDs/names and total version counts are not returned. See [versioned notes](note-revisions.md).
