# Storage and memory research

Research informing Carbon's generic resource store. This is background, not an implemented feature contract.

## Comparison

Reviewed 2026-09-26; these patterns inform our Rust backend, without committing to either runtime. Carbon exposes only scoped application functions and does not inherit either runtime's execution capabilities.

| System | Observed storage pattern | Implication for Carbon |
|---|---|---|
| Hermes | `memories/USER.md` and `MEMORY.md` hold compact profile/memory; `state.db` stores structured sessions and messages. Media can be represented by saved paths and extracted descriptions. | Readable memory and structured history complement each other; Markdown is not the whole database. |
| OpenClaw | Workspace Markdown includes `USER.md`, `MEMORY.md`, and dated notes. Current workspace docs describe separate session state and an agent SQLite database. Its workspace is explicitly not a security sandbox by itself. | Use small retrieved notes with separate operational state, and enforce participant isolation independently of directory names. |

Sources: [Hermes file roles](https://hermes-agent.nousresearch.com/docs/user-guide/which-file-does-what), [Hermes sessions and media](https://hermes-agent.nousresearch.com/docs/user-guide/sessions), [OpenClaw memory](https://docs.openclaw.ai/concepts/memory), [OpenClaw workspace and isolation](https://docs.openclaw.ai/concepts/agent-workspace).

**Conclusion for this app:** Markdown and ordinary media files complement structured identity, access, conversation, action, and scheduling records. Carbon's generic resource schema is our proposal, not a claim that either agent runtime implements the same model.

### Main lessons from agent memory systems

- Separate compact durable facts from full conversation history. Read relevant resources when needed rather than putting the entire participant archive in the prompt.
- Store original attachments separately and retain links plus searchable captions/transcripts. A remembered description is not a substitute for the original photograph.
- Give memory explicit write/update tools and source references. A fact mentioned in a conversation is not automatically reliable, permanent memory.
- Use structured state for operational correctness. Hermes documents SQLite sessions; current OpenClaw documentation also describes an agent SQLite database alongside workspace Markdown. These examples inform the storage separation, not a mandatory business-domain schema.
- Separate file organization from access enforcement. The same storage ingredients do not automatically make a personal-agent workspace safe for multiple participants.

### Obsidian: useful design patterns

Obsidian's vault is a local folder of Markdown notes and regular attachments. It maintains a metadata cache and uses IndexedDB for internal state; its documentation does not describe SQLite as the authoritative note store. [Storage](https://help.obsidian.md/Files+and+folders/How+Obsidian+stores+data), [attachments](https://help.obsidian.md/attachments).

Borrow these ideas for Carbon:

- **Linked records:** make an AC note link to its orders, photos, warranty, and relevant community requirements. Reverse links answer “what work has happened on this unit?” without duplicating records.
- **Properties:** use YAML frontmatter for stable record IDs, record type, source references, revision, and review date. Keep authorization in the backend; an editable `owner_id` property must never grant access. [Obsidian properties](https://help.obsidian.md/Editing+and+formatting/Properties).
- **Portable links:** prefer standard Markdown links and stable ID-based paths. Display names may change independently. Backend moves must update references; do not assume Obsidian's own rename handling runs when Rust modifies files. [Internal links](https://help.obsidian.md/Linking+notes+and+files/Internal+links).
- **Useful views over files:** Obsidian Bases can show sortable/filterable tables and cards using Markdown properties, with view definitions in `.base` files. This could help maintain community documents or inspect exported equipment/order summaries; it does not replace Carbon's transactional database. [Bases](https://help.obsidian.md/bases).
- **Optional human editor:** keep curated knowledge Obsidian-compatible without making the backend depend on Obsidian or plugins. Maintainer edits enter a validation/review/publish flow; file edits cannot grant access or establish that an external action was executed.

Do not open the whole multi-participant runtime tree as a participant-facing vault. Provide explicitly scoped exports or a separately managed community-authoring vault. Obsidian vault boundaries are an organization choice; Carbon remains responsible for authorization and data ownership. Avoid concurrent uncontrolled edits to live runtime files.

Our proposed split remains: **files for knowledge and evidence; SQLite for identity, permissions, history, transactions, and workflow; rebuildable indexes for retrieval.** “Markdown + media + SQLite” matches broad patterns in the agent systems, but Carbon's identity, permission, and scheduling semantics are application-specific additions.

