# Spec Guide

Quick reference for spec structure and conventions. Business specs (under `business-spec/`) describe **what** the system does. Architecture specs (under `architecture/`) describe **how**.

---

## File Organisation

```
.spec/
├── SPEC_GUIDE.md                ← this file
├── business-spec/
│   ├── user-stories/<actor>.md  ← end-to-end narratives by actor
│   ├── <context>/<entity>.md    ← entity spec (backend behavior)
│   ├── <context>/<entity>.ui.md ← UI spec (screens, interactions)
│   └── task-system/
│       └── task-catalog.md      ← all background task types
└── architecture/
    ├── system-architecture.md
    └── backend/                 ← per-concern design docs
```

Bounded contexts: **video** (uploads, transcoding, playback), **task-system** (background jobs).

---

## ID System

Every use case, screen, and interaction has a stable ID. Never reuse or renumber a retired ID.

| Kind | Pattern | Example |
|---|---|---|
| Use case | `UC-<BC>-NNN` | `UC-VID-001` |
| Screen | `SCR-<BC>-NNN` | `SCR-VID-001` |
| UI interaction | `UI-<BC>-NNN` | `UI-VID-001` |

| Context | Code |
|---|---|
| Video | `VID` |
| Task System | `TSK` |

IDs become anchors: `### Initiate Upload {#UC-VID-001}`. Cross-reference with `[UC-VID-001](video.md#uc-vid-001)`.

---

## Entity Spec Template (`<entity>.md`)

```markdown
# [Entity Name]

## Overview
[1-3 sentences. What the entity is and why it exists.]

---

## Definitions

### Attributes
| Attribute | Type | Nullable | Description |

### Enums
#### [EnumName]
| Value | Description |

---

## Use Cases

### [Use Case Name] {#UC-<BC>-NNN}

**Actor**: [Who]
**Triggered by**: REST: `METHOD /path`

**Input**
| Field | Required | Description and validation |

**Guards**
1. [Pre-condition]

**Mutations**
- [What changes]

**Output**
| Field | Description |

**Error Codes**
| Code | When it occurs |

**Side Effects**: [N/A, or tasks scheduled]
**Idempotency**: [Idempotent / Not idempotent — why]

---

## Limits and Quotas
| Limit | Value | Enforcement |
```

### Conventions

- Section order is fixed. Omit inapplicable sections, never reorder.
- Use cases ordered by lifecycle: create, read, update, delete, system-only last.
- Guards are numbered (ordered preconditions). Mutations are bullets (unordered set).
- Error codes are `SCREAMING_SNAKE_CASE`.
- System use cases: **Actor**: `System — not user-facing`.

### Attribute type vocabulary

| Type | Meaning |
|---|---|
| `Unique identifier` | System-assigned opaque ID |
| `Text` / `Text (N-M chars)` | String, optionally length-constrained |
| `Number` / `Decimal` | Integer / decimal |
| `Date/time` | Timestamp with timezone |
| `Boolean` | True / false |
| `Reference to [Entity]` | Foreign key |
| `[EnumName] (see Enums)` | Enum value |
| `List of [type]` | Ordered collection |

---

## UI Spec Template (`<entity>.ui.md`)

```markdown
# [Entity] — Frontend Spec

> **Backend spec**: [<entity>.md](<entity>.md)

## Screens

### [Screen Name] {#SCR-<BC>-NNN}
**Route**: `/path`
**Entry points**: [How users arrive]
**Layout**: [Visual structure]
**Interactions**: UI-XXX-NNN, ...

## Interactions

### [Interaction Name] {#UI-<BC>-NNN}
**Type**: Local | Connected
**Use Case**: [UC-<BC>-NNN]   ← Connected only
**Triggered by**: [User action]
**Screen**: [SCR-<BC>-NNN]

**Behavior**
1. [Step]

**States**
| State | Visual Behavior |

**Error Display**
| Error Code | Display |
```

- **Local**: no backend call. Omit `Use Case`.
- **Connected**: invokes backend use case(s). Chain multiples with `→`.

---

## User Stories Template (`user-stories/<actor>.md`)

```markdown
# [Actor] Flows

> [One-line description of what this actor does.]

**BCs touched**: [contexts]

---

## 1. [Scenario Name]
[3-5 sentence narrative. Present tense, third person. Plain language — no widget names, error codes, or UC IDs.]
```

---

## Task Catalog Template (`task-system/task-catalog.md`)

```markdown
## [Task Name]

[1-2 sentences: what and why. Link to use case.]

| | |
|---|---|
| **Metadata type name** | `XxxTaskMetadata` |
| **Fields** | `field: Type` or `None (unit struct)` |
| **Use case** | [UC link] |
| **Ordering key** | `template:{field}` or `constant` or `null` |
| **Max retries** | `N` or `null` |
| **Retry base delay** | `N seconds` / `N minutes` |
| **Execution interval** | `N hours` or `N/A (one-shot)` |
| **Processing timeout** | `N minutes` |
| **System task** | `true` / `false` |

**Failure model**: [How errors map to TaskResult variants.]
```

Implementation must match the catalog exactly.
