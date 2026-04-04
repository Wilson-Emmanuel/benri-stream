# Spec Guide — How to Write and Update Business Specs

> Read this file before making ANY change to a business spec.

---

## What Goes Here (and What Doesn't)

**Business specs describe what the system does** — entities, their attributes, permitted
operations, who can do them, what changes, what errors can occur.

**Business specs do not contain**:
- Code or programming language syntax
- Framework-specific types
- Rust/JS-specific types
- Implementation details (how transcoding works internally, FFmpeg flags, etc.)
- Infrastructure concerns (CDN config, storage provider specifics)

Use plain business language. Write as you would for a product manager or a new team member
who understands the domain but not the implementation.

---

## File Organisation

```
business-spec/
├── SPEC_GUIDE.md                    ← this file
├── user-stories/
│   └── anonymous-user.md            ← upload and watch flows
├── video/
│   ├── video.md                     ← video entity, use cases
│   └── video.ui.md                  ← screens and interactions
└── task-system/
    └── task-catalog.md              ← all background task types (entity/lifecycle in architecture)
```

---

## Entity File Template

Every entity file follows this exact structure. Omit sections that don't apply, but keep
the order of what is included.

```markdown
# [Entity Name]

## Overview
[1–3 paragraphs. Business explanation: what this entity is, why it exists, how it is created
or managed. Written in plain language for non-engineers.]

## Changelog
| Date | Change | Author |
|------|--------|--------|

## Definitions

### Attributes
| Attribute | Type | Nullable | Description |
|-----------|------|----------|-------------|

### Enums
#### [EnumName]
| Value | Description |
|-------|-------------|

### Child Entities
#### [ChildEntityName]
[Brief description]

| Attribute | Type | Nullable | Description |
|-----------|------|----------|-------------|

## Use Cases

### [Use Case Name] {#UC-XXX-NNN}

**Actor**: [Who can trigger this]

**Triggered by**: REST: `METHOD /path`

**Input**
| Field | Required | Description and validation |
|-------|----------|---------------------------|

**Guards**
1. [Pre-condition]

**Mutations**
- [What changes]

**Output**
| Field | Description |
|-------|-------------|

**Error Codes**
| Code | When it occurs |
|------|---------------|

**Side Effects**
N/A or task description

**Idempotency**: [description]

## Limits and Quotas
| Limit | Value | Enforcement |
|-------|-------|-------------|
```

---

## Attribute Type Vocabulary

| Plain type | Meaning |
|------------|---------|
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

---

## Use Case ID Prefixes

| Context | Use Case Prefix |
|---------|-----------------|
| Video | UC-VID |
| Task System | UC-TSK |
