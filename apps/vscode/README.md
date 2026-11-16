# ognivo

A coding agent for your own models, on llama.cpp's `llama-server`: gpt-oss, Qwen, Gemma
and the others, with tool calling done right for each model. ognivo never starts a
server, and has no telemetry.

- **The agent** in the secondary side bar, on the right, beside your Explorer (or in an
  editor tab): write to it, and it reads, edits and runs commands in your workspace.
  `Ctrl+Alt+O` focuses it (with a selection, it adds the selection to the message), and so
  does the `ognivo` item in the status bar. The file you work in and your selection go
  with the message as chips you can remove; `@` finds any file of the workspace.
- **Edits in the editor**: the lines the agent changed are tinted where you work, with
  `Keep · Undo · Diff` above each change (`Ctrl+Alt+Enter` keeps the change at the cursor,
  `Ctrl+Alt+Backspace` undoes it; `Alt+F5` goes to the next one). Changed files carry a ✦
  in the Explorer and on their tabs, with Keep, Undo and the next and previous change in
  the editor's title. `ognivo: Review the Changes` opens every changed file in one tab,
  against what it held before the session.
- **Ask from where you are**: `Ctrl+I` on a selection asks what to do with it, right in the
  editor ("Edit", or "Ask in the Panel"); "Fix with ognivo" is a Quick Fix on a problem;
  a command that failed in your terminal is offered as context (`ognivo.terminalSuggestions`).
- **While it works**: the status bar says what it does (`Writing · 34 tok/s`), and the
  agent's commands show in the "ognivo" terminal (`ognivo: Show the Agent's Commands`).
- **Approvals** in the panel, and a notice when the panel is out of sight or VS Code is not
  in front (`ognivo.notifications`); so is the end of a turn.
- **Modes**: Manual, Edit automatically, Plan, Auto (`Shift+Tab` in the message).
- **Sessions**: `ognivo: Open a Session…` goes on with any session of the workspace.
- **The server and the model**: `ognivo: Choose the Server and the Model…`, or the
  `ognivo.*` settings.

The agent runs in ognivo's core, which the extension starts when none runs: one per user,
shared by every VS Code window.
