#!/usr/bin/env node
import * as NodeChildProcess from "node:child_process";
import * as NodeFS from "node:fs";
import * as NodePath from "node:path";
import * as NodeURL from "node:url";
import * as NodeUtil from "node:util";

export const UPSTREAM_REPO = "pingdotgg/t3code";
export const WATCHED_ROOTS = [
  "apps/web/src",
  "packages/client-runtime/src",
  "packages/contracts/src",
  "packages/shared/src/keybindings.ts",
];
const CONTRACTS_ROOT = "packages/contracts/src";
const ROOT_LABELS = [
  ["apps/web/src", "web"],
  ["packages/client-runtime/src", "client-runtime"],
  ["packages/contracts/src", "contracts"],
  ["packages/shared/src", "shared"],
] as const;

// Priority order: the digest lists each PR once, under the first class it carries.
export const CLASSES = ["wire", "regression-risk", "new-surface", "todo", "noise"] as const;
export type PrClass = (typeof CLASSES)[number];
const ISSUE_CLASSES: readonly PrClass[] = ["wire", "regression-risk"];
const LABELED_CLASSES: readonly PrClass[] = ["wire", "regression-risk", "new-surface"];
const KNOWN_STATUSES = new Set(["done", "partial", "todo", "n/a"]);
// GitHub rejects issue bodies over 65,536 characters.
const ISSUE_BODY_LIMIT = 60_000;
const CREATE_DELAY_MS = 1_000;
const FILES_SHOWN = 6;

const LABELS = [
  { name: "parity", color: "5319e7", description: "Native client parity with the web client" },
  {
    name: "parity:wire",
    color: "d93f0b",
    description: "Upstream contracts change the native client may need to follow",
  },
  {
    name: "parity:regression-risk",
    color: "e99695",
    description: "Upstream change to a feature the native client already mirrors",
  },
  {
    name: "parity:new-surface",
    color: "0e8a16",
    description: "Upstream web surface with no parity ledger row",
  },
  {
    name: "upstream-digest",
    color: "c5def5",
    description: "Digest of upstream changes since the last parity sync",
  },
];

const NOISE_PATTERNS = [
  /\.(test|spec|test-d)\.[cm]?[jt]sx?$/,
  /\.stories?\.[cm]?[jt]sx?$/,
  /\.snap$/,
  /(^|\/)__(tests|snapshots|screenshots|mocks|fixtures)__\//,
  /(^|\/|[._-])test-?(fixtures?|utils?|helpers?)\.[cm]?[jt]sx?$/,
  /[a-z0-9]Test(Fixtures?|Utils?|Helpers?)\.[cm]?[jt]sx?$/,
];

export interface LedgerRow {
  id: string;
  area: string;
  feature: string;
  sources: string[];
  native: string;
  status: string;
  issue: string;
}

export interface Commit {
  sha: string;
  shortSha: string;
  subject: string;
  files: string[];
}

export interface Pr {
  ref: string;
  number: number | null;
  sha: string;
  shortSha: string;
  subject: string;
  title: string;
  type: string | null;
  scope: string | null;
  breaking: boolean;
  files: string[];
}

export interface ClassifiedPr extends Pr {
  classes: PrClass[];
  rows: Partial<Record<PrClass, string[]>>;
  areas: Partial<Record<PrClass, string[]>>;
}

export interface Digest {
  upstream: string;
  from: string;
  to: string;
  commitsInRange: number;
  counts: { prs: number } & Record<PrClass, number>;
  groups: Partial<Record<PrClass, Record<string, string[]>>>;
  prs: ClassifiedPr[];
  warnings: string[];
}

type LedgerIndex = { source: string; row: LedgerRow }[];

const COLUMN_NAMES = ["id", "area", "feature", "web source", "native", "status", "issue"] as const;
type Column = (typeof COLUMN_NAMES)[number];
const DEFAULT_COLUMNS = new Map<Column, number>(COLUMN_NAMES.map((name, index) => [name, index]));

function splitRow(line: string): string[] {
  const cells = line.split(/(?<!\\)\|/).map((cell) => cell.trim().replaceAll("\\|", "|"));
  return cells.slice(1, line.endsWith("|") && !line.endsWith("\\|") ? -1 : undefined);
}

function cleanCell(cell: string | undefined): string {
  return (cell ?? "").replace(/[`*]/g, "").trim();
}

export function parseSources(cell: string): string[] {
  const sources = new Set<string>();
  for (const part of cell.split(/,|;|<br\s*\/?>/i)) {
    const source = cleanCell(part)
      .replace(/^\.\//, "")
      .replace(/\/\*\*?$/, "")
      .replace(/\/+$/, "");
    if (!source || /\s/.test(source) || /^(-|—|–|n\/a|none|tbd)$/i.test(source)) continue;
    if (source.startsWith("/") || source.split("/").includes("..")) continue;
    sources.add(source);
  }
  return [...sources];
}

export function parseLedger(text: string): LedgerRow[] {
  const rows: LedgerRow[] = [];
  let columns = DEFAULT_COLUMNS;
  for (const line of text.split(/\r?\n/)) {
    const trimmed = line.trim();
    if (!trimmed.startsWith("|")) {
      columns = DEFAULT_COLUMNS;
      continue;
    }
    const cells = splitRow(trimmed);
    const lower = cells.map((cell) => cleanCell(cell).toLowerCase());
    if (lower.includes("id") && lower.includes("web source")) {
      columns = new Map(COLUMN_NAMES.map((name) => [name, lower.indexOf(name)]));
      continue;
    }
    if (cells.every((cell) => /^:?-+:?$/.test(cell))) continue;
    const cell = (name: Column) => {
      const index = columns.get(name) ?? -1;
      return index < 0 ? "" : (cells[index] ?? "");
    };
    const id = cleanCell(cell("id"));
    if (!id) continue;
    rows.push({
      id,
      area: cleanCell(cell("area")),
      feature: cleanCell(cell("feature")),
      sources: parseSources(cell("web source")),
      native: cleanCell(cell("native")),
      status: cleanCell(cell("status")).toLowerCase(),
      issue: cleanCell(cell("issue")),
    });
  }
  return rows;
}

export function ledgerWarnings(rows: LedgerRow[]): string[] {
  const warnings: string[] = [];
  const seen = new Set<string>();
  for (const row of rows) {
    if (seen.has(row.id)) warnings.push(`ledger row ${row.id} appears more than once`);
    seen.add(row.id);
    if (!KNOWN_STATUSES.has(row.status)) {
      warnings.push(`ledger row ${row.id} has unknown status "${row.status}"; treated as todo`);
    }
    if (row.sources.length === 0 && row.status !== "n/a") {
      warnings.push(
        `ledger row ${row.id} has no web source paths, so no upstream change can match it`,
      );
    }
  }
  return warnings;
}

export function covers(source: string, file: string): boolean {
  return file === source || file.startsWith(`${source}/`);
}

export function watchedPathspecs(rows: LedgerRow[]): string[] {
  const specs = new Set(WATCHED_ROOTS);
  for (const row of rows) {
    for (const source of row.sources) {
      if (!WATCHED_ROOTS.some((root) => covers(root, source))) specs.add(source);
    }
  }
  return [...specs];
}

export function parseSubject(subject: string) {
  const merge = subject.match(/^Merge pull request #(\d+)/);
  const suffix = subject.match(/\s*\(#(\d+)\)\s*$/);
  const title = suffix ? subject.slice(0, suffix.index).trimEnd() : subject.trim();
  const conventional = title.match(/^(\w+)(?:\(([^)]*)\))?(!)?:\s*/);
  const number = suffix?.[1] ?? merge?.[1];
  return {
    number: number ? Number(number) : null,
    title,
    type: conventional?.[1]?.toLowerCase() ?? null,
    scope: conventional?.[2]?.trim() || null,
    breaking: Boolean(conventional?.[3]),
  };
}

export function parseGitLog(output: string): Commit[] {
  const commits: Commit[] = [];
  for (const record of output.split("\x1e")) {
    if (!record.trim()) continue;
    const [header = "", ...files] = record.split("\n");
    const [sha = "", shortSha = "", subject = ""] = header.split("\x1f");
    commits.push({ sha, shortSha, subject, files: files.filter((file) => file.length > 0) });
  }
  return commits;
}

export function groupCommits(commits: Commit[]): Pr[] {
  const prs = new Map<string, Pr>();
  for (const commit of commits) {
    const parsed = parseSubject(commit.subject);
    const ref =
      parsed.number === null
        ? `${UPSTREAM_REPO}@${commit.shortSha}`
        : `${UPSTREAM_REPO}#${parsed.number}`;
    const existing = prs.get(ref);
    const files = [...new Set([...(existing?.files ?? []), ...commit.files])].sort();
    prs.set(ref, {
      ref,
      ...parsed,
      sha: commit.sha,
      shortSha: commit.shortSha,
      subject: commit.subject,
      files,
    });
  }
  return [...prs.values()];
}

export function isNoiseFile(file: string): boolean {
  return NOISE_PATTERNS.some((pattern) => pattern.test(file));
}

export function unmappedArea(file: string): string {
  for (const [root, label] of ROOT_LABELS) {
    if (!file.startsWith(`${root}/`)) continue;
    const dirs = file
      .slice(root.length + 1)
      .split("/")
      .slice(0, -1)
      .slice(0, 2);
    return `unmapped: ${[label, ...dirs].join("/")}`;
  }
  return `unmapped: ${NodePath.posix.dirname(file)}`;
}

export function buildLedgerIndex(rows: LedgerRow[]): LedgerIndex {
  return rows
    .flatMap((row) => row.sources.map((source) => ({ source, row })))
    .sort((a, b) => b.source.length - a.source.length);
}

// The longest matching source wins, so a specific row can carve an exception out of a broad one.
export function mostSpecificRows(file: string, index: LedgerIndex): LedgerRow[] {
  const rows: LedgerRow[] = [];
  let best = -1;
  for (const { source, row } of index) {
    if (source.length < best) break;
    if (!covers(source, file)) continue;
    best = source.length;
    if (!rows.includes(row)) rows.push(row);
  }
  return rows;
}

export function classifyPr(pr: Pr, index: LedgerIndex): ClassifiedPr {
  const rows = new Map<PrClass, Set<string>>();
  const areaCounts = new Map<PrClass, Map<string, number>>();
  const add = (cls: PrClass, area: string, rowId?: string) => {
    const set = rows.get(cls) ?? new Set<string>();
    if (rowId) set.add(rowId);
    rows.set(cls, set);
    const counts = areaCounts.get(cls) ?? new Map<string, number>();
    counts.set(area, (counts.get(area) ?? 0) + 1);
    areaCounts.set(cls, counts);
  };
  const ignored: { area: string; rowId?: string }[] = [];

  for (const file of pr.files) {
    if (isNoiseFile(file)) {
      ignored.push({ area: "tests" });
      continue;
    }
    const matched = mostSpecificRows(file, index);
    const live = matched.filter((row) => row.status !== "n/a");
    if (matched.length > 0 && live.length === 0) {
      for (const row of matched) ignored.push({ area: row.area || "(no area)", rowId: row.id });
      continue;
    }
    if (covers(CONTRACTS_ROOT, file)) {
      if (live.length === 0) add("wire", "contracts");
      for (const row of live) add("wire", row.area || "contracts", row.id);
    }
    if (matched.length === 0) {
      if (!covers(CONTRACTS_ROOT, file)) add("new-surface", unmappedArea(file));
      continue;
    }
    for (const row of live) {
      const cls = row.status === "done" || row.status === "partial" ? "regression-risk" : "todo";
      add(cls, row.area || "(no area)", row.id);
    }
  }

  if (rows.size === 0) for (const { area, rowId } of ignored) add("noise", area, rowId);
  if (rows.size === 0) add("noise", "tests");
  const classes = CLASSES.filter((cls) => rows.has(cls));
  const sortedAreas = (counts: Map<string, number>) =>
    [...counts].sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0])).map(([area]) => area);
  return {
    ...pr,
    classes,
    rows: Object.fromEntries(classes.map((cls) => [cls, [...(rows.get(cls) ?? [])].sort()])),
    areas: Object.fromEntries(
      classes.map((cls) => [cls, sortedAreas(areaCounts.get(cls) ?? new Map())]),
    ),
  };
}

export function buildDigest(input: {
  from: string;
  to: string;
  commitsInRange: number;
  prs: Pr[];
  rows: LedgerRow[];
}): Digest {
  const index = buildLedgerIndex(input.rows);
  const prs = input.prs.map((pr) => classifyPr(pr, index));
  const count = (cls: PrClass) => prs.filter((pr) => pr.classes.includes(cls)).length;
  const groups: Digest["groups"] = {};
  for (const pr of prs) {
    for (const cls of pr.classes) {
      const byArea = (groups[cls] ??= {});
      for (const area of pr.areas[cls] ?? []) (byArea[area] ??= []).push(pr.ref);
    }
  }
  return {
    upstream: UPSTREAM_REPO,
    from: input.from,
    to: input.to,
    commitsInRange: input.commitsInRange,
    counts: {
      prs: prs.length,
      wire: count("wire"),
      "regression-risk": count("regression-risk"),
      "new-surface": count("new-surface"),
      todo: count("todo"),
      noise: count("noise"),
    },
    groups,
    prs,
    warnings: ledgerWarnings(input.rows),
  };
}

const ZERO_WIDTH_SPACE = String.fromCodePoint(0x200b);
const CODE_SPAN = /(`[^`\n]*`)/;
const LINKABLE =
  /((?:https?:\/\/|www\.)\S+|\S*github\.com\/\S+|[\w.-]+\/[\w.-]+(?:#\d+|@[0-9a-f]{7,40}))/i;

// GitHub does not autolink code spans, so upstream references stay unlinked and never add a
// cross-reference to pingdotgg/t3code. Any link or link-like ref in output must go through these.
export function code(text: string): string {
  return `\`${text}\``;
}

// Upstream titles can carry URLs, cross-repo refs, mentions, and bare #N; neutralize all of them.
export function sanitize(text: string): string {
  return text
    .split(CODE_SPAN)
    .map((part, i) =>
      i % 2 === 1
        ? part
        : part
            .split(LINKABLE)
            .map((piece, j) =>
              j % 2 === 1
                ? code(piece)
                : piece
                    .replace(/@(?=\w)/g, `@${ZERO_WIDTH_SPACE}`)
                    .replace(/#(?=\d)/g, `#${ZERO_WIDTH_SPACE}`),
            )
            .join(""),
    )
    .join("");
}

export function upstreamMarker(pr: Pr): string {
  return pr.number === null
    ? `<!-- upstream-commit:${pr.shortSha} -->`
    : `<!-- upstream-pr:${pr.number} -->`;
}

export function findExistingIssue<T extends { title: string; body?: string }>(
  pr: Pr,
  issues: T[],
): T | undefined {
  const marker = upstreamMarker(pr);
  const titleRef = `(${code(pr.ref)})`;
  return issues.find((issue) => issue.body?.includes(marker) || issue.title.includes(titleRef));
}

export function digestTitle(digest: Digest): string {
  return `Upstream digest ${digest.from.slice(0, 7)}..${digest.to.slice(0, 7)} (${digest.counts.prs} PRs)`;
}

export function summaryLine(digest: Digest): string {
  return [
    `**${digest.counts.prs} PRs**`,
    ...CLASSES.map((cls) => `${cls} ${digest.counts[cls]}`),
  ].join(" · ");
}

function areaOrder(a: string, b: string): number {
  return (
    Number(a.startsWith("unmapped:")) - Number(b.startsWith("unmapped:")) || a.localeCompare(b)
  );
}

function renderPr(pr: ClassifiedPr, cls: PrClass, options: RenderOptions): string[] {
  const bullet = options.checkboxes && cls !== "noise" ? "- [ ] " : "- ";
  const lines = [`${bullet}${code(pr.ref)} ${sanitize(pr.title)}`];
  if (cls === "noise") return lines;
  const rowIds = [...new Set(CLASSES.flatMap((c) => pr.rows[c] ?? []))];
  const otherAreas = (pr.areas[cls] ?? []).slice(1);
  const meta = [pr.classes.map((c) => `\`${c}\``).join(" ")];
  if (rowIds.length > 0) meta.push(`rows: ${rowIds.join(", ")}`);
  if (otherAreas.length > 0) meta.push(`also: ${otherAreas.join(", ")}`);
  lines.push(`  - ${meta.join(" · ")}`);
  if (!options.compact) {
    const ordered = [
      ...pr.files.filter((file) => !isNoiseFile(file)),
      ...pr.files.filter(isNoiseFile),
    ];
    const shown = ordered.slice(0, FILES_SHOWN).map((file) => `\`${file}\``);
    const more = pr.files.length > FILES_SHOWN ? ` (+${pr.files.length - FILES_SHOWN} more)` : "";
    lines.push(`  - files: ${shown.join(", ")}${more}`);
  }
  return lines;
}

export interface RenderOptions {
  checkboxes?: boolean;
  compact?: boolean;
}

export function renderMarkdown(digest: Digest, options: RenderOptions = {}): string {
  const { from, to } = digest;
  const lines = [
    `# ${digestTitle(digest)}`,
    "",
    `Upstream ${code(digest.upstream)} range ${code(`${from.slice(0, 10)}..${to.slice(0, 10)}`)} · ${digest.commitsInRange} first-parent commits, ${digest.counts.prs} touch watched paths`,
    "",
    summaryLine(digest),
    "",
  ];
  if (digest.counts.prs === 0) return `${lines.join("\n")}No upstream PRs touched watched paths.\n`;
  lines.push(
    "Counts include every class a PR carries. Each PR is listed once, under its first class in that order.",
    "",
  );
  for (const cls of CLASSES) {
    const prs = digest.prs.filter((pr) => pr.classes[0] === cls);
    if (prs.length === 0) continue;
    if (cls === "noise") {
      if (options.compact) continue;
      lines.push(`<details><summary>noise (${prs.length})</summary>`, "");
      for (const pr of prs) lines.push(...renderPr(pr, cls, options));
      lines.push("", "</details>", "");
      continue;
    }
    lines.push(`## ${cls} (${prs.length})`, "");
    const byArea = new Map<string, ClassifiedPr[]>();
    for (const pr of prs) {
      const area = pr.areas[cls]?.[0] ?? "(no area)";
      const group = byArea.get(area) ?? [];
      group.push(pr);
      byArea.set(area, group);
    }
    for (const area of [...byArea.keys()].sort(areaOrder)) {
      lines.push(`### ${area}`, "");
      for (const pr of byArea.get(area) ?? []) lines.push(...renderPr(pr, cls, options));
      lines.push("");
    }
  }
  return `${lines.join("\n").trimEnd()}\n`;
}

export function fitIssueBody(digest: Digest, limit = ISSUE_BODY_LIMIT): string {
  const full = renderMarkdown(digest, { checkboxes: true });
  if (full.length <= limit) return full;
  const compact = renderMarkdown(digest, { checkboxes: true, compact: true });
  if (compact.length <= limit) return compact;
  const note =
    "\n\n_Truncated to fit GitHub's issue size limit. The JSON digest (`--json`) lists every PR._\n";
  return compact.slice(0, compact.lastIndexOf("\n", limit - note.length)) + note;
}

export function prIssueTitle(pr: Pr): string {
  const title = pr.title.length > 200 ? `${pr.title.slice(0, 199)}…` : pr.title;
  return `parity: ${sanitize(title)} (${code(pr.ref)})`;
}

export function prIssueBody(pr: ClassifiedPr, digest: Digest, digestNumber: number | null): string {
  const lines = [
    upstreamMarker(pr),
    `Upstream ${code(pr.ref)} (${code(pr.shortSha)}) changed code the native client depends on.`,
    "",
    `- Classes: ${pr.classes.join(", ")}`,
  ];
  for (const cls of pr.classes) {
    if (pr.rows[cls]?.length) lines.push(`- ${cls} ledger rows: ${pr.rows[cls].join(", ")}`);
  }
  lines.push(
    `- Digest: ${digestNumber === null ? digestTitle(digest) : `#${digestNumber}`}`,
    "",
    "Watched files:",
    "",
  );
  for (const file of pr.files.slice(0, 100)) lines.push(`- \`${file}\``);
  if (pr.files.length > 100) lines.push(`- …and ${pr.files.length - 100} more`);
  return `${lines.join("\n")}\n`;
}

export function shellQuote(arg: string): string {
  return /^[\w@%+=:,./-]+$/.test(arg) ? arg : `'${arg.replaceAll("'", `'\\''`)}'`;
}

const SCRIPT_PATH = NodeURL.fileURLToPath(import.meta.url);
const SCRIPT_DIR = NodePath.dirname(SCRIPT_PATH);
const UPSTREAM_PATH = NodePath.join(SCRIPT_DIR, "UPSTREAM");
const LEDGER_PATH = NodePath.join(SCRIPT_DIR, "LEDGER.md");

const USAGE = `Usage: node apps/native/parity/sync.ts [mode flags] [options]

Modes (default is --report):
  --report        Print the markdown digest. No writes, no GitHub calls.
  --merge         Merge origin/main into the native branch when the tree is clean.
  --file-issues   File the digest issue and per-PR wire/regression-risk issues.
  --dry-run       With --file-issues: print the gh commands instead of running them.
  --advance       Rewrite UPSTREAM to the to-SHA after the other steps succeed.

Options:
  --from <rev>    Override the from-SHA read from UPSTREAM.
  --ledger <file> Read this ledger instead of LEDGER.md beside the script.
  --out <file>    Write the markdown digest to a file instead of stdout.
  --json <file>   Write the JSON digest (and step results) to a file.
  --no-fetch      Skip "git fetch origin main".
  -h, --help      Show this help.

Environment:
  PARITY_REPO     Repository that receives issues (default rynfar/t3code).
`;

const out = (line: string) => process.stdout.write(`${line}\n`);
const err = (line: string) => process.stderr.write(`${line}\n`);
const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

function makeGit(repoRoot: string) {
  return (args: string[]) =>
    NodeChildProcess.execFileSync("git", ["-C", repoRoot, ...args], {
      encoding: "utf8",
      maxBuffer: 1 << 30,
      stdio: ["ignore", "pipe", "pipe"],
    });
}

function revParse(git: ReturnType<typeof makeGit>, rev: string): string {
  try {
    return git(["rev-parse", "--verify", "--quiet", `${rev}^{commit}`]).trim();
  } catch {
    throw new Error(`cannot resolve "${rev}" to a commit`);
  }
}

async function runGh(args: string[], input?: string): Promise<string> {
  for (let attempt = 1; ; attempt++) {
    const result = NodeChildProcess.spawnSync("gh", args, {
      input,
      encoding: "utf8",
      maxBuffer: 1 << 28,
    });
    if (result.error) throw result.error;
    if (result.status === 0) return result.stdout.trim();
    const stderr = result.stderr.trim();
    if (!/rate limit|secondary|abuse/i.test(stderr) || attempt === 3) {
      throw new Error(`gh ${args.slice(0, 2).join(" ")} failed: ${stderr}`);
    }
    err(`GitHub rate limit hit; retrying in ${60 * attempt}s`);
    await sleep(60_000 * attempt);
  }
}

async function listIssues(
  repo: string,
  label: string,
): Promise<{ number: number; title: string; body: string }[]> {
  const json = await runGh([
    "issue",
    "list",
    "--repo",
    repo,
    "--label",
    label,
    "--state",
    "all",
    "--limit",
    "10000",
    "--json",
    "number,title,body",
  ]);
  return JSON.parse(json);
}

interface MergeResult {
  status: "merged" | "skipped" | "conflict" | "failed";
  detail: string;
  conflicts?: string[];
}

function mergeUpstream(git: ReturnType<typeof makeGit>, repoRoot: string, to: string): MergeResult {
  const branch = git(["branch", "--show-current"]).trim();
  if (branch !== "native")
    return { status: "skipped", detail: `current branch is "${branch}", not native` };
  if (git(["status", "--porcelain", "--untracked-files=no"]).trim()) {
    return { status: "skipped", detail: "tracked files have uncommitted changes" };
  }
  if (revParse(git, "origin/main") !== to) {
    return {
      status: "skipped",
      detail: "origin/main moved during this run; rerun to digest the new range",
    };
  }
  const merge = NodeChildProcess.spawnSync(
    "git",
    ["-C", repoRoot, "merge", "--no-edit", "origin/main"],
    {
      encoding: "utf8",
    },
  );
  if (merge.status === 0)
    return { status: "merged", detail: merge.stdout.trim().split("\n")[0] ?? "merged" };
  const conflicts = git(["diff", "--name-only", "--diff-filter=U"])
    .trim()
    .split("\n")
    .filter(Boolean);
  const inProgress =
    NodeChildProcess.spawnSync("git", ["-C", repoRoot, "rev-parse", "-q", "--verify", "MERGE_HEAD"])
      .status === 0;
  if (inProgress) NodeChildProcess.spawnSync("git", ["-C", repoRoot, "merge", "--abort"]);
  if (conflicts.length > 0)
    return { status: "conflict", detail: "merge aborted; branch unchanged", conflicts };
  return { status: "failed", detail: (merge.stderr || merge.stdout).trim() };
}

async function fileIssues(digest: Digest, repo: string, dryRun: boolean): Promise<string[]> {
  const results: string[] = [];
  const report = (line: string) => {
    results.push(line);
    out(line);
  };
  let creates = 0;
  const create = async (args: string[], body?: string): Promise<string> => {
    if (dryRun) {
      const command = ["gh", ...args].map(shellQuote).join(" ");
      out(
        body === undefined ? command : `${command} <<'PARITY_EOF'\n${body.trimEnd()}\nPARITY_EOF`,
      );
      return "";
    }
    if (creates++ > 0) await sleep(CREATE_DELAY_MS);
    return runGh(args, body);
  };

  if (dryRun)
    out("# Dry run. Existing labels and issues are checked at run time, and matches are skipped.");
  const existingLabels = dryRun
    ? new Set<string>()
    : new Set<string>(
        JSON.parse(
          await runGh(["label", "list", "--repo", repo, "--limit", "1000", "--json", "name"]),
        ).map((label: { name: string }) => label.name),
      );
  for (const label of LABELS) {
    if (existingLabels.has(label.name)) continue;
    await create([
      "label",
      "create",
      label.name,
      "--repo",
      repo,
      "--color",
      label.color,
      "--description",
      label.description,
    ]);
    if (!dryRun) report(`created label ${label.name}`);
  }

  if (digest.counts.prs === 0) {
    report("no upstream PRs touched watched paths; no issues filed");
    return results;
  }

  const title = digestTitle(digest);
  const existingDigest = dryRun
    ? undefined
    : (await listIssues(repo, "upstream-digest")).find((issue) => issue.title === title);
  let digestNumber = existingDigest?.number ?? null;
  if (existingDigest) {
    report(`exists #${existingDigest.number} ${title}`);
  } else {
    const url = await create(
      [
        "issue",
        "create",
        "--repo",
        repo,
        "--title",
        title,
        "--label",
        "upstream-digest",
        "--body-file",
        "-",
      ],
      fitIssueBody(digest),
    );
    if (!dryRun) {
      digestNumber = Number(url.match(/\/issues\/(\d+)/)?.[1]) || null;
      report(`created ${url} ${title}`);
    }
  }

  const parityIssues = dryRun ? [] : await listIssues(repo, "parity");
  for (const pr of digest.prs) {
    if (!pr.classes.some((cls) => ISSUE_CLASSES.includes(cls))) continue;
    const existing = findExistingIssue(pr, parityIssues);
    if (existing) {
      report(`exists #${existing.number} for ${pr.ref}`);
      continue;
    }
    const labels = [
      "parity",
      ...pr.classes.filter((cls) => LABELED_CLASSES.includes(cls)).map((cls) => `parity:${cls}`),
    ];
    const url = await create(
      [
        "issue",
        "create",
        "--repo",
        repo,
        "--title",
        prIssueTitle(pr),
        ...labels.flatMap((label) => ["--label", label]),
        "--body-file",
        "-",
      ],
      prIssueBody(pr, digest, digestNumber),
    );
    if (!dryRun) report(`created ${url} for ${pr.ref}`);
  }
  return results;
}

function parseCli(argv: string[]) {
  return NodeUtil.parseArgs({
    args: argv,
    strict: true,
    options: {
      report: { type: "boolean" },
      merge: { type: "boolean" },
      "file-issues": { type: "boolean" },
      "dry-run": { type: "boolean" },
      advance: { type: "boolean" },
      from: { type: "string" },
      ledger: { type: "string" },
      out: { type: "string" },
      json: { type: "string" },
      "no-fetch": { type: "boolean" },
      help: { type: "boolean", short: "h" },
    },
  }).values;
}

async function main(argv: string[]): Promise<number> {
  let values: ReturnType<typeof parseCli>;
  try {
    values = parseCli(argv);
  } catch (error) {
    err(`${error instanceof Error ? error.message : String(error)}\n\n${USAGE}`);
    return 2;
  }
  if (values.help) {
    out(USAGE);
    return 0;
  }
  if (values["dry-run"] && !values["file-issues"]) {
    err("--dry-run only applies to --file-issues");
    return 2;
  }
  if (values.report && (values.merge || values["file-issues"] || values.advance)) {
    err("--report is a dry run and cannot be combined with --merge, --file-issues, or --advance");
    return 2;
  }

  const repoRoot = NodeChildProcess.execFileSync(
    "git",
    ["-C", SCRIPT_DIR, "rev-parse", "--show-toplevel"],
    {
      encoding: "utf8",
    },
  ).trim();
  const git = makeGit(repoRoot);
  const run: { digest?: Digest; merge?: MergeResult; issues?: string[]; advanced?: string } = {};
  try {
    if (!values["no-fetch"]) git(["fetch", "--quiet", "origin", "main"]);
    const to = revParse(git, "origin/main");
    const fromRev =
      values.from ??
      (NodeFS.existsSync(UPSTREAM_PATH) ? NodeFS.readFileSync(UPSTREAM_PATH, "utf8").trim() : "");
    if (!fromRev)
      throw new Error(`no from-SHA: ${UPSTREAM_PATH} is missing or empty and --from was not given`);
    const from = revParse(git, fromRev);
    const ledgerPath = values.ledger ?? LEDGER_PATH;
    if (values.ledger && !NodeFS.existsSync(ledgerPath))
      throw new Error(`ledger not found: ${ledgerPath}`);
    const rows = NodeFS.existsSync(ledgerPath)
      ? parseLedger(NodeFS.readFileSync(ledgerPath, "utf8"))
      : [];

    const commitsInRange = Number(
      git(["rev-list", "--count", "--first-parent", `${from}..${to}`]).trim(),
    );
    // --no-renames lists both sides of a move, so moving a mirrored file still matches its ledger row.
    const log = git([
      "--literal-pathspecs",
      "-c",
      "core.quotePath=false",
      "log",
      "--first-parent",
      "--diff-merges=first-parent",
      "--no-renames",
      "--reverse",
      "--format=%x1e%H%x1f%h%x1f%s",
      "--name-only",
      `${from}..${to}`,
      "--",
      ...watchedPathspecs(rows),
    ]);
    const digest = buildDigest({
      from,
      to,
      commitsInRange,
      prs: groupCommits(parseGitLog(log)),
      rows,
    });
    if (
      NodeChildProcess.spawnSync("git", ["-C", repoRoot, "merge-base", "--is-ancestor", from, to])
        .status !== 0
    ) {
      digest.warnings.push(
        `from ${from.slice(0, 10)} is not an ancestor of origin/main; upstream history may have been rewritten`,
      );
    }
    run.digest = digest;
    for (const warning of digest.warnings) err(`warning: ${warning}`);

    const markdown = renderMarkdown(digest);
    if (values.out) NodeFS.writeFileSync(values.out, markdown);
    else if (!values["file-issues"]) process.stdout.write(markdown);

    if (values.merge) {
      run.merge = mergeUpstream(git, repoRoot, to);
      err(`merge ${run.merge.status}: ${run.merge.detail}`);
      for (const file of run.merge.conflicts ?? []) err(`  conflict: ${file}`);
      if (run.merge.status !== "merged") return 1;
    }
    if (values["file-issues"]) {
      run.issues = await fileIssues(
        digest,
        process.env.PARITY_REPO || "rynfar/t3code",
        Boolean(values["dry-run"]),
      );
    }
    if (values.advance) {
      NodeFS.writeFileSync(UPSTREAM_PATH, `${to}\n`);
      run.advanced = to;
      err(`UPSTREAM advanced to ${to}`);
    }
    return 0;
  } finally {
    if (values.json && run.digest) {
      const { digest, ...steps } = run;
      NodeFS.writeFileSync(values.json, `${JSON.stringify({ ...digest, ...steps }, null, 2)}\n`);
    }
  }
}

if (process.argv[1] && NodeFS.realpathSync(process.argv[1]) === NodeFS.realpathSync(SCRIPT_PATH)) {
  main(process.argv.slice(2)).then(
    (code) => {
      process.exitCode = code;
    },
    (error: unknown) => {
      err(`parity sync failed: ${error instanceof Error ? error.message : String(error)}`);
      process.exitCode = 1;
    },
  );
}
