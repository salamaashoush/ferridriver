---
name: Plan done marking convention
description: When marking plans as done, use [DONE] prefix before Feature and include plan file in the same commit as the implementation
type: feedback
---

When marking a plan as done: use `# [DONE] Feature: ...` format (prefix, not suffix). Always include the plan file change in the same commit as the implementation — never commit it separately.

**Why:** User corrected after making separate commit and using wrong [DONE] position. Separate commits for plan marks are unnecessary noise.

**How to apply:** Before committing implementation, also stage the plan file with `[DONE]` mark so it's all one commit.
