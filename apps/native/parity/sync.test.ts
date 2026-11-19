import * as NodeAssert from "node:assert/strict";
import * as NodeChildProcess from "node:child_process";
import * as NodeFS from "node:fs";
import * as NodeOS from "node:os";
import * as NodePath from "node:path";
import * as NodeTest from "node:test";

import {
  buildDigest,
  buildLedgerIndex,
  classifyPr,
  findExistingIssue,
  fitIssueBody,
  groupCommits,
  isNoiseFile,
  makeGit,
  mergeUpstream,
  parseGitLog,
  parseLedger,
  parseSubject,
  prIssueBody,
  prIssueTitle,
  renderMarkdown,
  upstreamMarker,
  watchedPathspecs,
} from "./sync.ts";
import type { LedgerRow, Pr } from "./sync.ts";

const LEDGER = `# Ledger

Intro text | with a pipe that is not a table row.

| ID | Area | Feature | Web source | Native | Status | Issue |
|----|------|---------|------------|--------|--------|-------|
| SB-ROW | sidebar | Thread row | \`apps/web/src/components/sidebar/SidebarThreadRow.tsx\` | crates/app/src/sidebar.rs | partial | #12 |
| CHAT | chat | Chat view | apps/web/src/components/chat/, apps/web/src/hooks/useChat.ts | crates/app/src/chat.rs | **Done** | |
| VOICE | chat | Voice input | apps/web/src/components/chat/Voice.tsx | — | n/a | |
| SET | settings | Settings | apps/web/src/components/settings | | n/a | |
| THEME | settings | Theme picker | apps/web/src/components/settings/ThemePicker.tsx | | done | |
| KEYS | keys | Keybindings | packages/shared/src/keybindings.ts | | todo | |
| AUTH | auth | Auth contract | packages/contracts/src/auth.ts | | done | |
| MOB | mobile | Mobile pairing | packages/contracts/src/device.ts | | n/a | |
| OUT | server | Outside root | apps/server/src/ws.ts | | partial | |
|  | ignored | row without id | apps/web/src/x.ts | | done | |

| Feature | Status | Web source | ID |
|---|---|---|---|
| Reordered columns | done | apps/web/src/routes/__root.tsx | ROOT |
`;

const rows = parseLedger(LEDGER);
const index = buildLedgerIndex(rows);

function pr(files: string[], number = 1): Pr {
  return {
    ref: `pingdotgg/t3code#${number}`,
    number,
    sha: `${number}`.padStart(40, "0"),
    shortSha: `${number}`.padStart(10, "0"),
    subject: `fix(web): change ${number} (#${number})`,
    title: `fix(web): change ${number}`,
    type: "fix",
    scope: "web",
    breaking: false,
    files,
  };
}

const classify = (files: string[]) => classifyPr(pr(files), index);

NodeTest.describe("parseLedger", () => {
  NodeTest.it("reads table rows, skips headers, separators, prose, and id-less rows", () => {
    NodeAssert.deepEqual(
      rows.map((row) => row.id),
      ["SB-ROW", "CHAT", "VOICE", "SET", "THEME", "KEYS", "AUTH", "MOB", "OUT", "ROOT"],
    );
  });

  NodeTest.it("normalizes sources and status", () => {
    const chat = rows.find((row) => row.id === "CHAT");
    NodeAssert.deepEqual(chat?.sources, [
      "apps/web/src/components/chat",
      "apps/web/src/hooks/useChat.ts",
    ]);
    NodeAssert.equal(chat?.status, "done");
    NodeAssert.deepEqual(rows.find((row) => row.id === "SB-ROW")?.sources, [
      "apps/web/src/components/sidebar/SidebarThreadRow.tsx",
    ]);
    NodeAssert.deepEqual(rows.find((row) => row.id === "VOICE")?.native, "—");
  });

  NodeTest.it("maps columns by header name when a table reorders them", () => {
    const root = rows.find((row) => row.id === "ROOT");
    NodeAssert.deepEqual(root?.sources, ["apps/web/src/routes/__root.tsx"]);
    NodeAssert.equal(root?.status, "done");
  });

  NodeTest.it("treats a missing ledger as empty", () => {
    NodeAssert.deepEqual(parseLedger(""), []);
  });
});

NodeTest.describe("parseSubject", () => {
  const cases: [string, ReturnType<typeof parseSubject>][] = [
    [
      "fix(web): model picker uses the full window height (#29017)",
      {
        number: 29017,
        title: "fix(web): model picker uses the full window height",
        type: "fix",
        scope: "web",
        breaking: false,
      },
    ],
    [
      "feat!: drop v1 protocol (#5)",
      { number: 5, title: "feat!: drop v1 protocol", type: "feat", scope: null, breaking: true },
    ],
    [
      "update the claude entrypoint identifier (#28410)",
      {
        number: 28410,
        title: "update the claude entrypoint identifier",
        type: null,
        scope: null,
        breaking: false,
      },
    ],
    [
      'Revert "feat: a (#100)"',
      { number: null, title: 'Revert "feat: a (#100)"', type: null, scope: null, breaking: false },
    ],
    [
      "chore: direct push",
      { number: null, title: "chore: direct push", type: "chore", scope: null, breaking: false },
    ],
  ];
  for (const [subject, expected] of cases) {
    NodeTest.it(subject, () => NodeAssert.deepEqual(parseSubject(subject), expected));
  }
});

NodeTest.describe("parseGitLog and groupCommits", () => {
  NodeTest.it("parses batched log output and merges commits that share a PR", () => {
    const log = [
      "\x1eaaa\x1faaa1\x1ffix(web): one (#7)\n\napps/web/src/a.ts\napps/web/src/b.ts\n",
      "\x1ebbb\x1fbbb1\x1fchore: no pr\n\npackages/contracts/src/x.ts\n",
      "\x1eccc\x1fccc1\x1ffix(web): one again (#7)\n\napps/web/src/c.ts\napps/web/src/a.ts\n",
    ].join("");
    const commits = parseGitLog(log);
    NodeAssert.equal(commits.length, 3);
    NodeAssert.deepEqual(commits[1], {
      sha: "bbb",
      shortSha: "bbb1",
      subject: "chore: no pr",
      files: ["packages/contracts/src/x.ts"],
    });
    const prs = groupCommits(commits);
    NodeAssert.deepEqual(
      prs.map((p) => [p.ref, p.files]),
      [
        ["pingdotgg/t3code#7", ["apps/web/src/a.ts", "apps/web/src/b.ts", "apps/web/src/c.ts"]],
        ["pingdotgg/t3code@bbb1", ["packages/contracts/src/x.ts"]],
      ],
    );
  });
});

NodeTest.describe("isNoiseFile", () => {
  const cases: [string, boolean][] = [
    ["apps/web/src/components/chat/Chat.test.tsx", true],
    ["apps/web/src/components/chat/Chat.browser.test.tsx", true],
    ["apps/web/src/components/chat/Chat.logic.test.ts", true],
    ["apps/web/src/components/Button.stories.tsx", true],
    ["apps/web/src/components/__snapshots__/Chat.tsx", true],
    ["apps/web/src/components/Chat.tsx.snap", true],
    ["apps/web/src/test-fixtures.ts", true],
    ["packages/client-runtime/src/state/orchestrationV2TestFixtures.ts", true],
    ["apps/web/src/components/settings/SnapShotSettings.tsx", false],
    ["apps/web/src/assets/snap-shot-click.mp3", false],
    ["apps/web/src/lib/latestUtils.ts", false],
    ["packages/client-runtime/src/state/threadSnapshotHttp.ts", false],
  ];
  for (const [file, expected] of cases) {
    NodeTest.it(file, () => NodeAssert.equal(isNoiseFile(file), expected));
  }
});

NodeTest.describe("classifyPr", () => {
  const cases: [string, string[], Partial<Record<string, string[]>>][] = [
    ["contracts source is wire", ["packages/contracts/src/orchestrationV2.ts"], { wire: [] }],
    [
      "contracts test alone is noise",
      ["packages/contracts/src/orchestrationV2.test.ts"],
      { noise: [] },
    ],
    [
      "done and partial rows are regression-risk",
      [
        "apps/web/src/components/chat/Composer.tsx",
        "apps/web/src/components/sidebar/SidebarThreadRow.tsx",
      ],
      { "regression-risk": ["CHAT", "SB-ROW"] },
    ],
    [
      "todo rows are todo, not regression-risk",
      ["packages/shared/src/keybindings.ts"],
      { todo: ["KEYS"] },
    ],
    [
      "uncovered web path is new-surface",
      ["apps/web/src/components/files/Tree.tsx"],
      { "new-surface": [] },
    ],
    [
      "uncovered client-runtime path is new-surface",
      ["packages/client-runtime/src/state/x.ts"],
      { "new-surface": [] },
    ],
    [
      "n/a rows alone are noise",
      ["apps/web/src/components/settings/General.tsx"],
      { noise: ["SET"] },
    ],
    [
      "specific n/a row carves out of a broad done row",
      ["apps/web/src/components/chat/Voice.tsx"],
      { noise: ["VOICE"] },
    ],
    [
      "specific done row overrides a broad n/a row",
      ["apps/web/src/components/settings/ThemePicker.tsx"],
      { "regression-risk": ["THEME"] },
    ],
    ["n/a row suppresses wire", ["packages/contracts/src/device.ts"], { noise: ["MOB"] }],
    [
      "covered contracts file is wire and regression-risk",
      ["packages/contracts/src/auth.ts"],
      { wire: ["AUTH"], "regression-risk": ["AUTH"] },
    ],
    [
      "ledger paths outside watched roots still match",
      ["apps/server/src/ws.ts"],
      { "regression-risk": ["OUT"] },
    ],
    [
      "classes stack in priority order",
      [
        "apps/web/src/components/files/Tree.tsx",
        "packages/contracts/src/git.ts",
        "apps/web/src/hooks/useChat.ts",
        "apps/web/src/hooks/useChat.test.ts",
      ],
      { wire: [], "regression-risk": ["CHAT"], "new-surface": [] },
    ],
  ];
  for (const [name, files, expected] of cases) {
    NodeTest.it(name, () => {
      const result = classify(files);
      NodeAssert.deepEqual(result.classes, Object.keys(expected));
      NodeAssert.deepEqual(result.rows, expected);
    });
  }

  NodeTest.it("groups uncovered files by their directory", () => {
    NodeAssert.deepEqual(classify(["apps/web/src/components/files/Tree.tsx"]).areas, {
      "new-surface": ["unmapped: web/components/files"],
    });
  });
});

NodeTest.describe("watchedPathspecs", () => {
  NodeTest.it("adds ledger paths outside the watched roots once", () => {
    const extra: LedgerRow = {
      ...rows[0]!,
      id: "X",
      sources: ["apps/server/src/ws.ts", "apps/web/src/a.ts"],
    };
    NodeAssert.deepEqual(watchedPathspecs([...rows, extra]), [
      "apps/web/src",
      "packages/client-runtime/src",
      "packages/contracts/src",
      "packages/shared/src/keybindings.ts",
      "apps/server/src/ws.ts",
    ]);
  });
});

NodeTest.describe("digest", () => {
  const digest = buildDigest({
    from: "a".repeat(40),
    to: "b".repeat(40),
    commitsInRange: 9,
    rows,
    prs: [
      pr(["packages/contracts/src/auth.ts"], 1),
      pr(
        [
          "apps/web/src/components/chat/Composer.tsx",
          "apps/web/src/components/sidebar/SidebarThreadRow.tsx",
        ],
        2,
      ),
      pr(["apps/web/src/components/files/Tree.tsx"], 3),
      pr(["apps/web/src/components/chat/Composer.test.tsx"], 4),
    ],
  });

  NodeTest.it("counts every class a PR carries and groups by class then area", () => {
    NodeAssert.deepEqual(digest.counts, {
      prs: 4,
      wire: 1,
      "regression-risk": 2,
      "new-surface": 1,
      todo: 0,
      noise: 1,
    });
    NodeAssert.deepEqual(digest.groups["regression-risk"], {
      auth: ["pingdotgg/t3code#1"],
      chat: ["pingdotgg/t3code#2"],
      sidebar: ["pingdotgg/t3code#2"],
    });
  });

  NodeTest.it("lists each PR once with a checkbox for every non-noise PR", () => {
    const body = renderMarkdown(digest, { checkboxes: true });
    NodeAssert.match(body, /^# Upstream digest aaaaaaa\.\.bbbbbbb \(4 PRs\)$/m);
    NodeAssert.match(
      body,
      /^\*\*4 PRs\*\* · wire 1 · regression-risk 2 · new-surface 1 · todo 0 · noise 1$/m,
    );
    NodeAssert.equal(body.match(/^- \[ \] /gm)?.length, 3);
    NodeAssert.equal(body.match(/`pingdotgg\/t3code#1`/g)?.length, 1);
    NodeAssert.match(body, /^- `pingdotgg\/t3code#4` /m);
    NodeAssert.ok(body.indexOf("## wire") < body.indexOf("## regression-risk"));
    NodeAssert.match(body, /### unmapped: web\/components\/files\n\n- \[ \] `pingdotgg\/t3code#3`/);
  });

  NodeTest.it("never links upstream: refs, URLs, and mentions stay out of plain text", () => {
    const hostile = {
      ...pr(["packages/contracts/src/auth.ts"], 9),
      title:
        "fix: see pingdotgg/t3code#123, https://github.com/pingdotgg/t3code/pull/5, @theo and #12",
    };
    const directPush = {
      ...pr(["apps/web/src/components/files/Tree.tsx"], 5),
      number: null,
      ref: "pingdotgg/t3code@0000000005",
    };
    const unsafe = buildDigest({ ...digest, rows, prs: [...digest.prs, hostile, directPush] });
    const outputs = [
      renderMarkdown(unsafe),
      renderMarkdown(unsafe, { checkboxes: true, compact: true }),
      fitIssueBody(unsafe),
      ...unsafe.prs.flatMap((p) => [prIssueTitle(p), prIssueBody(p, unsafe, 1)]),
    ];
    for (const output of outputs) {
      const plain = output.replace(/`[^`\n]*`/g, "");
      for (const linkable of [
        "pingdotgg/t3code#",
        "pingdotgg/t3code@",
        "github.com/pingdotgg",
        "@theo",
        " #12",
      ]) {
        NodeAssert.ok(
          !plain.includes(linkable),
          `${JSON.stringify(linkable)} outside a code span in:\n${output}`,
        );
      }
    }
    NodeAssert.match(outputs[0]!, /`pingdotgg\/t3code#9` fix: see `pingdotgg\/t3code#123`/);
    NodeAssert.equal(prIssueTitle(hostile).endsWith("(`pingdotgg/t3code#9`)"), true);
  });

  NodeTest.it("dedupes on the hidden marker or the code-span title ref, never on a prefix", () => {
    const target = pr(["apps/web/src/a.ts"], 29017);
    const prefix = pr(["apps/web/src/a.ts"], 2901);
    const body = prIssueBody(classifyPr(target, index), digest, 3);
    NodeAssert.ok(body.startsWith(`${upstreamMarker(target)}\n`));
    NodeAssert.equal(upstreamMarker(target), "<!-- upstream-pr:29017 -->");
    NodeAssert.ok(findExistingIssue(target, [{ title: "retitled by a human", body }]));
    NodeAssert.ok(findExistingIssue(target, [{ title: prIssueTitle(target), body: "" }]));
    NodeAssert.equal(findExistingIssue(prefix, [{ title: prIssueTitle(target), body }]), undefined);
  });

  NodeTest.it("fits huge digests under the issue body limit", () => {
    const many = buildDigest({
      ...digest,
      rows,
      prs: Array.from({ length: 3000 }, (_, i) =>
        pr([`apps/web/src/components/area${i % 40}/File${i}.tsx`], i + 1),
      ),
    });
    const body = fitIssueBody(many);
    NodeAssert.ok(body.length <= 60_000);
    NodeAssert.match(body, /Truncated to fit/);
  });
});

function initMergeFixture(
  setupUpstream: (repoRoot: string, git: ReturnType<typeof makeGit>) => void,
) {
  const repoRoot = NodeFS.mkdtempSync(NodePath.join(NodeOS.tmpdir(), "t3-parity-merge-"));
  const git = makeGit(repoRoot);
  git(["init", "--initial-branch=main"]);
  git(["config", "user.email", "test@example.com"]);
  git(["config", "user.name", "Test"]);
  git(["config", "commit.gpgsign", "false"]);
  NodeFS.writeFileSync(NodePath.join(repoRoot, "shared.txt"), "base\n");
  git(["add", "shared.txt"]);
  git(["commit", "-m", "base"]);

  git(["checkout", "-b", "native"]);
  NodeFS.writeFileSync(NodePath.join(repoRoot, "native-only.txt"), "native\n");
  git(["add", "native-only.txt"]);
  git(["commit", "-m", "native commit"]);
  const nativeHead = git(["rev-parse", "HEAD"]).trim();

  git(["checkout", "main"]);
  setupUpstream(repoRoot, git);
  const upstreamHead = git(["rev-parse", "HEAD"]).trim();
  git(["update-ref", "refs/remotes/origin/main", upstreamHead]);

  git(["checkout", "native"]);
  return { repoRoot, git, nativeHead, upstreamHead };
}

function addManyFilesViaFastImport(repoRoot: string, count: number) {
  const lines: string[] = [
    "commit refs/heads/main",
    "committer Test <test@example.com> 1700000000 +0000",
    "data 22",
    "large upstream release",
    "from refs/heads/main^0",
  ];
  const segment = "a".repeat(60);
  for (let i = 0; i < count; i++) {
    lines.push(`M 100644 inline dir-${i % 20}/${segment}/file-${String(i).padStart(5, "0")}.txt`);
    lines.push("data 2");
    lines.push("x");
  }
  const importResult = NodeChildProcess.spawnSync(
    "git",
    ["-C", repoRoot, "fast-import", "--quiet"],
    {
      input: `${lines.join("\n")}\n`,
      encoding: "utf8",
      maxBuffer: 1 << 28,
    },
  );
  NodeAssert.equal(importResult.status, 0, importResult.stderr);
  NodeChildProcess.execFileSync("git", ["-C", repoRoot, "reset", "--hard", "HEAD"]);
}

NodeTest.describe("mergeUpstream", () => {
  NodeTest.it("merges an oversized diffstat (>1 MiB) cleanly with bounded output (#253)", () => {
    const fixture = initMergeFixture((repoRoot) => {
      // ~14,000 long paths produce >1.2 MiB of `git diff --stat` output.
      addManyFilesViaFastImport(repoRoot, 14_000);
    });
    try {
      const stat = fixture.git(["diff", "--stat=120", "native", "origin/main"]);
      NodeAssert.ok(
        stat.length > 1024 * 1024,
        `expected fixture diffstat > 1 MiB, got ${stat.length} bytes`,
      );
      const result = mergeUpstream(fixture.git, fixture.repoRoot, fixture.upstreamHead);
      NodeAssert.equal(result.status, "merged", JSON.stringify(result));
      NodeAssert.ok(result.detail.length <= 4_000);
      NodeAssert.equal(fixture.git(["status", "--porcelain", "--untracked-files=no"]).trim(), "");
      const parents = fixture
        .git(["rev-list", "--parents", "-n", "1", "HEAD"])
        .trim()
        .split(/\s+/)
        .slice(1);
      NodeAssert.deepEqual(parents, [fixture.nativeHead, fixture.upstreamHead]);
      const mergeHead = NodeChildProcess.spawnSync("git", [
        "-C",
        fixture.repoRoot,
        "rev-parse",
        "-q",
        "--verify",
        "MERGE_HEAD",
      ]);
      NodeAssert.notEqual(mergeHead.status, 0);
    } finally {
      NodeFS.rmSync(fixture.repoRoot, { recursive: true, force: true });
    }
  });

  NodeTest.it("recognizes a clean committed merge even when spawnSync overflows maxBuffer", () => {
    const fixture = initMergeFixture((repoRoot) => {
      addManyFilesViaFastImport(repoRoot, 2_000);
    });
    try {
      // Force `--stat` with a tiny maxBuffer so git commits the merge while stdout overflows Node's buffer.
      const result = mergeUpstream(fixture.git, fixture.repoRoot, fixture.upstreamHead, {
        mergeFlags: ["--no-edit", "--stat"],
        maxBuffer: 1024,
      });
      NodeAssert.equal(result.status, "merged", JSON.stringify(result));
      NodeAssert.ok(result.detail.length <= 4_000);
      NodeAssert.ok(
        result.error !== undefined ||
          result.signal !== undefined ||
          result.exitStatus !== undefined,
        `expected child diagnostic preserved, got ${JSON.stringify(result)}`,
      );
      const parents = fixture
        .git(["rev-list", "--parents", "-n", "1", "HEAD"])
        .trim()
        .split(/\s+/)
        .slice(1);
      NodeAssert.deepEqual(parents, [fixture.nativeHead, fixture.upstreamHead]);
      NodeAssert.equal(fixture.git(["status", "--porcelain", "--untracked-files=no"]).trim(), "");
      const mergeHead = NodeChildProcess.spawnSync("git", [
        "-C",
        fixture.repoRoot,
        "rev-parse",
        "-q",
        "--verify",
        "MERGE_HEAD",
      ]);
      NodeAssert.notEqual(mergeHead.status, 0);
    } finally {
      NodeFS.rmSync(fixture.repoRoot, { recursive: true, force: true });
    }
  });

  NodeTest.it("aborts cleanly and reports conflicts without altering native HEAD", () => {
    const fixture = initMergeFixture((repoRoot, git) => {
      NodeFS.writeFileSync(NodePath.join(repoRoot, "shared.txt"), "upstream change\n");
      git(["add", "shared.txt"]);
      git(["commit", "-m", "upstream conflict"]);
    });
    try {
      NodeFS.writeFileSync(NodePath.join(fixture.repoRoot, "shared.txt"), "native change\n");
      fixture.git(["add", "shared.txt"]);
      fixture.git(["commit", "-m", "native conflict"]);
      const headBefore = fixture.git(["rev-parse", "HEAD"]).trim();

      const result = mergeUpstream(fixture.git, fixture.repoRoot, fixture.upstreamHead);
      NodeAssert.equal(result.status, "conflict");
      NodeAssert.deepEqual(result.conflicts, ["shared.txt"]);
      NodeAssert.equal(result.exitStatus, 1);
      NodeAssert.equal(fixture.git(["rev-parse", "HEAD"]).trim(), headBefore);
      NodeAssert.equal(fixture.git(["status", "--porcelain", "--untracked-files=no"]).trim(), "");
      const mergeHead = NodeChildProcess.spawnSync("git", [
        "-C",
        fixture.repoRoot,
        "rev-parse",
        "-q",
        "--verify",
        "MERGE_HEAD",
      ]);
      NodeAssert.notEqual(mergeHead.status, 0);
    } finally {
      NodeFS.rmSync(fixture.repoRoot, { recursive: true, force: true });
    }
  });

  NodeTest.it("aborts an in-progress non-conflict merge failure and preserves diagnostics", () => {
    const fixture = initMergeFixture((repoRoot, git) => {
      NodeFS.writeFileSync(NodePath.join(repoRoot, "upstream.txt"), "upstream\n");
      git(["add", "upstream.txt"]);
      git(["commit", "-m", "upstream clean change"]);
    });
    try {
      // A failing pre-merge-commit hook leaves MERGE_HEAD active with no unmerged paths.
      const hookPath = NodePath.join(fixture.repoRoot, ".git", "hooks", "pre-merge-commit");
      NodeFS.writeFileSync(hookPath, "#!/bin/sh\necho 'blocked by hook' >&2\nexit 1\n", {
        mode: 0o755,
      });

      const result = mergeUpstream(fixture.git, fixture.repoRoot, fixture.upstreamHead);
      NodeAssert.equal(result.status, "failed");
      NodeAssert.equal(result.exitStatus, 1);
      NodeAssert.match(result.detail, /blocked by hook/);
      NodeAssert.equal(fixture.git(["rev-parse", "HEAD"]).trim(), fixture.nativeHead);
      NodeAssert.equal(fixture.git(["status", "--porcelain", "--untracked-files=no"]).trim(), "");
      const mergeHead = NodeChildProcess.spawnSync("git", [
        "-C",
        fixture.repoRoot,
        "rev-parse",
        "-q",
        "--verify",
        "MERGE_HEAD",
      ]);
      NodeAssert.notEqual(mergeHead.status, 0);
    } finally {
      NodeFS.rmSync(fixture.repoRoot, { recursive: true, force: true });
    }
  });

  NodeTest.it("skips when not on native branch, tree is dirty, or origin/main moved", () => {
    const fixture = initMergeFixture((repoRoot, git) => {
      NodeFS.writeFileSync(NodePath.join(repoRoot, "upstream.txt"), "upstream\n");
      git(["add", "upstream.txt"]);
      git(["commit", "-m", "upstream change"]);
    });
    try {
      const moved = mergeUpstream(fixture.git, fixture.repoRoot, "0".repeat(40));
      NodeAssert.equal(moved.status, "skipped");
      NodeAssert.match(moved.detail, /origin\/main moved/);

      NodeFS.writeFileSync(NodePath.join(fixture.repoRoot, "native-only.txt"), "dirty\n");
      const dirty = mergeUpstream(fixture.git, fixture.repoRoot, fixture.upstreamHead);
      NodeAssert.equal(dirty.status, "skipped");
      NodeAssert.match(dirty.detail, /uncommitted changes/);
      fixture.git(["checkout", "--", "native-only.txt"]);

      fixture.git(["checkout", "main"]);
      const wrongBranch = mergeUpstream(fixture.git, fixture.repoRoot, fixture.upstreamHead);
      NodeAssert.equal(wrongBranch.status, "skipped");
      NodeAssert.match(wrongBranch.detail, /not native/);
    } finally {
      NodeFS.rmSync(fixture.repoRoot, { recursive: true, force: true });
    }
  });
});
