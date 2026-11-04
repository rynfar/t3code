//! Immutable patch payloads shared by successive lazy comparison snapshots.
use super::Hunk;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq)]
pub struct FileHunks(Arc<Data>);

#[derive(Debug, PartialEq)]
struct Data {
    hunks: Vec<Hunk>,
    line_count: usize,
    max_line_len: usize,
    gutter_digits: usize,
}

impl From<Vec<Hunk>> for FileHunks {
    fn from(hunks: Vec<Hunk>) -> Self {
        let line_count = hunks.iter().map(|h| h.lines.len()).sum();
        let max_line_len = hunks.iter().flat_map(|h| &h.lines).map(|l| l.text.len()).max().unwrap_or(0);
        let largest = hunks
            .iter()
            .map(|h| h.old_start.saturating_add(h.old_count).max(h.new_start.saturating_add(h.new_count)))
            .max()
            .unwrap_or(1)
            .max(1);
        Self(Arc::new(Data {
            hunks,
            line_count,
            max_line_len,
            gutter_digits: largest.ilog10() as usize + 1,
        }))
    }
}

impl std::ops::Deref for FileHunks {
    type Target = [Hunk];
    fn deref(&self) -> &[Hunk] {
        &self.0.hunks
    }
}

impl<'a> IntoIterator for &'a FileHunks {
    type Item = &'a Hunk;
    type IntoIter = std::slice::Iter<'a, Hunk>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl FileHunks {
    pub fn shares_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
    pub fn max_line_len(&self) -> usize {
        self.0.max_line_len
    }
    pub fn gutter_digits(&self) -> usize {
        self.0.gutter_digits
    }
    pub fn line_count(&self) -> usize {
        self.0.line_count
    }
}
