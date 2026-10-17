# Self-managed participant profiles

Participants have stable UUIDs and optional self-reported directory profiles. A description may contain expertise, services, location, interests, preferences or other information the participant explicitly wants in the directory. There are no AC/plumber/resident role enums. Anyone can request help regardless of their own description.

Profiles are opt-in. The LLM interprets an authenticated participant's request and calls bounded Rust tools. Carbon never treats profile contents as instructions, verified qualifications, trust, or permission to contact someone.

## WhatsApp examples

- “Create my directory profile. My name is Alex. I repair AC systems and heat pumps around Test Community.”
- “Show my profile.”
- “Update my profile: I now offer heat-pump installation only.”
- “Withdraw my profile from the directory.”
- “Find someone who can repair my AC. I am an AC professional too, but need another person.”

These are invented examples. A profile request describes information intended for discovery; normal private conversations are not automatically published. Profile management needs no admin approval. Discovery requires no admin setup. Standing contact scopes are still required for outbound messaging through the existing administration flow.

## Tools and audience

The main coordinator has `read_my_profile`, `manage_my_profile` and `search_people`. `manage_my_profile` takes no planner text, draft or target ID. It invokes an isolated model editor that sees only the authenticated fresh message and current curated profile. Private tasks, history, resource contents and directory results from the planner are excluded. At most one isolated edit session is allowed per incoming turn, bounded to four model rounds and three tool calls per response. Direct publication tools requested by the main planner are rejected, even if their names are guessed.

Only that isolated editor receives `save_my_profile` and `withdraw_my_profile`. Saving uses the exact immutable `base` (null initially); withdrawal uses the current base. The name is at most 80 bytes and complete Markdown description at most 4096 bytes. Internal task events cannot publish or withdraw profiles. Caller identity is injected by Rust; model-supplied user IDs are rejected. The LLM decides whether the fresh message actually requests a profile change; Rust bounds the available data, owner and actions. Photo/document extraction is not part of this first profile editor.

`search_people` accepts focused keyword queries and returns up to ten currently published profiles of other registered participants in this Carbon instance. Profiles are public to registered participants without group membership or contact-scope setup; they are not an anonymous internet directory. Results include participant UUIDs, resource IDs, self-reported text and separate `can_contact`/`scopes` fields. The compatibility `scope` field is the first eligible messaging scope or null. Empty scopes do not hide a profile or authorize outreach. Transport phone numbers and private history are excluded. Search uses keyword prefixes requiring all terms; the LLM chooses synonyms and decides relevance. This is lexical retrieval, not trust ranking or a vector database.

Search-only responses show matching names and relevant descriptions, labeled self-reported once. They do not add unsolicited vetting disclaimers or contact-permission commentary; contact restrictions are explained when an actual contact request is blocked.

Discovery and contact eligibility remain separate from relevance. A published profile does not create a scope, membership, admin capability or permission to send a message. The requester’s own profession does not constrain the search. Self is excluded from contact results; own-profile inspection uses `read_my_profile`.

## Storage, consistency and revocation

Profile descriptions use existing immutable Markdown resources in the internal Git repository under `/data/content/resources/<resource UUID>/content.md`. SQLite under `/data/state` links the owner UUID to its current resource and published state. Publication, revision linkage, current pointer and search indexing commit in one SQLite transaction. Owner constraints prevent linking another participant's resource. Existing storage/request quotas apply.

Only the current published revision has implicit directory readability to all registered participants in this Carbon instance. Older versions remain available to their owner; no directory access to history is inherited. Ordinary `revise_note` cannot edit profile lineage: use the profile tool so the pointer and visibility remain consistent. Updating or withdrawing does not silently alter independently approved resource grants or delete historical Git content.

The isolated editor's inputs are fresh participant text and the current curated profile. Unrelated resources read by the private planner are not copied into profile provenance or publication context. Existing dependencies of the current profile are retained; genuinely derived information remains unreadable to audiences without source access. The first version takes a self-contained text description and does not read private notes/photos/documents or trigger sharing-permission prompts.

Reads are recorded per model operation. Profile withdrawal/supersession invalidates affected in-flight work and cancels its unsent queued outputs; unrelated tasks and the global permission epoch remain unchanged. Task dispatch also rechecks profile-source readability. Already delivered information or transport sends already in flight cannot be recalled.

All new state stays inside the existing `/data` backup boundary. Rollback to an older application treats these resources as ordinary private notes; the newer tables remain preserved.

## Tests

Deterministic tests cover self-management, cross-account argument rejection, stale edits, current-only access, public discovery without scopes or memberships, contact denial without scopes, private-source protection, queued-output cancellation, internal-event rejection, persistent identity linkage and transaction failure without SQLite/search drift. A mock HTTP model E2E invokes actual Rust profile tools through the production model loop and durable reply queue. It places a private-task canary in the planner context and proves the editor cannot see it, the planner cannot publish a direct draft, and extra planner arguments cannot enter the editor.

The ignored `live_profile_directory_journey` uses the real model proxy with simulated WhatsApp intake. With no contact scopes or memberships configured, three participants ask the model to publish profiles; an AC professional then asks for another AC professional. It requires real profile-tool execution, finds the relevant participant, excludes the plumber and stages no outreach. `CARBON_E2E_LIVE=1`, `CARBON_E2E_MODEL`, `OPENAI_BASE_URL` and a fresh `CARBON_E2E_ARTIFACT_DIR` are required. Transcripts and raw model/tool traces are retained for comparison. No live WhatsApp messages are sent.

## Deferred: independent trust and abuse resistance

Self-reported profiles deliberately have no vetted status or trust level. Later work should separately research independent verification, authored endorsements, evidence, expiry/revocation, graded trust, bot resistance and abuse signals. Any future trusted claims must have protected authorship and provenance; a participant cannot grant themselves trust or privileges by editing Markdown. Existing request/storage limits remain the first protection in this version. No trust system is implemented here.

## Acceptance result