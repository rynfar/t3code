---
name: native-factory
description: Orchestrate the native client parity factory - resume state from git and GitHub, run intake when stale, pick and dispatch issues to workers, route PRs to an independent Sol 6.1 reviewer, merge, and leave a handoff. Use when asked to run the factory, continue the native client, or pick up where the last session stopped.
---

# Native factory orchestrator

`apps/native/FACTORY.md` is the process; this is how to drive it.

## Resume (always first)

1. `cd /Volumes/DevSSD/t3code-native && git switch native && git pull --ff-only fork native`
2. `gh issue list -R rynfar/t3code --label "Factory status" --state open` or the pinned
   `Factory status` issue: read in-flight, blocked, next.
3. `gh issue list -R rynfar/t3code --label status:in-progress` and
   `gh pr list -R rynfar/t3code --base native` — reconcile each with its branch.
4. If `apps/native/parity/UPSTREAM` is more than a day behind `origin/main`, run `native-sync`.

## Plan

- Work milestone by milestone (M0 → M3). Within one, prefer `parity:wire` (protocol breakage),
  then `parity:regression-risk`, then `native:feature` in ledger order.
- Keep at most three workers in flight, on disjoint files (one per crate area is a good rule).
- Issues too large for one PR get split into slices first, each a checkbox in the parent.

## Dispatch

Give each worker one issue and the `native-worker` skill. Run workers in isolated worktrees.
A worker brief names: the issue, the ledger row, the web source paths, the acceptance criteria,
and what is out of scope.

## Review

Every PR gets an independent review from Sol 6.1 through Zeron's Codex harness
(`create_chat` with `harness: codex`, `model: gpt-6.1-sol`, `kind: side`). Give it the PR
number, `apps/native/AGENTS.md`, `apps/native/docs/design-system.md`, and the web source the
PR claims to port, and ask for correctness, parity gaps, perf risks, and design-system
violations, ranked. Do not give it your conclusion. Relay findings to the worker; verify each
against the code before acting on it.

## Merge

Gates in FACTORY.md must be green. Squash-merge into `native`, close the issue, and confirm
the ledger row changed in the merged diff.

## Hand off (always last)

Rewrite the pinned `Factory status` issue: in flight (issue, branch, state), blocked (why),
next three, decisions the human owns. Then summarize the session for the human in plain words.
