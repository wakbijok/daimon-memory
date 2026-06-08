---
description: Recall from or store to shared memory (daimon-memory)
---
The user invoked /daimon. Handle their request using the daimon-memory tools (the `daimon`
MCP server exposes `recall`, `remember`, and `read`).

Request: $ARGUMENTS

Guidance:
- Question, or empty: use `recall` to search shared memory, then answer from the hits. Use
  `read` on a `daimon://` uri when you need the full record.
- Asking to save or remember something: use `remember` with a typed record (kind, namespace,
  title, body, and that kind's required fields). Use namespace `resources/<project>` for
  team-wide knowledge, or `agent/lessons` for personal notes.
- Relevant memory is also injected automatically each turn; use this command for explicit,
  targeted memory operations.
