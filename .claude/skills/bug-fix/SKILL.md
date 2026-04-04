---
name: bug-fix
description: Classify and fix a bug in spec or code.
argument-hint: <bug description>
disable-model-invocation: true
---

Bug report: $ARGUMENTS

Before making any changes, enter plan mode. Locate the bug and read the relevant spec:

- **Use case bug** -> read `business-spec/video/video.md`
- **Infrastructure bug** -> read the relevant `architecture/backend/` doc
- **Task system bug** -> read `architecture/backend/task-system.md`
- **Unclear** -> read spec and architecture for all layers involved, or ask for clarification

Propose your plan. State:
- Where the bug is — which crate, which file(s)
- Classification: **code bug** (code doesn't match spec), **spec bug** (spec is wrong), or **infra bug** (config/infrastructure issue)
- What will change — which spec sections and/or code files
- Nothing beyond the reported bug

After approval:

**Code bug** -> fix code to match spec. No spec changes.

**Spec bug** -> update spec first (add Changelog entry), then fix code to match.

**Infra bug** -> fix infrastructure code. No spec changes.

Finally, confirm code and spec agree on guards, mutations, error codes, side effects.

Do not fix beyond the reported bug. Do not refactor surrounding code.
