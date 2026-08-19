//! PRAGMA setup applied on every connection (writer and readers).

pub const PRAGMAS: &[&str] = &[
    "PRAGMA journal_mode=WAL;",
    "PRAGMA foreign_keys=ON;",
    "PRAGMA busy_timeout=5000;",
    "PRAGMA synchronous=NORMAL;",
];

pub const LEDGER_SCHEMA_VERSION: i64 = 10;
pub const PROJECTION_VERSION: i64 = 1;
