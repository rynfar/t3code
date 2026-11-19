//! Where a pull request link points inside the pull request: a tab, a comment, a diff line, the
//! checks or the commits. Ports `packages/shared/src/pullRequestAnchor.ts` and the detail
//! panel's `linkedTab`.

use std::sync::LazyLock;

use regex::Regex;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

/// `PullRequestAnchorRange`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    pub start: u64,
    pub side: Side,
    pub end: u64,
    pub end_side: Side,
    /// A GitLab line code's old-side counter: the line it names when that old line was removed
    /// (`pingdotgg/t3code#34662`).
    pub old_line: Option<u64>,
}

/// `PullRequestAnchor`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Anchor {
    Code,
    Commits,
    Checks,
    /// A file's diff, by the host's digest of its path, and the lines it names.
    Diff { digest: String, range: Option<Range> },
    /// A comment or review, normalized so line comments match the timeline's urls.
    Timeline(String),
    /// A heading or other fragment of the description.
    Summary(String),
}

/// The detail tab a link opens (`linkedTab`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkedTab {
    Summary,
    Timeline,
    Code,
}

impl Anchor {
    pub fn tab(&self) -> LinkedTab {
        match self {
            Anchor::Code | Anchor::Diff { .. } => LinkedTab::Code,
            Anchor::Commits | Anchor::Timeline(_) => LinkedTab::Timeline,
            Anchor::Checks | Anchor::Summary(_) => LinkedTab::Summary,
        }
    }
}

fn url_hash(url: &str) -> String {
    url::Url::parse(url).ok().and_then(|url| url.fragment().map(crate::media::percent_decode)).unwrap_or_default()
}

static COMMENT_ANCHOR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^(?:r|discussion-diff-)(\d+)(?:[LR]\d+(?:-[LR]\d+)?)?$").unwrap());

fn normalize_comment_anchor(anchor: &str) -> String {
    COMMENT_ANCHOR.replace(anchor, "discussion_r$1").into_owned()
}

static PATH_TAB: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)/(?:pull|-/merge_requests)/\d+/([a-z]+)(?:/(.*))?$").unwrap());
static FULL_SHA: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^[a-f0-9]{40}/?$").unwrap());

/// GitHub pull request tabs and their GitLab merge request equivalents.
fn path_tab(path: &str) -> Option<Anchor> {
    let captures = PATH_TAB.captures(path)?;
    let rest = captures.get(2).map_or("", |rest| rest.as_str());
    match captures.get(1)?.as_str().to_lowercase().as_str() {
        "files" | "changes" | "diffs" => Some(Anchor::Code),
        "commits" if rest.is_empty() => Some(Anchor::Commits),
        "commits" => FULL_SHA.is_match(rest).then_some(Anchor::Code),
        "checks" | "pipelines" => rest.is_empty().then_some(Anchor::Checks),
        _ => None,
    }
}

static GITHUB_DIFF: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^diff-([a-f0-9]+)(?:([LR])(\d+)(?:-([LR])(\d+))?)?$").unwrap());
static GITLAB_RAPID_DIFF: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^line_([a-f0-9]{9})_(A?)(\d+)$").unwrap());
static GITLAB_DIFF: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^(?:diff-content-)?([a-f0-9]{40})(?:_(\d+)_(\d+))?$").unwrap());
static GITLAB_DIFFS_PATH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)/-/merge_requests/\d+/diffs/?$").unwrap());
static GITLAB_FILE_SHA1: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^[a-f0-9]{40}$").unwrap());
static TIMELINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?:(?:issuecomment-|pullrequestreview-|discussion_r|r|commitcomment-|event-|note_|comment-)\d+|discussion-diff-\d+(?:[LR]\d+(?:-[LR]\d+)?)?)$").unwrap()
});

/// `Number.isSafeInteger(n) && n >= 1`.
fn line_number(text: &str) -> Option<u64> {
    text.parse::<u64>().ok().filter(|line| (1..=9_007_199_254_740_991).contains(line))
}

fn side(letter: &str) -> Side {
    if letter.eq_ignore_ascii_case("L") { Side::Left } else { Side::Right }
}

fn gitlab_line(start: u64, side: Side, old_line: Option<u64>) -> Option<Range> {
    if !(1..=9_007_199_254_740_991).contains(&start) {
        return None;
    }
    let old = old_line.filter(|line| (1..=9_007_199_254_740_991).contains(line));
    Some(Range { start, side, end: start, end_side: side, old_line: old })
}

/// GitLab diff anchors: Rapid Diffs `line_<9-hex>_[A]<line>`, `diff-content-<40-hex>`,
/// `<40-hex>_<old>_<new>`, or `?file=<40-hex>` on `/-/merge_requests/<n>/diffs`
/// (`pingdotgg/t3code#34662`).
fn gitlab_diff_anchor(anchor: &str, file_param: Option<&str>) -> Option<Anchor> {
    if let Some(rapid) = GITLAB_RAPID_DIFF.captures(anchor) {
        let digest = rapid[1].to_lowercase();
        let line = rapid[3].parse::<u64>().unwrap_or(0);
        let s = if !rapid[2].is_empty() { Side::Right } else { Side::Left };
        return Some(Anchor::Diff { digest, range: gitlab_line(line, s, None) });
    }
    if let Some(code) = GITLAB_DIFF.captures(anchor) {
        let digest = code[1].to_lowercase();
        let (Some(old), Some(new)) = (code.get(2), code.get(3)) else {
            return Some(Anchor::Diff { digest, range: None });
        };
        let old_line = old.as_str().parse::<u64>().unwrap_or(0);
        let new_line = new.as_str().parse::<u64>().unwrap_or(0);
        let range = if new_line == 0 {
            gitlab_line(old_line, Side::Left, None)
        } else {
            gitlab_line(new_line, Side::Right, Some(old_line))
        };
        return Some(Anchor::Diff { digest, range });
    }
    if anchor.is_empty()
        && let Some(file) = file_param
        && GITLAB_FILE_SHA1.is_match(file)
    {
        return Some(Anchor::Diff { digest: file.to_lowercase(), range: None });
    }
    None
}

/// `resolvePullRequestAnchorRange` (`pingdotgg/t3code#34662`): once the diff is loaded, switches
/// a GitLab `<sha1>_<old>_<new>` anchor to the left side when `old_line` was removed.
pub fn resolve_anchor_range(range: Range, is_removed_line: impl FnOnce(u64) -> bool) -> Range {
    if let Some(old_line) = range.old_line
        && is_removed_line(old_line)
    {
        return gitlab_line(old_line, Side::Left, None).unwrap_or(range);
    }
    range
}

/// `parsePullRequestAnchor`.
pub fn parse_anchor(url: &str) -> Option<Anchor> {
    let parsed = url::Url::parse(url).ok()?;
    let fallback = path_tab(parsed.path());
    let anchor = url_hash(url);
    let file_param = if GITLAB_DIFFS_PATH.is_match(parsed.path()) {
        parsed.query_pairs().find(|(k, _)| k == "file").map(|(_, v)| v.into_owned())
    } else {
        None
    };
    if let Some(gitlab) = gitlab_diff_anchor(&anchor, file_param.as_deref()) {
        return Some(gitlab);
    }
    if anchor.is_empty() {
        return fallback;
    }
    // GitHub line ranges may cross diff sides; file digests were MD5 before SHA-256.
    if let Some(diff) = GITHUB_DIFF.captures(&anchor) {
        let digest = diff[1].to_lowercase();
        let Some(start_side) = diff.get(2) else { return Some(Anchor::Diff { digest, range: None }) };
        let start = line_number(&diff[3]);
        let end = line_number(diff.get(5).map_or(&diff[3], |end| end.as_str()));
        let (Some(start), Some(end)) = (start, end) else { return fallback };
        let start_side = side(start_side.as_str());
        let end_side = diff.get(4).map_or(start_side, |end| side(end.as_str()));
        return Some(Anchor::Diff { digest, range: Some(Range { start, side: start_side, end, end_side, old_line: None }) });
    }
    if TIMELINE.is_match(&anchor) {
        return Some(Anchor::Timeline(normalize_comment_anchor(&anchor)));
    }
    Some(fallback.unwrap_or(Anchor::Summary(anchor)))
}

/// `pullRequestAnchorMatchesUrl`: whether a comment's url is the one a timeline anchor names.
pub fn anchor_matches_url(anchor: &str, url: Option<&str>) -> bool {
    url.is_some_and(|url| normalize_comment_anchor(&url_hash(url)) == anchor)
}

static REVIEW_ANCHOR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^pullrequestreview-\d+$").unwrap());

/// `resolvePullRequestReviewAnchor`: a review with no text of its own appears only through the
/// line comments it posted, so its link resolves to the earliest of those. Any other anchor, or
/// a review shown in its own right, is kept.
pub fn resolve_review_anchor(anchor: &str, comments: &[super::wire::Comment], review_threads: &[super::wire::ReviewThread]) -> String {
    if !REVIEW_ANCHOR.is_match(anchor) {
        return anchor.to_owned();
    }
    if comments.iter().any(|comment| anchor_matches_url(anchor, comment.url.as_deref())) {
        return anchor.to_owned();
    }
    let mut posted: Option<&super::wire::ThreadComment> = None;
    for comment in review_threads.iter().flat_map(|thread| &thread.comments) {
        if comment.url.is_none() || !anchor_matches_url(anchor, comment.review_url.as_deref()) {
            continue;
        }
        if posted.is_none_or(|current| comment.created_at < current.created_at) {
            posted = Some(comment);
        }
    }
    let own = posted.and_then(|comment| comment.url.as_deref()).map(|url| normalize_comment_anchor(&url_hash(url))).unwrap_or_default();
    if own.is_empty() { anchor.to_owned() } else { own }
}

/// `pullRequestDiffPathMatches`: the host's digest of a file path, by its length (GitHub MD5
/// legacy / SHA-256 current, GitLab SHA-1 or 9-char Rapid Diffs prefix).
pub fn diff_path_matches(digest: &str, path: &str) -> bool {
    let hex = |bytes: &[u8]| bytes.iter().map(|byte| format!("{byte:02x}")).collect::<String>();
    match digest.len() {
        9 => hex(&sha1_digest(path.as_bytes())).starts_with(digest),
        32 => digest == hex(&md5_digest(path.as_bytes())),
        40 => digest == hex(&sha1_digest(path.as_bytes())),
        _ => digest == hex(&Sha256::digest(path.as_bytes())),
    }
}

/// RFC 1321 MD5 for matching legacy GitHub diff file path digests without extra crate deps.
fn md5_digest(input: &[u8]) -> [u8; 16] {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15,
        21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    const K: [u32; 64] = [
        0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613, 0xfd469501, 0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193, 0xa679438e,
        0x49b40821, 0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d, 0x02441453, 0xd8a1e681, 0xe7d3fbc8, 0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed, 0xa9e3e905, 0xfcefa3f8,
        0x676f02d9, 0x8d2a4c8a, 0xfffa3942, 0x8771f681, 0x6d9d6122, 0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70, 0x289b7ec6, 0xeaa127fa, 0xd4ef3085, 0x04881d05, 0xd9d4d039,
        0xe6db99e5, 0x1fa27cf8, 0xc4ac5665, 0xf4292244, 0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1, 0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1,
        0xf7537e82, 0xbd3af235, 0x2ad7d2bb, 0xeb86d391,
    ];
    let mut msg = input.to_vec();
    msg.push(0x80);
    while (msg.len() % 64) != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&((input.len() as u64).wrapping_mul(8)).to_le_bytes());
    let (mut a0, mut b0, mut c0, mut d0) = (0x67452301u32, 0xefcdab89u32, 0x98badcfeu32, 0x10325476u32);
    for chunk in msg.chunks_exact(64) {
        let mut m = [0u32; 16];
        for (i, word) in chunk.chunks_exact(4).enumerate() {
            m[i] = u32::from_le_bytes([word[0], word[1], word[2], word[3]]);
        }
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = match i {
                0..=15 => ((b & c) | (!b & d), i),
                16..=31 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                32..=47 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let f = f.wrapping_add(a).wrapping_add(K[i]).wrapping_add(m[g]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(f.rotate_left(S[i]));
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }
    let mut out = [0u8; 16];
    out[0..4].copy_from_slice(&a0.to_le_bytes());
    out[4..8].copy_from_slice(&b0.to_le_bytes());
    out[8..12].copy_from_slice(&c0.to_le_bytes());
    out[12..16].copy_from_slice(&d0.to_le_bytes());
    out
}

/// FIPS 180-4 SHA-1 for matching GitLab diff file path digests without extra crate deps.
fn sha1_digest(input: &[u8]) -> [u8; 20] {
    let mut msg = input.to_vec();
    msg.push(0x80);
    while (msg.len() % 64) != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&((input.len() as u64).wrapping_mul(8)).to_be_bytes());
    let (mut h0, mut h1, mut h2, mut h3, mut h4) = (0x67452301u32, 0xefcdab89u32, 0x98badcfeu32, 0x10325476u32, 0xc3d2e1f0u32);
    for chunk in msg.chunks_exact(64) {
        let mut w = [0u32; 80];
        for (i, word) in chunk.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (h0, h1, h2, h3, h4);
        for (i, &word) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5a827999u32),
                20..=39 => (b ^ c ^ d, 0x6ed9eba1u32),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8f1bbcdcu32),
                _ => (b ^ c ^ d, 0xca62c1d6u32),
            };
            let temp = a.rotate_left(5).wrapping_add(f).wrapping_add(e).wrapping_add(k).wrapping_add(word);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }
        h0 = h0.wrapping_add(a);
        h1 = h1.wrapping_add(b);
        h2 = h2.wrapping_add(c);
        h3 = h3.wrapping_add(d);
        h4 = h4.wrapping_add(e);
    }
    let mut out = [0u8; 20];
    out[0..4].copy_from_slice(&h0.to_be_bytes());
    out[4..8].copy_from_slice(&h1.to_be_bytes());
    out[8..12].copy_from_slice(&h2.to_be_bytes());
    out[12..16].copy_from_slice(&h3.to_be_bytes());
    out[16..20].copy_from_slice(&h4.to_be_bytes());
    out
}

static LINK_COMMIT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)/(?:pull|-/merge_requests)/\d+/(?:commits|diffs)/([a-f0-9]{40})/?$").unwrap());

/// `pullRequestLinkCommit`: the commit a GitHub PR commit link or GitLab MR `?commit_id=` link names.
pub fn link_commit(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    if let Some(captures) = LINK_COMMIT.captures(parsed.path()) {
        return Some(captures[1].to_owned());
    }
    if GITLAB_DIFFS_PATH.is_match(parsed.path()) {
        let sha = parsed.query_pairs().find(|(k, _)| k == "commit_id").map(|(_, v)| v.into_owned())?;
        if GITLAB_FILE_SHA1.is_match(&sha) {
            return Some(sha);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const PR: &str = "https://github.com/acme/app/pull/7";

    #[test]
    fn links_name_tabs_by_path_and_places_by_fragment() {
        assert_eq!(parse_anchor(PR), None);
        assert_eq!(parse_anchor("not a url"), None);
        assert_eq!(parse_anchor(&format!("{PR}/files")), Some(Anchor::Code));
        assert_eq!(parse_anchor(&format!("{PR}/commits")), Some(Anchor::Commits));
        assert_eq!(parse_anchor(&format!("{PR}/commits/{}", "a".repeat(40))), Some(Anchor::Code));
        assert_eq!(parse_anchor(&format!("{PR}/commits/abc")), None);
        assert_eq!(parse_anchor(&format!("{PR}/checks")), Some(Anchor::Checks));
        assert_eq!(parse_anchor("https://gitlab.com/acme/app/-/merge_requests/3/pipelines"), Some(Anchor::Checks));
        assert_eq!(parse_anchor("https://gitlab.com/acme/app/-/merge_requests/3/diffs"), Some(Anchor::Code));
        assert_eq!(parse_anchor(&format!("{PR}#issuecomment-12")), Some(Anchor::Timeline("issuecomment-12".into())));
        assert_eq!(parse_anchor(&format!("{PR}/files#r99")), Some(Anchor::Timeline("discussion_r99".into())));
        assert_eq!(parse_anchor(&format!("{PR}#discussion-diff-5R3-R4")), Some(Anchor::Timeline("discussion_r5".into())));
        assert_eq!(parse_anchor(&format!("{PR}#note_42")), Some(Anchor::Timeline("note_42".into())));
        assert_eq!(parse_anchor(&format!("{PR}#user-content-setup")), Some(Anchor::Summary("user-content-setup".into())));
        assert_eq!(parse_anchor(&format!("{PR}/checks#whatever")), Some(Anchor::Checks));
    }

    #[test]
    fn diff_fragments_carry_their_digest_and_lines() {
        assert_eq!(parse_anchor(&format!("{PR}/files#diff-ABC")), Some(Anchor::Diff { digest: "abc".into(), range: None }));
        assert_eq!(
            parse_anchor(&format!("{PR}/files#diff-abcL3-R5")),
            Some(Anchor::Diff { digest: "abc".into(), range: Some(Range { start: 3, side: Side::Left, end: 5, end_side: Side::Right, old_line: None }) })
        );
        assert_eq!(
            parse_anchor(&format!("{PR}/files#diff-abcR8")),
            Some(Anchor::Diff { digest: "abc".into(), range: Some(Range { start: 8, side: Side::Right, end: 8, end_side: Side::Right, old_line: None }) })
        );
        assert_eq!(parse_anchor(&format!("{PR}/files#diff-abcR0")), Some(Anchor::Code), "a line zero falls back to the path's tab");
        let sha = "b".repeat(40);
        assert_eq!(
            parse_anchor(&format!("https://gitlab.com/a/b/-/merge_requests/1/diffs#{sha}_4_0")),
            Some(Anchor::Diff { digest: sha.clone(), range: Some(Range { start: 4, side: Side::Left, end: 4, end_side: Side::Left, old_line: None }) })
        );
        assert_eq!(
            parse_anchor(&format!("https://gitlab.com/a/b/-/merge_requests/1/diffs#{sha}_4_6")),
            Some(Anchor::Diff { digest: sha.clone(), range: Some(Range { start: 6, side: Side::Right, end: 6, end_side: Side::Right, old_line: Some(4) }) })
        );
        assert_eq!(parse_anchor(&format!("https://gitlab.com/a/b/-/merge_requests/1/diffs#{sha}")), Some(Anchor::Diff { digest: sha.clone(), range: None }));
        // GitLab Rapid Diffs, diff-content, ?file=, ?commit_id=, and removed-line resolution (`pingdotgg/t3code#34662`).
        let rapid = format!("https://gitlab.com/a/b/-/merge_requests/1/diffs#line_{}_A12", &sha[..9]);
        assert_eq!(
            parse_anchor(&rapid),
            Some(Anchor::Diff { digest: sha[..9].to_owned(), range: Some(Range { start: 12, side: Side::Right, end: 12, end_side: Side::Right, old_line: None }) })
        );
        assert_eq!(
            parse_anchor(&format!("https://gitlab.com/a/b/-/merge_requests/1/diffs?file={sha}")),
            Some(Anchor::Diff { digest: sha.clone(), range: None })
        );
        assert_eq!(
            link_commit(&format!("https://gitlab.com/a/b/-/merge_requests/1/diffs?commit_id={sha}")),
            Some(sha.clone())
        );
        let removed_resolved = resolve_anchor_range(
            Range { start: 6, side: Side::Right, end: 6, end_side: Side::Right, old_line: Some(4) },
            |old| old == 4,
        );
        assert_eq!(removed_resolved, Range { start: 4, side: Side::Left, end: 4, end_side: Side::Left, old_line: None });
        assert!(diff_path_matches("c46b6f338", "src/a.rs"));
    }

    #[test]
    fn anchors_open_their_tab() {
        assert_eq!(Anchor::Diff { digest: String::new(), range: None }.tab(), LinkedTab::Code);
        assert_eq!(Anchor::Commits.tab(), LinkedTab::Timeline);
        assert_eq!(Anchor::Timeline("note_1".into()).tab(), LinkedTab::Timeline);
        assert_eq!(Anchor::Checks.tab(), LinkedTab::Summary);
        assert_eq!(Anchor::Summary("x".into()).tab(), LinkedTab::Summary);
    }

    #[test]
    fn comment_urls_match_normalized_anchors_and_digests_match_paths() {
        assert!(anchor_matches_url("discussion_r5", Some("https://github.com/a/b/pull/1#discussion_r5")));
        assert!(anchor_matches_url("discussion_r5", Some("https://github.com/a/b/pull/1/files#r5")));
        assert!(!anchor_matches_url("discussion_r5", Some("https://github.com/a/b/pull/1#discussion_r6")));
        assert!(!anchor_matches_url("discussion_r5", None));
        // GitHub's current SHA-256, its older MD5, and GitLab's SHA-1 of "src/a.rs".
        assert!(diff_path_matches("bdfc5619650b795d1ffec5e8af3154a9a4b1833e5ed458694907d33709d12825", "src/a.rs"));
        assert!(diff_path_matches("50d4cef7cf92195395c86490e7d51ed6", "src/a.rs"));
        assert!(diff_path_matches("c46b6f3386ba91fc8fcb5f960fd24012ff3c1460", "src/a.rs"));
        assert!(!diff_path_matches("c46b6f3386ba91fc8fcb5f960fd24012ff3c1460", "src/b.rs"));
        assert_eq!(link_commit(&format!("{PR}/commits/{}", "c".repeat(40))), Some("c".repeat(40)));
        assert_eq!(link_commit(PR), None);
    }

    #[test]
    fn bodyless_review_anchors_resolve_to_their_earliest_inline_comment() {
        use super::super::wire::{Comment, CommentKind, ReviewThread, ThreadComment};
        let comment_with_url = |id: &str, url: &str| Comment {
            id: id.into(),
            kind: CommentKind::Review,
            author: None,
            body: "looks good".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
            edited_at: None,
            url: Some(url.into()),
            path: None,
            review_state: None,
            reactions: Vec::new(),
            can_edit: None,
        };
        let review_url = format!("{PR}#pullrequestreview-42");
        let thread = ReviewThread {
            id: "t1".into(),
            path: Some("src/a.rs".into()),
            line: Some(10),
            side: Some("right".into()),
            is_resolved: false,
            is_outdated: false,
            comments: vec![
                ThreadComment {
                    id: "c2".into(),
                    author: None,
                    body: "second".into(),
                    created_at: "2026-01-02T00:00:00Z".into(),
                    edited_at: None,
                    url: Some(format!("{PR}#discussion_r200")),
                    review_url: Some(review_url.clone()),
                    reactions: Vec::new(),
                    can_edit: None,
                },
                ThreadComment {
                    id: "c1".into(),
                    author: None,
                    body: "first".into(),
                    created_at: "2026-01-01T00:00:00Z".into(),
                    edited_at: None,
                    url: Some(format!("{PR}/files#r100")),
                    review_url: Some(review_url.clone()),
                    reactions: Vec::new(),
                    can_edit: None,
                },
            ],
            comment_count: None,
            next_comments_cursor: None,
            can_resolve: None,
        };
        assert_eq!(resolve_review_anchor("pullrequestreview-42", &[], std::slice::from_ref(&thread)), "discussion_r100");
        // A review with its own top-level comment stays on the review anchor.
        assert_eq!(resolve_review_anchor("pullrequestreview-42", &[comment_with_url("r", &review_url)], std::slice::from_ref(&thread)), "pullrequestreview-42");
        // Non-review anchors are untouched.
        assert_eq!(resolve_review_anchor("issuecomment-9", &[], &[thread]), "issuecomment-9");
    }
}
