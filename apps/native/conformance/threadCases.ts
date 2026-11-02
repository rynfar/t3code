// Thread cases: wire items through the stream handling and `loadEarlier` loop of
// client-runtime/src/state/threads.ts, with server-built bounded snapshots and history pages.
import type {
  OrchestrationV2Run,
  OrchestrationV2ThreadHistoryPage,
  OrchestrationV2ThreadStreamItem,
} from "../../../packages/contracts/src/index.ts";

import {
  BoundedSnapshot,
  decodeHistoryPage,
  decodeStreamItem,
  DedupedBodies,
  encodeHistoryPage,
  encodeProjection,
  encodeStreamItem,
  HistoryMerge,
  HistoryPaging,
  observed,
  Projection,
  ThreadStream,
  WireProjection,
} from "./runtime.ts";
import type { HistoryMeta, Item, Json, ThreadProjection } from "./runtime.ts";
import {
  attempt,
  checkpointFor,
  event,
  fullProjection,
  itemEvent,
  messageFor,
  run,
  runEvent,
  subagentFor,
  turnItem,
} from "./builders.ts";

type ThreadOp =
  | { readonly stream: Json }
  | { readonly begin: string | null }
  | { readonly page: Json; readonly requestCursor: string; readonly step: string }
  | { readonly fail: string; readonly requestCursor: string; readonly step: string };

type ThreadClient = {
  projection: ThreadProjection | null;
  sequence: number | null;
  history: HistoryMeta;
};

function newClient(): ThreadClient {
  return { projection: null, sequence: null, history: HistoryMerge.EMPTY_THREAD_HISTORY_META };
}

/** `applyItemLocked` / `applyEventsLocked` for one decoded item. */
function applyStreamItem(client: ThreadClient, item: OrchestrationV2ThreadStreamItem) {
  if (item.kind === "synchronized") return;
  if (item.kind === "snapshot") {
    client.sequence = item.snapshotSequence;
    const progressive =
      item.historyCursor !== undefined ||
      item.hasMoreHistory !== undefined ||
      item.latestLocalTurnOrdinal !== undefined;
    client.projection = BoundedSnapshot.boundedSnapshotProjection(item);
    client.history = progressive
      ? {
          historyCursor: item.historyCursor ?? null,
          hasMoreHistory: item.hasMoreHistory ?? false,
          loading: false,
          error: null,
          expanded: false,
          latestLocalTurnOrdinal: item.latestLocalTurnOrdinal ?? null,
        }
      : HistoryMerge.EMPTY_THREAD_HISTORY_META;
    return;
  }
  if (client.sequence !== null && item.sequence <= client.sequence) return;
  client.sequence = item.sequence;
  if (item.kind !== "event" || client.projection === null) return;
  const history = client.history;
  const partial =
    history.hasMoreHistory || history.historyCursor !== null
      ? { partialTimeline: true as const, latestLocalTurnOrdinal: history.latestLocalTurnOrdinal }
      : undefined;
  const next = Projection.applyOrchestrationV2ProjectionEvent(
    client.projection,
    item.event,
    partial,
  );
  if (next === null || next === client.projection) return;
  if (partial !== undefined && item.event.type === "turn-item.updated") {
    const ordinal = item.event.payload.ordinal;
    if (history.latestLocalTurnOrdinal === null || ordinal > history.latestLocalTurnOrdinal) {
      client.history = { ...history, latestLocalTurnOrdinal: ordinal };
    }
  }
  client.projection = next;
}

/** `loadEarlier` preconditions and the loading flag. */
function beginLoad(client: ThreadClient): string | null {
  const cursor = client.history.historyCursor;
  if (
    client.projection === null ||
    cursor === null ||
    !client.history.hasMoreHistory ||
    client.history.loading
  )
    return null;
  client.history = { ...client.history, loading: true, error: null };
  return cursor;
}

function failLoad(client: ThreadClient, requestCursor: string, message: string): string {
  if (!HistoryMerge.isActiveHistoryRequestCursor(requestCursor, client.history)) return "stale";
  client.history = { ...client.history, loading: false, error: message };
  return `failed:${message}`;
}

/** One `loadEarlier` page without a work anchor or throughEntryId. */
function applyPage(
  client: ThreadClient,
  requestCursor: string,
  page: OrchestrationV2ThreadHistoryPage,
): string {
  const keepPaging =
    page.hasMoreHistory &&
    !page.items.some(({ item }) => item.type === "user_message" && item.createdBy === "user");
  if (keepPaging && (page.nextCursor === null || page.nextCursor === requestCursor)) {
    return failLoad(client, requestCursor, "History did not advance.");
  }
  if (!HistoryMerge.isActiveHistoryRequestCursor(requestCursor, client.history)) return "stale";
  if (client.projection === null) {
    client.history = HistoryMerge.EMPTY_THREAD_HISTORY_META;
    return "stale";
  }
  client.projection = HistoryMerge.mergeOlderHistoryIntoProjection(client.projection, page.items);
  client.history = {
    ...HistoryMerge.applyHistoryPageMeta(client.history, page),
    loading: keepPaging,
  };
  return keepPaging && page.nextCursor !== null ? `continue:${page.nextCursor}` : "done";
}

class ThreadCase {
  readonly ops: ThreadOp[] = [];
  readonly client = newClient();
  readonly dedupe = WireProjection.makeLiveBodyDeduper();
  readonly name: string;
  constructor(name: string) {
    this.name = name;
  }

  /** Sends one item through the server's live deduper and wire codec, then the client. */
  stream(item: OrchestrationV2ThreadStreamItem) {
    this.raw(encodeStreamItem(this.dedupe(item)));
  }

  raw(json: Json) {
    this.ops.push({ stream: json });
    applyStreamItem(this.client, decodeStreamItem(json));
  }

  begin() {
    const cursor = beginLoad(this.client);
    this.ops.push({ begin: cursor });
    return cursor;
  }

  page(requestCursor: string, page: OrchestrationV2ThreadHistoryPage) {
    const json = encodeHistoryPage(page);
    const step = applyPage(this.client, requestCursor, decodeHistoryPage(json));
    this.ops.push({ page: json, requestCursor, step });
    return step;
  }

  fail(requestCursor: string, message: string) {
    this.ops.push({
      fail: message,
      requestCursor,
      step: failLoad(this.client, requestCursor, message),
    });
  }

  /** Loads every earlier page the way a user scrolling up would. */
  loadAll(full: ThreadProjection, snapshotSequence: number, limit = 40) {
    let cursor = this.begin();
    for (let pages = 0; cursor !== null && pages < limit; pages += 1) {
      const step = this.page(cursor, serverHistoryPage(full, cursor, snapshotSequence));
      cursor = step.startsWith("continue:")
        ? step.slice("continue:".length)
        : step === "done"
          ? this.begin()
          : null;
    }
  }

  done() {
    const { projection, sequence, history } = this.client;
    return {
      name: this.name,
      ops: this.ops,
      expected: {
        projection: projection === null ? null : observed(encodeProjection(projection)),
        sequence,
        history: history as unknown as Json,
      },
    };
  }
}

/** `ProjectionStore.getThreadHistoryPage` without SQL: the same selection over the full timeline. */
function serverHistoryPage(
  full: ThreadProjection,
  cursor: string,
  snapshotSequence: number,
): OrchestrationV2ThreadHistoryPage {
  const wire = WireProjection.projectThreadProjectionForWire(full);
  const indexed = wire.visibleTurnItems.map((visible, position) => ({
    ...visible,
    position,
    canonicalPosition: position,
  }));
  const policy = HistoryPaging.olderHistoryPagePolicy({ items: indexed, cursor });
  const page = HistoryPaging.selectHistoryPageFromCursor({
    items: indexed,
    cursor,
    snapshotSequence,
    rowEncodedBytes: () => 0,
    policy,
  });
  const items = page.items.map(({ canonicalPosition, ...visible }) => ({
    ...visible,
    position: canonicalPosition,
  }));
  const bounded = HistoryPaging.selectRecentTimelineWindow({ items, snapshotSequence, policy });
  return {
    snapshotSequence,
    items: bounded.items.map((visible, position) => ({ ...visible, position })),
    nextCursor: bounded.hasMoreHistory ? bounded.nextCursor : page.nextCursor,
    hasMoreHistory: bounded.hasMoreHistory || page.hasMoreHistory,
    idTable: true,
  } as OrchestrationV2ThreadHistoryPage;
}

const boundedSnapshot = (projection: ThreadProjection, snapshotSequence: number) =>
  ThreadStream.buildBoundedThreadStreamSnapshot({
    snapshotSequence,
    projection,
    compactTurnItems: true,
    dedupeBodies: true,
    compactIds: true,
  });

/** A long thread: `turns` user turns, each with an assistant reply and `work` work items. */
function longThread(turns: number, work: number, options: { agentOnly?: boolean } = {}) {
  const items: Item[] = [];
  const runs: OrchestrationV2Run[] = [];
  let ordinal = 1;
  for (let turn = 0; turn < turns; turn += 1) {
    const runId = `run-${turn}`;
    runs.push(run(runId, turn + 1, "completed"));
    items.push(
      turnItem("user", `turn-${turn}-user`, ordinal++, {
        runId,
        createdBy: options.agentOnly ? "agent" : "user",
      }),
    );
    for (let step = 0; step < work; step += 1) {
      const kind = step % 3 === 0 ? "reasoning" : step % 3 === 1 ? "command" : "checkpoint";
      items.push(turnItem(kind, `turn-${turn}-work-${step}`, ordinal++, { runId }));
    }
    items.push(
      turnItem("assistant", `turn-${turn}-reply`, ordinal++, { runId, text: `Reply ${turn}` }),
    );
  }
  return fullProjection(items, runs);
}

export function threadCases() {
  const cases = [];

  const small = longThread(3, 2);
  {
    const c = new ThreadCase("full snapshot then live dedupe");
    c.stream({
      kind: "snapshot",
      snapshotSequence: 10,
      projection: DedupedBodies.dedupeSnapshotBodies(
        WireProjection.projectThreadProjectionForWire(small),
      ),
    });
    c.stream({ kind: "synchronized" });
    const runId = "run-live";
    const live = run(runId, 9, "running");
    const reply = turnItem("assistant", "live-reply", 1000, { runId, text: "streamed text" });
    let sequence = 10;
    c.stream({ kind: "event", sequence: ++sequence, event: runEvent(live) });
    c.stream({
      kind: "event",
      sequence: ++sequence,
      event: event("message.updated", messageFor(reply)),
    });
    c.stream({ kind: "event", sequence: ++sequence, event: itemEvent(reply) });
    c.stream({ kind: "event", sequence: sequence, event: itemEvent({ ...reply, ordinal: 1 }) });
    c.raw({ kind: "event", sequence: ++sequence, event: { type: "brand.new-event", payload: {} } });
    const sub = turnItem("subagent", "live-task", 1001, { runId });
    c.stream({
      kind: "event",
      sequence: ++sequence,
      event: event("subagent.updated", subagentFor(sub)),
    });
    c.stream({ kind: "event", sequence: ++sequence, event: itemEvent(sub) });
    c.stream({
      kind: "event",
      sequence: ++sequence,
      event: event("subagent.updated", subagentFor(sub, { progress: "half" })),
    });
    const cp = turnItem("checkpoint", "live-cp", 1002, { runId });
    c.stream({
      kind: "event",
      sequence: ++sequence,
      event: event("checkpoint.captured", checkpointFor(cp)),
    });
    c.stream({ kind: "event", sequence: ++sequence, event: itemEvent(cp) });
    c.stream({
      kind: "event",
      sequence: ++sequence,
      event: runEvent({ ...live, status: "completed" }),
    });
    cases.push(c.done());
  }

  const big = longThread(32, 2);
  {
    const c = new ThreadCase("bounded snapshot, live tail, then page all history");
    c.stream(boundedSnapshot(big, 500));
    c.stream({ kind: "synchronized" });
    const runId = "run-tail";
    const tail = run(runId, 99, "running");
    let sequence = 500;
    c.stream({ kind: "event", sequence: ++sequence, event: runEvent(tail) });
    const user = turnItem("user", "tail-user", 5000, { runId });
    c.stream({
      kind: "event",
      sequence: ++sequence,
      event: event("message.updated", messageFor(user)),
    });
    c.stream({ kind: "event", sequence: ++sequence, event: itemEvent(user) });
    // An update for a row outside the window is dropped; one for a row inside it applies.
    const oldRow = big.visibleTurnItems[3]!.item;
    c.stream({
      kind: "event",
      sequence: ++sequence,
      event: itemEvent({ ...oldRow, title: "edited" } as Item),
    });
    const lastRow = big.visibleTurnItems.at(-1)!.item;
    c.stream({
      kind: "event",
      sequence: ++sequence,
      event: itemEvent({ ...lastRow, title: "edited" } as Item),
    });
    c.loadAll(big, 500);
    c.stream({
      kind: "event",
      sequence: ++sequence,
      event: runEvent({ ...tail, status: "completed" }),
    });
    cases.push(c.done());
  }

  {
    const agent = longThread(1, 170, { agentOnly: true });
    const c = new ThreadCase("agent-only thread keeps paging until history ends");
    c.stream(boundedSnapshot(agent, 40));
    c.loadAll(agent, 40);
    cases.push(c.done());
  }

  {
    const c = new ThreadCase("snapshot during a load makes the page stale");
    c.stream(boundedSnapshot(big, 500));
    const cursor = c.begin();
    if (cursor === null) throw new Error("expected more history");
    c.stream(boundedSnapshot(longThread(33, 2), 600));
    c.page(cursor, serverHistoryPage(big, cursor, 500));
    c.begin();
    cases.push(c.done());
  }

  {
    const c = new ThreadCase("history that does not advance fails, then a retry loads");
    c.stream(boundedSnapshot(big, 500));
    const cursor = c.begin();
    if (cursor === null) throw new Error("expected more history");
    c.page(cursor, { snapshotSequence: 500, items: [], nextCursor: cursor, hasMoreHistory: true });
    c.begin();
    c.fail(c.client.history.historyCursor ?? cursor, "Environment is not connected.");
    const retry = c.begin();
    if (retry !== null) c.page(retry, serverHistoryPage(big, retry, 500));
    cases.push(c.done());
  }

  {
    const c = new ThreadCase("complete bounded snapshot is not partial");
    c.stream(boundedSnapshot(small, 7));
    let sequence = 7;
    c.stream({
      kind: "event",
      sequence: ++sequence,
      event: itemEvent(turnItem("command", "late-low", 0, { runId: "run-0" })),
    });
    c.stream({
      kind: "event",
      sequence: ++sequence,
      event: itemEvent({ ...small.visibleTurnItems[0]!.item, title: "x" } as Item),
    });
    c.begin();
    cases.push(c.done());
  }

  {
    // The server keeps an interrupt request outside the window when a visible result needs it.
    // Agent-only turns page by rows, so the window can split the run between the two.
    const items: Item[] = [];
    const runs = [run("run-a", 1, "completed")];
    items.push(turnItem("user", "a-user", 1, { runId: "run-a", createdBy: "agent" }));
    items.push(turnItem("interrupt_request", "a-request", 2, { runId: "run-a" }));
    for (let i = 0; i < 100; i += 1)
      items.push(turnItem("command", `a-work-${i}`, 3 + i, { runId: "run-a" }));
    items.push(
      turnItem("interrupt_result", "a-result", 200, { runId: "run-a", nodeId: "node-root" }),
    );
    const attempts = [attempt("attempt-a", "run-a", "node-root", "superseded")];
    const interrupted = fullProjection(items, runs, { attempts });
    const c = new ThreadCase("retained interrupt request merges with its page");
    c.stream(boundedSnapshot(interrupted, 90));
    c.loadAll(interrupted, 90);
    cases.push(c.done());
  }

  return cases;
}
