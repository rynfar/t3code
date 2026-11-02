// Shell cases: shell.ts applies snapshots with `mergeShellSnapshotProjects` and each run of
// deltas with `applyShellStreamEvents`; ported from shellReducer.test.ts.
import type {
  OrchestrationProjectShell,
  OrchestrationV2ShellSnapshot,
  OrchestrationV2ShellStreamItem,
  OrchestrationV2ThreadShell,
} from "../../../packages/contracts/src/index.ts";
import type * as ShellReducerModule from "../../../packages/client-runtime/src/state/shellReducer.ts";

import {
  decodeShellItem,
  encodeShellItem,
  encodeShellSnapshot,
  Interner,
  Fixtures,
  ShellReducer,
} from "./runtime.ts";
import type { Json } from "./runtime.ts";

const repositoryIdentity = {
  canonicalKey: "github.com/example/repo",
  locator: {
    source: "git-remote",
    remoteName: "origin",
    remoteUrl: "https://github.com/example/repo.git",
  },
} as unknown as NonNullable<OrchestrationProjectShell["repositoryIdentity"]>;
const otherIdentity = {
  canonicalKey: "github.com/example/other",
  locator: {
    source: "git-remote",
    remoteName: "origin",
    remoteUrl: "https://github.com/example/other.git",
  },
} as unknown as NonNullable<OrchestrationProjectShell["repositoryIdentity"]>;

function applyShellItem(
  snapshot: OrchestrationV2ShellSnapshot | null,
  item: OrchestrationV2ShellStreamItem,
): OrchestrationV2ShellSnapshot | null {
  if (item.kind === "synchronized") return snapshot;
  if (item.kind === "snapshot") {
    return ShellReducer.mergeShellSnapshotProjects(
      snapshot,
      item.snapshot,
      item.resolvedRepositoryIdentityRoots === undefined
        ? undefined
        : { resolvedRepositoryIdentityRoots: item.resolvedRepositoryIdentityRoots },
    );
  }
  return snapshot === null ? null : ShellReducer.applyShellStreamEvents(snapshot, [item]);
}

/** States after every item, applying deltas one at a time; shell.ts batches a frame's deltas,
 *  and its own invariant says that is the same, which this checks before writing. */
function shellCase(name: string, items: ReadonlyArray<OrchestrationV2ShellStreamItem>) {
  const json = items.map(encodeShellItem);
  const decoded = json.map(decodeShellItem);
  const interner = new Interner();
  const states: Json[] = [];
  let snapshot: OrchestrationV2ShellSnapshot | null = null;
  for (const item of decoded) {
    snapshot = applyShellItem(snapshot, item);
    states.push(snapshot === null ? null : interner.record(encodeShellSnapshot(snapshot)));
  }
  let batched: OrchestrationV2ShellSnapshot | null = null;
  let deltas: ShellReducerModule.ShellStreamEvent[] = [];
  for (const item of [...decoded, { kind: "synchronized" } as const]) {
    if (item.kind !== "snapshot" && item.kind !== "synchronized") {
      deltas.push(item);
      continue;
    }
    if (batched !== null && deltas.length > 0)
      batched = ShellReducer.applyShellStreamEvents(batched, deltas);
    deltas = [];
    batched = applyShellItem(batched, item);
  }
  const finalState = (value: OrchestrationV2ShellSnapshot | null) =>
    JSON.stringify(value === null ? null : encodeShellSnapshot(value));
  if (finalState(batched) !== finalState(snapshot)) {
    throw new Error(`${name}: batched shell deltas differ from sequential ones`);
  }
  return { name, items: json, table: interner.table, states };
}

export function shellCases() {
  const project = Fixtures.v2Project;
  const base = Fixtures.v2ShellSnapshot;
  const thread = Fixtures.v2ThreadShell;
  const shellRow = (id: string, extra: Partial<OrchestrationV2ThreadShell> = {}) =>
    ({ ...thread, id, ...extra }) as OrchestrationV2ThreadShell;
  const snapshot = (
    value: Partial<OrchestrationV2ShellSnapshot>,
    roots?: string[],
  ): OrchestrationV2ShellStreamItem =>
    ({
      kind: "snapshot",
      snapshot: { ...base, ...value },
      ...(roots === undefined ? {} : { resolvedRepositoryIdentityRoots: roots }),
    }) as OrchestrationV2ShellStreamItem;
  const projectUpdated = (
    sequence: number,
    value: OrchestrationProjectShell,
  ): OrchestrationV2ShellStreamItem => ({ kind: "project.updated", sequence, project: value });
  const update = (
    sequence: number,
    value: OrchestrationV2ThreadShell,
    location: "active" | "archive" = "active",
  ): OrchestrationV2ShellStreamItem => ({
    kind: "thread.updated",
    sequence,
    location,
    thread: value,
  });
  const remove = (sequence: number, threadId: string): OrchestrationV2ShellStreamItem =>
    ({
      kind: "thread.removed",
      sequence,
      location: "active",
      threadId,
    }) as OrchestrationV2ShellStreamItem;
  const sync: OrchestrationV2ShellStreamItem = { kind: "synchronized" };
  const otherProject = {
    ...project,
    id: "project-other",
    workspaceRoot: "/workspace/other",
    repositoryIdentity: otherIdentity,
  } as OrchestrationProjectShell;

  return [
    shellCase("stale project updates are ignored", [
      snapshot({ snapshotSequence: 4 }),
      sync,
      projectUpdated(3, { ...project, title: "Stale" }),
      sync,
      projectUpdated(4, { ...project, title: "Stale" }),
    ]),
    shellCase("delta with null identity keeps the prior identity", [
      snapshot({}),
      sync,
      projectUpdated(1, { ...project, repositoryIdentity }),
      sync,
      projectUpdated(2, { ...project, title: "Same root", repositoryIdentity: null }),
    ]),
    shellCase("workspace root move drops the identity", [
      snapshot({}),
      projectUpdated(1, { ...project, repositoryIdentity }),
      sync,
      projectUpdated(2, { ...project, workspaceRoot: "/tmp/other-root", repositoryIdentity: null }),
    ]),
    shellCase("authoritative lower snapshot resets but keeps identities", [
      snapshot({
        snapshotSequence: 9,
        projects: [{ ...project, title: "Client ahead", repositoryIdentity }, otherProject],
        threads: [shellRow(thread.id, { title: "Local-only thread" })],
      }),
      snapshot({
        snapshotSequence: 3,
        projects: [
          { ...project, title: "Server reset", repositoryIdentity: null },
          { ...otherProject, repositoryIdentity: null },
        ],
        threads: [shellRow(thread.id, { title: "Server thread" })],
      }),
    ]),
    shellCase("marked enrichment keeps structure, then an authoritative snapshot resets", [
      snapshot({
        snapshotSequence: 20,
        projects: [{ ...project, title: "Client ahead", repositoryIdentity }],
        threads: [shellRow(thread.id, { title: "Local" })],
      }),
      snapshot(
        {
          snapshotSequence: 3,
          projects: [{ ...project, title: "Server", repositoryIdentity: null }],
          threads: [shellRow(thread.id, { title: "Server" })],
        },
        [project.workspaceRoot],
      ),
      snapshot({
        snapshotSequence: 3,
        projects: [{ ...project, title: "Server", repositoryIdentity: null }],
        threads: [shellRow(thread.id, { title: "Server" })],
      }),
    ]),
    shellCase("enrichment only fills identities", [
      snapshot({
        snapshotSequence: 5,
        projects: [
          { ...project, repositoryIdentity: null },
          { ...otherProject, repositoryIdentity: null },
        ],
      }),
      snapshot(
        {
          snapshotSequence: 4,
          projects: [{ ...project, repositoryIdentity }, otherProject],
          threads: [],
        },
        [],
      ),
      snapshot(
        {
          snapshotSequence: 4,
          projects: [
            { ...project, repositoryIdentity: null },
            { ...otherProject, repositoryIdentity: null },
          ],
        },
        [project.workspaceRoot],
      ),
      snapshot(
        {
          snapshotSequence: 4,
          projects: [
            {
              ...project,
              title: "Ignored",
              workspaceRoot: "/moved",
              repositoryIdentity: otherIdentity,
            },
          ],
        },
        ["/moved"],
      ),
    ]),
    shellCase("enrichment cannot resurrect empty structure", [
      snapshot({ snapshotSequence: 7, projects: [], threads: [], archivedThreads: [] }),
      snapshot(
        { snapshotSequence: 4, projects: [{ ...project, repositoryIdentity }], threads: [thread] },
        [project.workspaceRoot],
      ),
    ]),
    shellCase("archive deltas leave the shell", [
      snapshot({ archivedThreads: [shellRow("archived-old")] }),
      update(3, { ...thread, archivedAt: thread.updatedAt }, "archive"),
      update(4, thread),
      update(5, shellRow("archived-old"), "archive"),
    ]),
    shellCase("optional row fields the native types model", [
      snapshot({
        projects: [{ ...project, pinnedAt: "2026-06-20T01:00:00.000Z", archivedAt: null }],
        olderSettled: {
          before: "2026-06-01T00:00:00.000Z",
          asOfSequence: 0,
          counts: [{ projectId: project.id, count: 3 }],
        },
        threads: [
          shellRow("rich", {
            lineage: { ...thread.lineage, sideChat: false },
            activityRunStatus: "running",
            latestRunStopRequested: true,
            lastError: "Provider unavailable",
            snoozedUntil: thread.updatedAt,
            pinnedAt: thread.updatedAt,
            pinOrderKey: "a0",
            activeOrderKey: "b0",
            delegatedFrom: {
              environmentId: "environment-linked",
              threadId: "thread-linked",
              title: "Linked",
            },
            activeProviderThreadId: "provider-thread-rich",
            latestRunRequestedAt: thread.updatedAt,
            latestRunStartedAt: thread.updatedAt,
            latestRunCompletedAt: null,
            activityRunStartedAt: thread.updatedAt,
            blockedOnThreadWait: true,
            lastErrorClass: "usage_limit",
            pendingGuestMessageCount: 2,
            latestUserAuthoredMessageAt: thread.updatedAt,
            pendingBackgroundTasks: [
              { taskId: "task-1", kind: "subagent" },
              { taskId: "task-2", kind: "command", awaited: true },
            ],
            goal: { objective: "Ship the sidebar", status: "active" },
            unsettledAt: thread.updatedAt,
            snoozedAt: thread.updatedAt,
            autoSettleDisabledAt: thread.updatedAt,
            opens: {
              openedAt: 1_750_000_000_000,
              score: 2,
              picks: [{ query: "side", at: 1_750_000_000_000 }],
            },
            titleRegeneration: { requestId: "command-regenerate", startedAt: thread.updatedAt },
          } as Partial<OrchestrationV2ThreadShell>),
        ],
        archivedThreads: [
          shellRow("archived-rich", {
            lineage: { ...thread.lineage, sideChat: true },
            activityRunStatus: null,
            latestRunStopRequested: false,
            lastError: null,
            snoozedUntil: null,
            pinnedAt: null,
            pinOrderKey: null,
            activeOrderKey: null,
            archivedAt: thread.updatedAt,
            delegatedFrom: {
              environmentId: "environment-linked",
              threadId: "thread-archived",
              title: "Archived",
            },
            latestRunRequestedAt: null,
            latestRunStartedAt: null,
            blockedOnThreadWait: false,
            lastErrorClass: null,
            pendingGuestMessageCount: 0,
            goal: null,
            unsettledAt: null,
            snoozedAt: null,
            autoSettleDisabledAt: null,
            opens: null,
            titleRegeneration: null,
          } as Partial<OrchestrationV2ThreadShell>),
        ],
      }),
      update(1, shellRow("rich", { activityRunStatus: null, pinOrderKey: "a1" })),
    ]),
    shellCase("mixed deltas in one batch and across batches", [
      snapshot({
        snapshotSequence: 4,
        projects: [{ ...project, repositoryIdentity }, otherProject],
        threads: [shellRow("a"), shellRow("b"), shellRow("unchanged")],
        archivedThreads: [shellRow("archived"), shellRow("old-archive")],
      }),
      update(5, shellRow("b", { title: "Renamed" })),
      remove(5, "a"),
      remove(6, "a"),
      update(7, shellRow("a")),
      sync,
      update(8, shellRow("b"), "archive"),
      update(9, shellRow("archived")),
      update(10, shellRow("old-archive"), "archive"),
      update(11, shellRow("new")),
      remove(12, "missing"),
      remove(11, "new"),
      remove(13, "new"),
      update(14, shellRow("new")),
      sync,
      update(15, shellRow("archived")),
      projectUpdated(16, { ...project, title: "Updated" }),
      projectUpdated(17, { ...project, workspaceRoot: "/moved" }),
      {
        kind: "project.removed",
        sequence: 18,
        projectId: project.id,
      } as OrchestrationV2ShellStreamItem,
      projectUpdated(19, project),
      update(20, shellRow("unchanged")),
      update(21, shellRow("unchanged", { status: "running", activeRunId: "run-1" })),
    ]),
  ];
}
