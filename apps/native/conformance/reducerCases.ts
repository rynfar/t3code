// Reducer cases: `applyOrchestrationV2ProjectionEvent` on decoded projections and events, ported
// from orchestrationV2Projection.test.ts, plus id-collation ties and seeded fuzz.
import type {
  OrchestrationV2DomainEvent,
  OrchestrationV2Run,
} from "../../../packages/contracts/src/index.ts";
import type * as ProjectionModule from "../../../packages/client-runtime/src/state/orchestrationV2Projection.ts";

import {
  decodeEvent,
  decodeProjection,
  encodeEvent,
  encodeProjection,
  Fixtures,
  Interner,
  Projection,
} from "./runtime.ts";
import type { Item, Json, Row, ThreadProjection } from "./runtime.ts";
import {
  at,
  attempt,
  checkpointFor,
  event,
  fullProjection,
  itemEvent,
  messageFor,
  NOW,
  PARENT,
  projectionOf,
  random,
  row,
  run,
  runEvent,
  subagentFor,
  turnItem,
} from "./builders.ts";
import type { ItemKind } from "./builders.ts";

type ReducerStep = {
  readonly event: OrchestrationV2DomainEvent;
  readonly options?: ProjectionModule.ApplyOrchestrationV2ProjectionEventOptions;
};

function reducerCase(name: string, initial: ThreadProjection, steps: ReadonlyArray<ReducerStep>) {
  const initialJson = encodeProjection(initial);
  const stepsJson = steps.map((step) => ({
    event: encodeEvent(step.event),
    options:
      step.options === undefined
        ? null
        : ({
            partialTimeline: step.options.partialTimeline === true,
            latestLocalTurnOrdinal: step.options.latestLocalTurnOrdinal ?? null,
          } as Json),
  }));
  const interner = new Interner();
  let state: ThreadProjection | null = decodeProjection(initialJson);
  const states = [interner.record(encodeProjection(state))];
  for (const step of stepsJson) {
    const options = step.options as {
      partialTimeline: boolean;
      latestLocalTurnOrdinal: number | null;
    } | null;
    state = Projection.applyOrchestrationV2ProjectionEvent(
      state,
      decodeEvent(step.event),
      options ?? undefined,
    );
    if (state === null) throw new Error(`${name}: the reducer dropped the projection`);
    states.push(interner.record(encodeProjection(state)));
  }
  return { name, initial: initialJson, steps: stepsJson, table: interner.table, states };
}

export function portedReducerCases() {
  const runId = "run-reducer";
  const completed = run(runId, 1, "completed");
  const command = (id: string, ordinal = 1, output = "done") =>
    turnItem("command", id, ordinal, { runId, text: output });
  const cases = [];

  const stable = command("item-stable");
  cases.push(
    reducerCase(
      "run update keeps membership",
      projectionOf({ runs: [completed], items: [stable] }),
      [{ event: runEvent({ ...completed }) }],
    ),
  );

  const first = command("item-first", 1, "first");
  const second = command("item-second", 1, "second");
  cases.push(
    reducerCase(
      "streamed replacement keeps other rows",
      projectionOf({ runs: [completed], items: [first, second] }),
      [{ event: itemEvent(command("item-first", 1, "streamed output")) }],
    ),
  );

  const queuedFuture = command("item-queued-future", 300, "queued");
  cases.push(
    reducerCase(
      "live items insert by ordinal",
      projectionOf({ runs: [completed], items: [queuedFuture] }),
      [{ event: itemEvent(command("item-active-assistant", 201)) }],
    ),
  );

  const inherited = turnItem("command", "item-inherited", 0, { runId, threadId: PARENT });
  const moveItems = [
    command("item-a", 1, "a"),
    command("item-b", 2, "b"),
    command("item-c", 2, "c"),
  ];
  cases.push(
    reducerCase(
      "moved item reorders by ordinal then id",
      projectionOf({
        runs: [completed],
        turnItems: moveItems,
        visibleTurnItems: [
          row(inherited, 0, "inherited"),
          ...moveItems.map((item, i) => row(item, i + 1)),
        ],
      }),
      [
        { event: itemEvent(command("item-a", 2, "moved")) },
        { event: itemEvent(command("item-a", 3, "later")) },
      ],
    ),
  );

  const queuedRun = run("run-queued", 2, "queued");
  const steerRun = run("run-steer", 3, "completed");
  const runItem = (id: string, itemRunId: string, ordinal: number) =>
    turnItem("command", id, ordinal, { runId: itemRunId });
  const steerItems = [
    runItem("item-steer-user", "run-steer", 3_000_001),
    runItem("item-steer-reply", "run-steer", 3_000_002),
  ];
  let watermark = 3_000_002;
  const queuedSteps: ReducerStep[] = [];
  for (const next of [
    itemEvent(runItem("item-queued-user", "run-queued", 2_000_001)),
    runEvent({ ...queuedRun, status: "running" }),
    itemEvent(runItem("item-queued-reply", "run-queued", 2_000_002)),
    runEvent({ ...queuedRun, status: "completed" }),
  ]) {
    queuedSteps.push({
      event: next,
      options: { partialTimeline: true, latestLocalTurnOrdinal: watermark },
    });
    if (next.type === "turn-item.updated") watermark = Math.max(watermark, next.payload.ordinal);
  }
  cases.push(
    reducerCase(
      "partial: queued run after a steer raised the watermark",
      projectionOf({ runs: [completed, queuedRun, steerRun], items: steerItems }),
      queuedSteps,
    ),
  );
  cases.push(
    reducerCase(
      "partial: running run item below the window",
      projectionOf({
        runs: [completed, { ...queuedRun, status: "running" }, steerRun],
        items: steerItems,
      }),
      [
        {
          event: itemEvent(runItem("item-running-reply", "run-queued", 2_000_003)),
          options: { partialTimeline: true, latestLocalTurnOrdinal: 3_000_002 },
        },
      ],
    ),
  );
  for (const [label, itemRunId] of [
    ["kept", "run-steer"],
    ["dropped", runId],
  ] as const) {
    const windowItems = [
      runItem("item-first", "run-steer", 3_000_001),
      runItem("item-later", "run-steer", 3_000_003),
    ];
    cases.push(
      reducerCase(
        `partial: late item below the watermark is ${label}`,
        projectionOf({ runs: [completed, steerRun], items: windowItems }),
        [
          {
            event: itemEvent(runItem("item-late", itemRunId, 3_000_002)),
            options: { partialTimeline: true, latestLocalTurnOrdinal: 3_000_003 },
          },
        ],
      ),
    );
  }

  const local = command("item-local");
  cases.push(
    reducerCase(
      "rollback hides local rows only",
      projectionOf({
        runs: [completed],
        turnItems: [local],
        visibleTurnItems: [
          {
            ...row(command("item-inherited-2"), 0, "inherited"),
            sourceThreadId: "thread-source",
          } as Row,
          row(local, 1),
        ],
      }),
      [{ event: runEvent({ ...completed, status: "rolled_back" }) }],
    ),
  );

  const rolledBack = run("rolled-back", 2, "rolled_back");
  const visA = command("item-first", 1, "first");
  const visB = command("item-second", 2, "second");
  cases.push(
    reducerCase(
      "item moving to a rolled-back run hides then returns",
      projectionOf({ runs: [completed, rolledBack], items: [visA, visB] }),
      [{ event: itemEvent({ ...visA, runId: "rolled-back" }) }, { event: itemEvent(visA) }],
    ),
  );

  const reply = turnItem("assistant", "item-reply", 400, { runId, text: "the full reply" });
  const subagentItem = turnItem("subagent", "task", 401, { runId });
  const dedupeBase = projectionOf({
    runs: [completed],
    messages: [messageFor(reply)],
    subagents: [subagentFor(subagentItem)],
  });
  for (const options of [undefined, { partialTimeline: true, latestLocalTurnOrdinal: 300 }]) {
    cases.push(
      reducerCase(`deduped bodies restore${options ? " (partial)" : ""}`, dedupeBase, [
        { event: itemEvent({ ...reply, text: "", textOmitted: true } as Item), options },
      ]),
    );
  }
  cases.push(
    reducerCase("deduped subagent record restores its prompt", dedupeBase, [
      {
        event: event("subagent.updated", {
          ...subagentFor(subagentItem),
          prompt: "",
          promptOmitted: true,
          progress: "step 2",
        }),
      },
      { event: itemEvent({ ...subagentItem, prompt: "", promptOmitted: true } as Item) },
    ]),
  );
  cases.push(
    reducerCase(
      "deduped body without its record stays marked",
      projectionOf({ runs: [completed] }),
      [{ event: itemEvent({ ...reply, text: "", textOmitted: true } as Item) }],
    ),
  );

  const richUser = turnItem("user", "item-rich-user", 1, { runId, rich: true });
  const richCommand = turnItem("command", "item-rich-command", 2, { runId, rich: true });
  const richTask = turnItem("subagent", "item-rich-task", 3, { runId, rich: true });
  const richStop = turnItem("interrupt_request", "item-rich-stop", 4, { runId, rich: true });
  cases.push(
    reducerCase(
      "optional fields the native types model",
      projectionOf({ runs: [completed], items: [richUser, richCommand] }),
      [
        { event: itemEvent(richTask) },
        { event: itemEvent(richStop) },
        { event: itemEvent({ ...richCommand, status: "failed" } as Item) },
      ],
    ),
  );

  const providerTurn = {
    id: "provider-turn-reducer",
    providerThreadId: "provider-thread-reducer",
    nodeId: "provider-node-reducer",
    runAttemptId: null,
    nativeTurnRef: null,
    ordinal: 1,
    status: "running",
    startedAt: NOW,
    completedAt: null,
    tokenUsage: { usedTokens: 50_000, maxTokens: 200_000, updatedAt: "2026-08-29T00:00:00.000Z" },
  } as ThreadProjection["providerTurns"][number];
  const { tokenUsage: _usage, ...withoutUsage } = providerTurn;
  cases.push(
    reducerCase(
      "terminal provider turn keeps live token usage",
      projectionOf({ providerTurns: [providerTurn] }),
      [
        {
          event: event("provider-turn.updated", {
            ...withoutUsage,
            status: "completed",
            completedAt: NOW,
          }),
        },
      ],
    ),
  );

  const thread = Fixtures.v2Projection.thread;
  cases.push(
    reducerCase("thread lifecycle and touch events", projectionOf(), [
      { event: event("thread.archived", { ...thread, archivedAt: at(50), updatedAt: at(50) }) },
      { event: event("thread.visited", { ...thread, title: "Visited", lastVisitedAt: at(60) }) },
      {
        event: event("thread.metadata-updated", { ...thread, title: "Renamed", branch: "feature" }),
      },
      { event: event("thread.unarchived", { ...thread, archivedAt: null }) },
    ]),
  );
  cases.push(
    reducerCase(
      "events for another thread are ignored",
      projectionOf({ items: [command("item-x")] }),
      [
        {
          event: {
            ...itemEvent(command("item-y", 2)),
            threadId: "thread-other",
          } as OrchestrationV2DomainEvent,
        },
      ],
    ),
  );

  const interruptRun = run("run-interrupt", 1, "running");
  const result = turnItem("interrupt_result", "item-result", 5, {
    runId: "run-interrupt",
    nodeId: "node-root",
  });
  const request = turnItem("interrupt_request", "item-request", 4, { runId: "run-interrupt" });
  cases.push(
    reducerCase(
      "superseded interrupt result shows only with its request",
      projectionOf({
        runs: [interruptRun],
        attempts: [attempt("attempt-1", "run-interrupt", "node-root", "superseded")],
        items: [command("item-before", 1)],
      }),
      [
        { event: itemEvent(result) },
        { event: itemEvent(request) },
        { event: itemEvent({ ...request, runId: "run-other" }) },
      ],
    ),
  );

  const queued = turnItem("queued", "item-queued", 7, { runId: "run-queued" });
  cases.push(
    reducerCase(
      "cancelled queued input leaves the transcript",
      projectionOf({ runs: [queuedRun], items: [queued] }),
      [{ event: runEvent({ ...queuedRun, status: "cancelled" }) }],
    ),
  );

  const background = run("run-background", 1, "completed");
  cases.push(
    reducerCase("records upsert by id", projectionOf({ runs: [background] }), [
      {
        event: event("run.background-work-cancelled", {
          runId: "run-background",
          restartCancelledBackgroundWork: [],
        }),
      },
      { event: event("message.updated", messageFor(reply)) },
      { event: event("message.updated", { ...messageFor(reply), text: "edited" }) },
      { event: event("checkpoint.captured", checkpointFor(turnItem("checkpoint", "cp", 1))) },
      {
        event: event(
          "run-attempt.created",
          attempt("attempt-9", "run-background", "node-9", "running"),
        ),
      },
    ]),
  );
  return cases;
}

/** Collation ties: equal ordinals order by `localeCompare` of the ids. */
export function collationCase() {
  const ids = [
    "item-a",
    "item_a",
    "Item-a",
    "item-B",
    "item-b",
    "item-10",
    "item-9",
    "item.a",
    "item a",
    "ITEM",
    "item",
    "a-1",
    "A_1",
    "a1",
  ];
  const runId = "run-collate";
  const first = turnItem("command", "item-z", 5, { runId });
  return reducerCase(
    "equal ordinals order by localeCompare",
    projectionOf({ runs: [run(runId, 1, "completed")], items: [first] }),
    ids.map((id) => ({ event: itemEvent(turnItem("command", id, 5, { runId })) })),
  );
}

export function fuzzReducerCases(count: number) {
  const cases = [];
  for (let index = 0; index < count; index += 1) {
    const rng = random(1000 + index);
    const runIds = ["run-1", "run-2", "run-3", "run-4"];
    const runStatuses: OrchestrationV2Run["status"][] = [
      "completed",
      "running",
      "queued",
      "cancelled",
      "rolled_back",
      "failed",
    ];
    let runs = runIds.map((id, i) =>
      run(id, i + 1, rng.pick(["completed", "running", "queued"] as const)),
    );
    const kinds: ItemKind[] = [
      "assistant",
      "user",
      "queued",
      "command",
      "reasoning",
      "interrupt_request",
      "interrupt_result",
    ];
    const idPool = Array.from({ length: 14 }, (_, i) => (i % 5 === 0 ? `Item_${i}` : `item-${i}`));
    const makeItem = () =>
      turnItem(rng.pick(kinds), rng.pick(idPool), 1 + rng.int(30), {
        runId: rng.chance(0.15) ? null : rng.pick(runIds),
        nodeId: rng.chance(0.5) ? "node-root" : "node-other",
        text: `t${rng.int(1000)}`,
      });
    const seen = new Map<string, Item>();
    for (let i = 0; i < 8; i += 1) {
      const item = makeItem();
      seen.set(item.id, item);
    }
    const attempts = rng.chance(0.5)
      ? [attempt("attempt-1", rng.pick(runIds), "node-root", "superseded")]
      : [];
    const full = fullProjection([...seen.values()], runs, { attempts });
    const partial = rng.chance(0.5);
    const dropped = partial ? 1 + rng.int(3) : 0;
    const initial: ThreadProjection = {
      ...full,
      turnItems: full.turnItems.filter(
        (item) => !full.visibleTurnItems.slice(0, dropped).some((r) => r.sourceItemId === item.id),
      ),
      visibleTurnItems: full.visibleTurnItems
        .slice(dropped)
        .map((r, position) => ({ ...r, position })),
    };
    let latest = partial ? Math.max(0, ...initial.turnItems.map((item) => item.ordinal)) : null;
    const steps: ReducerStep[] = [];
    for (let i = 0; i < 24; i += 1) {
      const options = partial
        ? { partialTimeline: true, latestLocalTurnOrdinal: latest }
        : undefined;
      if (rng.chance(0.2)) {
        const target = rng.pick(runIds);
        const next = {
          ...runs.find((candidate) => candidate.id === target)!,
          status: rng.pick(runStatuses),
        };
        runs = runs.map((candidate) => (candidate.id === target ? next : candidate));
        steps.push({ event: runEvent(next), options });
      } else if (rng.chance(0.08)) {
        steps.push({
          event: event(
            "run-attempt.updated",
            attempt(
              "attempt-1",
              rng.pick(runIds),
              "node-root",
              rng.pick(["superseded", "running"]),
            ),
          ),
          options,
        });
      } else {
        const existing =
          rng.chance(0.5) && seen.size > 0 ? rng.pick([...seen.values()]) : undefined;
        const item = existing
          ? ({
              ...existing,
              ordinal: rng.chance(0.3) ? 1 + rng.int(30) : existing.ordinal,
              runId: rng.chance(0.2) ? rng.pick(runIds) : existing.runId,
            } as Item)
          : makeItem();
        seen.set(item.id, item);
        steps.push({ event: itemEvent(item), options });
        if (partial && (latest === null || item.ordinal > latest)) latest = item.ordinal;
      }
    }
    cases.push(reducerCase(`fuzz ${index}${partial ? " (partial)" : ""}`, initial, steps));
  }
  return cases;
}
