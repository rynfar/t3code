//! Read-only SQLite file engine for the binary/database preview (`BinaryViewer.tsx`,
//! `binaryPreview.worker.ts`). Writes downloaded bytes to a private temporary file that is removed
//! automatically on drop, opens SQLite with `SQLITE_OPEN_READ_ONLY | SQLITE_OPEN_NO_MUTEX` and
//! `PRAGMA query_only = ON; PRAGMA trusted_schema = OFF;`, lists user tables & views, and serves
//! bounded paged `SELECT` queries.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rusqlite::types::ValueRef;
use rusqlite::{Connection, ErrorCode, OpenFlags, params};
use t3_core::files::sqlite::{
    SQLITE_MAX_COLUMNS, SQLITE_MAX_TABLES, SQLITE_PAGE_SIZE, SQLITE_PREVIEW_MAX_BYTES, SqliteCell,
    SqliteColumnInfo, SqliteObjectKind, SqlitePageData, SqliteTableInfo,
    build_sqlite_page_select_sql, include_table_xinfo_column, is_generated_xinfo_column,
    is_internal_sqlite_table, is_virtual_table_sql, quote_sqlite_identifier,
    sqlite_immutable_file_uri, sqlite_page_range,
};

/// Temporary file holding downloaded SQLite bytes, cleaned up unconditionally when the last handle drops.
#[derive(Debug)]
struct TempSqliteFile {
    path: PathBuf,
}

impl TempSqliteFile {
    fn write(bytes: &[u8]) -> Result<Arc<Self>, String> {
        if bytes.len() > SQLITE_PREVIEW_MAX_BYTES {
            return Err("Preview limited to files under 50 MB.".to_owned());
        }
        let path = std::env::temp_dir().join(format!("t3-sqlite-preview-{}.sqlite", uuid::Uuid::new_v4()));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&path)
            .map_err(|_| "Could not prepare a temporary file for SQLite preview.".to_owned())?;
        if file.write_all(bytes).and_then(|()| file.flush()).is_err() {
            drop(file);
            let _ = fs::remove_file(&path);
            return Err("Could not prepare a temporary file for SQLite preview.".to_owned());
        }
        Ok(Arc::new(Self { path }))
    }
}

impl Drop for TempSqliteFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Read-only handle to a previewed SQLite database backed by an RAII temporary file.
#[derive(Debug, Clone)]
pub struct SqliteDatabaseHandle {
    temp: Arc<TempSqliteFile>,
    tables: Vec<SqliteTableInfo>,
}

impl SqliteDatabaseHandle {
    /// Opens downloaded SQLite bytes read-only, lists user tables & views with row counts, and
    /// immediately attempts to load page 0 of the first table (when one exists). A failure reading
    /// that initial table is returned in `Option<Result<SqlitePageData, String>>` so the database's
    /// table list stays accessible and other tables remain selectable.
    pub fn open(bytes: &[u8]) -> Result<(Self, Option<Result<SqlitePageData, String>>), String> {
        let temp = TempSqliteFile::write(bytes)?;
        let conn = open_readonly(&temp.path)?;
        let tables = list_tables(&conn)?;
        let initial_page = tables.first().map(|first| query_page_on_conn(&conn, first, 0));
        Ok((Self { temp, tables }, initial_page))
    }

    pub fn tables(&self) -> &[SqliteTableInfo] {
        &self.tables
    }

    pub fn table(&self, name: &str) -> Option<&SqliteTableInfo> {
        self.tables.iter().find(|t| t.name == name)
    }

    /// Serves one bounded page of rows for `table_name` on a fresh read-only connection.
    pub fn query_page(&self, table_name: &str, page: usize) -> Result<SqlitePageData, String> {
        let Some(table) = self.table(table_name) else {
            return Err("This table is unavailable.".to_owned());
        };
        let conn = open_readonly(&self.temp.path)?;
        query_page_on_conn(&conn, table, page)
    }
}

fn open_readonly(path: &Path) -> Result<Connection, String> {
    let flags =
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX | OpenFlags::SQLITE_OPEN_URI;
    let uri = sqlite_immutable_file_uri(&path.to_string_lossy());
    let conn = Connection::open_with_flags(&uri, flags).map_err(|err| classify_sqlite_error(&err))?;
    conn.execute_batch("PRAGMA query_only = ON; PRAGMA trusted_schema = OFF; PRAGMA busy_timeout = 1000;")
        .map_err(|err| classify_sqlite_error(&err))?;
    Ok(conn)
}

fn list_tables(conn: &Connection) -> Result<Vec<SqliteTableInfo>, String> {
    let limit = (SQLITE_MAX_TABLES + 1) as i64;
    let mut stmt = conn
        .prepare(
            "SELECT name, type, sql \
             FROM sqlite_master \
             WHERE type IN ('table', 'view') \
               AND substr(name, 1, 7) != 'sqlite_' \
             ORDER BY name \
             LIMIT ?1",
        )
        .map_err(|err| classify_sqlite_error(&err))?;

    let raw_rows = stmt
        .query_map(params![limit], |row| {
            let name: String = row.get(0)?;
            let kind: String = row.get(1)?;
            let sql: Option<String> = row.get(2)?;
            Ok((name, kind, sql))
        })
        .map_err(|err| classify_sqlite_error(&err))?;

    let mut candidates = Vec::new();
    for entry in raw_rows {
        let (name, kind_str, sql) = entry.map_err(|err| classify_sqlite_error(&err))?;
        if is_internal_sqlite_table(&name) || is_virtual_table_sql(sql.as_deref()) {
            continue;
        }
        let kind = match kind_str.as_str() {
            "view" => SqliteObjectKind::View,
            _ => SqliteObjectKind::Table,
        };
        candidates.push((name, kind));
    }

    if candidates.len() > SQLITE_MAX_TABLES {
        return Err("Database previews are limited to 100 tables.".to_owned());
    }

    let mut tables = Vec::with_capacity(candidates.len());
    for (name, kind) in candidates {
        let count_sql = format!("SELECT count(*) FROM {}", quote_sqlite_identifier(&name));
        match conn.query_row(&count_sql, [], |row| row.get::<_, i64>(0)) {
            Ok(count) => tables.push(SqliteTableInfo {
                name,
                kind,
                row_count: u64::try_from(count).unwrap_or(0),
            }),
            // A view referencing an unavailable virtual table or restricted function is skipped
            // rather than failing valid tables; a corrupt regular table surfaces its error.
            Err(_) if kind == SqliteObjectKind::View => continue,
            Err(err) => return Err(classify_sqlite_error(&err)),
        }
    }

    Ok(tables)
}

fn query_page_on_conn(
    conn: &Connection,
    table: &SqliteTableInfo,
    page: usize,
) -> Result<SqlitePageData, String> {
    let pragma_sql = format!("PRAGMA table_xinfo({})", quote_sqlite_identifier(&table.name));
    let mut pragma_stmt = conn
        .prepare(&pragma_sql)
        .map_err(|err| classify_sqlite_error(&err))?;

    let col_rows = pragma_stmt
        .query_map([], |row| {
            let name: String = row.get(1)?;
            let declared_type: String = row.get::<_, Option<String>>(2)?.unwrap_or_default();
            let pk: i64 = row.get(5)?;
            let hidden: i64 = row.get(6)?;
            Ok((name, declared_type, pk, hidden))
        })
        .map_err(|err| classify_sqlite_error(&err))?;

    let mut columns = Vec::new();
    let mut columns_truncated = false;
    for col in col_rows {
        let (name, declared_type, pk, hidden) = col.map_err(|err| classify_sqlite_error(&err))?;
        if !include_table_xinfo_column(hidden) {
            continue;
        }
        if columns.len() >= SQLITE_MAX_COLUMNS {
            columns_truncated = true;
            break;
        }
        columns.push(SqliteColumnInfo {
            name,
            declared_type,
            is_primary_key: pk > 0,
            is_generated: is_generated_xinfo_column(hidden),
        });
    }

    let range = sqlite_page_range(table.row_count, page, SQLITE_PAGE_SIZE);
    let Some(select_sql) = build_sqlite_page_select_sql(&table.name, &columns) else {
        return Ok(SqlitePageData {
            table: table.name.clone(),
            page: range.page,
            columns,
            columns_truncated,
            rows: Vec::new(),
        });
    };

    let mut select_stmt = conn
        .prepare(&select_sql)
        .map_err(|err| classify_sqlite_error(&err))?;
    let col_count = columns.len();
    let row_iter = select_stmt
        .query_map(params![range.limit as i64, range.offset as i64], |row| {
            let mut cells = Vec::with_capacity(col_count);
            for col_idx in 0..col_count {
                let value = row.get_ref(col_idx * 2)?;
                let blob_len = row.get_ref(col_idx * 2 + 1)?;
                let cell = match value {
                    ValueRef::Null => SqliteCell::null(),
                    ValueRef::Integer(n) => SqliteCell::integer(n),
                    ValueRef::Real(f) => SqliteCell::real(f),
                    ValueRef::Text(bytes) => SqliteCell::text(&String::from_utf8_lossy(bytes)),
                    ValueRef::Blob(prefix) => {
                        let total_len = match blob_len {
                            ValueRef::Integer(n) if n >= 0 => n as u64,
                            _ => prefix.len() as u64,
                        };
                        SqliteCell::blob(total_len, prefix)
                    }
                };
                cells.push(cell);
            }
            Ok(cells)
        })
        .map_err(|err| classify_sqlite_error(&err))?;

    let mut rows = Vec::new();
    for row in row_iter {
        rows.push(row.map_err(|err| classify_sqlite_error(&err))?);
    }

    Ok(SqlitePageData {
        table: table.name.clone(),
        page: range.page,
        columns,
        columns_truncated,
        rows,
    })
}

fn classify_sqlite_error(err: &rusqlite::Error) -> String {
    match err {
        rusqlite::Error::SqliteFailure(ffi_err, _) => match ffi_err.code {
            ErrorCode::NotADatabase => {
                "This file is not a valid SQLite database or may be encrypted.".to_owned()
            }
            ErrorCode::DatabaseCorrupt => "This SQLite database is damaged or corrupt.".to_owned(),
            ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked => {
                "This SQLite database is busy and could not be read.".to_owned()
            }
            ErrorCode::AuthorizationForStatementDenied | ErrorCode::ReadOnly => {
                "This SQLite query is not permitted in read-only preview.".to_owned()
            }
            _ => "Could not read this SQLite database.".to_owned(),
        },
        _ => "Could not read this SQLite database.".to_owned(),
    }
}

#[cfg(test)]
pub(super) fn build_test_sqlite_bytes(setup_sql: &str) -> Vec<u8> {
    let path = std::env::temp_dir().join(format!("t3-sqlite-test-fixture-{}.sqlite", uuid::Uuid::new_v4()));
    let conn = Connection::open(&path).expect("create test sqlite db");
    conn.execute_batch(setup_sql).expect("seed test sqlite db");
    drop(conn);
    let bytes = fs::read(&path).expect("read test sqlite bytes");
    let _ = fs::remove_file(&path);
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use t3_core::files::sqlite::{SQLITE_MAX_CELL_CHARS, SqliteCellKind};

    #[test]
    fn lists_tables_and_views_excludes_internal_and_queries_generated_and_without_rowid() {
        let bytes = build_test_sqlite_bytes(
            "CREATE TABLE sqliteData (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                price INTEGER,
                quantity INTEGER,
                total INTEGER GENERATED ALWAYS AS (price * quantity) STORED,
                doubled INTEGER GENERATED ALWAYS AS (price * 2) VIRTUAL
            );
            INSERT INTO sqliteData(price, quantity) VALUES (7, 3);

            CREATE TABLE kv_store (
                k TEXT PRIMARY KEY,
                val REAL,
                note TEXT,
                payload BLOB
            ) WITHOUT ROWID;
            INSERT INTO kv_store(k, val, note, payload) VALUES
                ('alpha', 3.25, NULL, X'DEADBEEF'),
                ('beta', -0.0, 'hello', X'000102030405060708090A0B');

            CREATE TABLE \"weird \"\"table\"\"\" (\"select\" TEXT);
            INSERT INTO \"weird \"\"table\"\"\" VALUES ('quoted');

            CREATE VIEW expensive_orders AS SELECT price, total FROM sqliteData WHERE total > 10;",
        );

        let (handle, initial_page) = SqliteDatabaseHandle::open(&bytes).expect("open valid sqlite");
        let temp_path = handle.temp.path.clone();
        assert!(temp_path.exists());

        // `sqlite_sequence` from AUTOINCREMENT must be excluded; tables & views ordered by name.
        let names: Vec<(&str, SqliteObjectKind, u64)> = handle
            .tables()
            .iter()
            .map(|t| (t.name.as_str(), t.kind, t.row_count))
            .collect();
        assert_eq!(
            names,
            vec![
                ("expensive_orders", SqliteObjectKind::View, 1),
                ("kv_store", SqliteObjectKind::Table, 2),
                ("sqliteData", SqliteObjectKind::Table, 1),
                ("weird \"table\"", SqliteObjectKind::Table, 1),
            ]
        );

        // Initial page is the first listed object (`expensive_orders` view).
        let first_page = initial_page.expect("initial page loaded").expect("initial page ok");
        assert_eq!(first_page.table, "expensive_orders");
        assert_eq!(first_page.rows.len(), 1);
        assert_eq!(first_page.rows[0][0].display, "7");
        assert_eq!(first_page.rows[0][1].display, "21");

        // Generated STORED + VIRTUAL columns must appear via PRAGMA table_xinfo.
        let generated_page = handle.query_page("sqliteData", 0).expect("query sqliteData");
        let col_names: Vec<&str> = generated_page.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(col_names, vec!["id", "price", "quantity", "total", "doubled"]);
        assert!(generated_page.columns[3].is_generated);
        assert!(generated_page.columns[4].is_generated);
        assert_eq!(generated_page.rows[0][1].display, "7");
        assert_eq!(generated_page.rows[0][2].display, "3");
        assert_eq!(generated_page.rows[0][3].display, "21");
        assert_eq!(generated_page.rows[0][4].display, "14");

        // WITHOUT ROWID + NULL + REAL + BLOB hex summary.
        let kv_page = handle.query_page("kv_store", 0).expect("query kv_store");
        assert_eq!(kv_page.rows.len(), 2);
        assert_eq!(kv_page.rows[0][0].display, "alpha");
        assert_eq!(kv_page.rows[0][1].display, "3.25");
        assert_eq!(kv_page.rows[0][2].kind, SqliteCellKind::Null);
        assert_eq!(kv_page.rows[0][2].display, "NULL");
        assert_eq!(kv_page.rows[0][3].kind, SqliteCellKind::Blob);
        assert_eq!(kv_page.rows[0][3].display, "[blob: 4 bytes · de ad be ef]");
        assert_eq!(kv_page.rows[1][1].display, "0");
        assert_eq!(kv_page.rows[1][3].display, "[blob: 12 bytes · 00 01 02 03 04 05 06 07…]");
        assert!(kv_page.rows[1][3].truncated);

        // Quoted table & keyword column identifiers.
        let weird_page = handle.query_page("weird \"table\"", 0).expect("query weird table");
        assert_eq!(weird_page.columns[0].name, "select");
        assert_eq!(weird_page.rows[0][0].display, "quoted");

        // Unavailable table fails cleanly.
        assert_eq!(
            handle.query_page("nonexistent", 0).unwrap_err(),
            "This table is unavailable."
        );

        // Dropping handle deletes the temporary file.
        drop(handle);
        assert!(!temp_path.exists(), "temp sqlite file must be cleaned up on drop");
    }

    #[test]
    fn pages_large_tables_and_truncates_long_text_cells() {
        let long_text = "x".repeat(SQLITE_MAX_CELL_CHARS + 50);
        let mut sql = String::from("CREATE TABLE items (n INTEGER PRIMARY KEY, body TEXT);");
        for idx in 1..=125 {
            if idx == 1 {
                sql.push_str(&format!("INSERT INTO items VALUES ({idx}, '{long_text}');"));
            } else {
                sql.push_str(&format!("INSERT INTO items VALUES ({idx}, 'row-{idx}');"));
            }
        }
        let bytes = build_test_sqlite_bytes(&sql);
        let (handle, page0) = SqliteDatabaseHandle::open(&bytes).expect("open paged db");
        assert_eq!(handle.tables()[0].row_count, 125);

        let p0 = page0.expect("page 0").expect("page 0 ok");
        assert_eq!(p0.rows.len(), SQLITE_PAGE_SIZE);
        assert_eq!(p0.rows[0][0].display, "1");
        assert_eq!(p0.rows[0][1].display.chars().count(), SQLITE_MAX_CELL_CHARS);
        assert!(p0.rows[0][1].truncated);
        assert_eq!(p0.rows[49][0].display, "50");

        let p1 = handle.query_page("items", 1).expect("page 1");
        assert_eq!(p1.rows.len(), SQLITE_PAGE_SIZE);
        assert_eq!(p1.rows[0][0].display, "51");
        assert_eq!(p1.rows[49][0].display, "100");

        let p2 = handle.query_page("items", 2).expect("page 2");
        assert_eq!(p2.rows.len(), 25);
        assert_eq!(p2.rows[0][0].display, "101");
        assert_eq!(p2.rows[24][0].display, "125");
    }

    #[test]
    fn rejects_corrupt_encrypted_and_over_table_limit_databases() {
        let encrypted = b"this is random encrypted garbage not sqlite format 3";
        let err = SqliteDatabaseHandle::open(encrypted).unwrap_err();
        assert!(err.contains("not a valid SQLite database"), "got: {err}");

        // Valid header with corrupt page content.
        let mut corrupt = build_test_sqlite_bytes("CREATE TABLE t (x INTEGER); INSERT INTO t VALUES (1);");
        for byte in corrupt.iter_mut().skip(100) {
            *byte = 0xFF;
        }
        let corrupt_err = SqliteDatabaseHandle::open(&corrupt).unwrap_err();
        assert!(
            corrupt_err.contains("corrupt") || corrupt_err.contains("not a valid SQLite") || corrupt_err.contains("Could not read"),
            "got: {corrupt_err}"
        );

        // Exceeding 100 tables returns the exact bounded error.
        let mut many_tables = String::new();
        for idx in 0..=SQLITE_MAX_TABLES {
            many_tables.push_str(&format!("CREATE TABLE t_{idx:03} (id INTEGER);"));
        }
        let many_bytes = build_test_sqlite_bytes(&many_tables);
        assert_eq!(
            SqliteDatabaseHandle::open(&many_bytes).unwrap_err(),
            "Database previews are limited to 100 tables."
        );
    }

    #[test]
    fn opens_isolated_wal_mode_database_bytes_without_sidecar_files() {
        let path = std::env::temp_dir().join(format!("t3-sqlite-wal-fixture-{}.sqlite", uuid::Uuid::new_v4()));
        let conn = Connection::open(&path).expect("create wal fixture");
        let mode: String = conn
            .pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))
            .expect("enable WAL journal_mode");
        assert_eq!(mode.to_ascii_lowercase(), "wal");
        conn.execute_batch(
            "CREATE TABLE events (id INTEGER PRIMARY KEY, label TEXT);
             INSERT INTO events VALUES (1, 'boot'), (2, 'ready');
             PRAGMA wal_checkpoint(TRUNCATE);",
        )
        .expect("seed and checkpoint wal fixture");
        drop(conn);

        // Read ONLY the main `.sqlite` file bytes (no `-wal`/`-shm` sidecars) and remove fixture files.
        let wal_bytes = fs::read(&path).expect("read wal sqlite bytes");
        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(format!("{}-wal", path.display()));
        let _ = fs::remove_file(format!("{}-shm", path.display()));

        // Header bytes 18..=19 must still indicate WAL format (2, 2).
        assert_eq!(&wal_bytes[18..=19], &[2, 2]);

        let (handle, initial_page) =
            SqliteDatabaseHandle::open(&wal_bytes).expect("open isolated WAL-mode snapshot");
        assert_eq!(handle.tables().len(), 1);
        assert_eq!(handle.tables()[0].name, "events");
        assert_eq!(handle.tables()[0].row_count, 2);
        let page = initial_page.expect("initial page").expect("initial page ok");
        assert_eq!(page.rows.len(), 2);
        assert_eq!(page.rows[0][1].display, "boot");
        assert_eq!(page.rows[1][1].display, "ready");
    }
}
