use crate::jobs::{Job, JobStatus, JobType};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use thiserror::Error;

const SCHEMA_VERSION: i32 = 4;

#[derive(Debug, Error)]
pub enum DbError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error("{0}")]
    Message(String),
}

pub struct JobsDb {
    conn: Mutex<Connection>,
}

impl JobsDb {
    pub fn open(path: &Path) -> Result<Self, DbError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;")?;
        migrate(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn insert_job(&self, job: &mut Job) -> Result<(), DbError> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO jobs (name, status, log, format, type, url, pid, force_generic_extractor, extra_params, scheduled_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                job.name,
                job.status as i32,
                job.log,
                job.format,
                job.job_type as i32,
                job.urls.join("\n"),
                job.pid,
                job.force_generic_extractor as i32,
                serde_json::to_string(&job.extra_params).unwrap_or_else(|_| "{}".into()),
                job.scheduled_at,
            ],
        )?;
        job.id = conn.last_insert_rowid();
        Ok(())
    }

    pub fn update_job(&self, job: &Job) -> Result<(), DbError> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE jobs SET status = ?1, log = ?2, last_update = datetime('now'), force_generic_extractor = ?3, extra_params = ?4, scheduled_at = ?5 WHERE id = ?6",
            params![
                job.status as i32,
                job.log,
                job.force_generic_extractor as i32,
                serde_json::to_string(&job.extra_params).unwrap_or_else(|_| "{}".into()),
                job.scheduled_at,
                job.id,
            ],
        )?;
        Ok(())
    }

    pub fn set_status(&self, id: i64, status: JobStatus) -> Result<(), DbError> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE jobs SET status = ?1, last_update = datetime('now') WHERE id = ?2",
            params![status as i32, id],
        )?;
        Ok(())
    }

    pub fn set_pid(&self, id: i64, pid: i32) -> Result<(), DbError> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE jobs SET pid = ?1, last_update = datetime('now') WHERE id = ?2",
            params![pid, id],
        )?;
        Ok(())
    }

    pub fn set_log(&self, id: i64, log: &str) -> Result<(), DbError> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE jobs SET log = ?1, last_update = datetime('now') WHERE id = ?2",
            params![log, id],
        )?;
        Ok(())
    }

    pub fn set_name(&self, id: i64, name: &str) -> Result<(), DbError> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE jobs SET name = ?1, last_update = datetime('now') WHERE id = ?2",
            params![name, id],
        )?;
        Ok(())
    }

    pub fn get_job(&self, id: i64) -> Result<Option<JobRow>, DbError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, name, status, log, last_update, format, type, url, pid, force_generic_extractor, extra_params, scheduled_at FROM jobs WHERE id = ?1",
        )?;
        let row = stmt
            .query_row(params![id], row_to_job)
            .optional()?;
        Ok(row)
    }

    pub fn get_jobs(&self, limit: usize, status: Option<&str>, with_logs: bool) -> Result<Vec<JobRow>, DbError> {
        let conn = self.conn.lock().unwrap();
        let status_idx = status.and_then(JobStatus::from_name);
        let log_col = if with_logs { "log" } else { "''" };
        let sql = if status_idx.is_some() {
            format!(
                "SELECT id, name, status, {log_col}, last_update, format, type, url, pid, force_generic_extractor, extra_params, scheduled_at
                 FROM jobs WHERE status = ?1 ORDER BY last_update DESC LIMIT ?2"
            )
        } else {
            format!(
                "SELECT id, name, status, {log_col}, last_update, format, type, url, pid, force_generic_extractor, extra_params, scheduled_at
                 FROM jobs ORDER BY last_update DESC LIMIT ?1"
            )
        };
        let mut stmt = conn.prepare(&sql)?;
        let rows = if let Some(idx) = status_idx {
            stmt.query_map(params![idx as i32, limit as i64], row_to_job)?
                .collect::<Result<Vec<_>, _>>()?
        } else {
            stmt.query_map(params![limit as i64], row_to_job)?
                .collect::<Result<Vec<_>, _>>()?
        };
        Ok(rows)
    }

    pub fn get_due_scheduled(&self, now: i64) -> Result<Vec<JobRow>, DbError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, name, status, log, last_update, format, type, url, pid, force_generic_extractor, extra_params, scheduled_at
             FROM jobs WHERE status = ?1 AND scheduled_at <= ?2",
        )?;
        let rows = stmt
            .query_map(params![JobStatus::Scheduled as i32, now], row_to_job)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn pending_or_running(&self) -> Result<Vec<JobRow>, DbError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, name, status, log, last_update, format, type, url, pid, force_generic_extractor, extra_params, scheduled_at
             FROM jobs WHERE status IN (?1, ?2)",
        )?;
        let rows = stmt
            .query_map(params![JobStatus::Pending as i32, JobStatus::Running as i32], row_to_job)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn running_update(&self) -> Result<Option<JobRow>, DbError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, name, status, log, last_update, format, type, url, pid, force_generic_extractor, extra_params, scheduled_at
             FROM jobs WHERE type = ?1 AND status IN (?2, ?3) ORDER BY id DESC LIMIT 1",
        )?;
        let row = stmt
            .query_row(
                params![
                    JobType::YtdlpUpdate as i32,
                    JobStatus::Pending as i32,
                    JobStatus::Running as i32
                ],
                row_to_job,
            )
            .optional()?;
        Ok(row)
    }

    pub fn job_counts(&self) -> Result<HashMap<String, i64>, DbError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT status, COUNT(*) FROM jobs GROUP BY status")?;
        let mut counts = HashMap::from([
            ("running".into(), 0),
            ("completed".into(), 0),
            ("failed".into(), 0),
            ("pending".into(), 0),
            ("aborted".into(), 0),
            ("scheduled".into(), 0),
        ]);
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, i32>(0)?, row.get::<_, i64>(1)?))
        })?;
        for row in rows {
            let (status, n) = row?;
            if let Some(name) = JobStatus::from_i32(status) {
                counts.insert(name.as_str().to_lowercase(), n);
            }
        }
        Ok(counts)
    }

    pub fn purge(&self) -> Result<usize, DbError> {
        let conn = self.conn.lock().unwrap();
        let n = conn.execute("DELETE FROM jobs", [])?;
        conn.execute("VACUUM", []).ok();
        Ok(n)
    }

    pub fn delete_job(&self, id: i64) -> Result<usize, DbError> {
        let conn = self.conn.lock().unwrap();
        let n = conn.execute("DELETE FROM jobs WHERE id = ?1", params![id])?;
        Ok(n)
    }

    pub fn clean_old(&self, keep: usize) -> Result<usize, DbError> {
        let conn = self.conn.lock().unwrap();
        let n = conn.execute(
            "DELETE FROM jobs WHERE id NOT IN (SELECT id FROM jobs ORDER BY last_update DESC LIMIT ?1) AND status NOT IN (?2, ?3, ?4)",
            params![
                keep as i64,
                JobStatus::Pending as i32,
                JobStatus::Running as i32,
                JobStatus::Scheduled as i32
            ],
        )?;
        Ok(n)
    }
}

#[derive(Debug, Clone)]
pub struct JobRow {
    pub id: i64,
    pub name: String,
    pub status: JobStatus,
    pub log: String,
    pub last_update: String,
    pub format: Option<String>,
    pub job_type: JobType,
    pub urls: Vec<String>,
    pub pid: i32,
    pub force_generic_extractor: bool,
    pub extra_params: Value,
    pub scheduled_at: Option<i64>,
}

impl JobRow {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.id,
            "name": self.name,
            "status": self.status.as_str(),
            "log": self.log,
            "format": self.format,
            "last_update": self.last_update,
            "type": self.job_type as i32,
            "urls": self.urls,
            "pid": self.pid,
            "force_generic_extractor": self.force_generic_extractor,
            "extra_params": self.extra_params,
            "scheduled_at": self.scheduled_at,
        })
    }

    pub fn into_job(self) -> Job {
        Job {
            id: self.id,
            name: self.name,
            status: JobStatus::Pending,
            log: "Queue stopped".into(),
            format: self.format,
            job_type: self.job_type,
            urls: self.urls,
            pid: 0,
            force_generic_extractor: self.force_generic_extractor,
            extra_params: self.extra_params,
            scheduled_at: None,
        }
    }
}

fn row_to_job(row: &rusqlite::Row<'_>) -> rusqlite::Result<JobRow> {
    let url: String = row.get(7)?;
    let extra: String = row.get::<_, Option<String>>(10)?.unwrap_or_else(|| "{}".into());
    let extra_params = serde_json::from_str(&extra).unwrap_or(Value::Object(Default::default()));
    let last_update: String = row.get(4)?;
    Ok(JobRow {
        id: row.get(0)?,
        name: row.get(1)?,
        status: JobStatus::from_i32(row.get(2)?).unwrap_or(JobStatus::Pending),
        log: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
        last_update: convert_utc(&last_update),
        format: row.get(5)?,
        job_type: JobType::from_i32(row.get(6)?).unwrap_or(JobType::YdlDownload),
        urls: url.split('\n').filter(|s| !s.is_empty()).map(|s| s.to_string()).collect(),
        pid: row.get::<_, Option<i32>>(8)?.unwrap_or(0),
        force_generic_extractor: row.get::<_, Option<i32>>(9)?.unwrap_or(0) != 0,
        extra_params,
        scheduled_at: row.get(11)?,
    })
}

fn convert_utc(dt: &str) -> String {
    chrono::NaiveDateTime::parse_from_str(dt, "%Y-%m-%d %H:%M:%S")
        .ok()
        .map(|n| {
            n.and_utc()
                .with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M:%S")
                .to_string()
        })
        .unwrap_or_else(|| dt.to_string())
}

fn migrate(conn: &Connection) -> Result<(), DbError> {
    let table_exists: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='jobs'",
            [],
            |row| row.get::<_, i64>(0),
        )?
        != 0;
    if !table_exists {
        create(conn)?;
        return Ok(());
    }
    add_missing_columns(conn)?;
    Ok(())
}

fn create(conn: &Connection) -> Result<(), DbError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS jobs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL,
            status INTEGER NOT NULL,
            log TEXT,
            format TEXT,
            last_update DATETIME DEFAULT CURRENT_TIMESTAMP,
            type INTEGER NOT NULL,
            url TEXT,
            pid INTEGER,
            force_generic_extractor INTEGER DEFAULT 0,
            extra_params TEXT DEFAULT '{}',
            scheduled_at INTEGER
        );",
    )?;
    conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    Ok(())
}

fn add_missing_columns(conn: &Connection) -> Result<(), DbError> {
    let mut stmt = conn.prepare("PRAGMA table_info('jobs')")?;
    let existing: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    let needed = [
        ("force_generic_extractor", "INTEGER DEFAULT 0"),
        ("extra_params", "TEXT DEFAULT '{}'"),
        ("scheduled_at", "INTEGER"),
    ];
    for (name, def) in needed {
        if !existing.iter().any(|c| c == name) {
            conn.execute(&format!("ALTER TABLE jobs ADD COLUMN {name} {def}"), [])?;
        }
    }
    conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    Ok(())
}
