mod runner;
mod sanitize;
mod store;
mod tui;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use std::io::Read;
use std::path::PathBuf;
use store::{ArgSpec, NewCommand, Store};

#[derive(Parser)]
#[command(name = "cr", version, about = "Command registry: register, describe, run and replay shell commands")]
struct Cli {
    /// Path to the registry database [default: ~/.local/share/commands-registry/registry.db]
    #[arg(long, global = true, env = "CR_DB")]
    db: Option<PathBuf>,
    /// The session `add` registers into and `list` shows
    #[arg(long, global = true, env = "CR_SESSION")]
    session: Option<String>,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Open the two-pane TUI (default when no subcommand is given)
    Ui,
    /// Register a command, or update it if the name already exists
    Add {
        /// Unique, short, kebab-case name (e.g. `build-release`)
        name: String,
        /// The shell command line to run
        #[arg(short = 'c', long = "cmd")]
        command: String,
        /// One-line summary of what it does
        #[arg(short, long, default_value = "")]
        description: String,
        /// Detailed instructions (markdown). Pass `-` to read from stdin.
        #[arg(short, long)]
        instructions: Option<String>,
        /// Working directory to run in [default: current directory]
        #[arg(long, conflicts_with = "no_cwd")]
        cwd: Option<String>,
        /// Run wherever `cr run` is invoked instead of a fixed directory
        #[arg(long)]
        no_cwd: bool,
        /// Document an argument/flag: NAME=what it means (repeatable)
        #[arg(short = 'a', long = "arg", value_name = "NAME=DESC")]
        args: Vec<String>,
        /// Tag (repeatable)
        #[arg(short = 't', long = "tag")]
        tags: Vec<String>,
        /// Who registered it (e.g. `claude`, `user`)
        #[arg(long, default_value = "user")]
        by: String,
        /// Run it right after registering
        #[arg(long)]
        run: bool,
    },
    /// Run a registered command, streaming output and logging the run
    Run {
        name: String,
        /// Who is running it (recorded in the run history)
        #[arg(long, default_value = "user")]
        by: String,
        /// Extra arguments appended (shell-quoted) to the command
        #[arg(last = true)]
        extra: Vec<String>,
    },
    /// List registered commands, most recently used first
    List {
        #[arg(long)]
        json: bool,
    },
    /// Show a command's details and recent runs
    Show {
        name: String,
        #[arg(long)]
        json: bool,
    },
    /// Print the captured output of a run
    Log {
        name: String,
        /// Which run: 1 = latest, 2 = the one before, ...
        #[arg(short = 'n', long, default_value_t = 1)]
        nth: usize,
    },
    /// Remove a command and its run history
    Rm { name: String },
    /// Sessions: an ordered set of commands sharing environment variables, with one run history
    Session {
        #[command(subcommand)]
        cmd: SessionCmd,
    },
    /// Print the database path
    Path,
    /// Sanitize stdin and print it (preview what would be stored)
    Sanitize,
}

#[derive(Subcommand)]
enum SessionCmd {
    /// Create a session (or update its description)
    New {
        name: String,
        #[arg(short, long, default_value = "")]
        description: String,
    },
    /// List sessions, newest first
    List,
    /// Set the session's variables (KEY=VALUE ...), or print them with none given
    Env {
        name: String,
        #[arg(value_name = "KEY=VALUE")]
        pairs: Vec<String>,
    },
    /// Remove variables from the session
    Unset { name: String, keys: Vec<String> },
    /// The session's variables, its commands in order, and every run made in it
    Show { name: String },
    /// Every run in the session, oldest first: the command line, env, exit and output
    Log {
        name: String,
        /// Leave the outputs out
        #[arg(long)]
        brief: bool,
    },
}

fn main() {
    if let Err(e) = real_main() {
        eprintln!("cr: {e:#}");
        std::process::exit(1);
    }
}

fn real_main() -> Result<()> {
    let cli = Cli::parse();
    let db = cli.db.unwrap_or_else(store::default_db_path);
    let session = cli.session.filter(|s| !s.is_empty());

    match cli.cmd.unwrap_or(Cmd::Ui) {
        Cmd::Ui => tui::run(Store::open(&db)?, session),
        Cmd::Path => {
            println!("{}", db.display());
            Ok(())
        }
        Cmd::Sanitize => {
            let mut s = String::new();
            std::io::stdin().read_to_string(&mut s)?;
            let (out, n) = sanitize::sanitize(&s);
            print!("{out}");
            eprintln!("cr: {n} redaction(s)");
            Ok(())
        }
        Cmd::Add { name, command, description, instructions, cwd, no_cwd, args, tags, by, run } => {
            let store = Store::open(&db)?;
            validate_name(&name)?;
            let instructions = match instructions.as_deref() {
                Some("-") => {
                    let mut s = String::new();
                    std::io::stdin().read_to_string(&mut s)?;
                    s
                }
                Some(s) => s.to_string(),
                None => String::new(),
            };
            let cwd = if no_cwd {
                None
            } else {
                Some(match cwd {
                    Some(d) => std::fs::canonicalize(&d)
                        .with_context(|| format!("--cwd {d}"))?
                        .to_string_lossy()
                        .into_owned(),
                    None => std::env::current_dir()?.to_string_lossy().into_owned(),
                })
            };
            let args = args
                .iter()
                .map(|a| {
                    let (n, d) = a.split_once('=').unwrap_or((a, ""));
                    ArgSpec { name: n.trim().to_string(), description: sanitize::sanitize(d.trim()).0 }
                })
                .collect();

            let (command, n_cmd) = sanitize::sanitize(&command);
            let (description, n_desc) = sanitize::sanitize(&description);
            let (instructions, n_inst) = sanitize::sanitize(instructions.trim());
            if n_cmd + n_desc + n_inst > 0 {
                eprintln!(
                    "cr: redacted {} secret(s) before storing{}",
                    n_cmd + n_desc + n_inst,
                    if n_cmd > 0 { " — the command can't be replayed until it uses $ENV references" } else { "" }
                );
            }
            let cmd = store.upsert(&NewCommand {
                name,
                command,
                description,
                instructions,
                cwd,
                args,
                tags,
                source: by.clone(),
                redacted: n_cmd > 0,
                session: session.clone(),
            })?;
            match &cmd.session {
                Some(s) => eprintln!("cr: registered {:?} in session {s:?}", cmd.name),
                None => eprintln!("cr: registered {:?}", cmd.name),
            }
            if run {
                let out = runner::run(&store, &cmd, &[], &by)?;
                std::process::exit(out.exit_code.unwrap_or(1));
            }
            Ok(())
        }
        Cmd::Run { name, by, extra } => {
            let store = Store::open(&db)?;
            let cmd = store.require(&name)?;
            let out = runner::run(&store, &cmd, &extra, &by)?;
            if out.redactions > 0 {
                eprintln!("cr: {} secret(s) redacted from the stored log", out.redactions);
            }
            std::process::exit(out.exit_code.unwrap_or(1));
        }
        Cmd::List { json } => {
            let store = Store::open(&db)?;
            let cmds = match &session {
                Some(s) => store.session_commands(store.session_require(s)?.id)?,
                None => store.list()?,
            };
            if json {
                println!("{}", serde_json::to_string_pretty(&cmds)?);
                return Ok(());
            }
            let width = cmds.iter().map(|c| c.name.len()).max().unwrap_or(4).max(4);
            for c in &cmds {
                let status = match &c.last_run {
                    Some(r) if r.exit_code == Some(0) => "✓",
                    Some(_) => "✗",
                    None => "·",
                };
                println!("{status} {:width$}  {}", c.name, c.description);
            }
            Ok(())
        }
        Cmd::Show { name, json } => {
            let store = Store::open(&db)?;
            let cmd = store.require(&name)?;
            let runs = store.runs(cmd.id, 10, false)?;
            if json {
                let v = serde_json::json!({ "command": cmd, "runs": runs });
                println!("{}", serde_json::to_string_pretty(&v)?);
                return Ok(());
            }
            println!("{}\n  {}\n", cmd.name, cmd.description);
            if let Some(s) = &cmd.session {
                println!("  session {s}");
            }
            println!("  $ {}", cmd.command);
            if let Some(d) = &cmd.cwd {
                println!("  in {d}");
            }
            if !cmd.args.is_empty() {
                println!("\nArguments:");
                for a in &cmd.args {
                    println!("  {:<16} {}", a.name, a.description);
                }
            }
            if !cmd.instructions.is_empty() {
                println!("\nInstructions:\n{}", cmd.instructions);
            }
            if !runs.is_empty() {
                println!("\nRecent runs:");
                for r in &runs {
                    println!(
                        "  {}  exit {:<4} {:>8}  by {}",
                        r.started_at.format("%Y-%m-%d %H:%M:%S"),
                        r.exit_code.map(|c| c.to_string()).unwrap_or_else(|| "sig".into()),
                        runner::fmt_duration(r.duration_ms),
                        r.by
                    );
                }
            }
            Ok(())
        }
        Cmd::Log { name, nth } => {
            let store = Store::open(&db)?;
            let cmd = store.require(&name)?;
            let runs = store.runs(cmd.id, nth.max(1), false)?;
            let Some(r) = runs.get(nth.max(1) - 1) else { bail!("{name:?} has fewer than {nth} run(s)") };
            eprintln!(
                "# {} · exit {} · $ {}",
                r.started_at.format("%Y-%m-%d %H:%M:%S"),
                r.exit_code.map(|c| c.to_string()).unwrap_or_else(|| "signal".into()),
                r.command_text
            );
            print!("{}", store.run_output(r.id)?);
            Ok(())
        }
        Cmd::Session { cmd } => session_cmd(&Store::open(&db)?, cmd),
        Cmd::Rm { name } => {
            let store = Store::open(&db)?;
            if !store.remove(&name)? {
                bail!("no command named {name:?}");
            }
            eprintln!("cr: removed {name:?}");
            Ok(())
        }
    }
}

fn session_cmd(store: &Store, cmd: SessionCmd) -> Result<()> {
    match cmd {
        SessionCmd::New { name, description } => {
            validate_name(&name)?;
            let s = store.session_upsert(&name, &description)?;
            eprintln!("cr: session {:?} ready; `export CR_SESSION={}` to register into it", s.name, s.name);
        }
        SessionCmd::List => {
            for s in store.sessions()? {
                let steps = store.session_commands(s.id)?.len();
                let runs = store.session_runs(s.id, false)?.len();
                println!(
                    "{}  {:<28} {steps} command(s), {runs} run(s), {} var(s)  {}",
                    s.created_at.format("%Y-%m-%d %H:%M"),
                    s.name,
                    s.env.len(),
                    s.description
                );
            }
        }
        SessionCmd::Env { name, pairs } => {
            for p in &pairs {
                let Some((k, v)) = p.split_once('=') else { bail!("{p:?} is not KEY=VALUE") };
                if k.is_empty() || !k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                    bail!("{k:?} is not a variable name");
                }
                if sanitize::sanitize(v).1 > 0 {
                    bail!("{k}'s value looks like a secret; keep secrets out of the registry");
                }
                store.session_set_env(&name, k, v)?;
            }
            for (k, v) in store.session_require(&name)?.env {
                println!("{k}={v}");
            }
        }
        SessionCmd::Unset { name, keys } => {
            for k in &keys {
                if !store.session_unset_env(&name, k)? {
                    eprintln!("cr: {name} has no {k}");
                }
            }
        }
        SessionCmd::Show { name } => {
            let s = store.session_require(&name)?;
            println!("session {}  ({})\n  {}", s.name, s.created_at.format("%Y-%m-%d %H:%M"), s.description);
            if !s.env.is_empty() {
                println!("\nVariables:");
                for (k, v) in &s.env {
                    println!("  {k}={v}");
                }
            }
            println!("\nCommands:");
            for (i, c) in store.session_commands(s.id)?.iter().enumerate() {
                println!("  {:>2}. {:<28} $ {}", i + 1, c.name, c.command.lines().next().unwrap_or_default());
            }
            let runs = store.session_runs(s.id, false)?;
            if !runs.is_empty() {
                println!("\nRuns:");
                for (cmd, r) in &runs {
                    println!(
                        "  {}  {:<28} exit {:<4} {:>8}  by {}",
                        r.started_at.format("%Y-%m-%d %H:%M:%S"),
                        cmd,
                        r.exit_code.map(|c| c.to_string()).unwrap_or_else(|| "sig".into()),
                        runner::fmt_duration(r.duration_ms),
                        r.by
                    );
                }
            }
        }
        SessionCmd::Log { name, brief } => {
            let s = store.session_require(&name)?;
            print!("{}", session_transcript(store, &s, !brief)?);
        }
    }
    Ok(())
}

/// The session's runs, oldest first, as one readable transcript.
pub fn session_transcript(store: &Store, s: &store::Session, with_output: bool) -> Result<String> {
    use std::fmt::Write;
    let mut t = String::new();
    writeln!(t, "# session {} · {}", s.name, s.description)?;
    for (i, (cmd, r)) in store.session_runs(s.id, with_output)?.iter().enumerate() {
        writeln!(
            t,
            "\n## {}. {} · {} · exit {} · {} · by {}",
            i + 1,
            cmd,
            r.started_at.format("%Y-%m-%d %H:%M:%S"),
            r.exit_code.map(|c| c.to_string()).unwrap_or_else(|| "signal".into()),
            runner::fmt_duration(r.duration_ms),
            r.by
        )?;
        for (k, v) in &r.env {
            writeln!(t, "env {k}={v}")?;
        }
        if let Some(d) = &r.cwd {
            writeln!(t, "cd {d}")?;
        }
        writeln!(t, "$ {}", r.command_text)?;
        if with_output {
            writeln!(t, "{}{}", if r.truncated { "[... head truncated ...]\n" } else { "" }, r.output.trim_end())?;
        }
    }
    Ok(t)
}

fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || "-_.:/".contains(c)) {
        bail!("name {name:?} must be non-empty and use only letters, digits, - _ . : /");
    }
    Ok(())
}
