# T3 Code Native

A native desktop client for T3 Code written in Rust on GPUI. It speaks the same WebSocket
protocol as the web, desktop, and mobile clients and runs against any existing T3 server
unchanged. The repo root `AGENTS.md` still applies; this file adds what is specific here.

## Ground rules

1. **The server is not ours to change.** Everything here must work against an unmodified
   T3 server, including older ones that negotiate the same orchestration protocol. If a feature
   seems to need a server change, it is out of scope: file it, don't build it.
2. **Never touch files outside `apps/native/` and `.agents/skills/native-*`.** That is what
   keeps upstream merges conflict-free. The only exceptions are files already listed in
   `FACTORY.md`.
3. **Parity is defined by the web client.** Behavior, copy, defaults, and keybindings follow
   `apps/web` and `packages/client-runtime`. When you port something, read the web source the
   ledger row points at; do not reinvent it.
4. **Performance is the reason this exists.** Users run dozens of agents at once. No continuous
   repaints, no work on the UI thread that can happen on the network runtime, virtualized lists
   everywhere, bounded caches.

## Layout

| Crate                         | Role                                                                           | Rule                                                     |
| ----------------------------- | ------------------------------------------------------------------------------ | -------------------------------------------------------- |
| `crates/core` (`t3-core`)     | Wire types, Effect RPC framing, reducers (shell, thread projection), selectors | Pure. No I/O, no GPUI. Every reducer change gets a test. |
| `crates/client` (`t3-client`) | Pairing, token exchange, WebSocket session, reconnecting supervisor            | Tokio. Decodes frames off the UI thread.                 |
| `crates/app` (`t3-native`)    | GPUI views                                                                     | Dumb views over `AppState`. No protocol logic.           |

- Wire types mirror `packages/contracts`; reducers mirror `packages/client-runtime`. Each module
  names the TypeScript file it ports in its header comment. Keep those references accurate.
- Decode forward-compatibly: unknown fields are ignored, unknown union tags decode to an
  `Unknown` variant and are skipped, never fail a whole frame.
- `AppState` owns connection, shell, and the open thread. Views subscribe to `AppEvent`s and
  notify only themselves. The sidebar is rendered `.cached()`; keep it that way.

## Design

`docs/design-system.md` is the contract for every pixel. Read it before any UI work.
Colors and scalar tokens come from `crates/app/src/theme/tokens.rs`, generated from the web
theme by `scripts/gen-theme.ts`. Never hand-edit it and never use raw colors in views.
Components own their look (`variant`/`size`); layout belongs to the parent.

## Commands

Run from `apps/native/`:

- `cargo test -p t3-core -p t3-client` — reducers, framing, pairing (fast; run constantly).
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo build -p t3-native` / `cargo run -p t3-native` — the app (first build ~5 min).
- `cargo run -p t3-client --example probe -- --seconds 10` — headless connect and print.
- `node scripts/gen-theme.ts --check` — tokens are current with the web theme.
- `node --test parity/sync.test.ts` — the parity intake.

Set `T3_NATIVE_HOME=<worktree>/.t3/native` whenever you launch the app or the probe, so you
never overwrite the developer's saved connection. Launch and screenshot procedure:
`.agents/skills/test-t3-native/SKILL.md`.

## Verification

- Logic, reducer, protocol, or state bugs: a failing test in `crates/core` or `crates/client`
  first, then the fix.
- Visual changes: before/after screenshots from a real window against a dev server, attached to
  the PR. No UI unit tests for nitpicks.
- Anything touching the wire: run the probe against a dev server and paste a short transcript.

## Parity bookkeeping

`parity/LEDGER.md` is the source of truth for what is ported. When you finish work, update the
row's **Status**, **Native** path, and **Issue** in the same PR. The factory process is in
`FACTORY.md`.
