//! State persistence for kaish kernels.
//!
//! Each kernel can persist its state to SQLite:
//! - Variables (scope)
//! - Mount configuration
//! - MCP server configuration
//! - Last result ($?)
//! - Current working directory
//!
//! State is stored at `$XDG_DATA_HOME/kaish/kernels/{id}.db`.

pub mod paths;

use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OpenFlags};

use crate::ast::Value;
use crate::interpreter::ExecResult;

/// Schema SQL embedded from schema/state.sql.
const SCHEMA_SQL: &str = include_str!("../../../../schema/state.sql");

/// Persistent state store backed by SQLite.
///
/// Provides incremental updates — change one variable without rewriting everything.
pub struct StateStore {
    conn: Connection,
}

impl StateStore {
    /// Open or create a state database at the given path.
    ///
    /// Creates parent directories and initializes schema if needed.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();

        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating state directory: {}", parent.display()))?;
        }

        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .with_context(|| format!("opening state database: {}", path.display()))?;

        let store = Self { conn };
        store.init_schema()?;
        Ok(store)
    }

    /// Create an in-memory state store (for testing or ephemeral kernels).
    pub fn in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()
            .context("creating in-memory state database")?;
        let store = Self { conn };
        store.init_schema()?;
        Ok(store)
    }

    /// Initialize the database schema.
    fn init_schema(&self) -> Result<()> {
        self.conn
            .execute_batch(SCHEMA_SQL)
            .context("initializing state schema")?;
        Ok(())
    }

    // ================================================================
    // Variables
    // ================================================================

    /// Save a variable to persistent storage.
    pub fn set_variable(&self, name: &str, value: &Value) -> Result<()> {
        let (value_type, value_small, value_blob) = serialize_value(value)?;

        self.conn.execute(
            "INSERT OR REPLACE INTO variables (name, value_type, value_small, value_blob, updated_at)
             VALUES (?1, ?2, ?3, ?4, strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))",
            params![name, value_type, value_small, value_blob],
        ).with_context(|| format!("saving variable: {}", name))?;

        Ok(())
    }

    /// Load a variable from persistent storage.
    pub fn get_variable(&self, name: &str) -> Result<Option<Value>> {
        let mut stmt = self.conn.prepare(
            "SELECT value_type, value_small, value_blob FROM variables WHERE name = ?1"
        )?;

        let result = stmt.query_row(params![name], |row| {
            let value_type: String = row.get(0)?;
            let value_small: Option<String> = row.get(1)?;
            let value_blob: Option<Vec<u8>> = row.get(2)?;
            Ok((value_type, value_small, value_blob))
        });

        match result {
            Ok((value_type, value_small, value_blob)) => {
                let value = deserialize_value(&value_type, value_small.as_deref(), value_blob.as_deref())?;
                Ok(Some(value))
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e).context(format!("loading variable: {}", name)),
        }
    }

    /// Delete a variable from persistent storage.
    pub fn delete_variable(&self, name: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM variables WHERE name = ?1",
            params![name],
        ).with_context(|| format!("deleting variable: {}", name))?;
        Ok(())
    }

    /// Delete all variables (for reset).
    pub fn delete_all_variables(&self) -> Result<()> {
        self.conn.execute("DELETE FROM variables", [])
            .context("deleting all variables")?;
        Ok(())
    }

    /// List all variable names.
    pub fn list_variables(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare("SELECT name FROM variables ORDER BY name")?;
        let names = stmt
            .query_map([], |row| row.get(0))?
            .collect::<std::result::Result<Vec<String>, _>>()?;
        Ok(names)
    }

    /// Load all variables as (name, value) pairs.
    pub fn load_all_variables(&self) -> Result<Vec<(String, Value)>> {
        let mut stmt = self.conn.prepare(
            "SELECT name, value_type, value_small, value_blob FROM variables ORDER BY name"
        )?;

        let results = stmt.query_map([], |row| {
            let name: String = row.get(0)?;
            let value_type: String = row.get(1)?;
            let value_small: Option<String> = row.get(2)?;
            let value_blob: Option<Vec<u8>> = row.get(3)?;
            Ok((name, value_type, value_small, value_blob))
        })?;

        let mut vars = Vec::new();
        for result in results {
            let (name, value_type, value_small, value_blob) = result?;
            let value = deserialize_value(&value_type, value_small.as_deref(), value_blob.as_deref())?;
            vars.push((name, value));
        }
        Ok(vars)
    }

    // ================================================================
    // Current Working Directory
    // ================================================================

    /// Get the persisted current working directory.
    pub fn get_cwd(&self) -> Result<String> {
        let cwd: String = self.conn.query_row(
            "SELECT path FROM cwd WHERE id = 1",
            [],
            |row| row.get(0),
        ).context("loading cwd")?;
        Ok(cwd)
    }

    /// Set the current working directory.
    pub fn set_cwd(&self, path: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE cwd SET path = ?1 WHERE id = 1",
            params![path],
        ).context("saving cwd")?;
        Ok(())
    }

    // ================================================================
    // Last Result ($?)
    // ================================================================

    /// Save the last command result.
    pub fn set_last_result(&self, result: &ExecResult) -> Result<()> {
        let data_json = result.data.as_ref().map(|v| {
            let json = value_to_json(v);
            serde_json::to_string(&json).unwrap_or_default()
        });

        self.conn.execute(
            "UPDATE last_result SET
                code = ?1,
                ok = ?2,
                err = ?3,
                stdout = ?4,
                stderr = ?5,
                data_json = ?6,
                updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
             WHERE id = 1",
            params![
                result.code,
                result.ok() as i32,
                if result.err.is_empty() { None } else { Some(&result.err) },
                &result.out,
                "", // stderr is in err field
                data_json,
            ],
        ).context("saving last result")?;
        Ok(())
    }

    /// Load the last command result.
    pub fn get_last_result(&self) -> Result<ExecResult> {
        let (code, stdout, err, data_json): (i64, String, Option<String>, Option<String>) =
            self.conn.query_row(
                "SELECT code, stdout, err, data_json FROM last_result WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            ).context("loading last result")?;

        let data = data_json.and_then(|s| {
            serde_json::from_str::<serde_json::Value>(&s)
                .ok()
                .map(|json| json_to_value(&json))
        });

        Ok(ExecResult {
            code,
            out: stdout,
            err: err.unwrap_or_default(),
            data,
        })
    }

    // ================================================================
    // Mount Configuration
    // ================================================================

    /// Save a mount configuration.
    pub fn set_mount(&self, path: &str, backend_type: &str, config: &serde_json::Value, read_only: bool) -> Result<()> {
        let config_json = serde_json::to_string(config)?;
        self.conn.execute(
            "INSERT OR REPLACE INTO mounts (path, backend_type, config_json, read_only, created_at)
             VALUES (?1, ?2, ?3, ?4, strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))",
            params![path, backend_type, config_json, read_only as i32],
        ).with_context(|| format!("saving mount: {}", path))?;
        Ok(())
    }

    /// Load a mount configuration.
    pub fn get_mount(&self, path: &str) -> Result<Option<MountConfig>> {
        let mut stmt = self.conn.prepare(
            "SELECT backend_type, config_json, read_only FROM mounts WHERE path = ?1"
        )?;

        let result = stmt.query_row(params![path], |row| {
            let backend_type: String = row.get(0)?;
            let config_json: String = row.get(1)?;
            let read_only: i32 = row.get(2)?;
            Ok((backend_type, config_json, read_only))
        });

        match result {
            Ok((backend_type, config_json, read_only)) => {
                let config = serde_json::from_str(&config_json)?;
                Ok(Some(MountConfig {
                    path: path.to_string(),
                    backend_type,
                    config,
                    read_only: read_only != 0,
                }))
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e).context(format!("loading mount: {}", path)),
        }
    }

    /// Delete a mount configuration.
    pub fn delete_mount(&self, path: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM mounts WHERE path = ?1",
            params![path],
        ).with_context(|| format!("deleting mount: {}", path))?;
        Ok(())
    }

    /// List all mount configurations.
    pub fn list_mounts(&self) -> Result<Vec<MountConfig>> {
        let mut stmt = self.conn.prepare(
            "SELECT path, backend_type, config_json, read_only FROM mounts ORDER BY path"
        )?;

        let results = stmt.query_map([], |row| {
            let path: String = row.get(0)?;
            let backend_type: String = row.get(1)?;
            let config_json: String = row.get(2)?;
            let read_only: i32 = row.get(3)?;
            Ok((path, backend_type, config_json, read_only))
        })?;

        let mut mounts = Vec::new();
        for result in results {
            let (path, backend_type, config_json, read_only) = result?;
            let config = serde_json::from_str(&config_json)?;
            mounts.push(MountConfig {
                path,
                backend_type,
                config,
                read_only: read_only != 0,
            });
        }
        Ok(mounts)
    }

    // ================================================================
    // MCP Server Configuration
    // ================================================================

    /// Save an MCP server configuration.
    pub fn set_mcp_server(&self, name: &str, transport_type: &str, config: &serde_json::Value, enabled: bool) -> Result<()> {
        let config_json = serde_json::to_string(config)?;
        self.conn.execute(
            "INSERT OR REPLACE INTO mcp_servers (name, transport_type, config_json, enabled, created_at)
             VALUES (?1, ?2, ?3, ?4, strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))",
            params![name, transport_type, config_json, enabled as i32],
        ).with_context(|| format!("saving MCP server: {}", name))?;
        Ok(())
    }

    /// Load an MCP server configuration.
    pub fn get_mcp_server(&self, name: &str) -> Result<Option<McpServerConfig>> {
        let mut stmt = self.conn.prepare(
            "SELECT transport_type, config_json, enabled FROM mcp_servers WHERE name = ?1"
        )?;

        let result = stmt.query_row(params![name], |row| {
            let transport_type: String = row.get(0)?;
            let config_json: String = row.get(1)?;
            let enabled: i32 = row.get(2)?;
            Ok((transport_type, config_json, enabled))
        });

        match result {
            Ok((transport_type, config_json, enabled)) => {
                let config = serde_json::from_str(&config_json)?;
                Ok(Some(McpServerConfig {
                    name: name.to_string(),
                    transport_type,
                    config,
                    enabled: enabled != 0,
                }))
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e).context(format!("loading MCP server: {}", name)),
        }
    }

    /// Delete an MCP server configuration.
    pub fn delete_mcp_server(&self, name: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM mcp_servers WHERE name = ?1",
            params![name],
        ).with_context(|| format!("deleting MCP server: {}", name))?;
        Ok(())
    }

    /// List all MCP server configurations.
    pub fn list_mcp_servers(&self, enabled_only: bool) -> Result<Vec<McpServerConfig>> {
        let sql = if enabled_only {
            "SELECT name, transport_type, config_json, enabled FROM mcp_servers WHERE enabled = 1 ORDER BY name"
        } else {
            "SELECT name, transport_type, config_json, enabled FROM mcp_servers ORDER BY name"
        };

        let mut stmt = self.conn.prepare(sql)?;

        let results = stmt.query_map([], |row| {
            let name: String = row.get(0)?;
            let transport_type: String = row.get(1)?;
            let config_json: String = row.get(2)?;
            let enabled: i32 = row.get(3)?;
            Ok((name, transport_type, config_json, enabled))
        })?;

        let mut servers = Vec::new();
        for result in results {
            let (name, transport_type, config_json, enabled) = result?;
            let config = serde_json::from_str(&config_json)?;
            servers.push(McpServerConfig {
                name,
                transport_type,
                config,
                enabled: enabled != 0,
            });
        }
        Ok(servers)
    }

    // ================================================================
    // Metadata
    // ================================================================

    /// Get a metadata value.
    pub fn get_meta(&self, key: &str) -> Result<Option<String>> {
        let result = self.conn.query_row(
            "SELECT value FROM meta WHERE key = ?1",
            params![key],
            |row| row.get(0),
        );

        match result {
            Ok(value) => Ok(Some(value)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e).context(format!("loading meta: {}", key)),
        }
    }

    /// Set a metadata value.
    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
            params![key, value],
        ).with_context(|| format!("saving meta: {}", key))?;
        Ok(())
    }

    /// Get the session ID.
    pub fn session_id(&self) -> Result<String> {
        self.get_meta("session_id")
            .map(|opt| opt.unwrap_or_else(|| "unknown".to_string()))
    }

    // ================================================================
    // Export / Import
    // ================================================================

    /// Export full state as JSON using the state_export view.
    pub fn export_json(&self) -> Result<String> {
        let json: String = self.conn.query_row(
            "SELECT state FROM state_export",
            [],
            |row| row.get(0),
        ).context("exporting state")?;
        Ok(json)
    }

    // ================================================================
    // History
    // ================================================================

    /// Record an execution in history.
    pub fn record_history(&self, entry: &HistoryEntry) -> Result<i64> {
        let data_json = entry.result_data.as_ref().map(|v| {
            let json = value_to_json(v);
            serde_json::to_string(&json).unwrap_or_default()
        });

        self.conn.execute(
            "INSERT INTO history (code, code_hash, result_code, result_ok, result_out, result_err, result_data_json, duration_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                entry.code,
                entry.code_hash,
                entry.result_code,
                entry.result_ok as i32,
                entry.result_out,
                entry.result_err,
                data_json,
                entry.duration_ms,
            ],
        ).context("recording history")?;

        Ok(self.conn.last_insert_rowid())
    }

    /// Get recent history entries.
    pub fn get_history(&self, limit: usize) -> Result<Vec<HistoryEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, code, code_hash, result_code, result_ok, result_out, result_err, result_data_json, duration_ms, created_at
             FROM history ORDER BY id DESC LIMIT ?1"
        )?;

        let results = stmt.query_map(params![limit as i64], |row| {
            Ok(HistoryRow {
                id: row.get(0)?,
                code: row.get(1)?,
                code_hash: row.get(2)?,
                result_code: row.get(3)?,
                result_ok: row.get(4)?,
                result_out: row.get(5)?,
                result_err: row.get(6)?,
                result_data_json: row.get(7)?,
                duration_ms: row.get(8)?,
                created_at: row.get(9)?,
            })
        })?;

        let mut entries = Vec::new();
        for result in results {
            let row = result?;
            let result_data = row.result_data_json.and_then(|s| {
                serde_json::from_str::<serde_json::Value>(&s)
                    .ok()
                    .map(|json| json_to_value(&json))
            });

            entries.push(HistoryEntry {
                id: Some(row.id),
                code: row.code,
                code_hash: row.code_hash,
                result_code: row.result_code,
                result_ok: row.result_ok != 0,
                result_out: row.result_out,
                result_err: row.result_err,
                result_data,
                duration_ms: row.duration_ms,
                created_at: row.created_at,
            });
        }

        // Reverse to get chronological order
        entries.reverse();
        Ok(entries)
    }

    /// Get history count.
    pub fn history_count(&self) -> Result<i64> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM history",
            [],
            |row| row.get(0),
        )?;
        Ok(count)
    }

    /// Get the latest history ID.
    pub fn latest_history_id(&self) -> Result<Option<i64>> {
        let result = self.conn.query_row(
            "SELECT MAX(id) FROM history",
            [],
            |row| row.get::<_, Option<i64>>(0),
        )?;
        Ok(result)
    }

    // ================================================================
    // Checkpoints
    // ================================================================

    /// Create a checkpoint that covers history up to the given ID.
    pub fn create_checkpoint(&self, checkpoint: &Checkpoint) -> Result<i64> {
        let variables_snapshot = checkpoint.variables_snapshot.as_ref().map(|v| {
            serde_json::to_string(v).unwrap_or_default()
        });

        let metadata_json = checkpoint.metadata.as_ref().map(|v| {
            serde_json::to_string(v).unwrap_or_default()
        });

        self.conn.execute(
            "INSERT INTO checkpoints (name, summary, up_to_history_id, variables_snapshot, metadata_json)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                checkpoint.name,
                checkpoint.summary,
                checkpoint.up_to_history_id,
                variables_snapshot,
                metadata_json,
            ],
        ).context("creating checkpoint")?;

        Ok(self.conn.last_insert_rowid())
    }

    /// Get the latest checkpoint.
    pub fn latest_checkpoint(&self) -> Result<Option<Checkpoint>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, summary, up_to_history_id, variables_snapshot, metadata_json, created_at
             FROM checkpoints ORDER BY id DESC LIMIT 1"
        )?;

        let result = stmt.query_row([], |row| {
            Ok(CheckpointRow {
                id: row.get(0)?,
                name: row.get(1)?,
                summary: row.get(2)?,
                up_to_history_id: row.get(3)?,
                variables_snapshot: row.get(4)?,
                metadata_json: row.get(5)?,
                created_at: row.get(6)?,
            })
        });

        match result {
            Ok(row) => Ok(Some(Checkpoint {
                id: Some(row.id),
                name: row.name,
                summary: row.summary,
                up_to_history_id: row.up_to_history_id,
                variables_snapshot: row.variables_snapshot.and_then(|s| serde_json::from_str(&s).ok()),
                metadata: row.metadata_json.and_then(|s| serde_json::from_str(&s).ok()),
                created_at: row.created_at,
            })),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e).context("loading latest checkpoint"),
        }
    }

    /// List all checkpoints.
    pub fn list_checkpoints(&self) -> Result<Vec<Checkpoint>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, summary, up_to_history_id, variables_snapshot, metadata_json, created_at
             FROM checkpoints ORDER BY id ASC"
        )?;

        let results = stmt.query_map([], |row| {
            Ok(CheckpointRow {
                id: row.get(0)?,
                name: row.get(1)?,
                summary: row.get(2)?,
                up_to_history_id: row.get(3)?,
                variables_snapshot: row.get(4)?,
                metadata_json: row.get(5)?,
                created_at: row.get(6)?,
            })
        })?;

        let mut checkpoints = Vec::new();
        for result in results {
            let row = result?;
            checkpoints.push(Checkpoint {
                id: Some(row.id),
                name: row.name,
                summary: row.summary,
                up_to_history_id: row.up_to_history_id,
                variables_snapshot: row.variables_snapshot.and_then(|s| serde_json::from_str(&s).ok()),
                metadata: row.metadata_json.and_then(|s| serde_json::from_str(&s).ok()),
                created_at: row.created_at,
            });
        }

        Ok(checkpoints)
    }

    /// Get history entries since the last checkpoint.
    pub fn history_since_checkpoint(&self) -> Result<Vec<HistoryEntry>> {
        let last_checkpoint_id = self.latest_checkpoint()?
            .and_then(|c| c.up_to_history_id)
            .unwrap_or(0);

        let mut stmt = self.conn.prepare(
            "SELECT id, code, code_hash, result_code, result_ok, result_out, result_err, result_data_json, duration_ms, created_at
             FROM history WHERE id > ?1 ORDER BY id ASC"
        )?;

        let results = stmt.query_map(params![last_checkpoint_id], |row| {
            Ok(HistoryRow {
                id: row.get(0)?,
                code: row.get(1)?,
                code_hash: row.get(2)?,
                result_code: row.get(3)?,
                result_ok: row.get(4)?,
                result_out: row.get(5)?,
                result_err: row.get(6)?,
                result_data_json: row.get(7)?,
                duration_ms: row.get(8)?,
                created_at: row.get(9)?,
            })
        })?;

        let mut entries = Vec::new();
        for result in results {
            let row = result?;
            let result_data = row.result_data_json.and_then(|s| {
                serde_json::from_str::<serde_json::Value>(&s)
                    .ok()
                    .map(|json| json_to_value(&json))
            });

            entries.push(HistoryEntry {
                id: Some(row.id),
                code: row.code,
                code_hash: row.code_hash,
                result_code: row.result_code,
                result_ok: row.result_ok != 0,
                result_out: row.result_out,
                result_err: row.result_err,
                result_data,
                duration_ms: row.duration_ms,
                created_at: row.created_at,
            });
        }

        Ok(entries)
    }
}

// ================================================================
// Config Types
// ================================================================

/// Mount configuration.
#[derive(Debug, Clone)]
pub struct MountConfig {
    pub path: String,
    pub backend_type: String,
    pub config: serde_json::Value,
    pub read_only: bool,
}

/// MCP server configuration.
#[derive(Debug, Clone)]
pub struct McpServerConfig {
    pub name: String,
    pub transport_type: String,
    pub config: serde_json::Value,
    pub enabled: bool,
}

// ================================================================
// History Types
// ================================================================

/// A history entry representing one execution.
#[derive(Debug, Clone)]
pub struct HistoryEntry {
    pub id: Option<i64>,
    pub code: String,
    pub code_hash: Option<String>,
    pub result_code: i64,
    pub result_ok: bool,
    pub result_out: Option<String>,
    pub result_err: Option<String>,
    pub result_data: Option<Value>,
    pub duration_ms: Option<i64>,
    pub created_at: Option<String>,
}

impl HistoryEntry {
    /// Create a new history entry from an execution.
    pub fn from_exec(code: &str, result: &ExecResult, duration_ms: Option<i64>) -> Self {
        Self {
            id: None,
            code: code.to_string(),
            code_hash: None, // Could compute SHA256 here
            result_code: result.code,
            result_ok: result.ok(),
            result_out: if result.out.is_empty() { None } else { Some(result.out.clone()) },
            result_err: if result.err.is_empty() { None } else { Some(result.err.clone()) },
            result_data: result.data.clone(),
            duration_ms,
            created_at: None,
        }
    }
}

/// Internal row type for history queries.
struct HistoryRow {
    id: i64,
    code: String,
    code_hash: Option<String>,
    result_code: i64,
    result_ok: i32,
    result_out: Option<String>,
    result_err: Option<String>,
    result_data_json: Option<String>,
    duration_ms: Option<i64>,
    created_at: Option<String>,
}

// ================================================================
// Checkpoint Types
// ================================================================

/// A checkpoint that distills history into a summary.
#[derive(Debug, Clone)]
pub struct Checkpoint {
    pub id: Option<i64>,
    pub name: Option<String>,
    pub summary: String,
    pub up_to_history_id: Option<i64>,
    pub variables_snapshot: Option<serde_json::Value>,
    pub metadata: Option<serde_json::Value>,
    pub created_at: Option<String>,
}

impl Checkpoint {
    /// Create a new checkpoint.
    pub fn new(summary: impl Into<String>, up_to_history_id: Option<i64>) -> Self {
        Self {
            id: None,
            name: None,
            summary: summary.into(),
            up_to_history_id,
            variables_snapshot: None,
            metadata: None,
            created_at: None,
        }
    }

    /// Add a name to the checkpoint.
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Add a variables snapshot.
    pub fn with_variables(mut self, vars: serde_json::Value) -> Self {
        self.variables_snapshot = Some(vars);
        self
    }

    /// Add metadata.
    pub fn with_metadata(mut self, meta: serde_json::Value) -> Self {
        self.metadata = Some(meta);
        self
    }
}

/// Internal row type for checkpoint queries.
struct CheckpointRow {
    id: i64,
    name: Option<String>,
    summary: String,
    up_to_history_id: Option<i64>,
    variables_snapshot: Option<String>,
    metadata_json: Option<String>,
    created_at: Option<String>,
}

// ================================================================
// Value Serialization
// ================================================================

/// Serialize a Value for SQLite storage.
///
/// Returns (type_name, small_value, blob_value).
/// Values under 1KB go in small_value, larger ones in blob_value.
fn serialize_value(value: &Value) -> Result<(String, Option<String>, Option<Vec<u8>>)> {
    let (type_name, serialized) = match value {
        Value::Null => ("null", "null".to_string()),
        Value::Bool(b) => ("bool", b.to_string()),
        Value::Int(i) => ("int", i.to_string()),
        Value::Float(f) => ("float", f.to_string()),
        Value::String(s) => ("string", s.clone()),
    };

    // Split at 1KB threshold
    if serialized.len() < 1024 {
        Ok((type_name.to_string(), Some(serialized), None))
    } else {
        Ok((type_name.to_string(), None, Some(serialized.into_bytes())))
    }
}

/// Deserialize a Value from SQLite storage.
fn deserialize_value(type_name: &str, small: Option<&str>, blob: Option<&[u8]>) -> Result<Value> {
    let data = small
        .map(|s| s.to_string())
        .or_else(|| blob.map(|b| String::from_utf8_lossy(b).to_string()))
        .unwrap_or_default();

    let value = match type_name {
        "null" => Value::Null,
        "bool" => Value::Bool(data.parse().unwrap_or(false)),
        "int" => Value::Int(data.parse().unwrap_or(0)),
        "float" => Value::Float(data.parse().unwrap_or(0.0)),
        "string" => Value::String(data),
        "json" => {
            let json: serde_json::Value = serde_json::from_str(&data)?;
            json_to_value(&json)
        }
        _ => Value::String(data), // Fallback
    };

    Ok(value)
}

/// Convert a kaish Value to serde_json::Value.
fn value_to_json(value: &Value) -> serde_json::Value {
    match value {
        Value::Null => serde_json::Value::Null,
        Value::Bool(b) => serde_json::Value::Bool(*b),
        Value::Int(i) => serde_json::Value::Number((*i).into()),
        Value::Float(f) => {
            serde_json::Number::from_f64(*f)
                .map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null)
        }
        Value::String(s) => serde_json::Value::String(s.clone()),
    }
}

/// Convert serde_json::Value to a kaish Value.
///
/// Arrays and objects are stored as JSON strings.
fn json_to_value(json: &serde_json::Value) -> Value {
    match json {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Bool(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Int(i)
            } else if let Some(f) = n.as_f64() {
                Value::Float(f)
            } else {
                Value::Int(0)
            }
        }
        serde_json::Value::String(s) => Value::String(s.clone()),
        // Arrays and objects are stored as JSON strings
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => {
            Value::String(json.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_open_in_memory() {
        let store = StateStore::in_memory().expect("should create in-memory store");
        let cwd = store.get_cwd().expect("should get cwd");
        assert_eq!(cwd, "/");
    }

    #[test]
    fn test_set_get_variable_string() {
        let store = StateStore::in_memory().expect("store");
        store.set_variable("NAME", &Value::String("Alice".into())).expect("set");
        let value = store.get_variable("NAME").expect("get").expect("exists");
        assert_eq!(value, Value::String("Alice".into()));
    }

    #[test]
    fn test_set_get_variable_int() {
        let store = StateStore::in_memory().expect("store");
        store.set_variable("COUNT", &Value::Int(42)).expect("set");
        let value = store.get_variable("COUNT").expect("get").expect("exists");
        assert_eq!(value, Value::Int(42));
    }

    #[test]
    fn test_set_get_variable_bool() {
        let store = StateStore::in_memory().expect("store");
        store.set_variable("FLAG", &Value::Bool(true)).expect("set");
        let value = store.get_variable("FLAG").expect("get").expect("exists");
        assert_eq!(value, Value::Bool(true));
    }

    #[test]
    fn test_set_get_variable_null() {
        let store = StateStore::in_memory().expect("store");
        store.set_variable("EMPTY", &Value::Null).expect("set");
        let value = store.get_variable("EMPTY").expect("get").expect("exists");
        assert_eq!(value, Value::Null);
    }

    #[test]
    fn test_get_nonexistent_variable() {
        let store = StateStore::in_memory().expect("store");
        let value = store.get_variable("MISSING").expect("get");
        assert!(value.is_none());
    }

    #[test]
    fn test_delete_variable() {
        let store = StateStore::in_memory().expect("store");
        store.set_variable("X", &Value::Int(1)).expect("set");
        store.delete_variable("X").expect("delete");
        let value = store.get_variable("X").expect("get");
        assert!(value.is_none());
    }

    #[test]
    fn test_list_variables() {
        let store = StateStore::in_memory().expect("store");
        store.set_variable("B", &Value::Int(2)).expect("set");
        store.set_variable("A", &Value::Int(1)).expect("set");
        store.set_variable("C", &Value::Int(3)).expect("set");

        let names = store.list_variables().expect("list");
        assert_eq!(names, vec!["A", "B", "C"]);
    }

    #[test]
    fn test_load_all_variables() {
        let store = StateStore::in_memory().expect("store");
        store.set_variable("X", &Value::Int(1)).expect("set");
        store.set_variable("Y", &Value::String("two".into())).expect("set");

        let vars = store.load_all_variables().expect("load");
        assert_eq!(vars.len(), 2);
        assert_eq!(vars[0], ("X".to_string(), Value::Int(1)));
        assert_eq!(vars[1], ("Y".to_string(), Value::String("two".into())));
    }

    #[test]
    fn test_cwd() {
        let store = StateStore::in_memory().expect("store");
        assert_eq!(store.get_cwd().expect("get"), "/");

        store.set_cwd("/home/user").expect("set");
        assert_eq!(store.get_cwd().expect("get"), "/home/user");
    }

    #[test]
    fn test_last_result() {
        let store = StateStore::in_memory().expect("store");

        let result = ExecResult::failure(1, "error message");
        store.set_last_result(&result).expect("set");

        let loaded = store.get_last_result().expect("get");
        assert_eq!(loaded.code, 1);
        assert_eq!(loaded.err, "error message");
    }

    #[test]
    fn test_mount_config() {
        let store = StateStore::in_memory().expect("store");

        let config = serde_json::json!({"root": "/home/user"});
        store.set_mount("/src", "local", &config, false).expect("set");

        let mount = store.get_mount("/src").expect("get").expect("exists");
        assert_eq!(mount.path, "/src");
        assert_eq!(mount.backend_type, "local");
        assert!(!mount.read_only);
    }

    #[test]
    fn test_list_mounts() {
        let store = StateStore::in_memory().expect("store");

        store.set_mount("/a", "memory", &serde_json::json!({}), false).expect("set");
        store.set_mount("/b", "local", &serde_json::json!({}), true).expect("set");

        let mounts = store.list_mounts().expect("list");
        assert_eq!(mounts.len(), 2);
        assert_eq!(mounts[0].path, "/a");
        assert_eq!(mounts[1].path, "/b");
    }

    #[test]
    fn test_mcp_server_config() {
        let store = StateStore::in_memory().expect("store");

        let config = serde_json::json!({"command": "npx", "args": ["-y", "@anthropic/mcp-server"]});
        store.set_mcp_server("claude", "stdio", &config, true).expect("set");

        let server = store.get_mcp_server("claude").expect("get").expect("exists");
        assert_eq!(server.name, "claude");
        assert_eq!(server.transport_type, "stdio");
        assert!(server.enabled);
    }

    #[test]
    fn test_list_mcp_servers_enabled_only() {
        let store = StateStore::in_memory().expect("store");

        store.set_mcp_server("a", "stdio", &serde_json::json!({}), true).expect("set");
        store.set_mcp_server("b", "stdio", &serde_json::json!({}), false).expect("set");

        let all = store.list_mcp_servers(false).expect("list all");
        assert_eq!(all.len(), 2);

        let enabled = store.list_mcp_servers(true).expect("list enabled");
        assert_eq!(enabled.len(), 1);
        assert_eq!(enabled[0].name, "a");
    }

    #[test]
    fn test_metadata() {
        let store = StateStore::in_memory().expect("store");

        let session_id = store.session_id().expect("session_id");
        assert!(!session_id.is_empty());

        store.set_meta("custom", "value").expect("set");
        let custom = store.get_meta("custom").expect("get").expect("exists");
        assert_eq!(custom, "value");
    }

    #[test]
    fn test_export_json() {
        let store = StateStore::in_memory().expect("store");
        store.set_variable("TEST", &Value::String("hello".into())).expect("set");

        let json = store.export_json().expect("export");
        assert!(json.contains("TEST"));
        assert!(json.contains("hello"));
    }

    #[test]
    fn test_large_value_goes_to_blob() {
        let store = StateStore::in_memory().expect("store");

        // Create a value larger than 1KB
        let large = "x".repeat(2000);
        store.set_variable("LARGE", &Value::String(large.clone())).expect("set");

        let value = store.get_variable("LARGE").expect("get").expect("exists");
        assert_eq!(value, Value::String(large));
    }

    #[test]
    fn test_variable_replace() {
        let store = StateStore::in_memory().expect("store");

        store.set_variable("X", &Value::Int(1)).expect("set");
        store.set_variable("X", &Value::Int(2)).expect("replace");

        let value = store.get_variable("X").expect("get").expect("exists");
        assert_eq!(value, Value::Int(2));
    }

    #[test]
    fn test_json_array_string_roundtrip() {
        let store = StateStore::in_memory().expect("store");

        // Arrays are now stored as JSON strings
        let array = Value::String(r#"[1,"two",true]"#.into());

        store.set_variable("ARR", &array).expect("set");
        let loaded = store.get_variable("ARR").expect("get").expect("exists");

        // Verify it's a string containing the JSON array
        if let Value::String(s) = loaded {
            assert!(s.contains("1"));
            assert!(s.contains("two"));
            assert!(s.contains("true"));
        } else {
            panic!("expected string, got {:?}", loaded);
        }
    }

    #[test]
    fn test_json_object_string_roundtrip() {
        let store = StateStore::in_memory().expect("store");

        // Objects are now stored as JSON strings
        let obj = Value::String(r#"{"name":"Alice","age":30,"active":true}"#.into());

        store.set_variable("USER", &obj).expect("set");
        let loaded = store.get_variable("USER").expect("get").expect("exists");

        // Verify it's a string containing the JSON object
        if let Value::String(s) = loaded {
            assert!(s.contains("Alice"));
            assert!(s.contains("30"));
            assert!(s.contains("active"));
        } else {
            panic!("expected string, got {:?}", loaded);
        }
    }

    #[test]
    fn test_nested_json_string_roundtrip() {
        let store = StateStore::in_memory().expect("store");

        // Nested structure: { "users": [{ "name": "Bob" }] } as JSON string
        let nested = Value::String(r#"{"users":[{"name":"Bob"}]}"#.into());

        store.set_variable("DATA", &nested).expect("set");
        let loaded = store.get_variable("DATA").expect("get").expect("exists");

        // Just verify it roundtrips without panic
        if let Value::String(s) = loaded {
            assert!(s.contains("users"));
            assert!(s.contains("Bob"));
        } else {
            panic!("expected string");
        }
    }

    #[test]
    fn test_float_value() {
        let store = StateStore::in_memory().expect("store");

        store.set_variable("PI", &Value::Float(3.14159)).expect("set");
        let value = store.get_variable("PI").expect("get").expect("exists");

        if let Value::Float(f) = value {
            assert!((f - 3.14159).abs() < 0.0001);
        } else {
            panic!("expected float, got {:?}", value);
        }
    }

    #[test]
    fn test_last_result_with_data() {
        let store = StateStore::in_memory().expect("store");

        // Data is now a JSON string
        let data = Value::String(r#"{"count":42}"#.into());

        let result = ExecResult {
            code: 0,
            out: "success".to_string(),
            err: String::new(),
            data: Some(data),
        };

        store.set_last_result(&result).expect("set");
        let loaded = store.get_last_result().expect("get");

        assert_eq!(loaded.code, 0);
        assert_eq!(loaded.out, "success");
        assert!(loaded.data.is_some());
    }

    // ================================================================
    // History Tests
    // ================================================================

    #[test]
    fn test_record_and_get_history() {
        let store = StateStore::in_memory().expect("store");

        let entry = HistoryEntry {
            id: None,
            code: "echo hello".to_string(),
            code_hash: None,
            result_code: 0,
            result_ok: true,
            result_out: Some("hello".to_string()),
            result_err: None,
            result_data: None,
            duration_ms: Some(5),
            created_at: None,
        };

        let id = store.record_history(&entry).expect("record");
        assert!(id > 0);

        let history = store.get_history(10).expect("get");
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].code, "echo hello");
        assert_eq!(history[0].result_code, 0);
    }

    #[test]
    fn test_history_from_exec() {
        let store = StateStore::in_memory().expect("store");

        let result = ExecResult::success("output");
        let entry = HistoryEntry::from_exec("ls -la", &result, Some(10));

        store.record_history(&entry).expect("record");

        let history = store.get_history(10).expect("get");
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].code, "ls -la");
        assert!(history[0].result_ok);
    }

    #[test]
    fn test_history_count() {
        let store = StateStore::in_memory().expect("store");

        assert_eq!(store.history_count().expect("count"), 0);

        let entry = HistoryEntry::from_exec("cmd1", &ExecResult::success(""), None);
        store.record_history(&entry).expect("record");

        let entry = HistoryEntry::from_exec("cmd2", &ExecResult::success(""), None);
        store.record_history(&entry).expect("record");

        assert_eq!(store.history_count().expect("count"), 2);
    }

    #[test]
    fn test_latest_history_id() {
        let store = StateStore::in_memory().expect("store");

        assert_eq!(store.latest_history_id().expect("id"), None);

        let entry = HistoryEntry::from_exec("cmd1", &ExecResult::success(""), None);
        let id1 = store.record_history(&entry).expect("record");

        let entry = HistoryEntry::from_exec("cmd2", &ExecResult::success(""), None);
        let id2 = store.record_history(&entry).expect("record");

        assert_eq!(store.latest_history_id().expect("id"), Some(id2));
        assert!(id2 > id1);
    }

    // ================================================================
    // Checkpoint Tests
    // ================================================================

    #[test]
    fn test_create_and_get_checkpoint() {
        let store = StateStore::in_memory().expect("store");

        // Record some history first
        let entry = HistoryEntry::from_exec("cmd1", &ExecResult::success(""), None);
        let history_id = store.record_history(&entry).expect("record");

        // Create checkpoint
        let checkpoint = Checkpoint::new("Summary of session", Some(history_id))
            .with_name("session-1");

        let id = store.create_checkpoint(&checkpoint).expect("create");
        assert!(id > 0);

        let loaded = store.latest_checkpoint().expect("get").expect("exists");
        assert_eq!(loaded.name, Some("session-1".to_string()));
        assert_eq!(loaded.summary, "Summary of session");
        assert_eq!(loaded.up_to_history_id, Some(history_id));
    }

    #[test]
    fn test_list_checkpoints() {
        let store = StateStore::in_memory().expect("store");

        let c1 = Checkpoint::new("First checkpoint", None);
        store.create_checkpoint(&c1).expect("create");

        let c2 = Checkpoint::new("Second checkpoint", None);
        store.create_checkpoint(&c2).expect("create");

        let checkpoints = store.list_checkpoints().expect("list");
        assert_eq!(checkpoints.len(), 2);
        assert_eq!(checkpoints[0].summary, "First checkpoint");
        assert_eq!(checkpoints[1].summary, "Second checkpoint");
    }

    #[test]
    fn test_checkpoint_with_metadata() {
        let store = StateStore::in_memory().expect("store");

        let checkpoint = Checkpoint::new("Test", None)
            .with_metadata(serde_json::json!({
                "model": "claude-3",
                "token_count": 1500
            }));

        store.create_checkpoint(&checkpoint).expect("create");

        let loaded = store.latest_checkpoint().expect("get").expect("exists");
        assert!(loaded.metadata.is_some());
        let meta = loaded.metadata.expect("metadata");
        assert_eq!(meta["model"], "claude-3");
    }

    #[test]
    fn test_history_since_checkpoint() {
        let store = StateStore::in_memory().expect("store");

        // Record some history
        let e1 = HistoryEntry::from_exec("cmd1", &ExecResult::success(""), None);
        let id1 = store.record_history(&e1).expect("record");

        let e2 = HistoryEntry::from_exec("cmd2", &ExecResult::success(""), None);
        store.record_history(&e2).expect("record");

        // Create checkpoint covering up to id1
        let checkpoint = Checkpoint::new("Checkpoint 1", Some(id1));
        store.create_checkpoint(&checkpoint).expect("create");

        // Record more history after checkpoint
        let e3 = HistoryEntry::from_exec("cmd3", &ExecResult::success(""), None);
        store.record_history(&e3).expect("record");

        let e4 = HistoryEntry::from_exec("cmd4", &ExecResult::success(""), None);
        store.record_history(&e4).expect("record");

        // Get history since checkpoint
        let since = store.history_since_checkpoint().expect("since");
        assert_eq!(since.len(), 3); // cmd2, cmd3, cmd4 (cmd1 is covered by checkpoint)

        // Verify we got the right entries
        let codes: Vec<&str> = since.iter().map(|e| e.code.as_str()).collect();
        assert!(codes.contains(&"cmd2"));
        assert!(codes.contains(&"cmd3"));
        assert!(codes.contains(&"cmd4"));
    }

    #[test]
    fn test_history_since_checkpoint_no_checkpoint() {
        let store = StateStore::in_memory().expect("store");

        // Record history with no checkpoints
        let e1 = HistoryEntry::from_exec("cmd1", &ExecResult::success(""), None);
        store.record_history(&e1).expect("record");

        let e2 = HistoryEntry::from_exec("cmd2", &ExecResult::success(""), None);
        store.record_history(&e2).expect("record");

        // Should return all history
        let since = store.history_since_checkpoint().expect("since");
        assert_eq!(since.len(), 2);
    }
}
