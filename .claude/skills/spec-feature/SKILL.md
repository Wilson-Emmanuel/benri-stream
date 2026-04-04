---
name: spec-feature
description: Add a new feature to the spec — user stories, entity specs, or both.
argument-hint: <feature description>
disable-model-invocation: true
---

Read `.spec/business-spec/SPEC_GUIDE.md`.

New feature request: $ARGUMENTS

Before making any changes, enter plan mode. Explore the relevant spec files, then propose your plan.

Your plan must clarify what's needed:

**User stories** (skip if no user-facing flow):
- Belongs in existing story file or needs a new one?
- Which flows are added or changed?

**Entity specs** (skip if user only wants stories):
- New entity, new use case on existing entity, or both?
- Which spec files will be created or modified?
- Any new background tasks? (update `business-spec/task-system/task-catalog.md`)

**UI specs** (skip if no frontend change):
- New screens or interactions?
- Which `.ui.md` file to update?

After approval — spec changes only, no code:

1. Update or create `business-spec/[context]/[entity].md` using SPEC_GUIDE template
2. Update or create user stories in `business-spec/user-stories/`
3. Update `.ui.md` if frontend interactions change
4. Update `task-catalog.md` if new background tasks
5. Add Changelog entry to every spec file touched

Summarise what changed when done.
