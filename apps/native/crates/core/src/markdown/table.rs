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
}

fn escape_markdown_cell(cell: &str) -> String {
    cell.replace('|', "\\|").replace(['\r', '\n'], " ")
}

fn collect_cell_text(cell: &mdast::TableCell) -> String {
    fn visit(node: &mdast::Node, out: &mut String) {
        match node {
            mdast::Node::Text(t) => out.push_str(&t.value),
            mdast::Node::InlineCode(c) => out.push_str(&c.value),
            mdast::Node::Break(_) => out.push(' '),
            mdast::Node::Image(img) => out.push_str(&img.alt),
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
    }
}
