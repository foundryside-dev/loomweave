//! Versioned read-only `SQLite` boundary for federation consumers.
//!
//! Internal migration acceptance and external compatibility are deliberately
//! different contracts. Writers may migrate any historical Loomweave database;
//! an external consumer may issue catalogue SQL only after this module confirms
//! the frozen v1 projection is present.

use std::collections::BTreeSet;

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use crate::Result;
use crate::pragma::LOOMWEAVE_APPLICATION_ID;

/// Versioned name of the external read-only `SQLite` projection.
pub const EXTERNAL_SQLITE_SCHEMA: &str = "loomweave.external-sqlite.v1";

/// Oldest on-disk schema that contains the full v1 federation projection.
pub const EXTERNAL_READ_MIN_USER_VERSION: u32 = 5;

/// Newest reviewed on-disk schema for the v1 federation projection.
///
/// This is intentionally not an alias of the internal current schema version:
/// a new migration is not externally compatible until its safe projection has
/// been reviewed and this constant is deliberately advanced.
pub const EXTERNAL_READ_MAX_USER_VERSION: u32 = 12;

const _: () = assert!(EXTERNAL_READ_MAX_USER_VERSION <= crate::schema::CURRENT_SCHEMA_VERSION);

/// Exact table/column projection supported for external read-only consumers.
/// Consumers must name columns explicitly and must not rely on unlisted schema.
pub const EXTERNAL_SQLITE_REQUIRED_SURFACE: &[(&str, &[&str])] = &[
    (
        "runs",
        &["id", "started_at", "completed_at", "stats", "status"],
    ),
    (
        "entities",
        &[
            "id",
            "plugin_id",
            "kind",
            "source_file_path",
            "source_byte_start",
            "source_byte_end",
            "source_line_start",
            "source_line_end",
            "properties",
            "content_hash",
        ],
    ),
    ("entity_tags", &["entity_id", "plugin_id", "tag"]),
    (
        "sei_bindings",
        &["sei", "current_locator", "body_hash", "status"],
    ),
    (
        "sei_lineage",
        &[
            "sei",
            "event",
            "old_locator",
            "new_locator",
            "run_id",
            "recorded_at",
        ],
    ),
];

/// Typed compatibility class. Callers may branch without parsing diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalSqliteCompatibilityStatus {
    /// The database is at the newest reviewed external schema version.
    Compatible,
    /// The database is older but still contains the complete v1 projection.
    OlderSupported,
    /// The header or required projection is outside the supported contract.
    Incompatible,
}

/// Closed machine-readable reason for an incompatible external database.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalSqliteIncompatibilityReason {
    ForeignDatabase,
    Unmigrated,
    TooOld,
    TooNew,
    MalformedHeader,
    MissingRequiredSurface,
}

/// Serializable result of probing the external read-only `SQLite` boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExternalSqliteCompatibility {
    pub schema: &'static str,
    pub status: ExternalSqliteCompatibilityStatus,
    pub reason: Option<ExternalSqliteIncompatibilityReason>,
    pub application_id: i64,
    pub user_version: i64,
    pub min_user_version: u32,
    pub max_user_version: u32,
    /// `true` for an accepted pre-application-id Loomweave database. Structural
    /// compatibility does not authenticate that legacy file as Loomweave.
    pub legacy_application_id: bool,
    /// Deterministic `table:<name>` / `column:<table>.<name>` diagnostics.
    pub missing_surface: Vec<String>,
}

impl ExternalSqliteCompatibility {
    fn incompatible(
        application_id: i64,
        user_version: i64,
        reason: ExternalSqliteIncompatibilityReason,
    ) -> Self {
        Self {
            schema: EXTERNAL_SQLITE_SCHEMA,
            status: ExternalSqliteCompatibilityStatus::Incompatible,
            reason: Some(reason),
            application_id,
            user_version,
            min_user_version: EXTERNAL_READ_MIN_USER_VERSION,
            max_user_version: EXTERNAL_READ_MAX_USER_VERSION,
            legacy_application_id: application_id == 0,
            missing_surface: Vec::new(),
        }
    }
}

/// Classify a database before an external consumer issues catalogue SQL.
///
/// The probe is read-only and never runs migrations or writes header PRAGMAs.
/// Header incompatibility is returned before table/column introspection so a
/// future or foreign database cannot leak a misleading `no such table` error.
///
/// # Errors
///
/// Returns [`crate::StorageError::Sqlite`] only when `SQLite` cannot read the
/// PRAGMAs or schema metadata. Contract incompatibility is a successful typed
/// report with `status=incompatible`.
pub fn external_sqlite_compatibility(conn: &Connection) -> Result<ExternalSqliteCompatibility> {
    let application_id: i64 = conn.query_row("PRAGMA application_id", [], |row| row.get(0))?;
    let user_version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;

    if application_id < 0 || user_version < 0 {
        return Ok(ExternalSqliteCompatibility::incompatible(
            application_id,
            user_version,
            ExternalSqliteIncompatibilityReason::MalformedHeader,
        ));
    }
    if application_id != 0 && application_id != i64::from(LOOMWEAVE_APPLICATION_ID) {
        return Ok(ExternalSqliteCompatibility::incompatible(
            application_id,
            user_version,
            ExternalSqliteIncompatibilityReason::ForeignDatabase,
        ));
    }
    if user_version == 0 {
        return Ok(ExternalSqliteCompatibility::incompatible(
            application_id,
            user_version,
            ExternalSqliteIncompatibilityReason::Unmigrated,
        ));
    }
    if user_version < i64::from(EXTERNAL_READ_MIN_USER_VERSION) {
        return Ok(ExternalSqliteCompatibility::incompatible(
            application_id,
            user_version,
            ExternalSqliteIncompatibilityReason::TooOld,
        ));
    }
    if user_version > i64::from(EXTERNAL_READ_MAX_USER_VERSION) {
        return Ok(ExternalSqliteCompatibility::incompatible(
            application_id,
            user_version,
            ExternalSqliteIncompatibilityReason::TooNew,
        ));
    }

    let missing_surface = missing_required_surface(conn)?;
    if !missing_surface.is_empty() {
        let mut report = ExternalSqliteCompatibility::incompatible(
            application_id,
            user_version,
            ExternalSqliteIncompatibilityReason::MissingRequiredSurface,
        );
        report.missing_surface = missing_surface;
        return Ok(report);
    }

    let status = if user_version == i64::from(EXTERNAL_READ_MAX_USER_VERSION) {
        ExternalSqliteCompatibilityStatus::Compatible
    } else {
        ExternalSqliteCompatibilityStatus::OlderSupported
    };
    Ok(ExternalSqliteCompatibility {
        schema: EXTERNAL_SQLITE_SCHEMA,
        status,
        reason: None,
        application_id,
        user_version,
        min_user_version: EXTERNAL_READ_MIN_USER_VERSION,
        max_user_version: EXTERNAL_READ_MAX_USER_VERSION,
        legacy_application_id: application_id == 0,
        missing_surface: Vec::new(),
    })
}

fn missing_required_surface(conn: &Connection) -> Result<Vec<String>> {
    let mut missing = Vec::new();
    for (table, required_columns) in EXTERNAL_SQLITE_REQUIRED_SURFACE {
        let exists = conn
            .query_row(
                "SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?1",
                params![table],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !exists {
            missing.push(format!("table:{table}"));
            continue;
        }

        let mut statement = conn.prepare("SELECT name FROM pragma_table_info(?1)")?;
        let columns = statement
            .query_map(params![table], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<BTreeSet<_>, _>>()?;
        for column in *required_columns {
            if !columns.contains(*column) {
                missing.push(format!("column:{table}.{column}"));
            }
        }
    }
    Ok(missing)
}
