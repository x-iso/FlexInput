---
name: gsd-spike-wrap-up
description: Package spike findings into a persistent project skill for future build conversations
allowed-tools: Read, Write, Edit, Bash, Grep, Glob, AskUserQuestion
---

<objective>
Curate spike experiment findings and package them into a persistent project skill that the agent
auto-loads in future build conversations. Also writes a summary to `.planning/spikes/` for
project history. Output skill goes to `./.github/skills/spike-findings-[project]/` (project-local).
</objective>

<execution_context>
@.github/get-shit-done/workflows/spike-wrap-up.md
@.github/get-shit-done/references/ui-brand.md
</execution_context>

<runtime_note>
**Copilot (VS Code):** Use `vscode_askquestions` wherever this workflow calls `AskUserQuestion`.
</runtime_note>

<process>
Execute the spike-wrap-up workflow from @.github/get-shit-done/workflows/spike-wrap-up.md end-to-end.
Preserve all curation gates (per-spike review, grouping approval, copilot-instructions.md routing line).
</process>
