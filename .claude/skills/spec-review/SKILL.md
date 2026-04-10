---
name: spec-review
description: Review a spec for completeness and consistency.
argument-hint: <spec file or context name>
allowed-tools: Read, Grep, Glob
---

Read `.spec/SPEC_GUIDE.md`, then the spec file(s) being reviewed.

Review target: $ARGUMENTS

Check:

**Completeness**
- Every use case has: Actor, Triggered by, Input, Guards, Mutations, Output, Error Codes, Side Effects, Idempotency
- Every input with a constraint has a corresponding error code
- Every guard has a corresponding error code
- Side effects reference task types in `task-system/task-catalog.md`

**Consistency**
- Attributes referenced in use cases exist in the Attributes table
- Enum values in guards/mutations match the Enums section
- Error codes not duplicated with different meanings across use cases

**Naming and format**
- Use case headings have UC ID anchors (`{#UC-XXX-NNN}`)
- IDs are sequential, no gaps or duplicates
- Attribute types follow SPEC_GUIDE vocabulary

For each issue: state the section, quote the problem, explain what's wrong.
Conclude with: PASS or FAIL (with issue list).
