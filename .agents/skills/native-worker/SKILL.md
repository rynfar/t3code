---
name: native-worker
description: Implement one native-client issue end to end in its own worktree - port the web behavior a ledger row names into apps/native, test it, verify it in a real window, update the parity ledger, and open a PR into the `native` branch. Use when assigned a `native:feature`, `native:bug`, or `parity:*` issue.
---

# Native worker

Read `apps/native/AGENTS.md` and `apps/native/docs/design-system.md` before writing code.

## Claim

```sh
gh issue edit <n> -R rynfar/t3code --add-label status:in-progress
git -C /Volumes/DevSSD/t3code-native worktree add ../t3code-native-<n> -b nw/<n>-<slug> native
```

Work only in that worktree. Run `vp i` there if you need the dev server. Use
`T3_NATIVE_HOME=<your worktree>/.t3/native` for every launch.

## Port, don't invent

1. Open the ledger row. Read every Web source path it lists, and the `client-runtime` logic
   behind it. Note defaults, copy, keybindings, edge states (loading, empty, error, disabled,
   reverse actions).
2. Logic goes in `crates/core` (pure, tested) or `crates/client` (I/O). Views in `crates/app`
   stay dumb. Name the TypeScript source you ported in the module header.
3. Match the design system exactly: semantic tokens only, component variants, layout on parents.
4. Performance: no work on the UI thread that can happen on the network runtime; virtualize
   lists; notify only the view that changed; nothing repaints continuously.
5. Every behavior reachable from more than one entry point (button, menu, palette, keybinding)
   gets all of them or the ledger row says which are missing.

## Prove it

- `cargo test -p t3-core -p t3-client`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo build -p t3-native`.
- Logic bugs: failing test first. UI: before/after screenshots via `test-t3-native`. Wire:
  probe transcript.

## Land

1. Update the ledger row (Status, Native path, Issue) in the same branch.
2. Commit as `feat(native): …` / `fix(native): …`; push the branch to `fork`.
3. `gh pr create -R rynfar/t3code --base native` with: problem, approach, ledger row, evidence.
4. `gh issue edit <n> --remove-label status:in-progress --add-label status:review`.
5. Address the reviewer's findings; answer false positives with a reason. Remove your worktree
   after merge.
