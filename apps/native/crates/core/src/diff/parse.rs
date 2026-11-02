//! Unified diff parsing, the subset of `@pierre/diffs` `parsePatchFiles` the panel needs: git
//! extended headers (new/deleted/renamed/copied, binary), plain `---`/`+++` patches, quoted
//! paths, and `\ No newline at end of file`. Never panics on malformed input.

use std::sync::Arc;

use super::{ChangeKind, DiffLine, FileDiff, Hunk, LineKind, ParsedPatch};

/// Pierre's `--diffs-tab-size`.
const TAB_SIZE: usize = 2;

pub fn parse_patch(patch: &str) -> ParsedPatch {
    if patch.trim().is_empty() {
        return ParsedPatch::Empty;
    }
    let files = Parser { lines: patch.lines().collect(), at: 0 }.files();
    if files.is_empty() {
        ParsedPatch::Raw { text: patch.to_owned(), reason: "Unsupported diff format. Showing raw patch." }
    } else {
        ParsedPatch::Files(files)
    }
}

struct Parser<'a> {
    lines: Vec<&'a str>,
    at: usize,
}

#[derive(Default)]
struct Header {
    git_old: Option<String>,
    git_new: Option<String>,
    minus: Option<Option<String>>,
    plus: Option<Option<String>>,
    rename_from: Option<String>,
    rename_to: Option<String>,
    new_file: bool,
    deleted_file: bool,
    copied: bool,
    binary: bool,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<&'a str> {
        self.lines.get(self.at).copied()
    }

    fn files(mut self) -> Vec<FileDiff> {
        let mut files = Vec::new();
        while let Some(line) = self.peek() {
            if line.starts_with("diff --git ") || is_minus_header(line, self.lines.get(self.at + 1).copied()) {
                files.push(self.file());
            } else {
                self.at += 1;
            }
        }
        files
    }

    fn file(&mut self) -> FileDiff {
        let mut header = Header::default();
        if let Some(rest) = self.peek().and_then(|line| line.strip_prefix("diff --git ")) {
            let (old, new) = split_git_paths(rest);
            header.git_old = old;
            header.git_new = new;
            self.at += 1;
        }
        while let Some(line) = self.peek() {
            if line.starts_with("diff --git ") || line.starts_with("@@") {
                break;
            }
            if let Some(path) = line.strip_prefix("--- ") {
                if header.minus.is_some() {
                    break;
                }
                header.minus = Some(header_path(path, "a/"));
            } else if let Some(path) = line.strip_prefix("+++ ") {
                header.plus = Some(header_path(path, "b/"));
            } else if let Some(path) = line.strip_prefix("rename from ") {
                header.rename_from = Some(unquote(path));
            } else if let Some(path) = line.strip_prefix("rename to ") {
                header.rename_to = Some(unquote(path));
            } else if let Some(path) = line.strip_prefix("copy from ") {
                header.rename_from = Some(unquote(path));
                header.copied = true;
            } else if let Some(path) = line.strip_prefix("copy to ") {
                header.rename_to = Some(unquote(path));
                header.copied = true;
            } else if line.starts_with("new file mode") {
                header.new_file = true;
            } else if line.starts_with("deleted file mode") {
                header.deleted_file = true;
            } else if line.starts_with("Binary files ") || line == "GIT binary patch" {
                header.binary = true;
            } else if header.git_old.is_none() && header.minus.is_none() && !is_extended_header(line) {
                break;
            }
            self.at += 1;
        }
        let mut hunks = Vec::new();
        while let Some(line) = self.peek() {
            if line.starts_with("diff --git ") || is_minus_header(line, self.lines.get(self.at + 1).copied()) {
                break;
            }
            match parse_hunk_header(line) {
                Some(hunk) => {
                    self.at += 1;
                    hunks.push(self.hunk_body(hunk));
                }
                None => self.at += 1,
            }
        }
        build_file(header, hunks)
    }

    fn hunk_body(&mut self, mut hunk: Hunk) -> Hunk {
        let (mut old_left, mut new_left) = (hunk.old_count, hunk.new_count);
        let (mut old_line, mut new_line) = (hunk.old_start, hunk.new_start);
        while let Some(line) = self.peek() {
            if line.starts_with('\\') {
                if let Some(last) = hunk.lines.last_mut() {
                    last.no_newline_at_eof = true;
                }
                self.at += 1;
                continue;
            }
            if old_left == 0 && new_left == 0 {
                break;
            }
            let (kind, text) = match line.as_bytes().first() {
                Some(b'+') if new_left > 0 => (LineKind::Addition, &line[1..]),
                Some(b'-') if old_left > 0 => (LineKind::Deletion, &line[1..]),
                Some(b' ') if old_left > 0 && new_left > 0 => (LineKind::Context, &line[1..]),
                // Some tools strip the space from blank context lines.
                None if old_left > 0 && new_left > 0 => (LineKind::Context, ""),
                _ => break,
            };
            let (old, new) = match kind {
                LineKind::Addition => {
                    new_left -= 1;
                    new_line += 1;
                    (None, Some(new_line - 1))
                }
                LineKind::Deletion => {
                    old_left -= 1;
                    old_line += 1;
                    (Some(old_line - 1), None)
                }
                LineKind::Context => {
                    old_left -= 1;
                    new_left -= 1;
                    old_line += 1;
                    new_line += 1;
                    (Some(old_line - 1), Some(new_line - 1))
                }
            };
            hunk.lines.push(DiffLine {
                kind,
                old_line: old,
                new_line: new,
                text: expand_tabs(text),
                no_newline_at_eof: false,
            });
            self.at += 1;
        }
        hunk
    }
}

fn is_minus_header(line: &str, next: Option<&str>) -> bool {
    line.starts_with("--- ") && next.is_some_and(|next| next.starts_with("+++ "))
}

fn is_extended_header(line: &str) -> bool {
    ["index ", "old mode ", "new mode ", "similarity index ", "dissimilarity index "]
        .iter()
        .any(|prefix| line.starts_with(prefix))
}

fn build_file(header: Header, hunks: Vec<Hunk>) -> FileDiff {
    let renamed = header.rename_from.is_some() || header.rename_to.is_some();
    let old_path = match header.minus {
        Some(path) => path,
        None => header.rename_from.clone().or(header.git_old.clone()),
    };
    let new_path = match header.plus {
        Some(path) => path,
        None => header.rename_to.clone().or(header.git_new.clone()),
    };
    let (old_path, new_path) = match (old_path, new_path) {
        (None, None) => (header.rename_from.or(header.git_old), header.rename_to.or(header.git_new)),
        pair => pair,
    };
    let change = if header.new_file || header.copied || (old_path.is_none() && new_path.is_some()) {
        ChangeKind::New
    } else if header.deleted_file || (new_path.is_none() && old_path.is_some()) {
        ChangeKind::Deleted
    } else if renamed || old_path != new_path {
        if hunks.iter().all(|hunk| hunk.lines.is_empty()) {
            ChangeKind::RenamePure
        } else {
            ChangeKind::RenameChanged
        }
    } else {
        ChangeKind::Modified
    };
    let count = |kind: LineKind| {
        hunks.iter().flat_map(|hunk| &hunk.lines).filter(|line| line.kind == kind).count() as u32
    };
    FileDiff {
        additions: count(LineKind::Addition),
        deletions: count(LineKind::Deletion),
        old_path: if change == ChangeKind::New && !header.copied { None } else { old_path },
        new_path: if change == ChangeKind::Deleted { None } else { new_path },
        change,
        binary: header.binary,
        hunks,
    }
}

/// `@@ -a,b +c,d @@ context`; a missing count means 1.
fn parse_hunk_header(line: &str) -> Option<Hunk> {
    let rest = line.strip_prefix("@@ -")?;
    let (ranges, context) = rest.split_once(" @@")?;
    let (old, new) = ranges.split_once(" +")?;
    let range = |text: &str| -> Option<(u32, u32)> {
        match text.split_once(',') {
            Some((start, count)) => Some((start.parse().ok()?, count.parse().ok()?)),
            None => Some((text.parse().ok()?, 1)),
        }
    };
    let (old_start, old_count) = range(old)?;
    let (new_start, new_count) = range(new)?;
    Some(Hunk {
        old_start,
        old_count,
        new_start,
        new_count,
        context: context.trim().to_owned(),
        lines: Vec::new(),
    })
}

/// A `---`/`+++` path: `/dev/null` is `None`; drops git's prefix and any trailing timestamp.
fn header_path(text: &str, prefix: &str) -> Option<String> {
    let text = if text.starts_with('"') { text } else { text.split('\t').next().unwrap_or(text) };
    let path = unquote(text.trim_end());
    if path == "/dev/null" {
        return None;
    }
    Some(path.strip_prefix(prefix).map(str::to_owned).unwrap_or(path))
}

/// `a/<old> b/<new>`. Unquoted paths may contain spaces, so prefer the split where both halves
/// name the same file, which is every diff but a rename.
fn split_git_paths(rest: &str) -> (Option<String>, Option<String>) {
    let strip = |path: String, prefix: &str| path.strip_prefix(prefix).map(str::to_owned).unwrap_or(path);
    if rest.starts_with('"') {
        let (old, tail) = take_quoted(rest);
        let tail = tail.trim_start();
        let new = if tail.starts_with('"') { take_quoted(tail).0 } else { tail.to_owned() };
        return (Some(strip(old, "a/")), Some(strip(new, "b/")));
    }
    if let Some(index) = rest.find(" \"b/") {
        let new = take_quoted(&rest[index + 1..]).0;
        return (Some(strip(rest[..index].to_owned(), "a/")), Some(strip(new, "b/")));
    }
    let middle = rest.len() / 2;
    if rest.len() % 2 == 1 && rest.is_char_boundary(middle) && rest.as_bytes().get(middle) == Some(&b' ') {
        let (old, new) = (&rest[..middle], &rest[middle + 1..]);
        if old.strip_prefix("a/").is_some_and(|old| new.strip_prefix("b/") == Some(old)) {
            return (Some(strip(old.to_owned(), "a/")), Some(strip(new.to_owned(), "b/")));
        }
    }
    match rest.rfind(" b/") {
        Some(index) => (Some(strip(rest[..index].to_owned(), "a/")), Some(strip(rest[index + 1..].to_owned(), "b/"))),
        None => (None, None),
    }
}

fn unquote(text: &str) -> String {
    if text.starts_with('"') { take_quoted(text).0 } else { text.to_owned() }
}

/// Git's C-style quoting: `\t`, `\n`, `\"`, `\\` and octal bytes for non-ASCII.
fn take_quoted(text: &str) -> (String, &str) {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut index = 1;
    while index < bytes.len() {
        match bytes[index] {
            b'"' => {
                return (String::from_utf8_lossy(&out).into_owned(), &text[index + 1..]);
            }
            b'\\' if index + 1 < bytes.len() => {
                let escaped = bytes[index + 1];
                index += 2;
                match escaped {
                    b't' => out.push(b'\t'),
                    b'n' => out.push(b'\n'),
                    b'r' => out.push(b'\r'),
                    b'a' => out.push(7),
                    b'b' => out.push(8),
                    b'f' => out.push(12),
                    b'v' => out.push(11),
                    b'0'..=b'7' => {
                        let mut value = u32::from(escaped - b'0');
                        for _ in 0..2 {
                            match bytes.get(index) {
                                Some(digit @ b'0'..=b'7') => {
                                    value = value * 8 + u32::from(digit - b'0');
                                    index += 1;
                                }
                                _ => break,
                            }
                        }
                        out.push(value as u8);
                    }
                    other => out.push(other),
                }
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    (String::from_utf8_lossy(&out).into_owned(), "")
}

fn expand_tabs(text: &str) -> Arc<str> {
    if !text.contains('\t') {
        return Arc::from(text);
    }
    let mut out = String::with_capacity(text.len() + 8);
    let mut column = 0;
    for ch in text.chars() {
        if ch == '\t' {
            let pad = TAB_SIZE - column % TAB_SIZE;
            out.extend(std::iter::repeat_n(' ', pad));
            column += pad;
        } else {
            out.push(ch);
            column += 1;
        }
    }
    Arc::from(out)
}
