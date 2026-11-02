// Shelf cases: the Settled shelf's older-settled logic (olderSettledThreads.ts) and the merge of
// rows a shell omits (threadFamilies.ts), on seeded synthetic threads for one environment.
import type {
  EnvironmentId,
  OrchestrationV2OlderSettledCursor,
  OrchestrationV2ThreadFamily,
  OrchestrationV2ThreadShell,
  ProjectId,
  ThreadId,
} from "../../../packages/contracts/src/index.ts";
import type * as OlderSettledModule from "../../../packages/client-runtime/src/state/olderSettledThreads.ts";

import { random } from "./builders.ts";
import { DateTime, encodeThreadShell, Fixtures, OlderSettled, ThreadFamilies } from "./runtime.ts";
import type { Json } from "./runtime.ts";

type Shell = OrchestrationV2ThreadShell;
type Rng = ReturnType<typeof random>;
const ENVIRONMENT = "environment-1" as EnvironmentId;
const DAY = 86_400_000;
const T0 = Date.UTC(2026, 5, 1);
const iso = (ms: number) => new Date(ms).toISOString();
const PROJECTS = ["project-a", "project-b", "project-c"];

function shellThread(rng: Rng, index: number): Shell {
  // Few distinct days, so settle-time ties at a cursor are common.
  const settledAt = rng.chance(0.75) ? DateTime.makeUnsafe(T0 + (10 + rng.int(8)) * DAY) : null;
  const lineage = rng.chance(0.15)
    ? { rootThreadId: "root", parentThreadId: "root", relationshipToParent: "subagent" as const }
    : rng.chance(0.1)
      ? { rootThreadId: "root", parentThreadId: "root", relationshipToParent: null, sideChat: true }
      : { rootThreadId: `t${index}`, parentThreadId: null, relationshipToParent: null };
  return {
    ...Fixtures.v2ThreadShell,
    // Ids in random order, unique by index, mixed case like provider-derived ids.
    id: `t${String(rng.int(1000)).padStart(3, "0")}${rng.chance(0.3) ? "Z" : "a"}${index}`,
    projectId: rng.pick(PROJECTS),
    lineage,
    settledOverride: rng.pick(["settled", "settled", "active", null] as const),
    settledAt,
    createdAt: DateTime.makeUnsafe(T0 + rng.int(40) * DAY),
  } as Shell;
}

const encodeAll = (threads: ReadonlyArray<Shell>) => threads.map(encodeThreadShell);
const cursorJson = (cursor: OrchestrationV2OlderSettledCursor | null): Json =>
  cursor === null ? null : { settledAt: cursor.settledAt, threadId: cursor.threadId ?? null };

function cursorFor(rng: Rng, threads: ReadonlyArray<Shell>): OrchestrationV2OlderSettledCursor {
  const settled = threads.filter((thread) => thread.settledAt !== null);
  const anchor = settled.length > 0 && rng.chance(0.8) ? rng.pick(settled) : null;
  const settledAt = anchor?.settledAt
    ? DateTime.formatIso(anchor.settledAt)
    : iso(T0 + (10 + rng.int(8)) * DAY);
  return rng.chance(0.7)
    ? { settledAt, threadId: (anchor?.id ?? "t500a0") as ThreadId }
    : { settledAt };
}

function countKeyCase(seed: number) {
  const rng = random(seed);
  const threads = Array.from({ length: 12 }, (_, index) => shellThread(rng, index));
  const beforeMs = T0 + (10 + rng.int(9)) * DAY;
  const removedAt = new Map<ThreadId, number>(
    Array.from({ length: rng.int(3) }, (_, index) => [
      `gone${index}` as ThreadId,
      100 + rng.int(50),
    ]),
  );
  return {
    kind: "countKey",
    threads: encodeAll(threads),
    before: iso(beforeMs),
    removedAt: [...removedAt],
    expected: OlderSettled.olderSettledCountKey(threads, beforeMs, removedAt),
  };
}

function listedCase(seed: number) {
  const rng = random(seed);
  const threads = Array.from({ length: 16 }, (_, index) => shellThread(rng, index));
  const checks = threads.map((thread) => {
    const cursor = rng.chance(0.15) ? null : cursorFor(rng, threads);
    return {
      thread: encodeThreadShell(thread),
      cursor: cursorJson(cursor),
      expected: OlderSettled.isListedSettledThread(thread, cursor),
    };
  });
  return { kind: "listed", checks };
}

function unloadedCase(seed: number) {
  const rng = random(seed);
  const threads = Array.from({ length: 14 }, (_, index) => shellThread(rng, index));
  const beforeMs = T0 + (10 + rng.int(9)) * DAY;
  const counts = new Map(PROJECTS.map((project) => [project as ProjectId, rng.int(8)] as const));
  const projectIds = rng.chance(0.5)
    ? null
    : PROJECTS.filter(() => rng.chance(0.6)).map((id) => id as ProjectId);
  const older: OlderSettledModule.OlderSettled = { before: iso(beforeMs), beforeMs, counts };
  const expected = OlderSettled.unloadedOlderSettledCount({
    olderSettled: () => older,
    scopes: [{ environmentId: ENVIRONMENT, projectIds }],
    threads: threads.map((source) => ({ environmentId: ENVIRONMENT, source })),
  });
  return {
    kind: "unloaded",
    threads: encodeAll(threads),
    before: iso(beforeMs),
    counts: [...counts],
    projectIds,
    expected,
  };
}

type ShelfInput = {
  readonly older: boolean;
  readonly status: OlderSettledModule.OlderSettledPages["status"];
  readonly frontier: "none" | "cut" | "tie";
  readonly requested: number;
  readonly listedCount: number;
  readonly visibleCount: number;
};

function shelfStateCase(input: ShelfInput) {
  const beforeMs = T0 + 20 * DAY;
  const older: OlderSettledModule.OlderSettled | null = input.older
    ? { before: iso(beforeMs), beforeMs, counts: new Map() }
    : null;
  const frontier =
    input.frontier === "none"
      ? null
      : input.frontier === "cut"
        ? { settledAt: iso(beforeMs) }
        : { settledAt: iso(beforeMs - 3 * DAY), threadId: "t042a7" as ThreadId };
  const reads: OrchestrationV2ThreadFamily[] = Array.from({ length: input.requested }, () => ({
    snapshotSequence: 1,
    threads: [],
  }));
  const pages: OlderSettledModule.OlderSettledPages = { reads, frontier, status: input.status };
  const shelfFrontier = OlderSettled.settledShelfFrontier([{ older, pages }]);
  const wanted = OlderSettled.wantedOlderSettledPages({
    requested: new Map(input.requested > 0 ? [[ENVIRONMENT, input.requested]] : []),
    environments: new Map([[ENVIRONMENT, { older, pages }]]),
    listedCount: input.listedCount,
    visibleCount: input.visibleCount,
  });
  return {
    kind: "shelf",
    older: older === null ? null : older.before,
    requested: input.requested,
    status: input.status,
    frontier: cursorJson(frontier),
    listedCount: input.listedCount,
    visibleCount: input.visibleCount,
    expectedFrontier: cursorJson(shelfFrontier),
    expectedRequested: wanted.get(ENVIRONMENT) ?? 0,
  };
}

function rowsMissingCase(seed: number) {
  const rng = random(seed);
  const pool = Array.from({ length: 12 }, (_, index) => shellThread(rng, index));
  const shellThreads = pool.filter(() => rng.chance(0.4));
  const families: OrchestrationV2ThreadFamily[] = Array.from({ length: 1 + rng.int(3) }, () => ({
    snapshotSequence: 10 + rng.int(20),
    threads: pool.filter(() => rng.chance(0.5)),
  }));
  const removedAt = new Map<ThreadId, number>(
    pool.filter(() => rng.chance(0.2)).map((thread) => [thread.id, 10 + rng.int(20)]),
  );
  return {
    kind: "rowsMissing",
    shellThreads: encodeAll(shellThreads),
    families: families.map((family) => ({
      snapshotSequence: family.snapshotSequence,
      threads: encodeAll(family.threads),
    })),
    removedAt: [...removedAt],
    expected: ThreadFamilies.threadFamilyRowsMissingFromShell({
      shellThreads,
      families,
      removedAt,
    }).map((thread) => thread.id),
  };
}

export function shelfCases() {
  const cases = [];
  for (let seed = 0; seed < 12; seed += 1) {
    cases.push({ name: `count key ${seed}`, ...countKeyCase(5000 + seed) });
    cases.push({ name: `listed ${seed}`, ...listedCase(6000 + seed) });
    cases.push({ name: `unloaded ${seed}`, ...unloadedCase(7000 + seed) });
    cases.push({ name: `rows missing ${seed}`, ...rowsMissingCase(9000 + seed) });
  }
  for (const older of [true, false]) {
    for (const status of ["ready", "pending", "failed"] as const) {
      for (const frontier of ["none", "cut", "tie"] as const) {
        for (const [requested, listedCount, visibleCount] of [
          [0, 0, 0],
          [0, 3, 10],
          [1, 12, 10],
          [2, 20, 35],
        ] as const) {
          const input = { older, status, frontier, requested, listedCount, visibleCount };
          cases.push({ name: `shelf state ${JSON.stringify(input)}`, ...shelfStateCase(input) });
        }
      }
    }
  }
  return cases;
}
