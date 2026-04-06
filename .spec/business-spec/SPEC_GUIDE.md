# Spec Guide — How to Write and Update Business Specs

> Read this file before making ANY change to a business spec.
>
> This guide is the **authoritative reference** for the structure and format
> of every file under `business-spec/`. If a spec file deviates from this
> guide, fix the file — don't fix the guide. If the format itself needs to
> change, update this guide first, then propagate the change to existing
> files in the same commit.

---

## What Business Specs Are For

Business specs describe **what the system does** — entities, their attributes,
permitted operations, who can do them, what changes, what errors can occur,
what the user sees on screen.

Specs are read by humans (PMs, new engineers, designers) and by AI agents
during implementation. Both audiences need the same thing: a precise,
unambiguous description of behavior in plain business language, free of code.

## What Business Specs Don't Contain

- Code or programming language syntax
- Framework-specific types (`String`, `Option<T>`, `axum::Json`, etc.)
- Implementation details (FFmpeg flags, SQL queries, gstreamer pipelines)
- Infrastructure concerns (CDN config, S3 bucket names, Redis keys)
- Architectural decisions (those live in `.spec/architecture/`)

Write as you would for a product manager who understands the domain but
doesn't know the codebase.

---

## File Organisation

```
business-spec/
├── SPEC_GUIDE.md                ← this file
├── user-stories/
│   └── <actor>.md               ← end-to-end narratives by actor type
├── <bounded-context>/
│   ├── <entity>.md              ← entity spec (backend behavior)
│   └── <entity>.ui.md           ← UI spec (screens and interactions)
└── task-system/                 ← special bounded context
    └── task-catalog.md          ← all background task types
```

### Bounded contexts

Each top-level folder under `business-spec/` (other than `user-stories/`) is a
**bounded context** — a coherent slice of the domain with its own entities,
language, and rules. Today the bounded contexts are:

- **video** — uploads, transcoding, playback
- **task-system** — background jobs (the catalog is its only file; the entity
  and lifecycle are infrastructure concerns documented under `architecture/`)

Add a new bounded context as a new top-level folder. Don't mix entities from
different contexts in the same file.

### Bounded contexts a story touches

User stories cross bounded contexts by nature — they describe a user goal
that may involve multiple entities. Declare the contexts a story touches with
a header line near the top of the file:

```markdown
**BCs touched**: Video, Task System
```

Entity files do not declare BCs because they ARE one — implicit from their
folder.

---

## ID Prefixes

Every use case, screen, and interaction has a stable ID. The prefix encodes
the bounded context. The number is a zero-padded sequential within that
bounded context, scoped per ID kind.

| Kind | Prefix template | Example |
|---|---|---|
| Use case | `UC-<BC>-NNN` | `UC-VID-001` |
| Screen | `SCR-<BC>-NNN` | `SCR-VID-001` |
| UI interaction | `UI-<BC>-NNN` | `UI-VID-001` |

Bounded context codes currently in use:

| Bounded context | Code |
|---|---|
| Video | `VID` |
| Task System | `TSK` |

When adding a new bounded context, pick a 3-letter code, document it here in
the same commit.

### Anchors and cross-references

IDs become anchors via the inline `{#...}` syntax (lowercase):

```markdown
### Initiate Upload {#UC-VID-001}
```

Reference them within the same file:

```markdown
See [UC-VID-001](#uc-vid-001).
```

Reference across files:

```markdown
See [UC-VID-001](video.md#uc-vid-001).
[UC-VID-007](../video/video.md#uc-vid-007)
```

Once an ID is published, never reuse or renumber it. If a use case is
deleted, leave the number retired and start the next addition at the next
free number — old links stay broken in a single, obvious way.

---

## File Type 1: Entity Spec (`<entity>.md`)

Lives in the bounded-context folder. Describes one entity's data shape, its
operations (use cases), and quotas. Backend behavior only — UI behavior goes
in the `.ui.md` sibling file.

### Template

```markdown
# [Entity Name]

## Overview

[1–3 paragraphs. What this entity is, why it exists, how it is created or
managed. Plain business language. No code.]

## Changelog

| Date | Change | Author |
|------|--------|--------|
| YYYY-MM-DD | [What changed] | [Name] |

---

## Definitions

### Attributes

| Attribute | Type | Nullable | Description |
|-----------|------|----------|-------------|
| `id` | Unique identifier | No | ... |
| `name` | Text (1–100 chars) | No | ... |

### Enums

#### [EnumName]

| Value | Description |
|-------|-------------|
| `FOO` | ... |
| `BAR` | ... |

### Child Entities (optional)

#### [ChildEntityName]

[Brief description.]

| Attribute | Type | Nullable | Description |
|-----------|------|----------|-------------|

---

## Use Cases

### [Use Case Name] {#UC-<BC>-NNN}

**Actor**: [Who can trigger this]

**Triggered by**: REST: `METHOD /path`

[Optional: 1–2 sentences of context if the use case isn't self-explanatory.]

**Input**

| Field | Required | Description and validation |
|-------|----------|---------------------------|
| `field` | Yes | ... |

**Guards**
1. [Pre-condition the system enforces before any mutation]
2. ...

**Mutations**
- [What changes in the system, in plain language]

**Output**

| Field | Description |
|-------|-------------|
| `id` | ... |

**Error Codes**

| Code | When it occurs |
|------|---------------|
| `ERROR_CODE` | ... |

**Side Effects**
[N/A, or a description of tasks scheduled / events emitted]

**Idempotency**: [Idempotent / Not idempotent — and why]

---

## Limits and Quotas

| Limit | Value | Enforcement |
|-------|-------|-------------|
| Max upload file size | 1 GB | Presigned URL policy + client check + guard |
```

### Conventions

- **Section order is fixed.** Omit a section that doesn't apply (e.g. no
  Enums), but never reorder.
- **Use case order** is roughly the entity lifecycle: create, read, update,
  delete, system-only operations last.
- **Use cases are `### H3`** within the `## Use Cases` H2. Their fields are
  `**Bold labels**`, not nested headings.
- **Guards are numbered**, mutations are bullets — guards are an ordered
  precondition list, mutations are an unordered set of changes.
- **Use case error codes are SCREAMING_SNAKE_CASE** strings, never sentences.
  These are the contract values returned to API clients.
- **System use cases** (no human actor — worker, scheduler, etc.) still get a
  use case entry. Mark them with **Actor**: `System — not user-facing`.
- **Idempotency is mandatory.** Every use case declares whether retrying it
  has the same effect.
- **Don't over-specify.** If a value can change without breaking clients
  (e.g. polling interval), put it in `Limits and Quotas` rather than burying
  it in mutations.

### Attribute type vocabulary

Use only these plain-language types in entity attribute tables. Add new ones
to this table in the same commit if a new shape is genuinely needed.

| Plain type | Meaning |
|---|---|
| `Unique identifier` | System-assigned ID, opaque to users |
| `Text` | String of arbitrary length |
| `Text (N–M chars)` | String with length constraints |
| `Number` | Integer |
| `Decimal` | Decimal number |
| `Date/time` | Point in time with timezone |
| `Boolean` | True or false |
| `Reference to [Entity]` | Foreign key / link to another entity |
| `[EnumName] (see Enums)` | One of the values defined in the Enums section |
| `List of [type]` | Ordered collection |

### Workflow: adding a new use case

1. Pick the next free number in the bounded context (look at existing IDs in
   the file).
2. Add the use case section under `## Use Cases`, in lifecycle position.
3. Fill every required field. Mark `**Side Effects**: N/A` if there are
   genuinely none.
4. Add the entry to the `## Changelog` table.
5. If the use case schedules a task, add or update the corresponding entry
   in `task-system/task-catalog.md` in the same commit.

### Workflow: adding a new entity

1. Create `<entity>.md` in the appropriate bounded-context folder. Use the
   template above.
2. If the entity has user-facing screens, also create `<entity>.ui.md`.
3. Pick or confirm the bounded context's ID prefix code in the table above.
4. Cross-link from related user stories.

---

## File Type 2: UI Spec (`<entity>.ui.md`)

Lives next to the entity spec. Describes the screens, layout, interactions,
and visual states a user sees. Does NOT describe backend behavior — link to
the entity spec for that.

### Template

```markdown
# [Entity] — Frontend Spec

> **Backend spec**: [<entity>.md](<entity>.md)
> **User stories**: [../user-stories/<actor>.md](../user-stories/<actor>.md)

## Changelog

| Date | Change | Author |
|------|--------|--------|

---

## Screens

### [Screen Name] {#SCR-<BC>-NNN}

**Route**: `/path`
**Entry points**:
  - [How users arrive on this screen]
**Layout**: [1–2 sentences describing the visual structure]
**Interactions**: UI-XXX-NNN, UI-XXX-NNN, ...

---

## Interactions

### [Interaction Name] {#UI-<BC>-NNN}

**Type**: [Local | Connected]
**Use Case**: [UC-<BC>-NNN](<entity>.md#uc-<bc>-nnn)   ← only for Connected
**Triggered by**: [User action that fires this]
**Screen**: [SCR-<BC>-NNN](#scr-<bc>-nnn)

**Behavior**
1. [Step]
2. [Step]

**Form Fields** (optional, for input interactions)

| Field | Maps to UC Input | Widget | Client Validation |
|-------|-----------------|--------|-------------------|

**States**

| State | Visual Behavior |
|-------|----------------|

**Error Display**

| Error Code | Display |
|------------|---------|
```

### Conventions

- **`Type: Local`** means the interaction does not call the backend (e.g.
  client-side validation, copy-to-clipboard, file selection). Omit the
  `**Use Case**` line.
- **`Type: Connected`** means the interaction invokes one or more backend
  use cases. Always link the use case(s) — these links are the contract
  between frontend and backend.
- **Multiple use cases**: chain them with `→`:
  `**Use Case**: [UC-VID-001](video.md#uc-vid-001) → [UC-VID-002](video.md#uc-vid-002)`
- **Form Fields** declare both the widget (text input, drop zone) and the
  client-side validation that runs before submission. The error codes the
  backend can return are listed under `Error Display`, not duplicated here.
- **States** describes what the user sees during the interaction's lifecycle
  (loading, success, error). Not internal state machines.
- **Error Display** maps the backend's error codes (defined in the entity
  spec) to user-facing messages. If a code isn't listed, the UI either does
  not surface it or shows a generic "Something went wrong" — call this out
  explicitly when it matters.
- **Interactions reference screens by ID**, screens reference interactions
  by ID. Both use the anchor link form.

### Workflow: adding a new screen / interaction

1. Pick the next free `SCR-<BC>-NNN` or `UI-<BC>-NNN`.
2. Add the screen section, then the interactions it hosts.
3. If it's `Connected`, the use case must already exist in the entity spec
   — add it there first if not.
4. Update the screen's `**Interactions**` list.

---

## File Type 3: User Stories (`user-stories/<actor>.md`)

End-to-end narratives describing what a particular kind of user does with the
system. Plain prose. No tables. No use case IDs. No widgets. The point is to
capture the user's goal and journey, not the implementation contract.

### Template

```markdown
# [Actor] Flows

> [1-line elevator pitch of the actor and what they're trying to do.]

**BCs touched**: [comma-separated bounded context names]

---

## 1. [Scenario Name]

[1–2 paragraphs of plain narrative. What the user wants, what they do, what
they see. Use present tense, third person ("the user uploads…").]

---

## 2. [Next Scenario]

[...]
```

### Conventions

- **Scenarios are numbered, not tagged with IDs.** They're a narrative, not
  a contract.
- **Use plain language.** No widget names, no error code names, no
  references to use case IDs. If a behavior matters, describe it in
  outcome terms ("video starts playing right away") not mechanism terms
  ("HLS player initializes and the manifest loads").
- **One file per actor type.** If the same person can do two distinct things
  (e.g. upload and watch), both flows go in the same file.
- **`**BCs touched**`** is required — it tells reviewers which bounded
  contexts to consult when validating the story.
- **Open questions** can be flagged inline using a parenthetical and a link
  (e.g. `(See clarifications #8 — pending confirmation.)`). Don't hide
  unknowns; surface them where the unclear behavior is described.

---

## File Type 4: Task Catalog (special file)

`task-system/task-catalog.md` is the **single authoritative list** of every
background task type in the system. It is unique among business specs:

- It is per-task, not per-entity.
- The implementation must match the catalog exactly — metadata struct name,
  ordering key template, retries, delays, intervals, timeouts, system-task
  flag.
- It does not have use cases of its own. Each task entry links to the use
  case that the handler invokes.

For runtime architecture (how tasks flow through the system, what the consumer
loop does, etc.) see `architecture/backend/task-system.md`. The catalog is
strictly the per-type configuration.

### Per-task entry template

```markdown
## [Task Name]

[1–2 sentences: what the task does and why it exists. Link to the relevant
use case.]

| | |
|---|---|
| **Metadata type name** | `XxxTaskMetadata` |
| **Fields** | `field_name: Type, ...` (or `None (unit struct)`) |
| **Use case** | [UC-<BC>-NNN Name](../<bc>/<entity>.md#uc-<bc>-nnn) |
| **Ordering key** | `template:{field}` or `constant` (or `null` for none) |
| **Max retries** | `N` or `null` (no retries) |
| **Retry base delay** | `N seconds` / `N minutes` |
| **Execution interval** | `N hours` for recurring, or `N/A (one-shot)` |
| **Processing timeout** | `N minutes` |
| **System task** | `true` / `false` |

**Failure model**: [How handler maps use case errors to TaskResult variants —
which produce `Skip`, which produce `RetryableFailure`, which produce
`PermanentFailure`. One short paragraph.]
```

### Conventions

- **No "Triggered by" field.** A task can be scheduled from many places
  (rejection paths, success paths, sweeps, other handlers). Tracking every
  call site in the catalog is a maintenance burden and goes stale fast.
  Find call sites with grep, not by reading the catalog.
- **Ordering key templates use `{field}` placeholders** that match the
  metadata struct's field names. Constant ordering keys (like
  `cleanup_stale_videos`) use the literal string and indicate
  "single-instance system-wide" in the description.
- **Recurring tasks always have `Execution interval` set** and `System task`
  is normally `true`. The system task checker recreates them if no active
  instance exists.
- **One-shot tasks always have `Execution interval` = `N/A (one-shot)`** and
  `System task` = `false`.
- **Failure model is mandatory** — it's the contract between the handler
  and the task system's retry/dead-letter machinery.
- **The catalog has its own changelog** at the top of the file, same format
  as entity specs.

### Workflow: adding a new task type

1. **Catalog first.** Add a section in `task-system/task-catalog.md` using
   the per-task template above. Pick all values deliberately — the
   implementation will read them.
2. **Define the metadata struct** in `crates/domain/src/task/metadata/`.
   Include `pub const METADATA_TYPE: &'static str = "<StructName>";`.
   Implement `TaskMetadata`, returning `Self::METADATA_TYPE` from
   `metadata_type_name()`. The trait method values must match the catalog
   exactly.
3. **Define the use case** in `crates/application/src/usecases/`. Add it to
   the relevant entity spec under `## Use Cases` if it has user-facing
   semantics, or as a `System — not user-facing` use case otherwise.
4. **Write the handler** in `crates/worker/src/handlers/`. Implement
   `TypedTaskHandler` with `type Metadata = <YourMetadata>`. Map use case
   errors to `TaskResult` variants per the failure model in the catalog.
5. **Register the handler** in `crates/worker/src/main.rs` using
   `HandlerAdapter::wrap` and the metadata struct's `METADATA_TYPE` const
   as the dispatch key.
6. **If the task is `is_system_task = true`**, register the metadata in
   the system task checker's list (`crates/worker/src/system_checker.rs`).
7. **If the task is scheduled by a use case**, the use case calls
   `TaskScheduler::schedule(tx.tasks(), &metadata, trace_id, run_at)`
   inside its `uow.begin` transaction (project rule #8).
8. **Update the catalog's changelog** with the new entry.

### Workflow: changing a task's scheduling config

If you change a value in the catalog (max retries, retry base delay, etc.),
update the corresponding `TaskMetadata` trait method value in the same commit
and add a changelog entry. Existing scheduled tasks in the database keep
their previous values (the config is denormalized into the row at schedule
time) — the change only affects newly-scheduled tasks. Note this in the
changelog.

---

## Updating an Existing Spec

1. **Read the changelog first.** Understand the history before adding to it.
2. **Make the smallest change that captures the new behavior.** Don't
   refactor structure while adding content.
3. **Update the changelog table** with today's date, a one-line summary of
   the change, and the author.
4. **Cross-file consistency**: if the change affects another file (e.g. a
   new use case that has UI implications, a task config change that affects
   handler behavior), update both files in the same commit. Reviewers should
   never see a half-applied change.
5. **Don't delete published IDs.** If a use case or interaction is removed,
   leave a one-line "Removed YYYY-MM-DD: [reason]" stub at its anchor so
   inbound links don't break silently. The next addition still uses the
   next free number, not the removed one.

---

## Open Questions / Clarifications

When a spec captures a behavior that hasn't been fully decided yet, mark it
inline with a parenthetical:

```markdown
(See clarifications #8 — I'm assuming keep-what-works, pending confirmation.)
```

Don't hide assumptions in narrative prose. The `clarifications` reference is
informal — currently there is no central clarifications file, so reviewers
should treat any such marker as a flag to discuss before implementing.
