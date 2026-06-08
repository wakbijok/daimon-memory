---
title: Memory Save Discipline
namespace: agent/protocol
scope: What/which-kind/when to persist; agent-curated + typed; hooks enforce timing not content.
rules: trigger->kind via guided tools; recall-before-write dedup; control fixes append/update; new-writes curated; approval-gated; right bucket (user/resources/agent, user beats resources beats agent); hooks back-stop.
---
Scope: Governs WHAT the agent persists, WHICH kind/namespace, and WHEN. Every agent under this tenant. Capture is agent-curated and typed; no extraction model in the loop. The hooks enforce timing (cadence + session-end), never content.

Rules:
1. Trigger to kind, call the named guided tool: a non-obvious choice -> log_decision; something failed/broke/reverted -> log_incident; a reusable lesson or corrected mistake -> log_lesson; a dated follow-up -> add_reminder; a procedure worth repeating -> remember kind=runbook; a standing rule -> remember kind=project_convention; topology/failure-mode/fix-pattern/resource -> remember with that kind. Persona and protocol are system-layer (written by the daimon CLI).
2. Recall before write (dedup). Browse or recall the target namespace first; update or skip an existing record instead of making a near-duplicate.
3. Append vs Update is not your choice. The control layer fixes it per kind. To retract a wrong save, use forget.
4. New writes only, curated not raw. Persist a distilled, self-contained record, never a transcript or whole-session dump. One event, one record.
5. Approval-gated for sensitive saves. Do not persist credentials or secrets; ask before saving anything the user has not agreed to share.
6. Right bucket, chosen by SUBJECT in priority order user > resources > agent. Put it in user/<area> if it is a fact, preference, or boundary about the user (user/profile, user/preferences); else resources/<project>/<area> if it is about a named project, codebase, or host (resources/inpres, resources/daimon-memory, resources/homelab); else agent/<area> for the agent's own self and work (agent/persona, agent/protocol, agent/skills, agent/lessons, agent/decisions, agent/workstream); session/<run> for ephemeral only. daimon appends /<kind>/<id> itself; never put the kind in the path.
7. The hooks back-stop you, they do not replace you. A signal nudge per turn, a cadence nudge after N quiet turns (default 5), and a session-end pass will remind you and name the exact tool. Saving in the moment beats the back-stop.
