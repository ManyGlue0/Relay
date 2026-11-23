# Agent mode

Status: **design, not built.** Target: Relay 0.7.0. Written 2026-11-23 from a
question-and-answer design session with Andrea and two review rounds. Points
marked **Open** are not decided yet; everything else is the agreed default.

## What it is

Relay today talks: a model answers, runs MCP tools, skills and web search.
Agent mode lets a model **work in a folder**: read and write files, run
commands, and keep going on its own until the job is done. It's for coding,
writing documents and building small scripts and tools.

Goals:

- A model can do real work in a folder: files, commands, the web, MCP, skills.
- In **Auto** mode Andrea can walk away and come back to finished work and a
  list of what was skipped, and **nothing he cares about can be lost**.
- Everything the model changes in the folder can be undone, including changes
  made by scripts it ran.
- It feels like the rest of Relay: same composer, same cards, same calm.

Non-goals (for now):

- A sandbox. Auto mode guards against **accidents**, not against a model that
  tries to break out on purpose. Its limits are listed honestly below.
- Plan mode. Models plan well enough on their own; instead they get a to-do
  list and a way to ask questions.
- Other operating systems. Relay is Windows-only.

## Words used here

| Word | Meaning |
|---|---|
| Session | An agent conversation (an agent's chat). |
| Folder | The folder a session works in: picked by the user, or a workspace Relay made. |
| Step | One tool call that can change files: a write, an edit, a delete, any command. |
| Checkpoint | The state of the folder's protected files just before a step. |
| Scratch areas | Places tools write to on their own (caches, temp, the active Python environment). |
| Protected | Everything outside the folder that isn't a scratch area: Andrea's data. |

## The switch and the sidebar

- A **Chat / Agent** switch sits in the title bar next to the sidebar button,
  two small icons (a chat bubble, an agent icon), like Claude Desktop's.
- It **hides when the sidebar is collapsed**, so the title bar stays calm.
- **Separate lists:** in Chat the sidebar lists chats as today; in Agent it
  lists sessions **grouped by folder** (the folder's name as the group, newest
  session first). Search covers the list on screen.
- "New chat" becomes "New session" in Agent: it opens a session with the folder
  chip empty.

## Sessions and folders

- A session works in **one folder**. The composer shows a folder chip: click to
  pick a folder (the dialog plugin), or leave it empty.
- **No folder picked:** on the first send Relay creates a workspace for that
  session, `<data>\workspaces\<session id>\`, and shows it in the chip with
  "Open in Explorer". Deleting the session asks whether to delete its workspace
  too (a picked folder is never deleted).
- The folder is fixed once the session has started (changing it mid-session
  would make checkpoints and the conversation lie). **Open:** allow "continue
  in another folder" as a new session that carries a summary.
- The session header shows the folder and, in a git repository, the branch.

## What the model gets

### Instructions

- Relay's agent system prompt (how to work, the tools, the mode it's in), then
  the folder's **`AGENTS.md`**, else **`CLAUDE.md`**, then the preset and the
  session's own prompt.
- Read once when the session starts and kept fixed for the session, so the
  request prefix doesn't change and servers keep their prompt cache (the rule
  Relay already follows). A "Reload instructions" action re-reads them.
- Capped (32 KB) with a note when cut.

### Tools

Agent tools live on a new route server `relay.agent` / `Agent` (like
`relay.web` and `relay.skills`), listed in a fixed order for the prompt cache.

| Tool | Does |
|---|---|
| `agent__read_file` | A file as text with line numbers, in parts (`offset`, `limit`); images go to models that see. |
| `agent__write_file` | Creates or replaces a file. |
| `agent__edit_file` | Replaces an exact piece of text (must match once, or `all: true`). |
| `agent__delete_path` | Deletes a file or a folder. |
| `agent__list` | Files under a path, by glob, skipping the checkpoint skip list. |
| `agent__search` | Text search (regex) across files, with file:line results. |
| `agent__powershell` | Runs a command in PowerShell. Always offered. |
| `agent__bash` | Runs a command in Git Bash. Offered only when Git Bash is installed. |
| `agent__cmd` | Runs a command in cmd.exe. A setting, **off by default** (small models trip on cmd's escaping, and three shells confuse them). |
| `agent__todo` | The session's to-do list: the model writes and ticks steps; shown in the chat. |
| `agent__ask_user` | Questions with options (Relay's AskUserQuestion). Not offered in Auto or Bypass. |

- Each shell tool says which shell it is in its description; the model picks
  the tool, never a shell parameter.
- Commands run **in the folder**, without a window, inside Relay's
  kill-on-close job object (as MCP servers and skill scripts do, `mcp.rs`), with
  a time limit (default 2 minutes, `timeout` up to 10) and output capped through
  `limits.rs`. **Open:** long-running processes (a dev server): a
  `background: true` option with a process list and a stop button.
- Paths in file tools are relative to the folder; absolute paths go through the
  same rules as commands.
- Web search, MCP servers and skills are offered in sessions too, under their
  switches, exactly as in chats.

### AskUserQuestion (`agent__ask_user`)

- One to four questions, each with two to four options (label and a short
  description), single or multiple choice, and an "Other" field.
- Shows as a card in the chat and **pauses the reply** until answered (like a
  tool approval). The answers go back as the tool's result.
- Also offered in **Chat** mode when tools are on (it's useful everywhere).
- In Auto and Bypass it isn't offered: the model lists its open questions and
  the assumptions it made in its final message.

## Permission modes

A chip in the composer next to the model chip; **Shift+Tab** cycles. Saved per
session; a new session starts with the mode used last.

| | Ask every time | Accept edits | Auto | Bypass |
|---|---|---|---|---|
| Read files in the folder | runs | runs | runs | runs |
| Write / edit files in the folder | asks | runs | runs | runs |
| Delete in the folder | asks | asks | refused | runs |
| Commands | ask | ask | run if the rules allow, else refused | run |
| Anything outside the folder | asks | asks | rules (below) | runs |
| Web, MCP, skills | their own policies | their own policies | their own policies, "ask" counts as refused | run |
| Checkpoints | yes | yes | yes | yes |
| Secrets hidden from commands | no | no | yes | no |

- **Bypass** shows the chip in the warning color while on.
- In **Ask** modes, an approval card for a command that Auto would refuse says
  so ("Auto would refuse this: deletes files"), so the rules teach themselves.

## Auto mode

Auto is for working **unattended**. Three rules shape it:

1. **It never pauses.** Nothing in Auto waits for an answer. What isn't allowed
   is refused with a reason, and the model carries on with the rest.
2. **It's strict about losing things.** Deleting, overwriting outside the
   folder, destructive git, and reading credentials are refused.
3. **It doesn't get in the way of normal work.** Harmless commands and code run
   freely (`python -c "import torch; print(torch.__version__)"` is fine), and
   tools may write to their own scratch areas.

### The path map

| Zone | Read | Write | Delete |
|---|---|---|---|
| The folder | yes | yes | refused (checkpoints protect the rest) |
| Scratch areas | yes | yes | yes |
| Credential locations | **refused** | refused | refused |
| Everything else (protected) | yes | **refused** | **refused** |

**Scratch areas:** `%TEMP%`; `~/.cache` (Hugging Face included); the pip, npm,
pnpm, yarn, uv and cargo caches; the active Python environment's
`site-packages` (the workspace venv, or the environment picked for the folder);
the folder's `node_modules` (inside anyway).

**Credential locations (no reading in Auto):** `~/.ssh`, `~/.aws`, `~/.azure`,
`~/.kube`, `~/.docker/config.json`, `~/.gnupg`, `~/.netrc`, `~/.git-credentials`,
the GitHub CLI's config (`%APPDATA%\GitHub CLI`), browser profiles (Chrome,
Edge, Firefox, Brave), password managers' data folders, DPAPI's
`%APPDATA%\Microsoft\Protect`, Windows Credential Manager (`cmdkey`,
`vaultcmd`), and **Relay's own data folder and checkpoint store**.

Paths are compared after resolving `..`, `~`, environment variables
(`%USERPROFILE%`, `$env:...`, `$HOME`), short names (`PROGRA~1`), junctions and
symlinks, case-insensitively.

### Commands: parsed, not pattern-matched

- **PowerShell:** PowerShell's own parser
  (`[System.Management.Automation.Language.Parser]::ParseInput`) gives the
  command tree; aliases are resolved (`ri`, `rm`, `del`, `erase`, `rd`,
  `rmdir` → `Remove-Item`), pipelines and `;`, `&&`, `||` are each checked,
  redirections (`>`, `>>`, `Out-File`, `Set-Content`) count as writes.
- **Bash and cmd:** a careful tokenizer for each (quotes, escapes, `&&`, `||`,
  `;`, `|`, redirections, `$(...)` and backticks checked recursively; cmd's `^`
  and `%VAR%`).
- Every resulting command is checked against the rules. A command Relay can't
  parse is refused ("couldn't read this command safely").

Refused in Auto:

| Rule | Examples |
|---|---|
| Deleting | `Remove-Item`, `rm`, `del`, `rd /s`, `rmdir`, `unlink`, `shred`, `[IO.File]::Delete`, `cipher /w`, `robocopy /MIR` and `/PURGE`, `Clear-Content` on files |
| Destructive git | `git clean`, `reset --hard`, `checkout -- <path>` / `checkout .`, `restore` of files, `stash drop` / `clear`, `branch -D`, `push --force` (always), `filter-branch`, `gc --prune=now` |
| Writing outside | any write or redirection to a protected path |
| Reading credentials | any path in the credential list, `cmdkey /list`, `vaultcmd` |
| Uploading | `curl -T` / `-F` / `--data @file` / `--data-binary @file`, `Invoke-WebRequest` / `Invoke-RestMethod` with `-InFile` or a body read from a file, `scp`, `sftp`, `rsync` to a remote |
| Publishing | `git push`, `gh pr create`, `gh release`, `gh gist`, `npm publish`, unless the folder's **"Auto may push and publish"** setting is on (off by default; force push stays refused) |
| Global installs | `pip install` outside the workspace venv, `pip install --user`, `npm -g`, `cargo install`, `winget`, `choco`, `scoop` |
| System | registry writes (`reg add/delete`, `Set-ItemProperty HKLM:`), `format`, `diskpart`, `bcdedit`, `shutdown`, services, scheduled tasks, firewall, users, `Set-ExecutionPolicy` |
| Hidden code | `-EncodedCommand`, `Invoke-Expression` / `iex` of a built string, `powershell -Command` / `cmd /c` / `bash -c` nested (parsed and checked instead when possible) |

Allowed: everything else, including building, testing, package installs into
the workspace venv or local `node_modules`, downloads, `git` reads, commits,
branches, `git add`.

### Python: judged while it runs

Python is what models use most for one-off work, so it gets a runtime guard.

- **The hook:** Python's audit hooks (PEP 578) see every file open, delete,
  rename and new process. Relay's guard installs one that applies the path map
  as the code runs: no deleting outside scratch areas, no writing to protected
  paths, no reading credentials. How the code is written doesn't matter, and
  libraries it calls are covered too. A hook can't be removed once installed.
- **Direct calls:** when a parsed command runs `python` / `py` (with `-c`, `-m`
  or a script), Relay runs it through its own guard script: the guard installs
  the hook, then runs the original with `runpy` (keeping `sys.argv` and
  `__main__` as they'd be). This doesn't depend on `PYTHONPATH` or `site`, so
  `-I`, `-E` and `-S` don't skip it.
- **Indirect Pythons** (`pytest.exe`, `pip.exe`, a Python started by another
  tool): commands get `PYTHONPATH` pointing at Relay's `sitecustomize.py`,
  which installs the hook, then imports the `sitecustomize` it shadowed, if any.
- **Checked, not assumed:** the first time Relay sees an interpreter it runs a
  probe to confirm the guard loads (embeddable Pythons with a `._pth` file
  ignore `PYTHONPATH`). Where it doesn't, that interpreter's code is only read
  before it runs, and the session notes it.
- **Child processes:** the hook catches `subprocess`, `os.system`, `os.exec*`
  and `os.spawn*` and asks Relay whether the command may run, over a local pipe
  with a per-session token. The same rule engine judges it, so Python can't be
  a side door around the shell rules.

### Other code

- **Scripts the model runs** (`node x.js`, `.\build.ps1`, `bash x.sh`, `.bat`)
  are read before they run and checked with the same rules (PowerShell and
  shell scripts are parsed; Node and other languages are scanned for file
  deletion, writes outside the folder and process spawning).
- **`node -e`** code is scanned the same way.
- **Node's permission model** (`--permission`) is not used in v1: it blocks
  child processes and workers by default, so npm scripts and build tools would
  break. Revisit when it can allow them safely.

### Python packages

- Auto installs only into a **workspace venv** (`.venv` in the folder), created
  when needed with `--system-site-packages`, so big global installs (CUDA
  PyTorch) stay visible without downloading them again.
- An environment picked for the folder (an existing venv or conda env) counts
  as the project's: installing there is fine.

### Secrets in the environment

- In Auto, commands start with secret variables removed: names ending in
  `_TOKEN`, `_KEY`, `_SECRET`, `_PASSWORD`, `_PAT`, anything containing
  `API_KEY`, the AWS and Azure credential variables, `GH_TOKEN`,
  `GITHUB_TOKEN`, `HF_TOKEN`. (Relay's own provider keys never live in the
  environment; they're in Credential Manager.)
- The session header lists them ("Hidden in Auto: HF_TOKEN") with **"Allow for
  this folder"** next to each, so Andrea sees it before he walks away.
- An allowed secret's value is **masked** in any output sent to the model.

### Refusals

What the model gets back (the tool's result):

```
Blocked by Auto mode.
Rule: writing outside the folder
Why: C:\Users\andrea\Documents\notes.txt is outside E:\Projects\site.
Instead: write it inside the folder, for example .\out\notes.txt.
Don't try another way to do the same thing: carry on with the rest of the task.
```

- In the chat: an amber **Blocked** card (folded like a tool card).
- **The blocked list:** the session keeps every refusal (command, rule, reason
  and what the model was trying to do). It's in the session header as
  "Blocked: 3" and in the final summary. Each entry has **"Run it now"**, which
  runs that command once after Andrea's approval.
- **Repeats:** a second refusal under the same rule in the same task tells the
  model to **drop that sub-task**, note it, and carry on. The session ends with
  the work done and the list; it never stops on a question.

### Known limits (stated in the app's help too)

- A **compiled program** (an `.exe`) that deletes files or starts its own shell
  gets past every hook. Inside the folder, checkpoints undo it; outside, only
  the command rules stand in its way.
- **Uploads from code** (Python `requests.post`, a Node `fetch`) can't be told
  apart from downloads; only uploads visible in a command are refused.
- **Reads by compiled programs** of credential files aren't seen.
- The guards target accidents. A model writing deliberately disguised code can
  get around static checks (the Python hook still holds).

## Checkpoints

Every change the model makes in the folder can be undone, whoever made it: a
file tool, a command, a script.

- **When:** before every step (write, edit, delete, any command), Relay records
  the folder's protected files. Fast path: compare size and modified time with
  the last checkpoint; only changed files are read and stored.
- **What's protected:** every file in the folder **except** a fixed skip list
  of folders that can be rebuilt (`node_modules`, `target`, `.venv`, `venv`,
  `__pycache__`, `.next`, `.turbo`, `.gradle`, `.git`, which has its own
  history) and files over the size cap (default 50 MB). **Not `.gitignore`:**
  ignored files are often the precious ones (`.env`, local config).
  Unprotected files are listed in the files panel as "not protected".
- **Storage:** contents stored once each by hash (deduplicated), in
  `<data>\checkpoints\<folder id>\`, never inside the folder, and in the
  credential list so commands can't read or touch it.
- **Restore to before this:** on every step's card. It makes the protected
  files match that checkpoint exactly: changed files go back, deleted files
  return, **files created afterwards are removed**. The restore is itself
  checkpointed first, so it can be undone. Skipped folders are left alone.
- **Per-file history:** in the files panel, each file's versions (with
  "Restore this version" and a diff against now).
- **Pruning,** in this order:
  1. Never pruned: any file's latest version, and the checkpoint right before
     the current step.
  2. Soft budget per folder (default 2 GB, a setting): when over, older
     sessions' history goes first, down to each file's **last 5 versions**.
  3. The current session's steps stay restorable past the soft budget, up to a
     hard ceiling (twice the budget, or keeping at least 10 GB free on the
     disk).
  4. At the ceiling, the current session's oldest steps collapse to per-file
     history, and their cards say "no longer restorable" (also in the final
     summary). Never silent.
- Deleting a session asks whether to delete its checkpoints.

## In the chat

- **Diff cards** for file writes and edits: the changed lines (red and green),
  folded after a few lines, with "Restore to before this".
- **Command cards**, folded: the command and its status (running, exit code).
  Opening one shows the output in a terminal-style view, live while it runs.
- **Blocked cards** (Auto), amber.
- **To-do card:** the model's checklist, updated in place.
- **Files panel:** a side panel listing what the session created, changed or
  deleted, to open, preview (Relay's existing preview) or see the history of.
- **Final summary** (when the model ends its turn in Auto or Bypass): what was
  done, the blocked list, open questions and assumptions, steps that are no
  longer restorable.

## Long sessions

- When the conversation nears the model's context (85% of the detected
  window), the model writes a summary of the work so far (goals, decisions,
  files touched, what's left), and the session continues from the system
  prompt plus that summary plus the latest messages. The full history stays
  on screen, with a "Summarized here" divider.
- Old tool outputs are the first thing cut from the summary's input; file
  contents are never repeated in it (the model can read them again).

## Git

In a git repository:

- The session header shows the branch.
- **Checkpoint commit:** when a session starts on a repository with uncommitted
  changes, Ask modes offer "Commit first" (a WIP commit). Auto never commits on
  its own unless asked in the prompt.
- **See all changes:** a diff of the working tree against the session's start.
- Git's own history isn't checkpointed (`.git` is skipped); destructive git is
  refused in Auto instead.

## Background and notifications

- A session keeps working while Andrea uses another chat or session (replies
  already run in the backend; this extends to long tool loops).
- Holster notifications: when a session **finishes**, and in Ask modes when it
  **needs an approval**. The sidebar marks sessions that are working or waiting.

## Data

- Sessions are chats with `kind = 'agent'` (schema v7): `folder`,
  `folder_owned` (Relay's workspace), `mode`, `instructions` (the text read at
  start), `blocked` (JSON list).
- `checkpoints` (id, session, step message + call id, created_at),
  `checkpoint_files` (checkpoint, path, hash, size, mtime, deleted), blobs on
  disk by hash.
- Settings: the default mode, the soft budget, the file size cap, CMD on/off,
  per-folder settings (secrets allowed, "Auto may push and publish", the Python
  environment).

## Build order

Each step ends in something Andrea can test on its own.

1. **The switch and sessions:** title-bar switch, separate lists grouped by
   folder, folder chip, workspaces, session header. No new tools yet (a session
   is a chat with a folder).
2. **File tools + Ask every time:** read, write, edit, delete, list, search;
   approval cards with diffs; AGENTS.md / CLAUDE.md.
3. **Checkpoints:** taken before steps, "Restore to before this", files panel
   with history, pruning.
4. **Shell tools:** PowerShell (+ Bash when installed, CMD as a setting), job
   object, time limits, folded command cards; Accept edits and Bypass modes.
5. **AskUserQuestion and the to-do list** (also in Chat).
6. **Auto mode, rules:** the path map, the PowerShell parser and the Bash/cmd
   tokenizers, the rule table, refusals, blocked list, secrets, the workspace
   venv. A test suite of commands that must be refused and must be allowed.
7. **Auto mode, Python guard:** the guard script, `sitecustomize`, the probe,
   child-process routing.
8. **Long sessions:** summarizing near the context limit.
9. **Git and background:** branch, checkpoint commit, see all changes,
   notifications for approvals.

## Open questions

1. **Long-running processes** (dev servers, watchers): a `background` option
   with a process list and stop buttons, or out of scope for 0.7.0?
2. **Changing folder mid-session:** never, or "continue in another folder" as a
   new session with a summary?
3. **Accept edits and deletes in the folder:** ask (as above), or treat deletes
   inside the folder as edits since checkpoints can undo them?
4. **Models without tool calling:** hide them in Agent's model picker, or show
   them with a warning?
5. **Bypass and the credential list:** Bypass runs everything; should reading
   credentials still be refused there (it costs nothing in normal work)?
6. **The agent icon** for the switch (Lucide `bot`, `square-terminal`,
   `sparkles`?): to try on screen and center optically.
