---
name: infra-change
description: Swap or add an infrastructure tool without changing domain or application layers.
argument-hint: <infrastructure change description>
disable-model-invocation: true
---

Read the relevant `architecture/backend/` doc for the component being changed:
- Data store: `architecture/backend/data-store.md`
- Storage: `architecture/backend/storage-layout.md`
- Transcoding: `architecture/backend/transcoding.md`
- Task queue: `architecture/backend/task-system.md`
- Observability: `architecture/backend/observability.md`

Also read `architecture/backend/workspace-crates.md` for source layout.

Infrastructure change: $ARGUMENTS

Rules:
- Domain and application crates must not change — port traits stay identical
- Only `crates/infrastructure/` implementations and composition root wiring change
- If adding a new external service (not replacing): define the port trait in domain first

Steps:
1. Identify the domain port trait(s) involved
2. Write the new implementation in `crates/infrastructure/src/`
3. Update wiring in `crates/api/src/main.rs` and/or `crates/worker/src/main.rs`
4. Remove old implementation if fully replaced
5. Update the Technology Stack table in `architecture/system-architecture.md`
6. Update `architecture/decisions.md` if this is a deliberate architecture decision

Do not touch any use case, domain entity, or business-spec file.
