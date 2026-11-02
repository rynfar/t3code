---
name: native-sync
description: Daily intake for the native client's parity factory. Merges upstream T3 Code into the `native` branch, runs the parity digest, triages every upstream frontend change against apps/native/parity/LEDGER.md, files issues, and advances the reconciled upstream SHA. Use when asked to sync, catch up with upstream, or run intake.
---

# Native parity intake

Read `apps/native/FACTORY.md` first. Work in the `native` worktree
(`/Volumes/DevSSD/t3code-native`) with a clean tree on branch `native`.

## 1. Merge upstream

```sh
gh repo sync rynfar/t3code -b main                 # fork main mirrors upstream
node apps/native/parity/sync.ts --merge             # fetch origin/main, merge into native
```

A conflict means someone edited an upstream file on `native`. Stop and report it; do not
resolve it by editing upstream code. After the merge, run `vp i` if the lockfile changed, then
`cargo test -p t3-core -p t3-client` in `apps/native` and the probe against a dev server if
`packages/contracts` changed (see `test-t3-native`).

Then run the drift alarm from `apps/native`: `node conformance/generate.ts --check`. Exit 1
means upstream changed what its reducers, wire codecs, or history paging produce. Regenerate
(`node conformance/generate.ts`), run `cargo test -p t3-core --test conformance`, and file a
`parity:wire` issue for every case that now fails; commit the regenerated fixtures with the fix.

## 2. Digest

```sh
node apps/native/parity/sync.ts --report --out /tmp/digest.md --json /tmp/digest.json
```

## 3. Triage (the judgment step)

For every PR in the digest, decide and record:

- **wire**: read the contract diff (`git diff <from>..<to> -- packages/contracts/src`). If it
  changes a field, union member, or RPC the native client decodes or sends (grep
  `apps/native/crates`), it needs an issue, and a fix before the next release of native.
  Additive optional fields and new union members are already tolerated; note that and move on.
- **regression-risk**: the PR changed a feature native already ships (`done`/`partial`). Read
  the web diff; if behavior, copy, defaults, or keys changed, it needs an issue that names the
  ledger row.
- **new-surface**: add a ledger row (Status `todo`) with accurate Web source paths, or extend
  an existing row's paths, or mark it not applicable with a one-line reason in the digest.
- **todo**/**noise**: nothing to do beyond confirming the classification.

Edit `apps/native/parity/LEDGER.md` for every row you add or extend. Keep rows coarse (one
user-facing capability each) and paths specific (files over directories), because the longest
path match decides which row a change hits.

## 4. File and advance

```sh
node apps/native/parity/sync.ts --file-issues --dry-run   # review the gh commands
node apps/native/parity/sync.ts --file-issues
node apps/native/parity/sync.ts --advance
```

Commit `LEDGER.md` and `UPSTREAM` together: `chore(native): reconcile upstream <from7>..<to7>`.
Push `native` to the fork. Tick the digest issue's checklist as you go and close it when every
box is resolved. Never write a linked upstream reference (see FACTORY.md).

Report: PR counts by class, issues filed, ledger rows added, and anything that blocks the next
native release.
