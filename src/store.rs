//! SQLite-backed registry: commands (with detailed instructions) and their runs.

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Local};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS commands (
    id           INTEGER PRIMARY KEY,
    name         TEXT    NOT NULL UNIQUE,
    command      TEXT    NOT NULL,
    description  TEXT    NOT NULL DEFAULT '',
    instructions TEXT    NOT NULL DEFAULT '',
    cwd          TEXT,
    args_json    TEXT    NOT NULL DEFAULT '[]',
    tags         TEXT    NOT NULL DEFAULT '',
    source       TEXT    NOT NULL DEFAULT 'user',
    redacted     INTEGER NOT NULL DEFAULT 0,
    created_at   TEXT    NOT NULL,
    updated_at   TEXT    NOT NULL
);
CREATE TABLE IF NOT EXISTS runs (
    id           INTEGER PRIMARY KEY,
    command_id   INTEGER NOT NULL REFERENCES commands(id) ON DELETE CASCADE,
    started_at   TEXT    NOT NULL,
    duration_ms  INTEGER NOT NULL,
    exit_code    INTEGER,
    by           TEXT    NOT NULL,
    command_text TEXT    NOT NULL,
    cwd          TEXT,
    output       TEXT    NOT NULL DEFAULT '',
    truncated    INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS runs_by_command ON runs(command_id, started_at);
CREATE TABLE IF NOT EXISTS sessions (
    id           INTEGER PRIMARY KEY,
    name         TEXT    NOT NULL UNIQUE,
    description  TEXT    NOT NULL DEFAULT '',
    created_at   TEXT    NOT NULL
);
CREATE TABLE IF NOT EXISTS session_env (
    session_id   INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    key          TEXT    NOT NULL,
    value        TEXT    NOT NULL,
    updated_at   TEXT    NOT NULL,
    PRIMARY KEY (session_id, key)
);
"#;

/// Columns added after the first release: (table, column, definition).
const MIGRATIONS: &[(&str, &str, &str)] = &[
    ("commands", "session_id", "INTEGER REFERENCES sessions(id) ON DELETE SET NULL"),
    ("runs", "session_id", "INTEGER REFERENCES sessions(id) ON DELETE SET NULL"),
    ("runs", "env_json", "TEXT NOT NULL DEFAULT '{}'"),
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArgSpec {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Command {
    pub id: i64,
    pub name: String,
    pub command: String,
    pub description: String,
    pub instructions: String,
    pub cwd: Option<String>,
    pub args: Vec<ArgSpec>,
    pub tags: Vec<String>,
    pub source: String,
    pub redacted: bool,
    pub created_at: DateTime<Local>,
    pub updated_at: DateTime<Local>,
    pub run_count: i64,
    pub last_run: Option<Run>,
    pub session: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Session {
    pub id: i64,
    pub name: String,
    pub description: String,
    pub created_at: DateTime<Local>,
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Run {
    pub id: i64,
    pub started_at: DateTime<Local>,
    pub duration_ms: i64,
    pub exit_code: Option<i32>,
    pub by: String,
    pub command_text: String,
    pub cwd: Option<String>,
    /// Empty unless fetched with `run_output` / `runs(.., with_output = true)`.
    pub output: String,
    pub truncated: bool,
    /// The session variables injected into this run (empty unless fetched with `runs`).
    pub env: BTreeMap<String, String>,
}

/// What a caller supplies when registering a command (already sanitized).
pub struct NewCommand {
    pub name: String,
    pub command: String,
    pub description: String,
    pub instructions: String,
    pub cwd: Option<String>,
    pub args: Vec<ArgSpec>,
    pub tags: Vec<String>,
    pub source: String,
    pub redacted: bool,
    pub session: Option<String>,
}

pub struct NewRun {
    pub started_at: DateTime<Local>,
    pub duration_ms: i64,
    pub exit_code: Option<i32>,
    pub by: String,
    pub command_text: String,
    pub cwd: Option<String>,
    pub output: String,
    pub truncated: bool,
    pub session_id: Option<i64>,
    pub env: BTreeMap<String, String>,
}

pub struct Store {
    conn: Connection,
}

pub fn default_db_path() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("commands-registry")
        .join("registry.db")
}

fn ts(dt: &DateTime<Local>) -> String {
    dt.to_rfc3339()
}

fn parse_ts(s: &str) -> DateTime<Local> {
    DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&Local))
        .unwrap_or_else(|_| Local::now())
}

const CMD_COLS: &str = "c.id, c.name, c.command, c.description, c.instructions, c.cwd, c.args_json, \
     c.tags, c.source, c.redacted, c.created_at, c.updated_at, \
     (SELECT COUNT(*) FROM runs x WHERE x.command_id = c.id), \
     r.id, r.started_at, r.duration_ms, r.exit_code, r.by, r.command_text, r.cwd, r.truncated, \
     (SELECT name FROM sessions s WHERE s.id = c.session_id)";

const CMD_FROM: &str = "FROM commands c LEFT JOIN runs r ON r.id = \
     (SELECT id FROM runs WHERE command_id = c.id ORDER BY started_at DESC, id DESC LIMIT 1)";

fn command_from_row(row: &Row) -> rusqlite::Result<Command> {
    let args_json: String = row.get(6)?;
    let tags: String = row.get(7)?;
    let last_run = match row.get::<_, Option<i64>>(13)? {
        Some(id) => Some(Run {
            id,
            started_at: parse_ts(&row.get::<_, String>(14)?),
            duration_ms: row.get(15)?,
            exit_code: row.get(16)?,
            by: row.get(17)?,
            command_text: row.get(18)?,
            cwd: row.get(19)?,
            output: String::new(),
            truncated: row.get::<_, i64>(20)? != 0,
            env: BTreeMap::new(),
        }),
        None => None,
    };
    Ok(Command {
        id: row.get(0)?,
        name: row.get(1)?,
        command: row.get(2)?,
        description: row.get(3)?,
        instructions: row.get(4)?,
        cwd: row.get(5)?,
        args: serde_json::from_str(&args_json).unwrap_or_default(),
        tags: tags.split(',').map(str::trim).filter(|t| !t.is_empty()).map(String::from).collect(),
        source: row.get(8)?,
        redacted: row.get::<_, i64>(9)? != 0,
        created_at: parse_ts(&row.get::<_, String>(10)?),
        updated_at: parse_ts(&row.get::<_, String>(11)?),
        run_count: row.get(12)?,
        last_run,
        session: row.get(21)?,
    })
}

const RUN_COLS: &str = "r.id, r.started_at, r.duration_ms, r.exit_code, r.by, r.command_text, r.cwd";

fn run_from_row(row: &Row, output: String, truncated: bool, env_json: &str) -> rusqlite::Result<Run> {
    Ok(Run {
        id: row.get(0)?,
        started_at: parse_ts(&row.get::<_, String>(1)?),
        duration_ms: row.get(2)?,
        exit_code: row.get(3)?,
        by: row.get(4)?,
        command_text: row.get(5)?,
        cwd: row.get(6)?,
        output,
        truncated,
        env: serde_json::from_str(env_json).unwrap_or_default(),
    })
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON;")?;
        conn.execute_batch(SCHEMA)?;
        for (table, column, def) in MIGRATIONS {
            let has: bool = conn.query_row(
                &format!("SELECT COUNT(*) FROM pragma_table_info('{table}') WHERE name = ?1"),
                [column],
                |r| r.get::<_, i64>(0).map(|n| n > 0),
            )?;
            if !has {
                conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {def}"))?;
            }
        }
        Ok(Self { conn })
    }

    /// Insert or update by name. Empty fields on update keep their old values,
    /// so re-registering with just `--cmd` doesn't wipe the instructions.
    pub fn upsert(&self, c: &NewCommand) -> Result<Command> {
        let session_id = match &c.session {
            Some(name) => Some(self.session_require(name)?.id),
            None => None,
        };
        let now = ts(&Local::now());
        let args_json = if c.args.is_empty() { String::new() } else { serde_json::to_string(&c.args)? };
        self.conn.execute(
            "INSERT INTO commands (name, command, description, instructions, cwd, args_json, tags, source, redacted, created_at, updated_at, session_id)
             VALUES (?1, ?2, ?3, ?4, ?5, CASE WHEN ?6 = '' THEN '[]' ELSE ?6 END, ?7, ?8, ?9, ?10, ?10, ?11)
             ON CONFLICT(name) DO UPDATE SET
               command      = excluded.command,
               description  = CASE WHEN excluded.description  = '' THEN commands.description  ELSE excluded.description  END,
               instructions = CASE WHEN excluded.instructions = '' THEN commands.instructions ELSE excluded.instructions END,
               cwd          = COALESCE(excluded.cwd, commands.cwd),
               args_json    = CASE WHEN ?6 = '' THEN commands.args_json ELSE excluded.args_json END,
               tags         = CASE WHEN excluded.tags = '' THEN commands.tags ELSE excluded.tags END,
               source       = excluded.source,
               redacted     = excluded.redacted,
               updated_at   = excluded.updated_at,
               session_id   = COALESCE(excluded.session_id, commands.session_id)",
            params![
                c.name,
                c.command,
                c.description,
                c.instructions,
                c.cwd,
                args_json,
                c.tags.join(","),
                c.source,
                c.redacted as i64,
                now,
                session_id
            ],
        )?;
        self.get(&c.name)?.context("command vanished after upsert")
    }

    /// All commands, most recently used/updated first (the "history" order).
    pub fn list(&self) -> Result<Vec<Command>> {
        let sql = format!("SELECT {CMD_COLS} {CMD_FROM} ORDER BY COALESCE(r.started_at, c.updated_at) DESC");
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], command_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn get(&self, name: &str) -> Result<Option<Command>> {
        let sql = format!("SELECT {CMD_COLS} {CMD_FROM} WHERE c.name = ?1");
        Ok(self.conn.query_row(&sql, [name], command_from_row).optional()?)
    }

    pub fn require(&self, name: &str) -> Result<Command> {
        match self.get(name)? {
            Some(c) => Ok(c),
            None => bail!("no command named {name:?} (see `cr list`)"),
        }
    }

    pub fn remove(&self, name: &str) -> Result<bool> {
        Ok(self.conn.execute("DELETE FROM commands WHERE name = ?1", [name])? > 0)
    }

    pub fn record_run(&self, command_id: i64, r: &NewRun) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO runs (command_id, started_at, duration_ms, exit_code, by, command_text, cwd, output, truncated, session_id, env_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                command_id,
                ts(&r.started_at),
                r.duration_ms,
                r.exit_code,
                r.by,
                r.command_text,
                r.cwd,
                r.output,
                r.truncated as i64,
                r.session_id,
                serde_json::to_string(&r.env)?
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Recent runs, newest first. Output is only loaded when `with_output`.
    pub fn runs(&self, command_id: i64, limit: usize, with_output: bool) -> Result<Vec<Run>> {
        let out_col = if with_output { "r.output" } else { "''" };
        let sql = format!(
            "SELECT {RUN_COLS}, {out_col}, r.truncated, r.env_json
             FROM runs r WHERE r.command_id = ?1 ORDER BY r.started_at DESC, r.id DESC LIMIT ?2"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![command_id, limit as i64], |row| {
            run_from_row(row, row.get(7)?, row.get::<_, i64>(8)? != 0, &row.get::<_, String>(9)?)
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // ───────────────────────────── sessions ─────────────────────────────

    /// Create a session, or update its description if it exists.
    pub fn session_upsert(&self, name: &str, description: &str) -> Result<Session> {
        self.conn.execute(
            "INSERT INTO sessions (name, description, created_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(name) DO UPDATE SET
               description = CASE WHEN excluded.description = '' THEN sessions.description ELSE excluded.description END",
            params![name, description, ts(&Local::now())],
        )?;
        self.session_require(name)
    }

    pub fn session_get(&self, name: &str) -> Result<Option<Session>> {
        let row = self
            .conn
            .query_row("SELECT id, name, description, created_at FROM sessions WHERE name = ?1", [name], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?))
            })
            .optional()?;
        let Some((id, name, description, created_at)) = row else { return Ok(None) };
        Ok(Some(Session { id, name, description, created_at: parse_ts(&created_at), env: self.session_env(id)? }))
    }

    pub fn session_require(&self, name: &str) -> Result<Session> {
        match self.session_get(name)? {
            Some(s) => Ok(s),
            None => bail!("no session named {name:?} (create it with `cr session new {name}`)"),
        }
    }

    /// All sessions, newest first.
    pub fn sessions(&self) -> Result<Vec<Session>> {
        let mut stmt = self.conn.prepare("SELECT name FROM sessions ORDER BY created_at DESC, id DESC")?;
        let names: Vec<String> = stmt.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
        names.iter().map(|n| self.session_require(n)).collect()
    }

    fn session_env(&self, session_id: i64) -> Result<BTreeMap<String, String>> {
        let mut stmt = self.conn.prepare("SELECT key, value FROM session_env WHERE session_id = ?1")?;
        let rows = stmt.query_map([session_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn session_set_env(&self, name: &str, key: &str, value: &str) -> Result<()> {
        let s = self.session_require(name)?;
        self.conn.execute(
            "INSERT INTO session_env (session_id, key, value, updated_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(session_id, key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
            params![s.id, key, value, ts(&Local::now())],
        )?;
        Ok(())
    }

    pub fn session_unset_env(&self, name: &str, key: &str) -> Result<bool> {
        let s = self.session_require(name)?;
        Ok(self.conn.execute("DELETE FROM session_env WHERE session_id = ?1 AND key = ?2", params![s.id, key])? > 0)
    }

    /// The session's commands in the order they were registered (its steps).
    pub fn session_commands(&self, session_id: i64) -> Result<Vec<Command>> {
        let sql = format!("SELECT {CMD_COLS} {CMD_FROM} WHERE c.session_id = ?1 ORDER BY c.id");
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([session_id], command_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Every run made in the session, oldest first, with the name of its command.
    pub fn session_runs(&self, session_id: i64, with_output: bool) -> Result<Vec<(String, Run)>> {
        let out_col = if with_output { "r.output" } else { "''" };
        let sql = format!(
            "SELECT {RUN_COLS}, {out_col}, r.truncated, r.env_json, c.name
             FROM runs r JOIN commands c ON c.id = r.command_id
             WHERE r.session_id = ?1 ORDER BY r.started_at, r.id"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([session_id], |row| {
            let run = run_from_row(row, row.get(7)?, row.get::<_, i64>(8)? != 0, &row.get::<_, String>(9)?)?;
            Ok((row.get::<_, String>(10)?, run))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn run_output(&self, run_id: i64) -> Result<String> {
        Ok(self.conn.query_row("SELECT output FROM runs WHERE id = ?1", [run_id], |r| r.get(0))?)
    }
}
