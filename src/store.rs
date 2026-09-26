use parking_lot::Mutex;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::OnceLock;

use crate::config;
use crate::model_resolver::normalize_model_name;

pub const DB_FILENAME: &str = "dashboard.sqlite3";

struct Db {
    path: PathBuf,
    conn: Mutex<Connection>,
}

static DB: OnceLock<Db> = OnceLock::new();

pub fn database_path() -> PathBuf {
    PathBuf::from(&config::get().data_dir).join(DB_FILENAME)
}

fn open(path: &PathBuf) -> rusqlite::Result<Connection> {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let conn = Connection::open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    Ok(conn)
}

fn db() -> &'static Db {
    DB.get_or_init(|| {
        let path = database_path();
        let conn = open(&path).unwrap_or_else(|e| panic!("cannot open {}: {e}", path.display()));
        Db {
            path,
            conn: Mutex::new(conn),
        }
    })
}

pub fn path() -> PathBuf {
    db().path.clone()
}

/// Runs `f` against the shared connection inside one transaction.
pub fn with<T>(f: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> rusqlite::Result<T> {
    let conn = db().conn.lock();
    conn.execute_batch("BEGIN")?;
    match f(&conn) {
        Ok(value) => {
            conn.execute_batch("COMMIT")?;
            Ok(value)
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

/// Same as `with`, off the async runtime: sqlite I/O never runs on a reactor thread.
pub async fn run<T: Send + 'static>(
    f: impl FnOnce(&Connection) -> rusqlite::Result<T> + Send + 'static,
) -> rusqlite::Result<T> {
    tokio::task::spawn_blocking(move || with(f))
        .await
        .unwrap_or_else(|e| {
            Err(rusqlite::Error::ToSqlConversionFailure(Box::new(
                std::io::Error::other(e.to_string()),
            )))
        })
}

fn columns(conn: &Connection, table: &str) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
    rows.collect()
}

pub fn initialize() -> rusqlite::Result<()> {
    with(|conn| {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS account_sources (
                account_id TEXT PRIMARY KEY,
                position INTEGER NOT NULL,
                config_json TEXT NOT NULL,
                credential_json TEXT
            );
            CREATE TABLE IF NOT EXISTS account_runtime (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                state_json TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS store_migrations (name TEXT PRIMARY KEY, completed_at INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS runtime_writer (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                slot TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS credential_refresh_leases (
                account_id TEXT PRIMARY KEY,
                owner TEXT NOT NULL,
                expires_at REAL NOT NULL
            );
            CREATE TABLE IF NOT EXISTS settings (
                key TEXT PRIMARY KEY,
                value_json TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS request_logs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                created_at INTEGER NOT NULL,
                route TEXT NOT NULL,
                model TEXT,
                status_code INTEGER NOT NULL,
                latency_ms INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_request_logs_created_at ON request_logs(created_at);
            CREATE TABLE IF NOT EXISTS request_metric_rollups (
                route TEXT NOT NULL, model TEXT NOT NULL, status_code INTEGER NOT NULL,
                requests INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY(route, model, status_code)
            );
            CREATE TABLE IF NOT EXISTS request_latency_rollups (
                route TEXT NOT NULL, model TEXT NOT NULL,
                requests INTEGER NOT NULL DEFAULT 0, latency_ms INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY(route, model)
            );
            CREATE TABLE IF NOT EXISTS dashboard_migrations (name TEXT PRIMARY KEY);",
        )?;
        let done: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM dashboard_migrations WHERE name = 'request_rollups_v1'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        if done.is_none() {
            conn.execute_batch(
                "INSERT INTO request_metric_rollups(route, model, status_code, requests)
                 SELECT route, COALESCE(model, ''), status_code, COUNT(*)
                 FROM request_logs GROUP BY route, COALESCE(model, ''), status_code;
                 INSERT INTO request_latency_rollups(route, model, requests, latency_ms)
                 SELECT route, COALESCE(model, ''), COUNT(*), COALESCE(SUM(latency_ms), 0) FROM request_logs
                 WHERE status_code BETWEEN 200 AND 399 GROUP BY route, COALESCE(model, '');
                 INSERT INTO dashboard_migrations(name) VALUES ('request_rollups_v1');",
            )?;
        }
        conn.execute_batch(
            "CREATE TRIGGER IF NOT EXISTS rollup_request_log AFTER INSERT ON request_logs BEGIN
                INSERT INTO request_metric_rollups(route, model, status_code, requests)
                VALUES (NEW.route, COALESCE(NEW.model, ''), NEW.status_code, 1)
                ON CONFLICT(route, model, status_code) DO UPDATE SET requests = requests + 1;
                INSERT INTO request_latency_rollups(route, model, requests, latency_ms)
                SELECT NEW.route, COALESCE(NEW.model, ''), 1, NEW.latency_ms
                WHERE NEW.status_code BETWEEN 200 AND 399
                ON CONFLICT(route, model) DO UPDATE SET requests = requests + 1,
                    latency_ms = latency_ms + excluded.latency_ms;
            END;
            CREATE TABLE IF NOT EXISTS rate_observations (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                account_id TEXT NOT NULL,
                observed_at REAL NOT NULL,
                rpm INTEGER NOT NULL,
                rejected INTEGER NOT NULL,
                outcome TEXT NOT NULL DEFAULT 'success'
            );",
        )?;
        if !columns(conn, "rate_observations")?
            .iter()
            .any(|c| c == "outcome")
        {
            conn.execute_batch(
                "ALTER TABLE rate_observations ADD COLUMN outcome TEXT NOT NULL DEFAULT 'success';
                 UPDATE rate_observations SET outcome = 'rate_limited' WHERE rejected = 1;",
            )?;
        }
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_rate_observations_account ON rate_observations(account_id, observed_at);
            CREATE INDEX IF NOT EXISTS idx_rate_observations_observed ON rate_observations(observed_at);
            CREATE TABLE IF NOT EXISTS api_keys (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                key_prefix TEXT NOT NULL,
                salt BLOB NOT NULL,
                key_hash BLOB NOT NULL,
                created_at INTEGER NOT NULL,
                revoked_at INTEGER
            );
            CREATE INDEX IF NOT EXISTS idx_api_keys_prefix ON api_keys(key_prefix);
            CREATE TABLE IF NOT EXISTS key_model_usage (
                key_id TEXT NOT NULL,
                model TEXT NOT NULL,
                prompt_tokens INTEGER NOT NULL DEFAULT 0,
                completion_tokens INTEGER NOT NULL DEFAULT 0,
                requests INTEGER NOT NULL DEFAULT 0,
                generation_ms INTEGER NOT NULL DEFAULT 0,
                timed_completion_tokens INTEGER NOT NULL DEFAULT 0,
                updated_at INTEGER NOT NULL,
                PRIMARY KEY (key_id, model)
            );",
        )?;
        let usage_cols = columns(conn, "key_model_usage")?;
        for col in ["generation_ms", "timed_completion_tokens"] {
            if !usage_cols.iter().any(|c| c == col) {
                conn.execute_batch(&format!(
                    "ALTER TABLE key_model_usage ADD COLUMN {col} INTEGER NOT NULL DEFAULT 0"
                ))?;
            }
        }
        let log_cols = columns(conn, "request_logs")?;
        for (col, ddl) in [
            ("client_ip", "TEXT"),
            ("user_agent", "TEXT"),
            ("api_key_name", "TEXT"),
            ("input_tokens", "INTEGER"),
            ("output_tokens", "INTEGER"),
            ("credits", "REAL"),
            ("generation_ms", "INTEGER"),
        ] {
            if !log_cols.iter().any(|c| c == col) {
                conn.execute_batch(&format!("ALTER TABLE request_logs ADD COLUMN {col} {ddl}"))?;
            }
        }
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_request_logs_model ON request_logs(model, id);
            CREATE TABLE IF NOT EXISTS account_model_usage (
                key_id TEXT NOT NULL,
                account_id TEXT NOT NULL,
                model TEXT NOT NULL,
                prompt_tokens INTEGER NOT NULL DEFAULT 0,
                completion_tokens INTEGER NOT NULL DEFAULT 0,
                requests INTEGER NOT NULL DEFAULT 0,
                generation_ms INTEGER NOT NULL DEFAULT 0,
                timed_completion_tokens INTEGER NOT NULL DEFAULT 0,
                updated_at INTEGER NOT NULL,
                PRIMARY KEY (key_id, account_id, model)
            );
            CREATE INDEX IF NOT EXISTS idx_account_model_usage_account ON account_model_usage(account_id, model);
            CREATE TABLE IF NOT EXISTS account_usage (
                account_id TEXT PRIMARY KEY,
                email TEXT,
                subscription_title TEXT,
                subscription_type TEXT,
                resource_type TEXT,
                current_usage REAL,
                usage_limit REAL,
                usage_percent REAL,
                unit TEXT,
                next_date_reset TEXT,
                days_until_reset REAL,
                overage_status TEXT,
                overage_used REAL,
                updated_at INTEGER NOT NULL,
                error TEXT
            );",
        )?;
        let usage_cols = columns(conn, "account_usage")?;
        for (col, ddl) in [
            ("overage_status", "TEXT"),
            ("overage_used", "REAL"),
            ("email", "TEXT"),
        ] {
            if !usage_cols.iter().any(|c| c == col) {
                conn.execute_batch(&format!("ALTER TABLE account_usage ADD COLUMN {col} {ddl}"))?;
            }
        }
        merge_unnormalized_usage_models(conn)?;
        Ok(())
    })
}

fn merge_unnormalized_usage_models(conn: &Connection) -> rusqlite::Result<usize> {
    let rows: Vec<(String, String, i64, i64, i64, i64, i64, i64)> = {
        let mut stmt = conn.prepare(
            "SELECT key_id, model, prompt_tokens, completion_tokens, requests, generation_ms,
                    timed_completion_tokens, updated_at FROM key_model_usage",
        )?;
        let iter = stmt.query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
            ))
        })?;
        iter.collect::<rusqlite::Result<_>>()?
    };
    let mut merged = 0;
    for (key_id, model, p, c, req, gen, timed, updated) in rows {
        let canonical = normalize_model_name(&model);
        let canonical = if canonical.is_empty() {
            model.clone()
        } else {
            canonical
        };
        if canonical == model {
            continue;
        }
        conn.execute(
            "INSERT INTO key_model_usage(key_id, model, prompt_tokens, completion_tokens, requests, generation_ms, timed_completion_tokens, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(key_id, model) DO UPDATE SET
                prompt_tokens = prompt_tokens + excluded.prompt_tokens,
                completion_tokens = completion_tokens + excluded.completion_tokens,
                requests = requests + excluded.requests,
                generation_ms = generation_ms + excluded.generation_ms,
                timed_completion_tokens = timed_completion_tokens + excluded.timed_completion_tokens,
                updated_at = MAX(updated_at, excluded.updated_at)",
            params![key_id, canonical, p, c, req, gen, timed, updated],
        )?;
        conn.execute(
            "DELETE FROM key_model_usage WHERE key_id = ?1 AND model = ?2",
            params![key_id, model],
        )?;
        merged += 1;
    }
    if merged > 0 {
        tracing::info!("Merged {merged} usage row(s) stored under a non-normalized model name");
    }
    Ok(merged)
}

// ----- settings -----------------------------------------------------------------------------

pub fn load_setting(key: &str) -> Option<Value> {
    let raw: Option<String> = with(|c| {
        c.query_row(
            "SELECT value_json FROM settings WHERE key = ?1",
            [key],
            |r| r.get(0),
        )
        .optional()
    })
    .map_err(|e| tracing::warn!("[Store] Could not read setting {key:?}: {e}"))
    .ok()
    .flatten();
    raw.and_then(|s| {
        serde_json::from_str(&s)
            .map_err(|e| tracing::warn!("[Store] Discarding malformed setting {key:?}: {e}"))
            .ok()
    })
}

pub fn save_setting(key: &str, value: &Value) -> rusqlite::Result<()> {
    let payload = value.to_string();
    with(|c| {
        c.execute(
            "INSERT INTO settings(key, value_json) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value_json=excluded.value_json",
            params![key, payload],
        )
        .map(|_| ())
    })
}

// ----- blue/green runtime writer -----------------------------------------------------------

pub fn set_runtime_writer(slot: &str) -> rusqlite::Result<()> {
    with(|c| {
        c.execute(
            "INSERT INTO runtime_writer(id, slot) VALUES (1, ?1) ON CONFLICT(id) DO UPDATE SET slot=excluded.slot",
            [slot],
        )
        .map(|_| ())
    })
}

fn active_writer(conn: &Connection) -> rusqlite::Result<Option<String>> {
    conn.query_row("SELECT slot FROM runtime_writer WHERE id = 1", [], |r| {
        r.get(0)
    })
    .optional()
}

pub fn can_write_runtime_state() -> bool {
    let slot = &config::get().kiro_slot;
    if slot.is_empty() {
        return true;
    }
    with(active_writer).ok().flatten().as_deref() == Some(slot.as_str())
}

pub fn require_runtime_writer(conn: &Connection) -> rusqlite::Result<()> {
    let slot = &config::get().kiro_slot;
    if slot.is_empty() {
        return Ok(());
    }
    if active_writer(conn)?.as_deref() != Some(slot.as_str()) {
        return Err(rusqlite::Error::ToSqlConversionFailure(Box::new(
            std::io::Error::other(format!(
                "gateway store write rejected: slot {slot:?} is not the active writer"
            )),
        )));
    }
    Ok(())
}

// ----- account sources and credentials -----------------------------------------------------

pub fn load_account_sources_in(conn: &Connection) -> rusqlite::Result<Vec<Value>> {
    let mut stmt = conn.prepare("SELECT config_json FROM account_sources ORDER BY position")?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    Ok(rows
        .flatten()
        .filter_map(|s| serde_json::from_str(&s).ok())
        .collect())
}

pub fn load_account_sources() -> Vec<Value> {
    with(load_account_sources_in).unwrap_or_default()
}

pub fn account_id_for_entry(entry: &Value) -> String {
    match entry.get("type").and_then(Value::as_str) {
        Some("internal") => entry.get("id").map(value_to_plain).unwrap_or_default(),
        Some("refresh_token") => {
            use sha2::{Digest, Sha256};
            let token = entry
                .get("refresh_token")
                .map(value_to_plain)
                .unwrap_or_default();
            let digest = hex::encode(Sha256::digest(token.as_bytes()));
            format!("refresh_token_{}", &digest[..16])
        }
        _ => {
            let raw = entry.get("path").map(value_to_plain).unwrap_or_default();
            let expanded = expand_home(&raw);
            std::fs::canonicalize(&expanded)
                .map(|p| strip_verbatim(p.to_string_lossy().into_owned()))
                .unwrap_or_else(|_| absolute(&expanded))
        }
    }
}

fn value_to_plain(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

pub fn expand_home(path: &str) -> String {
    if let Some(rest) = path.strip_prefix('~') {
        if let Some(home) = home_dir() {
            return format!("{}{}", home.display(), rest);
        }
    }
    path.to_owned()
}

pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

fn strip_verbatim(s: String) -> String {
    s.strip_prefix(r"\\?\").map(str::to_owned).unwrap_or(s)
}

fn absolute(path: &str) -> String {
    let p = PathBuf::from(path);
    if p.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()
            .map(|d| d.join(p).to_string_lossy().into_owned())
            .unwrap_or_else(|_| path.to_owned())
    }
}

pub fn replace_account_sources(
    conn: &Connection,
    entries: &[Value],
    ungated: bool,
) -> rusqlite::Result<()> {
    if !ungated {
        require_runtime_writer(conn)?;
    }
    let existing: HashMap<String, Option<String>> = {
        let mut stmt = conn.prepare("SELECT account_id, credential_json FROM account_sources")?;
        let iter = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        })?;
        iter.collect::<rusqlite::Result<_>>()?
    };
    conn.execute("DELETE FROM account_sources", [])?;
    for (position, entry) in entries.iter().enumerate() {
        let account_id = account_id_for_entry(entry);
        let credential = if entry.get("type").and_then(Value::as_str) == Some("internal") {
            entry.get("credential").filter(|v| !v.is_null())
        } else {
            None
        };
        let mut credential_json = existing.get(&account_id).cloned().flatten();
        if credential_json.is_none() {
            credential_json = credential.map(Value::to_string);
        }
        let mut stored = entry.clone();
        if let Some(obj) = stored.as_object_mut() {
            obj.remove("credential");
        }
        conn.execute(
            "INSERT INTO account_sources(account_id, position, config_json, credential_json) VALUES (?1, ?2, ?3, ?4)",
            params![account_id, position as i64, stored.to_string(), credential_json],
        )?;
    }
    Ok(())
}

pub fn load_internal_credential(account_id: &str) -> Option<Value> {
    let raw: Option<Option<String>> = with(|c| {
        c.query_row(
            "SELECT credential_json FROM account_sources WHERE account_id = ?1",
            [account_id],
            |r| r.get(0),
        )
        .optional()
    })
    .ok()
    .flatten();
    raw.flatten().and_then(|s| serde_json::from_str(&s).ok())
}

pub fn save_internal_credential(account_id: &str, document: &Value) -> rusqlite::Result<()> {
    let payload = document.to_string();
    with(|c| {
        require_runtime_writer(c)?;
        let updated = c.execute(
            "UPDATE account_sources SET credential_json = ?1 WHERE account_id = ?2 AND credential_json IS NOT NULL",
            params![payload, account_id],
        )?;
        if updated == 0 {
            return Err(rusqlite::Error::QueryReturnedNoRows);
        }
        Ok(())
    })
}

pub fn try_acquire_refresh_lease(account_id: &str, lease_seconds: f64) -> Option<String> {
    let owner = uuid::Uuid::new_v4().simple().to_string();
    let now = now_f64();
    let slot = config::get().kiro_slot.clone();
    let rows = with(|c| {
        c.execute(
            "INSERT INTO credential_refresh_leases(account_id, owner, expires_at)
             SELECT ?1, ?2, ?3
             WHERE ?4 = '' OR EXISTS (SELECT 1 FROM runtime_writer WHERE id = 1 AND slot = ?4)
             ON CONFLICT(account_id) DO UPDATE SET owner=excluded.owner, expires_at=excluded.expires_at
             WHERE credential_refresh_leases.expires_at <= ?5",
            params![account_id, owner, now + lease_seconds, slot, now],
        )
    })
    .unwrap_or(0);
    (rows > 0).then_some(owner)
}

pub fn release_refresh_lease(account_id: &str, owner: &str) {
    let _ = with(|c| {
        c.execute(
            "DELETE FROM credential_refresh_leases WHERE account_id = ?1 AND owner = ?2",
            params![account_id, owner],
        )
    });
}

// ----- runtime state -----------------------------------------------------------------------

pub fn load_runtime_state() -> Option<Value> {
    let raw: Option<String> = with(|c| {
        c.query_row(
            "SELECT state_json FROM account_runtime WHERE id = 1",
            [],
            |r| r.get(0),
        )
        .optional()
    })
    .ok()
    .flatten();
    raw.and_then(|s| serde_json::from_str(&s).ok())
}

pub fn save_runtime_state_in(
    conn: &Connection,
    state: &Value,
    ungated: bool,
) -> rusqlite::Result<bool> {
    let slot = config::get().kiro_slot.clone();
    let written = conn.execute(
        "INSERT INTO account_runtime(id, state_json)
         SELECT 1, ?1
         WHERE ?2 OR ?3 = '' OR EXISTS (SELECT 1 FROM runtime_writer WHERE id = 1 AND slot = ?3)
         ON CONFLICT(id) DO UPDATE SET state_json=excluded.state_json",
        params![state.to_string(), ungated, slot],
    )?;
    Ok(written > 0)
}

pub fn save_runtime_state(state: &Value) -> bool {
    let state = state.clone();
    with(move |c| save_runtime_state_in(c, &state, false)).unwrap_or(false)
}

pub fn load_quota_headroom() -> HashMap<String, f64> {
    with(|c| {
        let mut stmt = c.prepare(
            "SELECT account_id, current_usage, usage_limit FROM account_usage
             WHERE error IS NULL AND current_usage IS NOT NULL AND usage_limit > 0",
        )?;
        let iter = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<f64>>(1)?,
                r.get::<_, Option<f64>>(2)?,
            ))
        })?;
        let mut out = HashMap::new();
        for (id, current, limit) in iter.flatten() {
            if let (Some(current), Some(limit)) = (current, limit) {
                if limit > 0.0 {
                    out.insert(id, (1.0 - current / limit).clamp(0.0, 1.0));
                }
            }
        }
        Ok(out)
    })
    .unwrap_or_default()
}

pub fn load_quota_period() -> HashMap<String, (Option<f64>, Option<bool>)> {
    with(|c| {
        let mut stmt =
            c.prepare("SELECT account_id, next_date_reset, overage_status FROM account_usage WHERE error IS NULL")?;
        let iter = stmt.query_map([], |r| {
            let reset: Option<rusqlite::types::Value> = r.get(1)?;
            Ok((r.get::<_, String>(0)?, reset, r.get::<_, Option<String>>(2)?))
        })?;
        let mut out = HashMap::new();
        for (id, reset, status) in iter.flatten() {
            let reset_at = match reset {
                Some(rusqlite::types::Value::Real(f)) => Some(f),
                Some(rusqlite::types::Value::Integer(i)) => Some(i as f64),
                Some(rusqlite::types::Value::Text(s)) => s.trim().parse::<f64>().ok(),
                _ => None,
            }
            .filter(|v| v.is_finite() && *v > 0.0);
            let overage = status.map(|s| s.trim().to_ascii_uppercase()).and_then(|s| match s.as_str() {
                "ENABLED" => Some(true),
                "DISABLED" => Some(false),
                _ => None,
            });
            if reset_at.is_none() && overage.is_none() {
                continue;
            }
            out.insert(id, (reset_at, overage));
        }
        Ok(out)
    })
    .unwrap_or_default()
}

pub fn now_f64() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

pub fn now_i64() -> i64 {
    now_f64() as i64
}
