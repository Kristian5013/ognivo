# ognivo

A coding agent for your own models, on llama.cpp's `llama-server`: gpt-oss, Qwen, Gemma
and the others, with tool calling done right for each model. ognivo never starts a
server, and has no telemetry.

- **The agent** in the side bar (or in an editor tab): write to it, and it reads, edits
  and runs commands in your workspace. `Ctrl+Alt+O` focuses it; with a selection, it adds
  the selection to the message.
- **Approvals** in the conversation, and a notice when the panel is out of sight.
- **Changes**: what the agent changed and you did not review yet, each file in the
  editor's own diff against what it held before the session; accept or revert one file
  or all.
- **Modes**: Manual, Edit automatically, Plan, Auto (`Shift+Tab` in the message).
- **Sessions**: `ognivo: Open a Session…` goes on with any session of the workspace.
- **The server and the model**: `ognivo: Choose the Server and the Model…`, or the
  `ognivo.*` settings.

The agent runs in ognivo's core, which the extension starts when none runs: one per user,
shared by every VS Code window.
