use loomweave_storage::{
    EXTERNAL_READ_MAX_USER_VERSION, EXTERNAL_READ_MIN_USER_VERSION,
    EXTERNAL_SQLITE_REQUIRED_SURFACE, ExternalSqliteCompatibilityStatus,
    ExternalSqliteIncompatibilityReason, external_sqlite_compatibility, pragma, schema,
};
use rusqlite::{Connection, OpenFlags};

fn current_database() -> Connection {
    let mut conn = Connection::open_in_memory().expect("open current database");
    schema::apply_migrations(&mut conn).expect("apply migrations");
    conn.execute_batch(&format!(
        "PRAGMA application_id = {};",
        pragma::LOOMWEAVE_APPLICATION_ID
    ))
    .expect("stamp Loomweave application id");
    conn
}

fn minimal_external_surface(user_version: u32, application_id: u32) -> Connection {
    let conn = Connection::open_in_memory().expect("open external database");
    conn.execute_batch(&format!(
        "PRAGMA application_id = {application_id}; PRAGMA user_version = {user_version};"
    ))
    .expect("stamp external read surface");
    create_surface(&conn, None, None);
    conn
}

fn create_surface(
    conn: &Connection,
    omitted_table: Option<&str>,
    omitted_column: Option<(&str, &str)>,
) {
    for (table, columns) in EXTERNAL_SQLITE_REQUIRED_SURFACE {
        if omitted_table == Some(*table) {
            continue;
        }
        let definitions = columns
            .iter()
            .filter(|column| omitted_column != Some((*table, **column)))
            .map(|column| format!("{column} TEXT"))
            .collect::<Vec<_>>()
            .join(", ");
        conn.execute_batch(&format!("CREATE TABLE {table} ({definitions});"))
            .unwrap_or_else(|error| panic!("create {table}: {error}"));
    }
}

#[test]
fn current_schema_is_compatible() {
    let conn = current_database();

    let report = external_sqlite_compatibility(&conn).expect("classify current database");

    assert_eq!(report.status, ExternalSqliteCompatibilityStatus::Compatible);
    assert_eq!(report.reason, None);
    assert_eq!(
        report.user_version,
        i64::from(EXTERNAL_READ_MAX_USER_VERSION)
    );
    assert_eq!(
        report.application_id,
        i64::from(pragma::LOOMWEAVE_APPLICATION_ID)
    );
    assert!(!report.legacy_application_id);
}

#[test]
fn legacy_application_id_at_minimum_version_is_older_supported_without_mutation() {
    let conn = minimal_external_surface(EXTERNAL_READ_MIN_USER_VERSION, 0);

    let report = external_sqlite_compatibility(&conn).expect("classify legacy database");

    assert_eq!(
        report.status,
        ExternalSqliteCompatibilityStatus::OlderSupported
    );
    assert_eq!(report.reason, None);
    assert!(report.legacy_application_id);
    let application_id: i64 = conn
        .query_row("PRAGMA application_id", [], |row| row.get(0))
        .expect("read application id after compatibility probe");
    assert_eq!(
        application_id, 0,
        "read compatibility probe must not mutate the header"
    );
}

#[test]
fn every_supported_older_version_is_distinguished_from_current() {
    for version in EXTERNAL_READ_MIN_USER_VERSION..EXTERNAL_READ_MAX_USER_VERSION {
        let conn = minimal_external_surface(version, pragma::LOOMWEAVE_APPLICATION_ID);
        let report = external_sqlite_compatibility(&conn).expect("classify older database");
        assert_eq!(
            report.status,
            ExternalSqliteCompatibilityStatus::OlderSupported,
            "user_version={version}"
        );
    }
}

#[test]
fn incompatible_header_is_rejected_before_catalogue_table_introspection() {
    for (application_id, user_version, expected) in [
        (
            pragma::LOOMWEAVE_APPLICATION_ID,
            0,
            ExternalSqliteIncompatibilityReason::Unmigrated,
        ),
        (
            pragma::LOOMWEAVE_APPLICATION_ID,
            EXTERNAL_READ_MIN_USER_VERSION - 1,
            ExternalSqliteIncompatibilityReason::TooOld,
        ),
        (
            pragma::LOOMWEAVE_APPLICATION_ID,
            EXTERNAL_READ_MAX_USER_VERSION + 1,
            ExternalSqliteIncompatibilityReason::TooNew,
        ),
        (
            0x1234_5678,
            EXTERNAL_READ_MAX_USER_VERSION,
            ExternalSqliteIncompatibilityReason::ForeignDatabase,
        ),
    ] {
        let conn = Connection::open_in_memory().expect("open header-only database");
        conn.execute_batch(&format!(
            "PRAGMA application_id = {application_id}; PRAGMA user_version = {user_version};"
        ))
        .expect("stamp incompatible header");

        let report = external_sqlite_compatibility(&conn).expect("classify incompatible header");

        assert_eq!(
            report.status,
            ExternalSqliteCompatibilityStatus::Incompatible
        );
        assert_eq!(report.reason, Some(expected));
        assert!(
            report.missing_surface.is_empty(),
            "header rejection must happen before table SQL: {report:?}"
        );
    }
}

#[test]
fn negative_headers_are_malformed_and_normalized_out_of_range_version_is_unmigrated() {
    for pragma_name in ["application_id", "user_version"] {
        let conn = Connection::open_in_memory().expect("open negative-header database");
        conn.execute_batch(&format!(
            "PRAGMA application_id = {}; PRAGMA user_version = {};
             PRAGMA {pragma_name} = -1;",
            pragma::LOOMWEAVE_APPLICATION_ID,
            EXTERNAL_READ_MAX_USER_VERSION,
        ))
        .expect("stamp negative header");
        let report = external_sqlite_compatibility(&conn).expect("classify negative header");
        assert_eq!(
            report.reason,
            Some(ExternalSqliteIncompatibilityReason::MalformedHeader),
            "negative {pragma_name}"
        );
    }

    let conn = Connection::open_in_memory().expect("open out-of-range database");
    conn.execute_batch(&format!(
        "PRAGMA application_id = {};
         PRAGMA user_version = 4294967295;",
        pragma::LOOMWEAVE_APPLICATION_ID,
    ))
    .expect("stamp out-of-range user version");
    let report = external_sqlite_compatibility(&conn).expect("classify normalized header");
    assert_eq!(
        report.user_version, 0,
        "SQLite normalizes out-of-range PRAGMA to zero"
    );
    assert_eq!(
        report.reason,
        Some(ExternalSqliteIncompatibilityReason::Unmigrated)
    );
}

#[test]
fn supported_header_with_missing_table_is_structurally_incompatible() {
    let conn = Connection::open_in_memory().expect("open incomplete database");
    conn.execute_batch(&format!(
        "PRAGMA application_id = {}; PRAGMA user_version = {};",
        pragma::LOOMWEAVE_APPLICATION_ID,
        EXTERNAL_READ_MAX_USER_VERSION,
    ))
    .expect("stamp compatible header");

    let report = external_sqlite_compatibility(&conn).expect("classify incomplete database");

    assert_eq!(
        report.status,
        ExternalSqliteCompatibilityStatus::Incompatible
    );
    assert_eq!(
        report.reason,
        Some(ExternalSqliteIncompatibilityReason::MissingRequiredSurface)
    );
    assert!(
        report
            .missing_surface
            .contains(&"table:entities".to_owned())
    );
}

#[test]
fn supported_header_with_missing_column_is_structurally_incompatible() {
    let conn = minimal_external_surface(
        EXTERNAL_READ_MAX_USER_VERSION,
        pragma::LOOMWEAVE_APPLICATION_ID,
    );
    conn.execute_batch(
        "ALTER TABLE sei_lineage RENAME TO old_sei_lineage;
         CREATE TABLE sei_lineage (
             sei TEXT, event TEXT, old_locator TEXT, new_locator TEXT, run_id TEXT
         );",
    )
    .expect("replace table with missing column");

    let report = external_sqlite_compatibility(&conn).expect("classify missing column");

    assert_eq!(
        report.status,
        ExternalSqliteCompatibilityStatus::Incompatible
    );
    assert_eq!(
        report.reason,
        Some(ExternalSqliteIncompatibilityReason::MissingRequiredSurface)
    );
    assert_eq!(report.missing_surface, ["column:sei_lineage.recorded_at"]);
}

#[test]
fn every_required_table_and_column_is_load_bearing() {
    for (table, columns) in EXTERNAL_SQLITE_REQUIRED_SURFACE {
        let conn = Connection::open_in_memory().expect("open missing-table database");
        conn.execute_batch(&format!(
            "PRAGMA application_id = {}; PRAGMA user_version = {};",
            pragma::LOOMWEAVE_APPLICATION_ID,
            EXTERNAL_READ_MAX_USER_VERSION,
        ))
        .expect("stamp compatible header");
        create_surface(&conn, Some(table), None);
        let report = external_sqlite_compatibility(&conn).expect("classify missing table");
        assert_eq!(
            report.missing_surface,
            [format!("table:{table}")],
            "required table {table}"
        );

        for column in *columns {
            let conn = Connection::open_in_memory().expect("open missing-column database");
            conn.execute_batch(&format!(
                "PRAGMA application_id = {}; PRAGMA user_version = {};",
                pragma::LOOMWEAVE_APPLICATION_ID,
                EXTERNAL_READ_MAX_USER_VERSION,
            ))
            .expect("stamp compatible header");
            create_surface(&conn, None, Some((table, column)));
            let report = external_sqlite_compatibility(&conn).expect("classify missing column");
            assert_eq!(
                report.missing_surface,
                [format!("column:{table}.{column}")],
                "required column {table}.{column}"
            );
        }
    }
}

#[test]
fn required_surface_literal_is_frozen() {
    assert_eq!(
        EXTERNAL_SQLITE_REQUIRED_SURFACE,
        &[
            (
                "runs",
                &["id", "started_at", "completed_at", "stats", "status"] as &[&str]
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
        ]
    );
}

#[test]
fn probe_accepts_a_real_read_only_connection_without_mutating_the_file() {
    let directory = tempfile::tempdir().expect("temp database directory");
    let path = directory.path().join("loomweave.db");
    {
        let conn = Connection::open(&path).expect("create on-disk database");
        conn.execute_batch(&format!(
            "PRAGMA application_id = {}; PRAGMA user_version = {};",
            pragma::LOOMWEAVE_APPLICATION_ID,
            EXTERNAL_READ_MAX_USER_VERSION,
        ))
        .expect("stamp on-disk database");
        create_surface(&conn, None, None);
    }
    let conn = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("open database read-only");

    let report = external_sqlite_compatibility(&conn).expect("probe read-only database");

    assert_eq!(report.status, ExternalSqliteCompatibilityStatus::Compatible);
    let write_error = conn
        .execute("INSERT INTO runs (id) VALUES ('must-fail')", [])
        .expect_err("read-only connection must reject writes");
    assert!(
        write_error.to_string().contains("readonly"),
        "unexpected read-only write error: {write_error}"
    );
}

#[test]
fn compatibility_report_serializes_closed_typed_fields() {
    let conn = current_database();
    let report = external_sqlite_compatibility(&conn).expect("classify current database");

    let value = serde_json::to_value(report).expect("serialize compatibility report");

    assert_eq!(value["schema"], "loomweave.external-sqlite.v1");
    assert_eq!(value["status"], "compatible");
    assert_eq!(value["reason"], serde_json::Value::Null);
    assert_eq!(value["min_user_version"], EXTERNAL_READ_MIN_USER_VERSION);
    assert_eq!(value["max_user_version"], EXTERNAL_READ_MAX_USER_VERSION);
    assert_eq!(value["missing_surface"], serde_json::json!([]));
}
