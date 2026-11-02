// Loads the real TypeScript sources the fixtures run, and the wire codecs the server and client
// share. Bare imports resolve from packages/client-runtime, so run from anywhere.
import * as NodeModule from "node:module";
import * as NodePath from "node:path";
import * as NodeURL from "node:url";

import type * as ContractsModule from "../../../packages/contracts/src/index.ts";
import type {
  OrchestrationV2DomainEvent,
  OrchestrationV2ProjectedTurnItem,
  OrchestrationV2ShellSnapshot,
  OrchestrationV2ShellStreamItem,
  OrchestrationV2ThreadHistoryPage,
  OrchestrationV2ThreadProjection,
  OrchestrationV2ThreadStreamItem,
  OrchestrationV2TurnItem,
} from "../../../packages/contracts/src/index.ts";
import type * as ProjectionModule from "../../../packages/client-runtime/src/state/orchestrationV2Projection.ts";
import type * as FixturesModule from "../../../packages/client-runtime/src/state/orchestrationV2TestFixtures.ts";
import type * as ShellReducerModule from "../../../packages/client-runtime/src/state/shellReducer.ts";
import type * as HistoryMergeModule from "../../../packages/client-runtime/src/state/threadHistoryMerge.ts";
import type * as BoundedSnapshotModule from "../../../packages/shared/src/orchestrationV2BoundedSnapshot.ts";
import type * as DedupedBodiesModule from "../../../packages/shared/src/orchestrationV2DedupedBodies.ts";
import type * as TimelineModule from "../../../packages/shared/src/orchestrationV2Timeline.ts";
import type * as ThreadStreamModule from "../../../apps/server/src/orchestration-v2/ThreadStream.ts";
import type * as HistoryPagingModule from "../../../apps/server/src/orchestration-v2/threadHistoryPaging.ts";
import type * as WireProjectionModule from "../../../apps/server/src/orchestration-v2/WireProjection.ts";

export const ROOT = NodePath.resolve(
  NodePath.dirname(NodeURL.fileURLToPath(import.meta.url)),
  "../../..",
);
const fromRuntime = NodeModule.createRequire(
  NodePath.join(ROOT, "packages/client-runtime/package.json"),
);

const importPackage = async <T>(specifier: string): Promise<T> =>
  (await import(NodeURL.pathToFileURL(fromRuntime.resolve(specifier)).href)) as T;
const importSource = async <T>(path: string): Promise<T> =>
  (await import(NodeURL.pathToFileURL(NodePath.join(ROOT, path)).href)) as T;

const Schema = await importPackage<typeof import("effect/Schema")>("effect/Schema");
export const DateTime = await importPackage<typeof import("effect/DateTime")>("effect/DateTime");
const Contracts = await importSource<typeof ContractsModule>("packages/contracts/src/index.ts");
export const Fixtures = await importSource<typeof FixturesModule>(
  "packages/client-runtime/src/state/orchestrationV2TestFixtures.ts",
);
export const Projection = await importSource<typeof ProjectionModule>(
  "packages/client-runtime/src/state/orchestrationV2Projection.ts",
);
export const ShellReducer = await importSource<typeof ShellReducerModule>(
  "packages/client-runtime/src/state/shellReducer.ts",
);
export const HistoryMerge = await importSource<typeof HistoryMergeModule>(
  "packages/client-runtime/src/state/threadHistoryMerge.ts",
);
export const BoundedSnapshot = await importSource<typeof BoundedSnapshotModule>(
  "packages/shared/src/orchestrationV2BoundedSnapshot.ts",
);
export const DedupedBodies = await importSource<typeof DedupedBodiesModule>(
  "packages/shared/src/orchestrationV2DedupedBodies.ts",
);
export const Timeline = await importSource<typeof TimelineModule>(
  "packages/shared/src/orchestrationV2Timeline.ts",
);
export const ThreadStream = await importSource<typeof ThreadStreamModule>(
  "apps/server/src/orchestration-v2/ThreadStream.ts",
);
export const HistoryPaging = await importSource<typeof HistoryPagingModule>(
  "apps/server/src/orchestration-v2/threadHistoryPaging.ts",
);
export const WireProjection = await importSource<typeof WireProjectionModule>(
  "apps/server/src/orchestration-v2/WireProjection.ts",
);

export type Json = null | boolean | number | string | Json[] | { [key: string]: Json };
export type Item = OrchestrationV2TurnItem;
export type Row = OrchestrationV2ProjectedTurnItem;
export type ThreadProjection = OrchestrationV2ThreadProjection;
export type HistoryMeta = HistoryMergeModule.ThreadHistoryMeta;

const codecs = {
  projection: Schema.toCodecJson(Contracts.OrchestrationV2ThreadProjection),
  event: Schema.toCodecJson(Contracts.OrchestrationV2DomainEvent),
  streamItem: Schema.toCodecJson(Contracts.OrchestrationV2ThreadStreamItem),
  historyPage: Schema.toCodecJson(Contracts.OrchestrationV2ThreadHistoryPage),
  shellItem: Schema.toCodecJson(Contracts.OrchestrationV2ShellStreamItem),
  shellSnapshot: Schema.toCodecJson(Contracts.OrchestrationV2ShellSnapshot),
};

export const toJson = (value: unknown): Json => JSON.parse(JSON.stringify(value)) as Json;
const encodeProjectionSync = Schema.encodeSync(codecs.projection);
export const encodeProjection = (value: ThreadProjection) => toJson(encodeProjectionSync(value));
const encodeEventSync = Schema.encodeSync(codecs.event);
export const encodeEvent = (value: OrchestrationV2DomainEvent) => toJson(encodeEventSync(value));
const encodeStreamItemSync = Schema.encodeSync(codecs.streamItem);
export const encodeStreamItem = (value: OrchestrationV2ThreadStreamItem) =>
  toJson(encodeStreamItemSync(value));
const encodeHistoryPageSync = Schema.encodeSync(codecs.historyPage);
export const encodeHistoryPage = (value: OrchestrationV2ThreadHistoryPage) =>
  toJson(encodeHistoryPageSync(value));
const encodeShellItemSync = Schema.encodeSync(codecs.shellItem);
export const encodeShellItem = (value: OrchestrationV2ShellStreamItem) =>
  toJson(encodeShellItemSync(value));
const encodeShellSnapshotSync = Schema.encodeSync(codecs.shellSnapshot);
export const encodeShellSnapshot = (value: OrchestrationV2ShellSnapshot) =>
  toJson(encodeShellSnapshotSync(value));
// Decoding mutates parsed JSON (the ID table is consumed in place), so decode a copy.
const decodeProjectionSync = Schema.decodeUnknownSync(codecs.projection);
export const decodeProjection = (json: Json) => decodeProjectionSync(structuredClone(json));
const decodeEventSync = Schema.decodeUnknownSync(codecs.event);
export const decodeEvent = (json: Json) => decodeEventSync(structuredClone(json));
const decodeStreamItemSync = Schema.decodeUnknownSync(codecs.streamItem);
export const decodeStreamItem = (json: Json) => decodeStreamItemSync(structuredClone(json));
const decodeHistoryPageSync = Schema.decodeUnknownSync(codecs.historyPage);
export const decodeHistoryPage = (json: Json) => decodeHistoryPageSync(structuredClone(json));
const decodeShellItemSync = Schema.decodeUnknownSync(codecs.shellItem);
export const decodeShellItem = (json: Json) => decodeShellItemSync(structuredClone(json));

const isObject = (value: Json | undefined): value is { [key: string]: Json } =>
  typeof value === "object" && value !== null && !Array.isArray(value);

/** One case's table of distinct records. States refer into it, so a record that persists
 *  across operations is written once. */
export class Interner {
  readonly table: Json[] = [];
  readonly #index = new Map<string, number>();

  ref(value: Json): number {
    const key = JSON.stringify(value);
    let index = this.#index.get(key);
    if (index === undefined) {
      index = this.table.length;
      this.table.push(value);
      this.#index.set(key, index);
    }
    return index;
  }

  /** A projection or shell snapshot as scalars, objects, and lists of refs. A listed row keeps
   *  its position beside the ref, so a row that only shifted reuses its entry. */
  record(value: Json): Json {
    if (!isObject(value)) return value;
    const scalars: { [key: string]: Json } = {};
    const objects: { [key: string]: Json } = {};
    const lists: { [key: string]: Json } = {};
    for (const [key, field] of Object.entries(value)) {
      if (Array.isArray(field)) {
        lists[key] = field.map((entry) => {
          if (!isObject(entry) || typeof entry.position !== "number") return this.ref(entry);
          const { position, ...rest } = entry;
          return [position, this.ref(rest)];
        });
      } else if (isObject(field)) {
        objects[key] = this.ref(field);
      } else {
        scalars[key] = field;
      }
    }
    return { scalars, objects, lists };
  }
}
