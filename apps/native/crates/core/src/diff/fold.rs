//! Diff collapse helpers, GitHub-style Viewed file folding (`pingdotgg/t3code#37605`),
//! hidden-test collapse preservation (`pingdotgg/t3code#39213`), and withheld-file cut-line notes
//! (`pingdotgg/t3code#37007`).
//!
//! Ports `apps/web/src/lib/diffCollapse.ts`, `packages/client-runtime/src/pullRequestDiffFold.ts`,
//! and `packages/shared/src/diffCutLines.ts`.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use regex::Regex;

/// `areAllDiffFilesCollapsed` (`apps/web/src/lib/diffCollapse.ts`).
pub fn are_all_diff_files_collapsed(file_keys: &[String], collapsed_file_keys: &HashSet<String>) -> bool {
    !file_keys.is_empty() && file_keys.iter().all(|key| collapsed_file_keys.contains(key))
}

/// `toggleAllDiffFiles` (`apps/web/src/lib/diffCollapse.ts`): expands or collapses all of
/// `file_keys`; every other file, such as a hidden test file, keeps its fold.
pub fn toggle_all_diff_files(file_keys: &[String], collapsed_file_keys: &HashSet<String>) -> HashSet<String> {
    let collapse = !are_all_diff_files_collapsed(file_keys, collapsed_file_keys);
    let mut next = collapsed_file_keys.clone();
    for key in file_keys {
        if collapse {
            next.insert(key.clone());
        } else {
            next.remove(key);
        }
    }
    next
}

/// `DiffFoldOverride` (`packages/client-runtime/src/pullRequestDiffFold.ts`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffFoldOverride {
    Expanded,
    Folded,
}

/// The reader's own fold choice per file. It is absolute, so a Viewed mark loading later keeps it.
pub type FileDiffFoldChoices = HashMap<String, bool>;

/// `isFileDiffCollapsed`: explicit reader choice wins; otherwise bulk override wins; otherwise
/// defaults to `viewed` (`true` folds Viewed files like GitHub).
pub fn is_file_diff_collapsed(
    file_key: &str,
    fold_override: Option<DiffFoldOverride>,
    choices: &FileDiffFoldChoices,
    viewed: bool,
) -> bool {
    if let Some(&choice) = choices.get(file_key) {
        return choice;
    }
    match fold_override {
        None => viewed,
        Some(DiffFoldOverride::Folded) => true,
        Some(DiffFoldOverride::Expanded) => false,
    }
}

/// `withFileDiffFold`: records or clears an explicit per-file fold choice.
pub fn with_file_diff_fold(
    choices: &mut FileDiffFoldChoices,
    file_key: &str,
    collapsed: Option<bool>,
) -> bool {
    if choices.get(file_key).copied() == collapsed {
        return false;
    }
    match collapsed {
        Some(value) => {
            choices.insert(file_key.to_owned(), value);
        }
        None => {
            choices.remove(file_key);
        }
    }
    true
}

/// `DiffFoldGroup`: one bulk override + explicit per-file choices.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiffFoldGroup {
    pub override_state: Option<DiffFoldOverride>,
    pub choices: FileDiffFoldChoices,
}

/// Test files keep their own group, so a bulk fold while they are hidden leaves them as they were
/// (`PullRequestDiffFolds` in `packages/client-runtime/src/pullRequestDiffFold.ts`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PullRequestDiffFolds {
    pub source: DiffFoldGroup,
    pub test: DiffFoldGroup,
}

impl PullRequestDiffFolds {
    pub fn group(&self, is_test: bool) -> &DiffFoldGroup {
        if is_test { &self.test } else { &self.source }
    }

    pub fn group_mut(&mut self, is_test: bool) -> &mut DiffFoldGroup {
        if is_test { &mut self.test } else { &mut self.source }
    }

    pub fn is_collapsed(&self, fold_key: &str, is_test: bool, viewed: bool) -> bool {
        let group = self.group(is_test);
        is_file_diff_collapsed(fold_key, group.override_state, &group.choices, viewed)
    }

    pub fn set_choice(&mut self, fold_key: &str, is_test: bool, collapsed: Option<bool>) -> bool {
        with_file_diff_fold(&mut self.group_mut(is_test).choices, fold_key, collapsed)
    }

    /// Applies a bulk fold/expand override (`foldAllPullRequestDiffs`). While test files are hidden,
    /// pass `Some(shown_test_keys)` so only revealed test files follow; hidden tests keep their
    /// choices and fallback.
    pub fn fold_all(&mut self, override_state: DiffFoldOverride, hidden_tests_shown_keys: Option<&[String]>) {
        let all = DiffFoldGroup {
            override_state: Some(override_state),
            choices: HashMap::new(),
        };
        match hidden_tests_shown_keys {
            None => {
                self.source = all.clone();
                self.test = all;
            }
            Some(shown_keys) => {
                self.source = all;
                let collapsed = override_state == DiffFoldOverride::Folded;
                for key in shown_keys {
                    with_file_diff_fold(&mut self.test.choices, key, Some(collapsed));
                }
            }
        }
    }

    /// Updates a file's fold state after toggling Viewed: when no bulk override is active, drops
    /// the explicit choice so `viewed` drives the fold naturally; under a bulk override, records
    /// the explicit fold matching `viewed` (`PullRequestCodeTab.tsx`).
    pub fn on_viewed_toggled(&mut self, fold_key: &str, is_test: bool, viewed: bool) {
        let has_override = self.group(is_test).override_state.is_some();
        let next = if has_override { Some(viewed) } else { None };
        self.set_choice(fold_key, is_test, next);
    }
}

/// Regex matching the server's cut-line suffix (`packages/shared/src/diffCutLines.ts`).
static CUT_LINE_NOTE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r" …\[[\d,]+ more characters\]$").unwrap());

/// Formats a cut-line note with comma-grouped character count (`cutLineNote`).
pub fn cut_line_note(omitted_characters: usize) -> String {
    let digits = omitted_characters.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (ix, ch) in digits.chars().enumerate() {
        let remaining = digits.len() - ix;
        if ix > 0 && remaining % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(ch);
    }
    format!(" …[{grouped} more characters]")
}

/// Byte offset where a cut line's note starts (past its leading space), or `None` for a whole line
/// (`cutLineNoteStart` in `packages/shared/src/diffCutLines.ts`).
pub fn cut_line_note_start(line: &str) -> Option<usize> {
    if !line.ends_with("more characters]") {
        return None;
    }
    CUT_LINE_NOTE.find(line).map(|m| m.start() + 1)
}

/// Whether any line in `patch` ends with a cut-line note (`hasCutLines`).
pub fn has_cut_lines(patch: &str) -> bool {
    patch.lines().any(|line| cut_line_note_start(line.trim_end_matches('\r')).is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_collapse_preserves_hidden_files_through_toggle_all() {
        let shown = vec!["src/a.rs".to_owned(), "src/b.rs".to_owned()];
        let initial = HashSet::from(["tests/hidden_test.rs".to_owned()]);
        assert!(!are_all_diff_files_collapsed(&shown, &initial));

        let collapsed = toggle_all_diff_files(&shown, &initial);
        assert!(are_all_diff_files_collapsed(&shown, &collapsed));
        assert!(collapsed.contains("tests/hidden_test.rs"));

        let expanded = toggle_all_diff_files(&shown, &collapsed);
        assert!(!are_all_diff_files_collapsed(&shown, &expanded));
        assert!(expanded.contains("tests/hidden_test.rs"), "hidden file keeps its fold");
        assert!(!are_all_diff_files_collapsed(&[], &expanded));
    }

    #[test]
    fn pull_request_diff_folds_follow_github_viewed_and_preserve_hidden_tests() {
        let mut folds = PullRequestDiffFolds::default();
        // Default: viewed files fold, unviewed stay open.
        assert!(folds.is_collapsed("a", false, true));
        assert!(!folds.is_collapsed("a", false, false));

        // Explicit reader unfold survives a later Viewed mark.
        folds.set_choice("a", false, Some(false));
        assert!(!folds.is_collapsed("a", false, true));

        // Bulk fold while test files are hidden folds source + revealed test only; hidden test stays open.
        folds.fold_all(DiffFoldOverride::Folded, Some(&["tests/revealed.rs".to_owned()]));
        assert!(folds.is_collapsed("a", false, false));
        assert!(folds.is_collapsed("tests/revealed.rs", true, false));
        assert!(!folds.is_collapsed("tests/hidden.rs", true, false));

        // Marking a file unviewed under a bulk Folded override explicitly unfolds it.
        folds.on_viewed_toggled("a", false, false);
        assert!(!folds.is_collapsed("a", false, false));

        // Bulk expand with tests visible resets both groups.
        folds.fold_all(DiffFoldOverride::Expanded, None);
        assert!(!folds.is_collapsed("a", false, true));
        assert!(!folds.is_collapsed("tests/hidden.rs", true, true));
    }

    #[test]
    fn cut_line_notes_detect_and_locate_truncated_diff_lines() {
        assert_eq!(cut_line_note(1234), " …[1,234 more characters]");
        assert_eq!(cut_line_note(42), " …[42 more characters]");
        let line = "+const data = \"abc\" …[12,345 more characters]";
        let start = cut_line_note_start(line).expect("cut note");
        assert_eq!(&line[start..], "…[12,345 more characters]");
        assert_eq!(cut_line_note_start("+const normal = 1;"), None);
        assert!(has_cut_lines(&format!("@@ -1 +1 @@\n{line}\n")));
        assert!(!has_cut_lines("@@ -1 +1 @@\n+hello\n"));
    }
}
