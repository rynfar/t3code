//! Pure data types, bounds, page math, identifier quoting, and cell formatting for the read-only
//! SQLite table & paged row viewer (`BinaryViewer.tsx`, `binaryPreview.worker.ts`,
//! `binaryPreviewProtocol.ts`). Kept completely I/O-free inside `t3-core`.

use super::group_thousands;

/// Rows fetched per page (`SQLITE_PAGE_SIZE` in `binaryPreviewProtocol.ts`).
pub const SQLITE_PAGE_SIZE: usize = 50;
/// Maximum user tables/views listed from a database (`binaryPreview.worker.ts`).
pub const SQLITE_MAX_TABLES: usize = 100;
/// Maximum columns projected per table (`binaryPreview.worker.ts`).
pub const SQLITE_MAX_COLUMNS: usize = 50;
/// Maximum characters retained per cell (`binaryPreview.worker.ts`).
pub const SQLITE_MAX_CELL_CHARS: usize = 1_024;
/// Leading bytes included in a BLOB hex summary.
pub const SQLITE_BLOB_HEX_PREVIEW_BYTES: usize = 8;
/// Maximum database file size opened for preview (`BINARY_PREVIEW_MAX_BYTES`).
pub const SQLITE_PREVIEW_MAX_BYTES: usize = super::binary::BINARY_PREVIEW_MAX_BYTES;

/// User-visible SQLite relation type listed from `sqlite_master`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqliteObjectKind {
    Table,
    View,
}

/// Summary metadata for one user table or view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqliteTableInfo {
    pub name: String,
    pub kind: SqliteObjectKind,
    pub row_count: u64,
}

/// Column metadata derived from `PRAGMA table_xinfo`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqliteColumnInfo {
    pub name: String,
    pub declared_type: String,
    pub is_primary_key: bool,
    pub is_generated: bool,
}

/// Storage class of a decoded SQLite cell value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqliteCellKind {
    Null,
    Integer,
    Real,
    Text,
    Blob,
}

/// Bounded display representation of one cell in a paged SQLite row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqliteCell {
    pub kind: SqliteCellKind,
    pub display: String,
    pub truncated: bool,
}

impl SqliteCell {
    pub fn null() -> Self {
        Self {
            kind: SqliteCellKind::Null,
            display: "NULL".to_owned(),
            truncated: false,
        }
    }

    pub fn integer(value: i64) -> Self {
        Self {
            kind: SqliteCellKind::Integer,
            display: value.to_string(),
            truncated: false,
        }
    }

    pub fn real(value: f64) -> Self {
        Self {
            kind: SqliteCellKind::Real,
            display: format_real_value(value),
            truncated: false,
        }
    }

    pub fn text(raw: &str) -> Self {
        let mut chars = raw.chars();
        let display: String = chars.by_ref().take(SQLITE_MAX_CELL_CHARS).collect();
        let truncated = chars.next().is_some();
        Self {
            kind: SqliteCellKind::Text,
            display,
            truncated,
        }
    }

    pub fn blob(total_len: u64, prefix: &[u8]) -> Self {
        let truncated = total_len > SQLITE_BLOB_HEX_PREVIEW_BYTES as u64;
        Self {
            kind: SqliteCellKind::Blob,
            display: format_blob_summary(total_len, prefix),
            truncated,
        }
    }
}

/// One loaded page of rows for a SQLite table or view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqlitePageData {
    pub table: String,
    pub page: usize,
    pub columns: Vec<SqliteColumnInfo>,
    pub columns_truncated: bool,
    pub rows: Vec<Vec<SqliteCell>>,
}

/// Page navigation bounds for a table with `total_rows`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SqlitePageRange {
    pub page: usize,
    pub page_count: usize,
    /// 1-based inclusive start row, or `0` when the table is empty.
    pub start_row: u64,
    /// 1-based inclusive end row, or `0` when the table is empty.
    pub end_row: u64,
    pub total_rows: u64,
    /// 0-based SQL `OFFSET`.
    pub offset: u64,
    /// SQL `LIMIT`.
    pub limit: usize,
    pub has_prev: bool,
    pub has_next: bool,
}

/// Computes clamped pagination bounds for `page` over `total_rows`.
pub fn sqlite_page_range(total_rows: u64, page: usize, page_size: usize) -> SqlitePageRange {
    let limit = if page_size == 0 { SQLITE_PAGE_SIZE } else { page_size };
    if total_rows == 0 {
        return SqlitePageRange {
            page: 0,
            page_count: 1,
            start_row: 0,
            end_row: 0,
            total_rows: 0,
            offset: 0,
            limit,
            has_prev: false,
            has_next: false,
        };
    }
    let page_size_u64 = limit as u64;
    let page_count_u64 = total_rows.div_ceil(page_size_u64).max(1);
    let page_count = usize::try_from(page_count_u64).unwrap_or(usize::MAX);
    let clamped_page = page.min(page_count.saturating_sub(1));
    let offset = (clamped_page as u64).saturating_mul(page_size_u64);
    let start_row = offset.saturating_add(1);
    let end_row = offset.saturating_add(page_size_u64).min(total_rows);
    SqlitePageRange {
        page: clamped_page,
        page_count,
        start_row,
        end_row,
        total_rows,
        offset,
        limit,
        has_prev: clamped_page > 0,
        has_next: end_row < total_rows,
    }
}

/// Quotes a SQLite table/view/column identifier safely using SQL double-quote escaping.
pub fn quote_sqlite_identifier(name: &str) -> String {
    let mut quoted = String::with_capacity(name.len() + 2);
    quoted.push('"');
    for ch in name.chars() {
        if ch == '"' {
            quoted.push('"');
        }
        quoted.push(ch);
    }
    quoted.push('"');
    quoted
}

/// Builds a percent-encoded SQLite URI (`file:<path>?immutable=1`) suitable for
/// `SQLITE_OPEN_READ_ONLY | SQLITE_OPEN_NO_MUTEX | SQLITE_OPEN_URI`.
/// Opening an isolated snapshot with `immutable=1` allows SQLite to read WAL-mode databases
/// (header bytes `18..=19 == [2, 2]`) without requiring or creating `-wal`/`-shm` sidecar files.
pub fn sqlite_immutable_file_uri(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    let mut uri = String::with_capacity(normalized.len() + 24);
    uri.push_str("file:");
    if normalized.starts_with('/') {
        uri.push_str("//");
    } else if normalized.as_bytes().get(1) == Some(&b':')
        && normalized.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
    {
        uri.push_str("///");
    }
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for &byte in normalized.as_bytes() {
        let keep = byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'.' | b'_' | b'~' | b':');
        if keep {
            uri.push(byte as char);
        } else {
            uri.push('%');
            uri.push(HEX[(byte >> 4) as usize] as char);
            uri.push(HEX[(byte & 0x0f) as usize] as char);
        }
    }
    uri.push_str("?immutable=1");
    uri
}

/// Returns true for internal SQLite catalog tables (`sqlite_%`).
pub fn is_internal_sqlite_table(name: &str) -> bool {
    name.len() >= 7 && name.as_bytes()[..7].eq_ignore_ascii_case(b"sqlite_")
}

/// Returns true when a `sqlite_master.sql` definition creates a virtual table.
pub fn is_virtual_table_sql(sql: Option<&str>) -> bool {
    let Some(sql) = sql else {
        return false;
    };
    let trimmed = sql.trim_start();
    trimmed.len() >= 20 && trimmed.as_bytes()[..20].eq_ignore_ascii_case(b"CREATE VIRTUAL TABLE")
}

/// `PRAGMA table_xinfo` `hidden` column filter (`binaryPreview.worker.ts`):
/// - `0`: regular column -> include
/// - `1`: hidden virtual-table column -> exclude
/// - `2`: `GENERATED ALWAYS AS (...) VIRTUAL` -> include
/// - `3`: `GENERATED ALWAYS AS (...) STORED` -> include
pub fn include_table_xinfo_column(hidden: i64) -> bool {
    hidden != 1
}

/// True when `PRAGMA table_xinfo` reports a generated column (`VIRTUAL` or `STORED`).
pub fn is_generated_xinfo_column(hidden: i64) -> bool {
    matches!(hidden, 2 | 3)
}

/// Formats a SQLite `REAL` cell cleanly (normalizing `-0.0`).
pub fn format_real_value(value: f64) -> String {
    if value == 0.0 {
        "0".to_owned()
    } else if value.is_nan() {
        "NaN".to_owned()
    } else if value.is_infinite() {
        if value.is_sign_positive() {
            "Infinity".to_owned()
        } else {
            "-Infinity".to_owned()
        }
    } else {
        value.to_string()
    }
}

/// Formats a bounded BLOB hex summary, e.g. `[blob: 4 bytes · de ad be ef]` or
/// `[blob: 1,024 bytes · 89 50 4e 47 0d 0a 1a 0a…]`.
pub fn format_blob_summary(total_len: u64, prefix: &[u8]) -> String {
    let unit = if total_len == 1 { "byte" } else { "bytes" };
    if total_len == 0 || prefix.is_empty() {
        return format!("[blob: {} {unit}]", group_thousands(total_len));
    }
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let shown = prefix.len().min(SQLITE_BLOB_HEX_PREVIEW_BYTES);
    let mut hex = String::with_capacity(shown * 3);
    for (idx, &byte) in prefix[..shown].iter().enumerate() {
        if idx > 0 {
            hex.push(' ');
        }
        hex.push(HEX[(byte >> 4) as usize] as char);
        hex.push(HEX[(byte & 0x0f) as usize] as char);
    }
    let ellipsis = if total_len > shown as u64 { "…" } else { "" };
    format!("[blob: {} {unit} · {hex}{ellipsis}]", group_thousands(total_len))
}

/// Builds a bounded paged `SELECT` query that projects two expressions per column:
/// `(bounded_value, blob_byte_length)` so huge TEXT or BLOB cells are sliced inside SQLite before
/// reaching Rust memory, while preserving `NULL`, `INTEGER`, `REAL`, `TEXT`, and `BLOB` types and
/// working cleanly with `WITHOUT ROWID` tables, views, and `GENERATED ALWAYS AS` columns.
pub fn build_sqlite_page_select_sql(table: &str, columns: &[SqliteColumnInfo]) -> Option<String> {
    if columns.is_empty() {
        return None;
    }
    let text_limit = SQLITE_MAX_CELL_CHARS + 1;
    let blob_limit = SQLITE_BLOB_HEX_PREVIEW_BYTES;
    let mut projections = Vec::with_capacity(columns.len() * 2);
    for col in columns.iter().take(SQLITE_MAX_COLUMNS) {
        let field = quote_sqlite_identifier(&col.name);
        projections.push(format!(
            "CASE typeof({field}) WHEN 'blob' THEN substr({field}, 1, {blob_limit}) WHEN 'text' THEN substr({field}, 1, {text_limit}) ELSE {field} END"
        ));
        projections.push(format!(
            "CASE typeof({field}) WHEN 'blob' THEN length({field}) ELSE NULL END"
        ));
    }
    Some(format!(
        "SELECT {} FROM {} LIMIT ?1 OFFSET ?2",
        projections.join(", "),
        quote_sqlite_identifier(table),
    ))
}

/// Estimates a readable pixel width for one SQLite table column based on its header and page cells.
pub fn estimate_sqlite_column_width(
    column: &SqliteColumnInfo,
    rows: &[Vec<SqliteCell>],
    col_index: usize,
) -> f32 {
    let mut max_chars = column.name.chars().count();
    if column.is_generated {
        max_chars = max_chars.saturating_add(4);
    }
    for row in rows.iter().take(SQLITE_PAGE_SIZE) {
        if let Some(cell) = row.get(col_index) {
            let line_chars = cell
                .display
                .lines()
                .next()
                .map_or(0, |line| line.chars().count())
                .min(40);
            max_chars = max_chars.max(line_chars);
        }
    }
    ((max_chars as f32) * 7.2 + 28.0).clamp(112.0, 280.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_range_handles_empty_single_and_multi_page_tables() {
        let empty = sqlite_page_range(0, 0, SQLITE_PAGE_SIZE);
        assert_eq!(empty.start_row, 0);
        assert_eq!(empty.end_row, 0);
        assert_eq!(empty.page_count, 1);
        assert!(!empty.has_prev);
        assert!(!empty.has_next);

        let first = sqlite_page_range(125, 0, 50);
        assert_eq!((first.page, first.page_count, first.start_row, first.end_row, first.offset), (0, 3, 1, 50, 0));
        assert!(!first.has_prev);
        assert!(first.has_next);

        let middle = sqlite_page_range(125, 1, 50);
        assert_eq!((middle.page, middle.start_row, middle.end_row, middle.offset), (1, 51, 100, 50));
        assert!(middle.has_prev);
        assert!(middle.has_next);

        let last = sqlite_page_range(125, 99, 50);
        assert_eq!((last.page, last.start_row, last.end_row, last.offset), (2, 101, 125, 100));
        assert!(last.has_prev);
        assert!(!last.has_next);
    }

    #[test]
    fn quotes_identifiers_and_filters_internal_and_hidden_columns() {
        assert_eq!(quote_sqlite_identifier("orders"), "\"orders\"");
        assert_eq!(quote_sqlite_identifier("weird \"name\""), "\"weird \"\"name\"\"\"");
        assert!(is_internal_sqlite_table("sqlite_sequence"));
        assert!(is_internal_sqlite_table("SQLITE_stat1"));
        assert!(!is_internal_sqlite_table("sqliteData"));
        assert!(is_virtual_table_sql(Some("  create virtual table docs using fts5(body)")));
        assert!(!is_virtual_table_sql(Some("CREATE TABLE docs (id INTEGER)")));
        assert!(include_table_xinfo_column(0));
        assert!(!include_table_xinfo_column(1));
        assert!(include_table_xinfo_column(2));
        assert!(include_table_xinfo_column(3));
        assert!(!is_generated_xinfo_column(0));
        assert!(is_generated_xinfo_column(2));
        assert!(is_generated_xinfo_column(3));
    }

    #[test]
    fn formats_cells_including_blob_hex_summary_null_real_and_truncation() {
        assert_eq!(SqliteCell::null().display, "NULL");
        assert_eq!(SqliteCell::integer(-42).display, "-42");
        assert_eq!(SqliteCell::real(-0.0).display, "0");
        assert_eq!(SqliteCell::real(3.5).display, "3.5");

        let long_text = "a".repeat(SQLITE_MAX_CELL_CHARS + 25);
        let text_cell = SqliteCell::text(&long_text);
        assert_eq!(text_cell.display.chars().count(), SQLITE_MAX_CELL_CHARS);
        assert!(text_cell.truncated);

        let empty_blob = SqliteCell::blob(0, &[]);
        assert_eq!(empty_blob.display, "[blob: 0 bytes]");
        assert!(!empty_blob.truncated);

        let short_blob = SqliteCell::blob(4, &[0xde, 0xad, 0xbe, 0xef]);
        assert_eq!(short_blob.display, "[blob: 4 bytes · de ad be ef]");
        assert!(!short_blob.truncated);

        let large_blob = SqliteCell::blob(1024, &[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
        assert_eq!(large_blob.display, "[blob: 1,024 bytes · 89 50 4e 47 0d 0a 1a 0a…]");
        assert!(large_blob.truncated);
    }

    #[test]
    fn builds_bounded_select_sql_with_quoted_identifiers() {
        let cols = vec![
            SqliteColumnInfo {
                name: "order".into(),
                declared_type: "INTEGER".into(),
                is_primary_key: true,
                is_generated: false,
            },
            SqliteColumnInfo {
                name: "a\"b".into(),
                declared_type: "BLOB".into(),
                is_primary_key: false,
                is_generated: false,
            },
        ];
        let sql = build_sqlite_page_select_sql("my \"table\"", &cols).expect("sql");
        assert!(sql.contains("FROM \"my \"\"table\"\"\" LIMIT ?1 OFFSET ?2"));
        assert!(sql.contains("\"order\""));
        assert!(sql.contains("\"a\"\"b\""));
        assert!(build_sqlite_page_select_sql("empty", &[]).is_none());
    }

    #[test]
    fn builds_percent_encoded_immutable_file_uri() {
        assert_eq!(
            sqlite_immutable_file_uri("/tmp/my db #1?x%20.sqlite"),
            "file:///tmp/my%20db%20%231%3Fx%2520.sqlite?immutable=1"
        );
        assert_eq!(
            sqlite_immutable_file_uri(r"C:\Users\me\state v2.sqlite"),
            "file:///C:/Users/me/state%20v2.sqlite?immutable=1"
        );
    }
}
