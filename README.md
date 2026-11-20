# commands-registry (`cr`)

A registry of the shell commands that matter. Each command has a
description, documented arguments, detailed instructions and a run history
with captured output. You browse and replay it in a two-pane TUI. Claude adds
to it with the `/cr-register` skill.

```
┌ Commands (3) ─────┐┌ Description ────────────────────────────────────┐
│ ✗ db-migrate      ││build-release                                    │
│▌✓ build-release   ││Optimized build of the cr binary                 │
│ ✓ test-all        ││$ cargo build --release                          │
│                   ││cwd       ~/Projects/commands-registry           │
│                   ││last run  2026-11-20 05:11 · exit 0 · 12.3s      │
│                   ││Arguments / Instructions / History …             │
│                   │└─────────────────────────────────────────────────┘
│                   │┌ Last output · just now · exit 0 ────────────────┐
│                   ││   Finished `release` profile …                  │
└───────────────────┘└─────────────────────────────────────────────────┘
 j/k move  Enter run  / filter  J/K scroll  o full log  d delete  r reload  q quit
```

## Install

```bash
./install.sh     # cargo install of the cr binary
```

The Claude side is the `cr` plugin in `plugin/`: the `cr:cr-register` skill
plus a Bash guard that sends long runs through `cr run --detach` instead of
nohup/setsid/pkill/`&`. Install it once, at the prompt of a Claude Code session:

```
/plugin install cr --marketplace shhra/command-registry
```

Answer `y` to add the marketplace, then pick the user scope.

## Use

```bash
cr                                   # TUI
cr add build-release --cmd 'cargo build --release' \
  -d 'Optimized build' -a '--release=optimized profile' -t rust -i - <<'EOF'
Detailed instructions in markdown…
EOF
cr run build-release [-- extra args]  # streams output, logs run + output
cr list | show NAME | log NAME [-n 2] | rm NAME | path
```

In Claude Code: `/cr-register` (main commands from this session) or
`/cr-register <command or description>`.

## Storage

SQLite at `~/.local/share/commands-registry/registry.db` (override with
`--db` or `CR_DB`). It has two tables: `commands` and `runs`. Runs store the
exact command line, cwd, exit code, duration, who ran it and the output (the
last 512 KB, ANSI stripped).

**Sanitizing:** commands, descriptions, instructions and run output pass
through a redactor before they're written (`src/sanitize.rs`). It catches
token shapes (GitHub, Anthropic/OpenAI `sk-`, Slack, AWS, JWT),
`*_TOKEN/SECRET/PASSWORD/KEY=…`, `--token/--password …` flags, auth headers,
`curl -u user:pass`, `scheme://user:pass@` and private key blocks. Env-var
references like `$TOKEN` are kept, so write commands that way. If a command
had a literal secret redacted, it is flagged and refused at run time, because
replaying `«redacted»` would be wrong. Preview what gets redacted with
`cr sanitize < file`.

## Later: narat

The DB is plain SQLite with RFC 3339 timestamps, so narat can read the
`commands` and `runs` tables directly, or attach the database.
