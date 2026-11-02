// Synthetic, deterministic orchestration values in contract (Type) form.
import type {
  OrchestrationV2DomainEvent,
  OrchestrationV2Run,
} from "../../../packages/contracts/src/index.ts";

import { DateTime, Fixtures, Timeline } from "./runtime.ts";
import type { Item, Row, ThreadProjection } from "./runtime.ts";

const THREAD = Fixtures.v2ThreadId;
export const PARENT = "thread-parent" as typeof THREAD;
const T0 = Date.UTC(2026, 5, 20);
export const at = (seconds: number) => DateTime.makeUnsafe(T0 + seconds * 1000);
export const NOW = at(0);

export type ItemKind =
  | "assistant"
  | "user"
  | "queued"
  | "command"
  | "reasoning"
  | "interrupt_request"
  | "interrupt_result"
  | "checkpoint"
  | "subagent";

type ItemOptions = {
  readonly runId?: string | null;
  readonly nodeId?: string | null;
  readonly threadId?: string;
  readonly status?: Item["status"];
  readonly text?: string;
  readonly createdBy?: "user" | "agent";
  /** Sets every optional field the native types model, so the replay compares them too. */
  readonly rich?: boolean;
};

const MODEL = { instanceId: "codex", model: "gpt-5.4" };

export function turnItem(
  kind: ItemKind,
  id: string,
  ordinal: number,
  options: ItemOptions = {},
): Item {
  const base = {
    id,
    threadId: options.threadId ?? THREAD,
    runId: options.runId === undefined ? null : options.runId,
    nodeId: options.nodeId === undefined ? null : options.nodeId,
    providerThreadId: null,
    providerTurnId: null,
    nativeItemRef: null,
    parentItemId: null,
    ordinal,
    status: options.status ?? "completed",
    title: null,
    startedAt: NOW,
    completedAt: NOW,
    updatedAt: NOW,
    ...(options.rich
      ? {
          toolSurface: "browser",
          toolSource: { key: "browser-1", name: "Browser", kind: "browser" },
        }
      : {}),
  };
  const text = options.text ?? `${kind} ${id}`;
  switch (kind) {
    case "assistant":
      return {
        ...base,
        type: "assistant_message",
        messageId: `message-${id}`,
        text,
        streaming: false,
      } as Item;
    case "user":
    case "queued":
      return {
        ...base,
        type: "user_message",
        messageId: `message-${id}`,
        createdBy: options.createdBy ?? "user",
        creationSource: "web",
        inputIntent: kind === "queued" ? "queued_turn" : "turn_start",
        text,
        attachments: [],
        ...(options.rich
          ? {
              scheduledTaskId: "scheduled-1",
              senderThreadId: "thread-sender",
              idleCompaction: "onReturn",
              modelSwitch: { from: MODEL, to: { ...MODEL, model: "gpt-5.5" } },
              context: { version: 1, records: [] },
            }
          : {}),
      } as Item;
    case "command":
      return {
        ...base,
        type: "command_execution",
        input: "pwd",
        output: text,
        exitCode: 0,
        ...(options.rich ? { outputIndicatesFailure: true } : {}),
      } as Item;
    case "reasoning":
      return { ...base, type: "reasoning", text, streaming: false } as Item;
    case "interrupt_request":
      return { ...base, type: "run_interrupt_request", message: "Stop" } as Item;
    case "interrupt_result":
      return { ...base, type: "run_interrupt_result", message: "Stopped" } as Item;
    case "checkpoint":
      return {
        ...base,
        type: "checkpoint",
        checkpointId: `checkpoint-${id}`,
        scopeId: "scope-1",
        files: [{ path: `src/${id}.ts`, kind: "modified", additions: ordinal, deletions: 1 }],
      } as Item;
    case "subagent":
      return {
        ...base,
        type: "subagent",
        subagentId: `node-${id}`,
        origin: "app_owned",
        driver: "codex",
        providerInstanceId: "codex",
        childThreadId: null,
        prompt: `brief ${id}`,
        result: null,
        ...(options.rich ? { progress: "halfway" } : {}),
      } as Item;
  }
}

export const row = (item: Item, position: number, visibility: Row["visibility"] = "local"): Row =>
  ({ position, visibility, sourceThreadId: item.threadId, sourceItemId: item.id, item }) as Row;
const rowsOf = (items: ReadonlyArray<Item>): Row[] =>
  items.map((item, position) => row(item, position));

export function run(
  id: string,
  ordinal: number,
  status: OrchestrationV2Run["status"],
): OrchestrationV2Run {
  return {
    id,
    threadId: THREAD,
    ordinal,
    providerInstanceId: "codex",
    modelSelection: { instanceId: "codex", model: "gpt-5.4" },
    providerThreadId: null,
    userMessageId: `message-${id}`,
    rootNodeId: null,
    activeAttemptId: null,
    status,
    requestedAt: NOW,
    startedAt: NOW,
    completedAt: null,
    checkpointId: null,
    contextHandoffId: null,
  } as OrchestrationV2Run;
}

export const attempt = (id: string, runId: string, rootNodeId: string, status: string) =>
  ({
    id,
    runId,
    attemptOrdinal: 1,
    rootNodeId,
    providerInstanceId: "codex",
    providerThreadId: "provider-thread-1",
    providerTurnId: null,
    reason: "initial",
    status,
    startedAt: NOW,
    completedAt: null,
  }) as ThreadProjection["attempts"][number];

export const messageFor = (item: Item) =>
  ({
    id: (item as { messageId: string }).messageId,
    threadId: THREAD,
    runId: item.runId,
    nodeId: null,
    role: item.type === "user_message" ? "user" : "assistant",
    text: (item as { text: string }).text,
    attachments: [],
    streaming: false,
    createdBy: item.type === "user_message" ? "user" : "agent",
    creationSource: item.type === "user_message" ? "web" : "provider",
    createdAt: NOW,
    updatedAt: NOW,
  }) as ThreadProjection["messages"][number];

export const checkpointFor = (item: Item) =>
  ({
    id: (item as { checkpointId: string }).checkpointId,
    threadId: THREAD,
    scopeId: "scope-1",
    runId: item.runId,
    nodeId: "node-root",
    parentCheckpointId: null,
    ordinalWithinScope: 0,
    appRunOrdinal: null,
    ref: `ref-${item.id}`,
    status: "ready",
    files: (item as { files: unknown }).files,
    capturedAt: NOW,
  }) as ThreadProjection["checkpoints"][number];

export const subagentFor = (
  item: Item,
  overrides: { result?: string | null; progress?: string } = {},
) =>
  ({
    id: (item as { subagentId: string }).subagentId,
    threadId: THREAD,
    runId: item.runId,
    parentNodeId: "node-root",
    origin: "app_owned",
    createdBy: "agent",
    driver: "codex",
    providerInstanceId: "codex",
    providerThreadId: null,
    childThreadId: null,
    nativeTaskRef: null,
    prompt: (item as { prompt: string }).prompt,
    title: null,
    model: null,
    status: "running",
    result: null,
    startedAt: NOW,
    completedAt: null,
    updatedAt: NOW,
    ...overrides,
  }) as ThreadProjection["subagents"][number];

/** Records the server keeps beside the items: the bodies dedupe compares against. */
function recordsFor(items: ReadonlyArray<Item>) {
  return {
    messages: items
      .filter((item) => item.type === "user_message" || item.type === "assistant_message")
      .map(messageFor),
    checkpoints: items.filter((item) => item.type === "checkpoint").map(checkpointFor),
    subagents: items.filter((item) => item.type === "subagent").map((item) => subagentFor(item)),
  };
}

/** The fixture thread, with every optional lineage field set. */
const baseThread = {
  ...Fixtures.v2Projection.thread,
  lineage: { ...Fixtures.v2Projection.thread.lineage, sideChat: false },
};

export function projectionOf(
  parts: Partial<ThreadProjection> & { readonly items?: ReadonlyArray<Item> } = {},
): ThreadProjection {
  const { items = [], ...rest } = parts;
  return {
    ...Fixtures.v2Projection,
    thread: baseThread,
    turnItems: items,
    visibleTurnItems: rowsOf(items),
    ...rest,
  };
}

/** A full timeline: visible rows are every visible item in ordinal order, as the server keeps them. */
export function fullProjection(
  items: ReadonlyArray<Item>,
  runs: ReadonlyArray<OrchestrationV2Run>,
  extra: Partial<ThreadProjection> = {},
): ThreadProjection {
  const sorted = [...items].sort((a, b) => a.ordinal - b.ordinal);
  const attempts = extra.attempts ?? [];
  const visible = sorted.filter((item) =>
    Timeline.isOrchestrationV2TurnItemVisible({ item, runs, attempts, items: sorted }),
  );
  return {
    ...Fixtures.v2Projection,
    thread: baseThread,
    ...recordsFor(sorted),
    runs,
    turnItems: sorted,
    visibleTurnItems: rowsOf(visible),
    ...extra,
  };
}

let eventCounter = 0;
export function event(type: string, payload: unknown): OrchestrationV2DomainEvent {
  eventCounter += 1;
  return {
    id: `event-${eventCounter}`,
    type,
    threadId: THREAD,
    occurredAt: at(eventCounter),
    payload,
  } as OrchestrationV2DomainEvent;
}
export const itemEvent = (item: Item) => event("turn-item.updated", item);
export const runEvent = (value: OrchestrationV2Run) => event("run.updated", value);

/** mulberry32: a fixed seed keeps the fixtures stable for `--check`. */
export function random(seed: number) {
  let state = seed >>> 0;
  const next = () => {
    state = (state + 0x6d2b79f5) >>> 0;
    let t = state;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
  return {
    next,
    int: (max: number) => Math.floor(next() * max),
    pick: <T>(values: ReadonlyArray<T>): T => values[Math.floor(next() * values.length)]!,
    chance: (p: number) => next() < p,
  };
}
