---
name: new-entity
description: Add a new entity to the spec — attributes, enums, use cases, UI interactions.
argument-hint: <entity description>
disable-model-invocation: true
---

Read `.spec/business-spec/SPEC_GUIDE.md`.

New entity: $ARGUMENTS

Before making any changes, enter plan mode. Propose:
- What context does this entity belong to (existing or new folder under `business-spec/`)?
- What are the attributes, enums, and use cases?
- Any background tasks needed? (add to `task-catalog.md`)
- Any frontend screens/interactions? (create or update `.ui.md`)
- Any user stories affected?

After approval — spec changes only, no code:

1. Create `business-spec/[context]/[entity].md` using SPEC_GUIDE template
2. Create `business-spec/[context]/[entity].ui.md` if frontend interactions needed
3. Update `business-spec/task-system/task-catalog.md` if new background tasks
4. Update user stories if user flows change
5. Add Changelog entry to every file touched

Summarise what changed when done.
