// Writes apps/native/crates/core/tests/conformance/*.json from the real TypeScript reducers and
// server wire code; crates/core/tests/conformance.rs replays them through the Rust reducers.
// Usage: node apps/native/conformance/generate.ts [--check]
import * as NodeFS from "node:fs";
import * as NodePath from "node:path";

import { ROOT, toJson } from "./runtime.ts";
import { collationCase, fuzzReducerCases, portedReducerCases } from "./reducerCases.ts";
import { threadCases } from "./threadCases.ts";
import { shellCases } from "./shellCases.ts";
import { shelfCases } from "./shelfCases.ts";
import { notifyCases } from "./notifyCases.ts";

const OUTPUT_DIR = "apps/native/crates/core/tests/conformance";

// One case per line: the formatter leaves .jsonl alone, and a changed case diffs as one line.
function fixtures(): Record<string, ReadonlyArray<unknown>> {
  return {
    "reducer.jsonl": [...portedReducerCases(), collationCase(), ...fuzzReducerCases(20)],
    "thread.jsonl": threadCases(),
    "shell.jsonl": shellCases(),
    "shelf.jsonl": shelfCases(),
    "notify.jsonl": notifyCases(),
  };
}

const check = process.argv.includes("--check");
const stale: string[] = [];
for (const [file, cases] of Object.entries(fixtures())) {
  const path = NodePath.join(ROOT, OUTPUT_DIR, file);
  const text = cases.map((entry) => `${JSON.stringify(toJson(entry))}\n`).join("");
  if (check) {
    if (!NodeFS.existsSync(path) || NodeFS.readFileSync(path, "utf8") !== text) stale.push(file);
    continue;
  }
  NodeFS.mkdirSync(NodePath.dirname(path), { recursive: true });
  NodeFS.writeFileSync(path, text);
}
if (stale.length > 0) {
  console.error(
    `Stale conformance fixtures: ${stale.join(", ")}. Run node apps/native/conformance/generate.ts.`,
  );
  process.exit(1);
}
