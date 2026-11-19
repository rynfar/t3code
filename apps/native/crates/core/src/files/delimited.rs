//! Delimited CSV/TSV preview parser and virtual table model.
//! Ports `packages/shared/src/delimitedPreview.ts`, `apps/web/src/components/files/DelimitedTablePreview.tsx`,
//! and `apps/web/src/components/files/VirtualTablePreview.tsx` (sortable, resizable columns over
//! virtualized rows).

use std::cmp::Ordering;

/// Default bounds for in-memory delimited preview parsing.
pub const DEFAULT_MAX_ROWS: usize = 50_000;
pub const DEFAULT_MAX_COLUMNS: usize = 100;
pub const DEFAULT_MAX_CELL_CHARS: usize = 2_000;
pub const MIN_COLUMN_WIDTH: f32 = 64.0;
pub const DEFAULT_MIN_AUTO_WIDTH: f32 = 128.0;
pub const DEFAULT_MAX_AUTO_WIDTH: f32 = 320.0;
pub const MAX_COLUMN_WIDTH: f32 = 640.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DelimitedLimits {
    pub max_rows: usize,
    pub max_columns: usize,
    pub max_cell_chars: usize,
}

impl Default for DelimitedLimits {
    fn default() -> Self {
        Self {
            max_rows: DEFAULT_MAX_ROWS,
            max_columns: DEFAULT_MAX_COLUMNS,
            max_cell_chars: DEFAULT_MAX_CELL_CHARS,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedDelimited {
    pub rows: Vec<Vec<String>>,
    pub truncated: bool,
}

/// RFC-4180 delimited parser matching `parseDelimitedPreview` in `packages/shared/src/delimitedPreview.ts`.
pub fn parse_delimited_preview(text: &str, delimiter: char, limits: DelimitedLimits) -> ParsedDelimited {
    let body = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut cell = String::new();
    let mut cell_chars = 0usize;
    let mut quoted = false;
    let mut truncated = false;
    let mut row_has_content = false;

    let mut chars = body.chars().peekable();
    while let Some(ch) = chars.next() {
        row_has_content = true;
        if ch == '"' {
            if quoted && chars.peek() == Some(&'"') {
                chars.next();
                if cell_chars < limits.max_cell_chars {
                    cell.push('"');
                    cell_chars += 1;
                } else {
                    truncated = true;
                }
                continue;
            }
            if quoted || cell.is_empty() {
                quoted = !quoted;
                continue;
            }
        }

        if !quoted && (ch == delimiter || ch == '\n' || ch == '\r') {
            if row.len() < limits.max_columns {
                row.push(std::mem::take(&mut cell));
            } else {
                cell.clear();
                truncated = true;
            }
            cell_chars = 0;
            if ch != delimiter {
                rows.push(std::mem::take(&mut row));
                if ch == '\r' && chars.peek() == Some(&'\n') {
                    chars.next();
                }
                row_has_content = false;
                if rows.len() == limits.max_rows {
                    let remaining = chars.peek().is_some();
                    return ParsedDelimited { rows, truncated: truncated || remaining };
                }
            }
        } else if cell_chars < limits.max_cell_chars {
            cell.push(ch);
            cell_chars += 1;
        } else {
            truncated = true;
        }
    }

    if row_has_content {
        if row.len() < limits.max_columns {
            row.push(cell);
        } else {
            truncated = true;
        }
        rows.push(row);
    }

    ParsedDelimited { rows, truncated: truncated || quoted }
}

/// Spreadsheet-style fallback column label (`VirtualTablePreview.tsx`: `A..Z`, `AA..`).
pub fn column_label(index: usize) -> String {
    let mut n = index;
    let mut letters = Vec::new();
    loop {
        letters.push((b'A' + (n % 26) as u8) as char);
        if n < 26 {
            break;
        }
        n = n / 26 - 1;
    }
    letters.into_iter().rev().collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDirection {
    Ascending,
    Descending,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColumnSort {
    pub column: usize,
    pub direction: SortDirection,
}

/// Virtualized, sortable, resizable delimited table state.
#[derive(Debug, Clone)]
pub struct DelimitedTable {
    pub delimiter: char,
    pub header: Vec<String>,
    rows: Vec<Vec<String>>,
    /// Index permutation into `rows` so sorting does not clone row data.
    order: Vec<usize>,
    pub column_widths: Vec<f32>,
    pub sort: Option<ColumnSort>,
    pub truncated: bool,
}

impl DelimitedTable {
    pub fn parse(text: &str, delimiter: char, limits: DelimitedLimits) -> Self {
        let parsed = parse_delimited_preview(text, delimiter, limits);
        let mut iter = parsed.rows.into_iter();
        let raw_header = iter.next().unwrap_or_default();
        let rows: Vec<Vec<String>> = iter.collect();
        let max_cols = raw_header
            .len()
            .max(rows.iter().map(Vec::len).max().unwrap_or(0));

        let mut header = Vec::with_capacity(max_cols);
        for index in 0..max_cols {
            let label = raw_header
                .get(index)
                .map(|cell| cell.trim())
                .filter(|cell| !cell.is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| column_label(index));
            header.push(label);
        }

        // Estimate column widths from header + first 20 data rows (matching VirtualTablePreview's measurement sample).
        let mut column_widths = Vec::with_capacity(max_cols);
        for col in 0..max_cols {
            let header_chars = header.get(col).map_or(0, |s| s.chars().count());
            let sample_chars = rows
                .iter()
                .take(20)
                .filter_map(|row| row.get(col))
                .map(|cell| cell.lines().map(|line| line.chars().count()).max().unwrap_or(0))
                .max()
                .unwrap_or(0);
            let longest = header_chars.max(sample_chars);
            let width = (longest as f32 * 7.6 + 32.0).clamp(DEFAULT_MIN_AUTO_WIDTH, DEFAULT_MAX_AUTO_WIDTH);
            column_widths.push(width);
        }

        let order = (0..rows.len()).collect();
        Self {
            delimiter,
            header,
            rows,
            order,
            column_widths,
            sort: None,
            truncated: parsed.truncated,
        }
    }

    pub fn column_count(&self) -> usize {
        self.header.len()
    }

    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.header.is_empty() && self.rows.is_empty()
    }

    pub fn total_width(&self, row_gutter: f32) -> f32 {
        row_gutter + self.column_widths.iter().sum::<f32>()
    }

    /// Returns `(1-based original row number, row cells)` for display position `display_index`.
    pub fn display_row(&self, display_index: usize) -> Option<(usize, &[String])> {
        let source_index = *self.order.get(display_index)?;
        let row = self.rows.get(source_index)?;
        Some((source_index + 1, row.as_slice()))
    }

    /// Resizes a column, clamped to `[MIN_COLUMN_WIDTH, MAX_COLUMN_WIDTH]`.
    pub fn resize_column(&mut self, column: usize, width: f32) {
        if let Some(slot) = self.column_widths.get_mut(column) {
            *slot = width.clamp(MIN_COLUMN_WIDTH, MAX_COLUMN_WIDTH);
        }
    }

    /// Toggles sort on `column`: None -> Ascending -> Descending -> None.
    pub fn toggle_sort(&mut self, column: usize) {
        if column >= self.header.len() {
            return;
        }
        let next = match self.sort {
            Some(current) if current.column == column && current.direction == SortDirection::Ascending => {
                Some(ColumnSort { column, direction: SortDirection::Descending })
            }
            Some(current) if current.column == column && current.direction == SortDirection::Descending => None,
            _ => Some(ColumnSort { column, direction: SortDirection::Ascending }),
        };
        self.sort = next;
        match next {
            None => {
                self.order.clear();
                self.order.extend(0..self.rows.len());
            }
            Some(ColumnSort { column, direction }) => {
                let rows = &self.rows;
                self.order.sort_by(|&a, &b| {
                    let left = rows[a].get(column).map(String::as_str).unwrap_or("");
                    let right = rows[b].get(column).map(String::as_str).unwrap_or("");
                    let ord = compare_cells(left, right).then_with(|| a.cmp(&b));
                    match direction {
                        SortDirection::Ascending => ord,
                        SortDirection::Descending => ord.reverse(),
                    }
                });
            }
        }
    }
}

/// Numeric-aware, case-insensitive comparison with empty cells pushed to the end on ascending sort.
fn compare_cells(left: &str, right: &str) -> Ordering {
    let left_t = left.trim();
    let right_t = right.trim();
    match (left_t.is_empty(), right_t.is_empty()) {
        (true, true) => return Ordering::Equal,
        (true, false) => return Ordering::Greater,
        (false, true) => return Ordering::Less,
        (false, false) => {}
    }
    if let (Ok(lnum), Ok(rnum)) = (parse_numeric(left_t), parse_numeric(right_t))
        && lnum.is_finite()
        && rnum.is_finite()
        && let Some(ord) = lnum.partial_cmp(&rnum)
    {
        return ord;
    }
    left_t.to_lowercase().cmp(&right_t.to_lowercase()).then_with(|| left_t.cmp(right_t))
}

fn parse_numeric(value: &str) -> Result<f64, std::num::ParseFloatError> {
    if value.contains(',') && !value.contains(' ') {
        value.replace(',', "").parse::<f64>()
    } else {
        value.parse::<f64>()
    }
}
