-- kaish Kernel State Schema (SQLite)
--
-- Design principles:
-- - Incremental updates (change one var, not full snapshot)
-- - Efficient blob storage for large values
-- - Query without loading everything
-- - Simple migration path (schema versions)

PRAGMA journal_mode = WAL;           -- Crash recovery
PRAGMA foreign_keys = ON;            -- Referential integrity
PRAGMA user_version = 1;             -- Schema version

-- ============================================================
-- Metadata
-- ============================================================

CREATE TABLE IF NOT EXISTS meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

-- Initialize metadata
INSERT OR IGNORE INTO meta (key, value) VALUES
    ('schema_version', '1'),
    ('created_at', strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    ('session_id', lower(hex(randomblob(16))));

-- ============================================================
-- Variables
-- ============================================================

CREATE TABLE IF NOT EXISTS variables (
    name TEXT PRIMARY KEY,
    value_type TEXT NOT NULL,          -- 'null', 'bool', 'int', 'float', 'string', 'json', 'blob'
    value_small TEXT,                  -- For small values (< 1KB)
    value_blob BLOB,                   -- For large values
    updated_at TEXT DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);

CREATE INDEX IF NOT EXISTS idx_variables_updated ON variables(updated_at);

-- ============================================================
-- User-Defined Tools
-- ============================================================

CREATE TABLE IF NOT EXISTS tools (
    name TEXT PRIMARY KEY,
    source TEXT NOT NULL,              -- Original source code
    params_json TEXT NOT NULL,         -- JSON array of param definitions
    description TEXT,
    created_at TEXT DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at TEXT DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);

-- ============================================================
-- VFS Mount Configuration
-- ============================================================

CREATE TABLE IF NOT EXISTS mounts (
    path TEXT PRIMARY KEY,             -- Mount point, e.g., "/src"
    backend_type TEXT NOT NULL,        -- 'memory', 'local', 'mcp'
    config_json TEXT NOT NULL,         -- Backend-specific config
    read_only INTEGER DEFAULT 0,       -- 1 = reject write operations
    created_at TEXT DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);

-- ============================================================
-- MCP Server Configuration
-- ============================================================

CREATE TABLE IF NOT EXISTS mcp_servers (
    name TEXT PRIMARY KEY,             -- Local name, e.g., "exa"
    transport_type TEXT NOT NULL,      -- 'stdio', 'http', 'sse'
    config_json TEXT NOT NULL,         -- Transport-specific config
    enabled INTEGER DEFAULT 1,
    created_at TEXT DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);

-- ============================================================
-- Last Result ($?)
-- ============================================================

CREATE TABLE IF NOT EXISTS last_result (
    id INTEGER PRIMARY KEY CHECK (id = 1),  -- Single row
    code INTEGER NOT NULL DEFAULT 0,
    ok INTEGER NOT NULL DEFAULT 1,
    err TEXT,
    stdout TEXT,
    stderr TEXT,
    data_json TEXT,                    -- Parsed JSON if applicable
    updated_at TEXT DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);

INSERT OR IGNORE INTO last_result (id, code, ok) VALUES (1, 0, 1);

-- ============================================================
-- Current Working Directory
-- ============================================================

CREATE TABLE IF NOT EXISTS cwd (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    path TEXT NOT NULL DEFAULT '/'
);

INSERT OR IGNORE INTO cwd (id, path) VALUES (1, '/');

-- ============================================================
-- Views for Convenience
-- ============================================================

-- All state as JSON (for export/debugging)
CREATE VIEW IF NOT EXISTS state_export AS
SELECT json_object(
    'schema_version', (SELECT value FROM meta WHERE key = 'schema_version'),
    'session_id', (SELECT value FROM meta WHERE key = 'session_id'),
    'cwd', (SELECT path FROM cwd WHERE id = 1),
    'variables', (SELECT json_group_array(json_object('name', name, 'type', value_type, 'value', value_small)) FROM variables),
    'tools', (SELECT json_group_array(json_object('name', name, 'source', source, 'params', json(params_json))) FROM tools),
    'mounts', (SELECT json_group_array(json_object('path', path, 'backend', backend_type, 'config', json(config_json))) FROM mounts),
    'mcp_servers', (SELECT json_group_array(json_object('name', name, 'transport', transport_type, 'config', json(config_json))) FROM mcp_servers WHERE enabled = 1)
) AS state;
