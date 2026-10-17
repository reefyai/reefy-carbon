# Alternatives researched and why we chose SQLite + Git/LFS

Research/decision summary, 2026-09-26. These alternatives do not introduce additional permission authorities.

| Option | Benefits | Costs and Carbon decision |
|---|---|---|
| Ordinary immutable files + SQLite | Familiar embedded transactions, indexed grants, simple resource lookup. | Cross-store recovery still needed; file history would be custom. Superseded by Git/LFS for resource revisions, retaining SQLite authority. |
| Git/LFS content + SQLite permissions | Content history and portable formats; transactional identity/ACL/workflow; no server needed locally. | Two persistence systems require content-first publication, retention and coordinated backups. **Selected.** |
| Git + per-file ACL sidecars | Content/policy can be inspected and committed together. | A Git commit is not atomic with live groups, approvals, jobs or external effects; historical ACLs can restore obsolete authority. Requires parser, concurrency, cache invalidation and indexed retrieval discipline. Not selected. |
| Folder ACL sidecars | Fewer files; convenient inherited policies. | Moves/ancestor changes affect many resources; missing versus damaged policy needs explicit distinction; per-file exceptions add complexity. Not selected. |
| Markdown-only operational state | Human-readable/exportable records and simple initial demos. | Would recreate transactional uniqueness, locking, indexing, job claims and recovery. Not selected. |
| Git-native permissions | Repository/ref controls help developers collaborate. | They do not supply participant-level reads of individual resources inside a shared repository. Git is storage, not Carbon's authorization engine. |
| Filesystem owner/mode, inode or xattr policy | Useful OS isolation or metadata primitives. | Carbon participants share one process/OS user; inode reuse, copies and restore complicate identity. Stable resource UUIDs plus Rust checks selected. SELinux integration declined. |
| Per-participant encryption keys | Can support additional at-rest threat models. | Shared content, SQLite/index leakage, key backup/recovery and revocation remain difficult. Deferred pending a concrete threat/key-management design. |

## ACL sidecars are established in specific systems

[Solid WAC](https://solidproject.org/TR/wac) defines ACL resources represented in RDF, with agents/groups and Read, Write, Append and Control modes. It is a Draft Community Group Report, explicitly not a W3C Standard. It describes resource-to-ACL associations, not a mandatory physical acl.json format or owner:uuid field. Carbon's proposed UUID JSON would have been its own policy, not automatic WAC conformance.

[Community Solid Server's tutorial](https://github.com/CommunitySolidServer/tutorials/blob/main/getting-started.md) provides a practical filesystem .acl example. [Dovecot](https://doc.dovecot.org/main/core/plugins/acl.html) uses per-mailbox dovecot-acl files and supporting caches/indexes. These show that adjacent policies can work with a defined enforcement model; they do not establish superiority over relational ACLs or a general multi-tenant startup trend.

Per-folder versus per-file policy is a semantic choice: folder policies reduce repetition, file policies allow exceptions. Neither removes the need to authorize all reads, handle moves/revocation/concurrency, and protect policy edits. Physical proximity does not ensure files and policy are backed up, renamed or published atomically.

## Reliability, indexing and security lessons

SQLite can atomically change grants, membership, approval consumption and audit/outbox records, with constraints and indexed lookups. Sidecars can be reliable too, but cross-file changes need their own transaction/recovery protocol. There is no universal evidence that placing an ACL beside bytes makes the whole system more reliable.

Likewise, files are not inherently slow and SQLite is not magically fast. Sidecar systems often cache/index policies; those indexes introduce invalidation work. For Carbon's many scoped searches and background operations, one indexed transactional authority is a simpler starting point. Benchmark actual resource/group sizes rather than claiming a performance ratio.

[OWASP authorization guidance](https://cheatsheetseries.owasp.org/cheatsheets/Authorization_Cheat_Sheet.html) and [multi-tenant guidance](https://cheatsheetseries.owasp.org/cheatsheets/Multi_Tenant_Security_Cheat_Sheet.html) inform deny-by-default checks, trusted identity scope, authorization on every access and isolation testing. Storage format alone supplies none of these. For uploads, apply [file-upload protections](https://cheatsheetseries.owasp.org/cheatsheets/File_Upload_Cheat_Sheet.html): generated internal paths, bounded content, safe processing and no direct public serving. Missing/corrupt authority must fail closed, never fall back to folder names or old Git ACLs.

The [identity/storage research](identity-permissions.md#established-systems-and-lessons-for-carbon) covers Paperless-ngx, Nextcloud and Seafile as examples of metadata/content separation and its recovery obligations.

## Git in AI systems

[Letta Context Repositories](https://www.letta.com/blog/context-repositories/) provide a concrete Git-backed agent-memory example. [OpenClaw workspaces](https://docs.openclaw.ai/concepts/agent-workspace) also support Git-based workspace backup while keeping other runtime state separate. This is evidence of adoption for inspectable/versioned memory, not evidence that Git solves multi-participant ACLs or is a universal AI storage standard.

[Git namespace security guidance](https://git-scm.com/docs/gitnamespaces#_security) warns against treating namespaces as confidentiality boundaries. Carbon therefore exposes no clone/raw-history access. Gitolite/Gerrit-style repository/ref or push restrictions do not make a shared Git object store safe for per-file participant reads.

The selected repository is runtime data, separate from reefy-carbon source. Local [LFS objects and pointers](https://github.com/git-lfs/git-lfs/blob/main/docs/spec.md) work without remote hosting; all objects must be retained/backed up with SQLite. Neither a commit nor a Git push substitutes for the [whole-volume backup contract](backup.md).

## Agent and Obsidian lessons retained

See [memory research](memory-research.md): Markdown notes plus media are a useful knowledge representation; structured session/operational state remains separate. Borrow bounded relevant context, explicit memory writes, provenance and linked records, not unrestricted personal-agent filesystem tools. Start Carbon retrieval with scoped full-text search; semantic/vector retrieval must demonstrate benefit before adding another subsystem.
