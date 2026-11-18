//! Sorting a changed file by its path alone. Ports `packages/shared/src/changedFileKind.ts`
//! (`classifyChangedFile`, `isTestChangedFile`), which the diff panel's "Hide test files" toggle
//! (`useHideTestFiles` in `DiffPanel.tsx`) filters on.

use std::sync::LazyLock;

use regex::Regex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangedFileKind {
    Generated,
    Mocks,
    Tests,
    Docs,
    Code,
}

const GENERATED_FILES: &[&str] = &[
    "bun.lock", "bun.lockb", "cargo.lock", "composer.lock", "deno.lock", "flake.lock", "gemfile.lock", "go.sum",
    "gradle.lockfile", "mix.lock", "npm-shrinkwrap.json", "package-lock.json", "packages.lock.json", "pipfile.lock",
    "pnpm-lock.yaml", "podfile.lock", "poetry.lock", "pubspec.lock", "uv.lock", "yarn.lock",
];
const GENERATED_DIRS: &[&str] = &["__generated__", "_generated", "generated"];
const MOCK_DIRS: &[&str] = &["__fixtures__", "__mocks__", "__snapshots__", "fakes", "fixture", "fixtures", "mock", "mocks", "stubs", "testdata"];
const TEST_DIRS: &[&str] = &["__tests__", "__test__", "cypress", "e2e", "evals", "spec", "specs", "test", "tests"];

fn pattern(source: &str) -> Regex {
    Regex::new(source).expect("changed file pattern")
}

static GENERATED_NAME: LazyLock<Regex> = LazyLock::new(|| {
    pattern(r"(\.generated\.|\.gen\.|\.g\.dart$|\.pb\.go$|_pb2(_grpc)?\.py$|\.pb\.(cc|h)$|\.min\.(js|css)$|\.(js|css)\.map$)")
});
static MOCK_NAME: LazyLock<Regex> = LazyLock::new(|| {
    pattern(r"(\.snap$|\.(mocks?|fixtures?|stubs?|fakes?)\.[^.]+$|^(mocks?|fixtures?|stubs?|fakes?)([-_.A-Z]|$))")
});
static TEST_NAME: LazyLock<Regex> = LazyLock::new(|| {
    pattern(r"(?i)(\.(test|spec|e2e|eval)\.[^.]+$|_(test|spec)\.[^.]+$|^test_.+\.py$|^conftest\.py$|^(vitest|jest)\.setup\.[^.]+$)")
});
// Case-sensitive: `FooTest.java` is a test class, `Latest.java` is not.
static CLASS_TEST_NAME: LazyLock<Regex> = LazyLock::new(|| pattern(r"(Tests?|Spec)\.(java|kt|kts|scala|groovy|cs|php|swift|m)$"));
static DOCS_NAME: LazyLock<Regex> = LazyLock::new(|| pattern(r"(?i)\.(md|mdx|markdown|rst|adoc|txt)$"));
// Plain-text files that tools read as config or input, not prose.
static NOT_DOCS_NAME: LazyLock<Regex> = LazyLock::new(|| pattern(r"(?i)^(requirements.*|constraints.*|CMakeLists|robots|llms)\.txt$"));

/// `classifyChangedFile`.
pub fn classify_changed_file(path: &str) -> ChangedFileKind {
    let normalized = path.replace('\\', "/");
    let mut segments: Vec<&str> = normalized.split('/').collect();
    let name = segments.pop().unwrap_or_default();
    let dirs: Vec<String> = segments.iter().map(|segment| segment.to_lowercase()).collect();
    let in_dir = |set: &[&str]| dirs.iter().any(|dir| set.contains(&dir.as_str()));
    if GENERATED_FILES.contains(&name.to_lowercase().as_str()) || GENERATED_NAME.is_match(name) || in_dir(GENERATED_DIRS) {
        return ChangedFileKind::Generated;
    }
    if MOCK_NAME.is_match(name) || in_dir(MOCK_DIRS) {
        return ChangedFileKind::Mocks;
    }
    if TEST_NAME.is_match(name) || CLASS_TEST_NAME.is_match(name) || in_dir(TEST_DIRS) {
        return ChangedFileKind::Tests;
    }
    if DOCS_NAME.is_match(name) && !NOT_DOCS_NAME.is_match(name) {
        return ChangedFileKind::Docs;
    }
    ChangedFileKind::Code
}

/// `isTestChangedFile`: tests and their support files.
pub fn is_test_changed_file(path: &str) -> bool {
    matches!(classify_changed_file(path), ChangedFileKind::Tests | ChangedFileKind::Mocks)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_like_the_web() {
        for (path, kind) in [
            ("src/contest.py", ChangedFileKind::Code),
            ("test/fixtures/pr.json", ChangedFileKind::Mocks),
            ("pnpm-lock.yaml", ChangedFileKind::Generated),
            ("convex/_generated/api.d.ts", ChangedFileKind::Generated),
            ("src/agent.eval.ts", ChangedFileKind::Tests),
            ("evals/title-generation.ts", ChangedFileKind::Tests),
            ("README.md", ChangedFileKind::Docs),
            ("tests/README.md", ChangedFileKind::Tests),
            ("src/FooTest.java", ChangedFileKind::Tests),
            ("src/Latest.java", ChangedFileKind::Code),
            ("requirements.txt", ChangedFileKind::Code),
            ("src\\__snapshots__\\a.snap", ChangedFileKind::Mocks),
        ] {
            assert_eq!(classify_changed_file(path), kind, "{path}");
        }
        assert!(is_test_changed_file("src/a.test.ts") && is_test_changed_file("__mocks__/fs.ts"));
        assert!(!is_test_changed_file("src/a.ts"));
    }
}
