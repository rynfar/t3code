# Parity intake

`sync.ts` digests upstream (`pingdotgg/t3code`) commits that touch code the native client mirrors, so the
factory knows what to port, re-verify, or add to the ledger. Node 22, no dependencies:
`node apps/native/parity/sync.ts [flags]`. Tests: `node --test apps/native/parity/sync.test.ts`.

## Inputs

- `UPSTREAM`: the upstream SHA parity was last reconciled against (the from-SHA).
- `origin/main` after `git fetch origin main` (the to-SHA). The range is first-parent `from..to`.
- `LEDGER.md`: tables of `| ID | Area | Feature | Web source | Native | Status | Issue |`. Missing means empty.

Watched paths are `apps/web/src`, `packages/client-runtime/src`, `packages/contracts/src`,
`packages/shared/src/keybindings.ts`, and every ledger Web source path, wherever it lives.

## Classes

A PR carries every class that applies. The digest lists it once, under the first class in this order.

- `wire`: touches `packages/contracts/src` outside tests. Possible protocol change.
- `regression-risk`: touches a `done` or `partial` row. Re-verify the native side.
- `new-surface`: touches watched client code no row covers. Candidate for a new ledger row.
- `todo`: touches rows still `todo` (or with an unknown status). Nothing to port yet.
- `noise`: only tests, stories, snapshots, or paths of `n/a` rows.

A file belongs to the rows with its longest matching Web source, so a specific row overrides a broad
directory row: an `n/a` file inside a `done` directory is noise, and `n/a` also silences `wire`.
Renames count both paths, so moving a mirrored file still flags its row.

## Modes

- default or `--report`: print the markdown digest. No writes and no GitHub calls (it still fetches).
- `--merge`: on branch `native` with no tracked changes, merge `origin/main`. On conflict it runs
  `git merge --abort`, lists the conflicted files, and exits 1. Never pushes.
- `--file-issues`: against `${PARITY_REPO:-rynfar/t3code}`, create missing labels, one `upstream-digest`
  issue with a checkbox per non-noise PR, and one ``parity: <title> (`pingdotgg/t3code#N`)`` issue
  per wire or regression-risk PR. A PR is skipped when a `parity`-labeled issue has its hidden
  `<!-- upstream-pr:N -->` body marker or its code-span ref in the title. Calls run one at a time,
  a second apart, with backoff on rate limits.
- `--dry-run` (with `--file-issues`): print the gh commands as a runnable shell script instead.
- `--advance`: write the to-SHA to `UPSTREAM`, only after the other requested steps succeed.

Options: `--from <rev>` overrides `UPSTREAM`, `--ledger <file>` overrides `LEDGER.md`,
`--out <file>` writes the markdown, `--json <file>` writes the digest plus step results,
`--no-fetch` skips the fetch.

Upstream PRs, commits, and ranges are always written as code spans (`` `pingdotgg/t3code#29017` ``),
never as bare refs or github.com URLs. Both of those add a cross-reference to the upstream PR, and at
~75 PRs a day the fork would spam pingdotgg/t3code timelines. GitHub does not autolink code spans.
Upstream titles get the same treatment, and mentions and bare `#N` in them are broken up.

## Factory loop

1. `node apps/native/parity/sync.ts --merge --file-issues --advance --json digest.json`.
   Any failure exits non-zero and leaves `UPSTREAM` untouched; rerun after fixing it.
2. Commit `UPSTREAM` on `native` next to the merge commit, so the next run starts from the merged SHA.
3. Work the issues: `wire` first (protocol), then `regression-risk` (re-verify the row), then
   `new-surface` (add or widen ledger rows). Tick the digest checkbox as each PR is handled.
4. Keep `LEDGER.md` current. A row whose Web source no longer exists upstream silently stops matching.
