//! GFM table extraction and Markdown / CSV serialization (`apps/web/src/components/chat/MarkdownTable.tsx`).

use markdown::mdast;

/// Column horizontal alignment matching GFM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnAlignment {
    None,
    Left,
    Center,
    Right,
}

impl From<mdast::AlignKind> for ColumnAlignment {
    fn from(value: mdast::AlignKind) -> Self {
        match value {
            mdast::AlignKind::Left => Self::Left,
            mdast::AlignKind::Center => Self::Center,
            mdast::AlignKind::Right => Self::Right,
            mdast::AlignKind::None => Self::None,
        }
    }
}

/// Extracted plain-text table data ready for Markdown or CSV copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableData {
    pub align: Vec<ColumnAlignment>,
    pub rows: Vec<Vec<String>>,
}

impl TableData {
    /// Extracts a `TableData` from a `markdown::mdast::Table` node.
    pub fn from_mdast(table: &mdast::Table) -> Self {
        let align = table.align.iter().copied().map(ColumnAlignment::from).collect();
        let rows = table
            .children
            .iter()
            .filter_map(|child| match child {
                mdast::Node::TableRow(row) => Some(
                    row.children
                        .iter()
                        .filter_map(|cell| match cell {
                            mdast::Node::TableCell(cell) => Some(collect_cell_text(cell)),
                            _ => None,
                        })
                        .collect(),
                ),
                _ => None,
            })
            .collect();
        Self { align, rows }
    }

    /// Extracts raw per-cell Markdown slices from `raw_source` for tables containing rich inline nodes
    /// (images/videos) that need custom block rendering per cell.
    pub fn cell_markdown_rows(table: &mdast::Table, raw_source: &str) -> Vec<Vec<String>> {
        table
            .children
            .iter()
            .filter_map(|child| match child {
                mdast::Node::TableRow(row) => Some(
                    row.children
                        .iter()
                        .filter_map(|cell| match cell {
                            mdast::Node::TableCell(cell) => Some(extract_cell_markdown(cell, raw_source)),
                            _ => None,
                        })
                        .collect(),
                ),
                _ => None,
            })
            .collect()
    }

    /// Serializes the table as GitHub-Flavored Markdown.
    pub fn to_markdown(&self) -> String {
        if self.rows.is_empty() {
            return String::new();
        }
        let col_count = self.rows.iter().map(Vec::len).max().unwrap_or(0).max(self.align.len());
        if col_count == 0 {
            return String::new();
        }
        let mut out = String::new();
        let write_row = |out: &mut String, row: &[String]| {
            out.push('|');
            for col in 0..col_count {
                out.push(' ');
                let cell = row.get(col).map(String::as_str).unwrap_or("");
                out.push_str(&escape_markdown_cell(cell));
                out.push_str(" |");
            }
            out.push('\n');
        };
        write_row(&mut out, &self.rows[0]);
        out.push('|');
        for col in 0..col_count {
            let align = self.align.get(col).copied().unwrap_or(ColumnAlignment::None);
            let sep = match align {
                ColumnAlignment::None => "---",
                ColumnAlignment::Left => ":---",
                ColumnAlignment::Center => ":---:",
                ColumnAlignment::Right => "---:",
            };
            out.push(' ');
            out.push_str(sep);
            out.push_str(" |");
        }
        out.push('\n');
        for row in self.rows.iter().skip(1) {
            write_row(&mut out, row);
        }
        if out.ends_with('\n') {
            out.pop();
        }
        out
    }

    /// Serializes the table as RFC 4180 CSV.
    pub fn to_csv(&self) -> String {
        let mut out = String::new();
        for (row_idx, row) in self.rows.iter().enumerate() {
            if row_idx > 0 {
                out.push('\n');
            }
            for (col_idx, cell) in row.iter().enumerate() {
                if col_idx > 0 {
                    out.push(',');
                }
                if cell.contains([',', '"', '\n', '\r']) {
                    out.push('"');
                    out.push_str(&cell.replace('"', "\"\""));
                    out.push('"');
                } else {
                    out.push_str(cell);
                }
            }
        }
        out
    }

    /// Serializes the table as standalone inline-styled HTML matching `serializeTableElementToHtml`.
    pub fn to_html(&self) -> String {
        if self.rows.is_empty() || self.rows.iter().all(|row| row.is_empty()) {
            return String::new();
        }
        const TABLE_STYLE: &str = "border-collapse:collapse";
        const CELL_STYLE: &str = "border:1px solid #d0d7de;padding:6px 12px;vertical-align:top";
        const HEADER_STYLE: &str = "border:1px solid #d0d7de;padding:6px 12px;vertical-align:top;background-color:#f6f8fa;color:#1f2328;font-weight:600";

        let col_count = self
            .rows
            .iter()
            .map(Vec::len)
            .max()
            .unwrap_or(0)
            .max(self.align.len());
        let align_suffix = |col: usize| match self.align.get(col).copied().unwrap_or(ColumnAlignment::None) {
            ColumnAlignment::Left => ";text-align:left",
            ColumnAlignment::Center => ";text-align:center",
            ColumnAlignment::Right => ";text-align:right",
            ColumnAlignment::None => "",
        };
        let render_row = |row: &[String], header: bool| -> String {
            let tag = if header { "th" } else { "td" };
            let base_style = if header { HEADER_STYLE } else { CELL_STYLE };
            let mut cells = String::new();
            for col in 0..col_count {
                let raw = row.get(col).map(String::as_str).unwrap_or("");
                let escaped = raw
                    .replace('&', "&amp;")
                    .replace('<', "&lt;")
                    .replace('>', "&gt;")
                    .replace('"', "&quot;");
                cells.push_str(&format!("<{tag} style=\"{base_style}{}\">{escaped}</{tag}>", align_suffix(col)));
            }
            format!("    <tr>{cells}</tr>")
        };

        let mut lines = vec![format!("<table dir=\"ltr\" style=\"{TABLE_STYLE}\">")];
        if let Some(head) = self.rows.first() {
            lines.push("  <thead>".into());
            lines.push(render_row(head, true));
            lines.push("  </thead>".into());
        }
        if self.rows.len() > 1 {
            lines.push("  <tbody>".into());
            for row in &self.rows[1..] {
                lines.push(render_row(row, false));
            }
            lines.push("  </tbody>".into());
        }
        lines.push("</table>".into());
        lines.join("\n")
    }
}

fn escape_markdown_cell(cell: &str) -> String {
    cell.replace('|', "\\|").replace(['\r', '\n'], " ")
}

fn extract_cell_markdown(cell: &mdast::TableCell, raw_source: &str) -> String {
    if let (Some(first), Some(last)) = (cell.children.first().and_then(|n| n.position()), cell.children.last().and_then(|n| n.position()))
        && let Some(slice) = raw_source.get(first.start.offset..last.end.offset)
    {
        return slice.trim().to_owned();
    }
    collect_cell_text(cell)
}

fn collect_cell_text(cell: &mdast::TableCell) -> String {
    fn visit(node: &mdast::Node, out: &mut String) {
        match node {
            mdast::Node::Text(t) => out.push_str(&t.value),
            mdast::Node::InlineCode(c) => out.push_str(&c.value),
            mdast::Node::Break(_) => out.push(' '),
            mdast::Node::Image(img) => out.push_str(&crate::media::strip_media_title_meta(&img.alt)),
            _ => {
                if let Some(children) = node.children() {
                    for child in children {
                        visit(child, out);
                    }
                }
            }
        }
    }
    let mut out = String::new();
    for child in &cell.children {
        visit(child, &mut out);
    }
    out.trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_table_to_markdown_and_csv() {
        let table = TableData {
            align: vec![ColumnAlignment::Left, ColumnAlignment::Right],
            rows: vec![
                vec!["Name".into(), "Value".into()],
                vec!["alpha | beta".into(), "42,000".into()],
                vec!["quote \"x\"".into(), "10".into()],
            ],
        };
        assert_eq!(
            table.to_markdown(),
            "| Name | Value |\n| :--- | ---: |\n| alpha \\| beta | 42,000 |\n| quote \"x\" | 10 |"
        );
        assert_eq!(
            table.to_csv(),
            "Name,Value\nalpha | beta,\"42,000\"\n\"quote \"\"x\"\"\",10"
        );
        assert_eq!(
            table.to_html(),
            "<table dir=\"ltr\" style=\"border-collapse:collapse\">\n  <thead>\n    <tr><th style=\"border:1px solid #d0d7de;padding:6px 12px;vertical-align:top;background-color:#f6f8fa;color:#1f2328;font-weight:600;text-align:left\">Name</th><th style=\"border:1px solid #d0d7de;padding:6px 12px;vertical-align:top;background-color:#f6f8fa;color:#1f2328;font-weight:600;text-align:right\">Value</th></tr>\n  </thead>\n  <tbody>\n    <tr><td style=\"border:1px solid #d0d7de;padding:6px 12px;vertical-align:top;text-align:left\">alpha | beta</td><td style=\"border:1px solid #d0d7de;padding:6px 12px;vertical-align:top;text-align:right\">42,000</td></tr>\n    <tr><td style=\"border:1px solid #d0d7de;padding:6px 12px;vertical-align:top;text-align:left\">quote &quot;x&quot;</td><td style=\"border:1px solid #d0d7de;padding:6px 12px;vertical-align:top;text-align:right\">10</td></tr>\n  </tbody>\n</table>"
        );
    }

    #[test]
    fn extracts_raw_cell_markdown_and_strips_media_hints_marker() {
        let hints_title = crate::media::encode_media_title(
            crate::media::MediaHints { width: Some(120), height: None, explicit_video: false },
            None,
        )
        .unwrap();
        let raw = format!("| Preview | Note |\n| --- | --- |\n| ![Shot](shot.png \"{hints_title}\") | **Ok** |");
        let root = markdown::to_mdast(&raw, &markdown::ParseOptions::gfm()).unwrap();
        let mdast::Node::Table(tbl) = &root.children().unwrap()[0] else { panic!("table") };
        let cells = TableData::cell_markdown_rows(tbl, &raw);
        assert_eq!(cells[1][0], format!("![Shot](shot.png \"{hints_title}\")"));
        assert_eq!(cells[1][1], "**Ok**");
        let data = TableData::from_mdast(tbl);
        assert_eq!(data.rows[1], vec!["Shot".to_owned(), "Ok".to_owned()]);
    }
}
