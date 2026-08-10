---
name: clippy-companion
description: Interact with the user's local Clippy lists and todos through the optional Clippy MCP server. Use when the user asks to list, find, or read saved Clippy items; add, edit, or complete a todo in a Clippy list; create a Clippy list when policy permits; inspect Clippy agent permissions; or configure how agents may use Clippy.
---

# Clippy Companion

Use the local `clippy` MCP server. It reads the same on-device database as the
desktop app; cloud sync is not required.

## Workflow

1. Call `clippy_get_policy` before the first Clippy operation in a task.
2. For reads, query only the list or phrase the user requested. Start with a
   small limit and expand only when needed.
3. Before a write, require an explicit user request. Preserve the user's text
   and named destination; ask if either is ambiguous.
4. Use `clippy_list_lists` to resolve names instead of guessing list IDs.
5. Report the created or changed item and its destination after a write.

Never infer a todo from conversation, create speculative reminders, or broaden
a read to unrelated lists. Attachment contents and local file paths are not
available through this MCP server.

## Policy

The server reloads `mcp-policy.json` for every call and fails closed if it is
invalid. Modes are:

- `read_only`: list, read, and search only.
- `todos_only`: also add, edit, and complete todos in existing lists.
- `manage_lists`: also create lists. Deletion is never exposed to agents.

To change policy, first summarize the exact effect and obtain explicit user
approval. Then run the installed local CLI, for example:

```sh
clippy-mcp configure --write-mode todos-only --default-list Inbox
```

Useful options include `--read-enabled`, `--allow-inbox`,
`--allow-list-ids`, `--include-completed`, `--max-results`, and
`--include-attachment-metadata`. An empty allowed-list set means all named
lists; attachment metadata never includes contents or paths.

Do not use MCP tools to alter their own policy. If the user requests a change
that the current mode rejects, explain the minimum policy change and wait for
approval before running `clippy-mcp configure`.

## Setup recovery

If Clippy tools are unavailable, run `clippy-mcp doctor` when local shell access
is available. With user approval, run `clippy-mcp install-codex`; then explain
that the local Codex client must restart or begin a new task before newly added
MCP tools become callable. Do not claim the current task gained tools merely
because configuration was written.
