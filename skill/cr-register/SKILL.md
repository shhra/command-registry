---
name: cr-register
description: Register a shell command in the user's command registry (`cr`) with a description, documented arguments and detailed instructions, then run it through `cr run` so the run and its output are logged for replay. Use when the user runs /cr-register, or asks to register/save/track a command so they can see and replay it later in the cr TUI.
---

# cr-register — save commands to the command registry

`cr` is a Rust TUI + CLI backed by SQLite (`cr path` prints the db). Every
registered command has a name, the command line, a one-line description,
documented arguments, detailed instructions, a working directory, and a run
history with captured (sanitized) output. The user browses it with `cr`
(two panes: commands | description + last output) and replays from there.

The registry is for the **main runs the user cares about**, not every shell
call. Don't register throwaway inspection commands (`ls`, `cat`, `git status`,
`grep`) unless the user explicitly asks.

## What to register

- `/cr-register <something>` — register what the argument describes: a
  literal command, or a description ("the release build we just did").
- `/cr-register` with no argument — register the main command(s) from this
  session that produced the result the user cares about (builds, deploys,
  migrations, benchmarks, data jobs, test suites, long one-off pipelines).
  If more than one is plausible, list the candidates and ask which ones.

## Sessions: one per task

A session is a task's commands, in order, with the environment they run
under and one run history. Work in a session whenever a task runs more than
one command, so the user can replay the task step by step and see exactly
what ran, with which variables, in which order (`cr session log`).

- **Start one** per task: `cr session new <task-name> -d '<what the task is>'`,
  then prefix every `cr add` with `CR_SESSION=<task-name>` (each Bash call
  is a fresh shell, so an `export` does not carry over).
- **Host variables live in the session, not in the commands or in shell
  rc files**: `cr session env <task> KEY=/abs/value ...`. `cr run` injects
  them into the command's shell, prints every `env KEY=VALUE` before the
  command line, and records them with the run. Commands reference them as
  `"$KEY"`, so the command line stays readable and nothing is hidden.
  Values are literal (no `$OTHER` expansion): store absolute paths.
- **One command per step**, registered in the order they run: setup
  (symlinks, folders), build, run, inspect. Each is its own `cr add`; no
  wrapper script that hides the steps.
- `cr session show <task>`: variables, steps, runs. `cr session log <task>`:
  the whole transcript (env, cwd, command line, exit, output) oldest first.
  In the TUI, `s` cycles the session scope and `H` opens its transcript.

## Steps

1. **Check for an existing entry** with `cr list` (`cr session list` for
   sessions). If the same command is
   already there, update it under the same name instead of adding a duplicate
   (`cr add` with an existing name upserts and keeps the run history).

2. **Pick a name**: short kebab-case, verb-first, scoped if useful,
   e.g. `build-release`, `narat:test-all`, `db-migrate-local`.

3. **Make it replayable and secret-free.** `cr` redacts anything that looks
   like a secret (tokens, passwords, `*_KEY=…`, auth headers, `user:pass@`
   URLs, private keys). A command with a redacted literal **can't be run**.
   So before registering, rewrite any literal secret as an env-var
   reference: `--token "$GH_TOKEN"`, `API_KEY="$API_KEY" make deploy`.
   Mention which env vars are needed in the instructions. Also replace
   one-off absolute paths or temp files with stable ones when you can.

4. **Register it.** Pass the instructions on stdin with a quoted heredoc so
   nothing gets expanded:

   ```bash
   cr add <name> --by claude \
     --cmd '<command line>' \
     -d '<one line: what it does and why you would run it>' \
     -a '<flag-or-arg>=<what it means / allowed values>' \
     -a '<another>=<…>' \
     -t <tag> -t <project-name> \
     -i - <<'EOF'
   ## What it does
   <2–4 sentences: what happens, in which order, what it touches/changes>

   ## Before running
   - <prerequisites: services up, env vars set, branch, credentials…>

   ## Expected result
   - <what success looks like: output lines, files created, exit code>
   - <rough duration if notable>

   ## Pitfalls
   - <known failure modes and what to do about them>
   - <side effects: anything destructive or slow>

   ## Context
   <why this was run in this session, related registered commands>
   EOF
   ```

   - `--cwd` defaults to the current directory. Pass `--cwd <dir>` to pin a
     different one, or `--no-cwd` for commands that should run wherever `cr`
     is launched.
   - Document **every** meaningful flag/arg with `-a`, so the user can tell
     what will happen before pressing Enter.
   - Multi-line commands are fine inside `--cmd '…'`. Use `'\''` for a single
     quote inside.

5. **Run it through cr** so the run gets logged:

   ```bash
   cr run <name> --by claude            # extra args: cr run <name> --by claude -- --flag value
   ```

   - If the command already ran in this session and re-running it would be
     slow, destructive, or have side effects (deploys, migrations, pushes,
     deletes), **don't** re-run it to log it. Register it and tell the user
     it's ready to replay from `cr`.
   - From now on in the session, when you run a registered command, run it
     as `cr run <name> --by claude` rather than by hand, so the history
     stays complete.
   - `cr run` exits with the command's exit code and streams its output
     normally.

6. **Report back** briefly: the name(s) registered, whether it ran (exit
   code), any env vars it needs, and that `cr` opens the TUI.

## Reference

```
cr session new NAME [-d DESC]        create a session (CR_SESSION=NAME cr add ... registers into it)
cr session env NAME [KEY=VALUE ...]  set its variables (none: print them)
cr session unset NAME KEY ...        remove variables
cr session show NAME                 variables, steps in order, runs
cr session log NAME [--brief]        every run oldest first: env, cwd, $ line, exit, output
cr session list                      sessions, newest first
CR_SESSION=NAME cr                   the TUI scoped to the session
```

```
cr                         open the TUI (Enter run · / filter · o full log · d delete · q quit)
cr add NAME --cmd '…' …    register or update
cr run NAME [--by claude] [-- extra args]
cr list [--json]           most recently used first
cr show NAME [--json]      details + recent runs
cr log NAME [-n N]         output of the Nth most recent run
cr rm NAME
cr sanitize < file         preview what redaction would do
```
