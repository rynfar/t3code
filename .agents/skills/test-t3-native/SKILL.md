---
name: test-t3-native
description: Build, launch, pair, and screenshot the native GPUI client (apps/native) against an isolated T3 dev server. Use to verify native UI changes, capture before/after evidence, or get a runtime transcript of the native client's protocol handling.
---

# Test the native client

The native app is a real macOS window. Verify it against the worktree's isolated dev server,
never against `~/.t3/userdata`, and never with the developer's saved native connection.

## 1. Dev server

Reuse a healthy dev server for this worktree if one is running. Otherwise, from the repo root,
start `vp run dev:server` in the background and keep its PID. Read the real HTTP port from the
`[dev-runner]` line. Seed data first if `.t3/userdata/statev2.sqlite` is missing
(`vp run migrate-dev-db` with the server stopped); an empty database is a bad test.

Mint a pairing link (standard scopes) with the server's home:

```sh
node apps/server/src/bin.ts pair --base-dir "$PWD/.t3"   # check `--help` if flags moved
```

Never print or commit the token. Never consume the developer's own pairing links.

Don't start the server with `--auto-bootstrap-project-from-cwd`: it registers the checkout you
started it from as a project, and anything that writes (git actions, worktree launches, file
saves, rewind/revert) then edits that checkout. For write tests, `git init` a scratch repo under
the scratchpad, add it as a project, test there, and remove it afterwards.

## 2. Build and launch

```sh
cd apps/native
export T3_NATIVE_HOME="$(git rev-parse --show-toplevel)/.t3/native"   # isolated credentials
cargo build -p t3-native
cargo run -p t3-native &   # keep the PID you spawned; stop only that PID
```

Headless check of the same path (pairs once, then reuses the saved credentials in
`T3_NATIVE_HOME`):

```sh
cargo run -p t3-client --example probe -- --pair '<pairing link>' --seconds 5
cargo run -p t3-client --example probe -- --thread <thread-id> --seconds 10
```

### One-click "Connect to T3 Code on this Mac"

The connect screen finds a running server under `$T3CODE_HOME` (default `~/.t3`) and pairs by
running `t3 pair`. Never let it reach the live install: point `T3CODE_HOME` at a scratch home whose
`userdata/server-runtime.json` is a copy of the dev server's, and `T3_NATIVE_T3_BIN` at a script
that drops the app's `pair` and `--base-dir` arguments and runs
`node src/bin.ts pair --base-dir <dev server home> …` from `apps/server`. With the override set,
the app never falls back to the installed `t3`. Before `pair`, the app runs `<t3> --version` and
refuses unless it prints exactly the runtime file's `serverVersion`; the wrapper passes `--version`
through, so it matches the dev server. `probe -- --local` runs the same flow headless.

## 3. Drive and capture

- First launch shows the connect screen; paste the pairing link (or run the probe with
  `--pair` first so the app starts already paired).
- Screenshot only the app's window:

```sh
apps/native/scripts/snap.sh /path/to/out.png
```

- Show the user what you captured with the host's image tool (Zeron: `show_image`).
- Real agent turns cost money: use the `Replay (dev)` provider for streaming tests, or a
  provider's cheapest model when the change depends on that provider.

## Screenshots without screen recording

When the screen is locked or screen recording is denied, a real window gets no frames and
`snap.sh` fails. Render the real views offscreen instead (GPUI's Metal test renderer):

```sh
cd apps/native
export T3_NATIVE_HOME="$(git rev-parse --show-toplevel)/.t3/native"   # whose saved environments to use
cargo run -p t3-native --features snapshot -- --snapshot /path/to/out.png \
  [--thread <id>] [--right-panel diff] [--settings appearance] [--palette] \
  [--new-thread] [--attach <file>]... [--prompt "@src"] \
  [--usage cost|tokens|limits] [--usage-period day|week|month|quarter] [--usage-model] \
  [--context-meter] [--worktree-setup <snapshot.json>] [--click <x>,<y>]... [--width 1440] [--height 900] [--dark | --light] [--wait 6]
```

- `--right-panel` takes a launcher entry name (`diff`, and any surface added to the launcher),
  `browser:new` for the browser's empty state, or `browser:<tab>` for an open preview session (open
  one on a scratch server with `cargo run -p t3-client --example preview_probe -- --keep`, then
  `--close <tab>`; viewing takes control of an unclaimed tab and resizes it);
  `--settings` a section (`appearance`, `chat`, `keybindings`, `diagnostics`, …), or a settings
  search result's id (`telemetry-export`) to land on that row scrolled into view.
- `--usage` opens the Usage page on a metric and waits for its summary; `--usage-model` opens
  the top model's breakdown. Limits skips its provider refresh so the run stays read-only.
  `--context-meter` turns on the composer's legacy context window indicator for `--thread`.
- `--click x,y` left-clicks a window point (logical px) after everything else opens, in order,
  so a run can expand a row or press a button; take a plain run first to find the coordinates.
  A click that writes (send, approve, archive) reaches the server, so use a scratch server.
- A home with no saved environment renders the connect screen (set `T3CODE_HOME` as above to
  show the one-click local option).
- `--worktree-setup` shows a `WorktreeSetupSnapshot` JSON file as `--thread`'s worktree setup
  instead of the server's, so the setup card's states render without a real worktree launch.
- `--new-thread` opens the new-thread draft; `--prompt` types into the composer last, so a trigger
  (`@src`, `/`, `$rev`) shows its menu; `--attach` drops files on the composer. Attaching uploads
  them (pending uploads the server sweeps after a day), so use it only against a scratch server.
- Otherwise it only subscribes and reads; it never writes to the server. Local UI state goes to a
  throwaway copy of the home, so `T3_NATIVE_HOME` keeps its panels and last thread.
- One PNG per run at 2x. The first build with the feature compiles GPUI's test support once.
- Backdrop blur (dialogs, menus, the composer) may render without blur offscreen.

## SSH environments without a real host

Never connect to a host you weren't given, and never edit `~/.ssh`. Drive the SSH flow with
fakes instead:

- `HOME=<scratch home>` points host discovery at a scratch `.ssh/config` and `known_hosts`, so
  screenshots never show the developer's hosts. Unset `T3CODE_HOME` with it.
- `T3_NATIVE_SSH_BIN=<script>` replaces `ssh`. The app passes `-G` to resolve, `sh -l -s --
  <state key>` for the launch script (print `{"remotePort":N}`), `sh -s` for the pairing script
  (print `{"credential":"…"}`) and the stop script, and `-N -L local:127.0.0.1:remote` for the
  tunnel. A tunnel stand-in must answer `GET /` with 2xx, as a release `t3 serve` does; a dev
  server redirects its root to Vite.
- `T3_NATIVE_T3_BIN=<script printing "t3 v<version>">` pins the release the remote would run.
- The snapshot renderer never quits the app, so tunnels it started outlive it: have the fake
  record `$$` at spawn and stop exactly those PIDs afterwards.

## 4. Clean up

Stop the app and dev server by the PIDs you captured. Never kill by name or pattern.
