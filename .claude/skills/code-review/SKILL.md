---
name: code-review
description: Review implementation against the spec and architecture guides.
argument-hint: <file, diff, or description>
allowed-tools: Read, Grep, Glob
---

Review target: $ARGUMENTS

Determine what's being reviewed and read only the relevant spec:

- **Use case / business logic** -> read `business-spec/video/video.md`
- **Domain types** -> read `architecture/backend/workspace-crates.md`
- **Infrastructure** -> read the relevant backend doc (data-store, storage-layout, transcoding)
- **Task system** -> read `architecture/backend/task-system.md`
- **Error handling** -> read `architecture/backend/error-handling.md`
- **Full feature** -> read the spec and architecture docs for all layers involved

Check:

**Business logic** (if a spec exists):
- Guards: all spec guards enforced, no extra
- Mutations: DB writes match spec
- Error codes: exact codes from spec
- Output: response shape matches spec
- Side effects: tasks scheduled per spec, none extra

**Architecture**:
- Dependency direction: application cannot import infrastructure
- Error types defined per use case, not per entity
- Port traits in domain, implementations in infrastructure
- No business logic in infrastructure or presentation

**Layer boundaries**:
- No infrastructure types (sqlx, aws-sdk) leaking above infrastructure
- No domain types in HTTP response DTOs

For each issue: state file and line, quote the spec requirement, describe what the code does instead.
Conclude with: PASS or FAIL (with issue list).
