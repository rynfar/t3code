// Notification cases: `createThreadNotificationGate` (ThreadNotificationCoordinator.logic.ts) on
// seeded observation sequences: attention changes, completions, archived and quiet runs, and
// threads that leave the snapshot.
import type * as GateModule from "../../../apps/web/src/components/ThreadNotificationCoordinator.logic.ts";

import { random } from "./builders.ts";
import { NotificationGate } from "./runtime.ts";
import type { Json } from "./runtime.ts";

type Observation = GateModule.ThreadNotificationObservation;

function gateCase(seed: number) {
  const rng = random(seed);
  const keys = ["a", "b", "c"];
  let fired: Array<[string, GateModule.ThreadNotificationKind]> = [];
  const gate = NotificationGate.createThreadNotificationGate<string>((key, kind) =>
    fired.push([key, kind]),
  );
  const previous = new Map<string, Observation>();
  const steps: Json[] = [];
  for (let step = 0; step < 40; step += 1) {
    if (rng.chance(0.1)) {
      const kept = keys.filter(() => rng.chance(0.6));
      for (const key of previous.keys()) if (!kept.includes(key)) previous.delete(key);
      gate.retain(new Set(kept));
      steps.push({ retain: kept });
      continue;
    }
    const key = rng.pick(keys);
    const settled = rng.chance(0.5);
    const next: Observation = {
      attention: rng.pick([null, null, "run-1:input", "run-1:approval", "run-2:failed"]),
      settled,
      completedAt: settled ? rng.pick([null, 100, 200, 300, 400]) : null,
    };
    const options = { archived: rng.chance(0.1), quiet: rng.chance(0.15) };
    const prior = previous.get(key);
    fired = [];
    gate.observe(key, prior, next, options);
    previous.set(key, next);
    steps.push({
      key,
      prior: prior ?? null,
      next,
      ...options,
      fired: fired.map(([, kind]) => kind),
    });
  }
  return { name: `gate ${seed}`, steps };
}

export function notifyCases() {
  return Array.from({ length: 24 }, (_, seed) => gateCase(11_000 + seed));
}
