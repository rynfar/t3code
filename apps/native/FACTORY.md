# The native parity factory

How the native client gets built and stays in step with a web client that merges ~75 frontend
PRs a day. Every piece of state lives in git or GitHub, so any session (human or agent) can stop
at any point and the next one resumes from the same place.

## Where state lives

| State                       | Location                                                           | Written by                         |
| --------------------------- | ------------------------------------------------------------------ | ---------------------------------- |
| Code                        | branch `native` on `rynfar/t3code` (worktree `t3code-native`)      | merged PRs only                    |
| What is ported              | `apps/native/parity/LEDGER.md`                                     | the PR that changes the feature    |
| Upstream reconciled through | `apps/native/parity/UPSTREAM`                                      | intake, after triage               |
| Work queue                  | issues on `rynfar/t3code`                                          | intake, orchestrator               |
| In-flight work              | issues labelled `status:in-progress`, one branch `nw/<issue>` each | workers                            |
| Session handoff             | the pinned issue titled `Factory status`                           | orchestrator, end of every session |

Nothing important lives only in a chat transcript, a scratch file, or an agent's memory.

## Branches and upstream

- `main` on the fork mirrors `pingdotgg/t3code` exactly (`gh repo sync rynfar/t3code -b main`).
  Never commit to it.
- `native` is the long-lived branch: upstream `main` merged in, plus `apps/native/` and
  `.agents/skills/native-*` and `.agents/skills/test-t3-native`. Upstream files are never
  edited here, so merges never conflict.
- Feature work happens on `nw/<issue-number>-<slug>` branches in their own worktrees and
  lands in `native` through a PR on the fork.
- Never push to `pingdotgg/t3code`, and never write a linked reference to an upstream PR or
  issue in fork content: use a code span like `` `pingdotgg/t3code#29017` ``, so nothing
  appears on the maintainers' timelines.

## Labels

| Label                                  | Meaning                                                  |
| -------------------------------------- | -------------------------------------------------------- |
| `upstream-digest`                      | One per intake run; checklist of upstream PRs to triage  |
| `parity` + `parity:wire`               | Upstream changed a contract; check decoding and commands |
| `parity` + `parity:regression-risk`    | Upstream changed a feature the native app already ships  |
| `parity` + `parity:new-surface`        | Upstream added UI no ledger row covers yet               |
| `native:feature`                       | Port one ledger row (or a coherent slice of one)         |
| `native:bug`                           | Native-only defect                                       |
| `status:in-progress` / `status:review` | Claimed by a worker / PR open and in review              |

Milestones order the work: **M0 Bones**, **M1 Daily driver** (everything needed to run a day of
agents from the native app), **M2 Parity core** (sidebar, transcript, composer, diff, palette,
keys at web fidelity), **M3 Full parity** (settings, PRs, panels, the long tail).

## The loop

1. **Intake** (daily, skill `native-sync`): merge upstream into `native`, run
   `parity/sync.ts`, triage the digest. Every `new-surface` PR either gets a ledger row or is
   ticked as not applicable with a reason; `wire` and `regression-risk` PRs get issues. Then
   advance `UPSTREAM` and commit ledger + `UPSTREAM` together.
2. **Plan** (orchestrator, skill `native-factory`): order open issues by milestone, keep at
   most a few workers in flight, never two on the same files.
3. **Build** (worker, skill `native-worker`): one issue, one worktree, one PR into `native`.
   Port from the web source the ledger row names. Tests for logic, screenshots for UI, probe
   transcript for wire changes. Update the ledger row in the same PR.
4. **Review** (independent reviewer: Sol 6.1 through the Codex harness): reviews the PR diff
   against `AGENTS.md`, `docs/design-system.md`, and the web source it claims to port. The
   worker fixes real findings and answers false positives in the PR.
5. **Merge** (orchestrator): squash into `native`, close the issue, confirm the ledger row
   changed.

## Gates every PR passes

- `cargo test -p t3-core -p t3-client` and `cargo clippy --workspace --all-targets -- -D warnings`
- `node scripts/gen-theme.ts --check` and `node --test parity/sync.test.ts`
- The app builds, and UI changes carry before/after screenshots from a real window
- The ledger row is updated

## Resuming in a new session

1. `cd /Volumes/DevSSD/t3code-native && git status && git log --oneline -5` on `native`.
2. Read the pinned `Factory status` issue: what was in flight, what is next.
3. `gh issue list -R rynfar/t3code --label status:in-progress` — for each, check its branch and
   PR; resume it or release the label.
4. If the last intake is more than a day old, run intake first.
5. Before ending the session, rewrite the `Factory status` issue body: in flight, blocked,
   next three, and anything the human must decide.
