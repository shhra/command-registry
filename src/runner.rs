//! Execute a registered command, streaming output to the terminal while
//! capturing it (sanitized) into the registry.

use crate::sanitize::{REDACTED, clean_output, sanitize};
use crate::store::{Command, NewRun, Store};
use anyhow::{Context, Result, bail};
use chrono::Local;
use std::io::{self, Read, Write};
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

fn pump<R, W>(mut src: R, mut dst: W, buf: Arc<Mutex<Vec<u8>>>) -> thread::JoinHandle<()>
where
    R: Read + Send + 'static,
    W: Write + Send + 'static,
{
    thread::spawn(move || {
        let mut chunk = [0u8; 8192];
        loop {
            match src.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let _ = dst.write_all(&chunk[..n]);
                    let _ = dst.flush();
                    buf.lock().unwrap().extend_from_slice(&chunk[..n]);
                }
            }
        }
    })
}

pub fn run(store: &Store, cmd: &Command, extra: &[String], by: &str) -> Result<Outcome> {
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
        if !std::path::Path::new(d).is_dir() {
            bail!("working directory {d} does not exist");
        }
    }

    let session = match &cmd.session {
        Some(name) => Some(store.session_require(name)?),
        None => None,
    };
    let env = session.as_ref().map(|s| s.env.clone()).unwrap_or_default();

    match &session {
        Some(s) => eprintln!("\x1b[2m▶ cr run {} · session {}\x1b[0m", cmd.name, s.name),
        None => eprintln!("\x1b[2m▶ cr run {}\x1b[0m", cmd.name),
    }
    for (k, v) in &env {
        eprintln!("\x1b[2m  env {k}={}\x1b[0m", sanitize(v).0);
    }
    eprintln!("\x1b[2m  $ {line}\x1b[0m");
    if let Some(d) = &cwd {
        eprintln!("\x1b[2m  in {d}\x1b[0m");
    }

    let shell = if std::path::Path::new("/bin/bash").exists() { "/bin/bash" } else { "/bin/sh" };
    let mut proc = Proc::new(shell);
    proc.arg("-c").arg(&line).envs(&env).stdin(Stdio::inherit()).stdout(Stdio::piped()).stderr(Stdio::piped());
    if let Some(d) = &cwd {
        proc.current_dir(d);
    }

    let started_at = Local::now();
    let clock = Instant::now();
    let mut child = proc.spawn().with_context(|| format!("spawning {shell}"))?;
    let buf = Arc::new(Mutex::new(Vec::new()));
    let t_out = pump(child.stdout.take().unwrap(), io::stdout(), buf.clone());
    let t_err = pump(child.stderr.take().unwrap(), io::stderr(), buf.clone());
    let status = child.wait()?;
    let _ = t_out.join();
    let _ = t_err.join();
    let duration_ms = clock.elapsed().as_millis() as i64;

    let raw = std::mem::take(&mut *buf.lock().unwrap());
    let truncated = raw.len() > MAX_OUTPUT;
    let tail = if truncated { &raw[raw.len() - MAX_OUTPUT..] } else { &raw[..] };
    let (output, out_redactions) = sanitize(&clean_output(&String::from_utf8_lossy(tail)));
    let (command_text, cmd_redactions) = sanitize(&line);

    store.record_run(
        cmd.id,
        &NewRun {
            started_at,
            duration_ms,
            exit_code: status.code(),
            by: by.to_string(),
            command_text,
            cwd,
            output,
            truncated,
            session_id: session.as_ref().map(|s| s.id),
            env: env.iter().map(|(k, v)| (k.clone(), sanitize(v).0)).collect(),
        },
    )?;

    let code = status.code();
    let mark = if code == Some(0) { "\x1b[32m✓" } else { "\x1b[31m✗" };
    eprintln!(
        "\x1b[2m{mark}\x1b[0m\x1b[2m exit {} · {} · logged to registry\x1b[0m",
        code.map(|c| c.to_string()).unwrap_or_else(|| "signal".into()),
        fmt_duration(duration_ms)
    );
    Ok(Outcome { exit_code: code, duration_ms, redactions: out_redactions + cmd_redactions })
}

pub fn fmt_duration(ms: i64) -> String {
    match ms {
        ms if ms < 1000 => format!("{ms}ms"),
        ms if ms < 60_000 => format!("{:.1}s", ms as f64 / 1000.0),
        ms => format!("{}m{:02}s", ms / 60_000, (ms % 60_000) / 1000),
    }
}
