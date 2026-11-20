//! Execute a registered command, streaming output to the terminal while
//! capturing it (sanitized) into the registry.

use crate::sanitize::{REDACTED, clean_output, sanitize};
use crate::store::{Command, NewRun, Store};
use anyhow::{Context, Result, bail};
use chrono::Local;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Command as Proc, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Instant;

/// Keep at most this much output per run (the tail is kept).
const MAX_OUTPUT: usize = 512 * 1024;

pub struct Outcome {
    pub exit_code: Option<i32>,
    pub duration_ms: i64,
    pub redactions: usize,
}

/// Shell-quote extra arguments appended at run time.
pub fn shell_quote(arg: &str) -> String {
    if !arg.is_empty() && arg.chars().all(|c| c.is_ascii_alphanumeric() || "-_./=:,@%+".contains(c)) {
        arg.to_string()
    } else {
        format!("'{}'", arg.replace('\'', r"'\''"))
    }
}

pub fn build_command_line(cmd: &Command, extra: &[String]) -> String {
    let mut line = cmd.command.clone();
    for a in extra {
        line.push(' ');
        line.push_str(&shell_quote(a));
    }
    line
}

fn pump<R>(mut src: R, mut term: Option<Box<dyn Write + Send>>, log: Arc<Mutex<File>>, buf: Arc<Mutex<Vec<u8>>>) -> thread::JoinHandle<()>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut chunk = [0u8; 8192];
        loop {
            match src.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if let Some(t) = term.as_mut() {
                        let _ = t.write_all(&chunk[..n]);
                        let _ = t.flush();
                    }
                    if let Ok(mut f) = log.lock() {
                        let _ = f.write_all(&chunk[..n]);
                        let _ = f.flush();
                    }
                    buf.lock().unwrap().extend_from_slice(&chunk[..n]);
                }
            }
        }
    })
}

/// Where each run's output goes as it is produced: <logs>/<name>/<start time>-<cr pid>.log
pub fn log_dir(db: &Path) -> PathBuf {
    db.parent().unwrap_or(Path::new(".")).join("logs")
}

/// Run `cmd`, recording it in the registry from the moment it starts. `daemon`: started by `cr run --detach`, no
/// terminal: the command gets its own process group (what `cr stop` signals) and its output only goes to the log.
pub fn run(store: &Store, cmd: &Command, extra: &[String], by: &str, db: &Path, daemon: bool) -> Result<Outcome> {
    if cmd.redacted || cmd.command.contains(REDACTED) {
        bail!(
            "{:?} was stored with a secret redacted, so it can't be replayed as-is.\n\
             Re-register it using env-var references instead, e.g. `--token \"$MY_TOKEN\"`.",
            cmd.name
        );
    }
    let line = build_command_line(cmd, extra);
    let cwd = cmd.cwd.clone().filter(|d| !d.is_empty());
    if let Some(d) = &cwd {
        if !Path::new(d).is_dir() {
            bail!("working directory {d} does not exist");
        }
    }
    let session = match &cmd.session {
        Some(name) => Some(store.session_require(name)?),
        None => None,
    };
    let env = session.as_ref().map(|s| s.env.clone()).unwrap_or_default();

    let started_at = Local::now();
    let log_path = log_dir(db).join(&cmd.name).join(format!(
        "{}-{}.log",
        started_at.format("%Y%m%d-%H%M%S"),
        std::process::id()
    ));
    std::fs::create_dir_all(log_path.parent().unwrap())?;
    let log = Arc::new(Mutex::new(File::create(&log_path).with_context(|| format!("creating {}", log_path.display()))?));

    let mut head = match &session {
        Some(s) => format!("▶ cr run {} · session {}\n", cmd.name, s.name),
        None => format!("▶ cr run {}\n", cmd.name),
    };
    for (k, v) in &env {
        head.push_str(&format!("  env {k}={}\n", sanitize(v).0));
    }
    head.push_str(&format!("  $ {line}\n"));
    if let Some(d) = &cwd {
        head.push_str(&format!("  in {d}\n"));
    }
    log.lock().unwrap().write_all(head.as_bytes())?;
    if !daemon {
        eprint!("\x1b[2m{head}\x1b[0m");
    }

    let shell = if Path::new("/bin/bash").exists() { "/bin/bash" } else { "/bin/sh" };
    let mut proc = Proc::new(shell);
    proc.arg("-c").arg(&line).envs(&env).stdout(Stdio::piped()).stderr(Stdio::piped());
    if daemon {
        proc.stdin(Stdio::null()).process_group(0);
    } else {
        proc.stdin(Stdio::inherit());
    }
    if let Some(d) = &cwd {
        proc.current_dir(d);
    }

    let clock = Instant::now();
    let mut child = proc.spawn().with_context(|| format!("spawning {shell}"))?;
    let pid = child.id() as i64;
    let pgid = unsafe { libc::getpgid(pid as libc::pid_t) } as i64;
    // the command takes Ctrl-C and `cr stop`; cr stays to record how it ended
    unsafe {
        libc::signal(libc::SIGINT, libc::SIG_IGN);
        libc::signal(libc::SIGTERM, libc::SIG_IGN);
        libc::signal(libc::SIGHUP, libc::SIG_IGN);
    }
    let (command_text, cmd_redactions) = sanitize(&line);
    let run_id = store.start_run(
        cmd.id,
        &NewRun {
            started_at,
            by: by.to_string(),
            command_text,
            cwd: cwd.clone(),
            session_id: session.as_ref().map(|s| s.id),
            env: env.iter().map(|(k, v)| (k.clone(), sanitize(v).0)).collect(),
            pid,
            pgid,
            log_path: log_path.to_string_lossy().into_owned(),
        },
    )?;
    let note = format!("  run #{run_id} · pid {pid} · pgid {pgid} · log {}\n", log_path.display());
    log.lock().unwrap().write_all(note.as_bytes())?;
    if !daemon {
        eprint!("\x1b[2m{note}\x1b[0m");
    }

    let buf = Arc::new(Mutex::new(Vec::new()));
    let (out_term, err_term): (Option<Box<dyn Write + Send>>, Option<Box<dyn Write + Send>>) =
        if daemon { (None, None) } else { (Some(Box::new(io::stdout())), Some(Box::new(io::stderr()))) };
    let t_out = pump(child.stdout.take().unwrap(), out_term, log.clone(), buf.clone());
    let t_err = pump(child.stderr.take().unwrap(), err_term, log.clone(), buf.clone());
    let status = child.wait()?;
    unsafe {
        libc::signal(libc::SIGINT, libc::SIG_DFL);
        libc::signal(libc::SIGTERM, libc::SIG_DFL);
        libc::signal(libc::SIGHUP, libc::SIG_DFL);
    }
    let _ = t_out.join();
    let _ = t_err.join();
    let duration_ms = clock.elapsed().as_millis() as i64;

    let raw = std::mem::take(&mut *buf.lock().unwrap());
    let truncated = raw.len() > MAX_OUTPUT;
    let tail = if truncated { &raw[raw.len() - MAX_OUTPUT..] } else { &raw[..] };
    let (output, out_redactions) = sanitize(&clean_output(&String::from_utf8_lossy(tail)));
    let code = status.code();
    store.finish_run(run_id, duration_ms, code, &output, truncated)?;

    let end = format!(
        "{} exit {} · {} · run #{run_id} logged to registry\n",
        if code == Some(0) { "✓" } else { "✗" },
        code.map(|c| c.to_string()).unwrap_or_else(|| format!("signal {}", status.signal().unwrap_or(0))),
        fmt_duration(duration_ms)
    );
    let _ = log.lock().unwrap().write_all(end.as_bytes());
    if !daemon {
        let color = if code == Some(0) { "\x1b[32m" } else { "\x1b[31m" };
        eprint!("\x1b[2m{color}{end}\x1b[0m");
    }
    Ok(Outcome { exit_code: code, duration_ms, redactions: out_redactions + cmd_redactions })
}

pub fn fmt_duration(ms: i64) -> String {
    match ms {
        ms if ms < 1000 => format!("{ms}ms"),
        ms if ms < 60_000 => format!("{:.1}s", ms as f64 / 1000.0),
        ms => format!("{}m{:02}s", ms / 60_000, (ms % 60_000) / 1000),
    }
}
