//! The shell the app sends over SSH: `REMOTE_RUNNER_SCRIPT`, `REMOTE_LAUNCH_SCRIPT`,
//! `REMOTE_PAIRING_SCRIPT` and `REMOTE_STOP_SCRIPT` of `packages/ssh/src/tunnel.ts`, in the
//! release-archive mode the packaged desktop uses. The desktop's development mode (a Node source
//! checkout on the remote) has no native counterpart, so its Node discovery is left out.

use super::is_exact_archive_version;

/// `cliReleaseDownloadBaseUrl` without the version segment.
const RELEASE_BASE_URL: &str = "https://github.com/pingdotgg/t3code/releases/download";

/// `shellSingleQuote`.
fn single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// Runs the pinned t3 release on the remote, downloading and verifying it on first use, and
/// passes its arguments on. Lock waits outlast the download and extraction (360 s); the downloads
/// themselves are bounded (30 s for the checksums, 240 s for the archive).
const RUNNER_SCRIPT: &str = r#"#!/bin/sh
set -eu
T3_ARCHIVE_VERSION=@@T3_ARCHIVE_VERSION@@
if [ -z "$T3_ARCHIVE_VERSION" ]; then
  printf 'No t3 release version was provided for the remote runtime.\n' >&2
  exit 1
fi
# Self-contained release archive: no Node, npm, or compiler on the remote.
# Unpacked into the pinned-runtime layout so `t3 service install` reuses it.
T3_RELEASE_BASE_URL=@@T3_RELEASE_BASE_URL@@
T3_RUNTIME_DIR="$HOME/.t3/runtime/versions/$T3_ARCHIVE_VERSION"
t3_runtime_ready() {
  [ -x "$T3_RUNTIME_DIR/t3" ] && [ "$(cat "$T3_RUNTIME_DIR/.install-complete" 2>/dev/null)" = "$T3_ARCHIVE_VERSION" ]
}
if ! t3_runtime_ready; then
  mkdir -p "$HOME/.t3/runtime/versions"
  T3_LOCK="$HOME/.t3/runtime/versions/.$T3_ARCHIVE_VERSION.install.lock"
  # uutils mkdir can report success on EEXIST, so use shell noclobber.
  # An empty lock gets a short grace period for its owner to publish a PID.
  T3_LOCK_WAITED=0
  T3_LOCK_UNOWNED=0
  while ! (set -C; printf '%s\n' "$$" > "$T3_LOCK") 2>/dev/null; do
    T3_LOCK_OWNER="$(cat "$T3_LOCK" 2>/dev/null || true)"
    if [ -n "$T3_LOCK_OWNER" ]; then
      T3_LOCK_UNOWNED=0
      if ! kill -0 "$T3_LOCK_OWNER" 2>/dev/null; then
        rm -rf "$T3_LOCK"
        continue
      fi
    else
      T3_LOCK_UNOWNED=$((T3_LOCK_UNOWNED + 1))
      if [ "$T3_LOCK_UNOWNED" -ge 5 ]; then
        rm -rf "$T3_LOCK"
        continue
      fi
    fi
    if [ "$T3_LOCK_WAITED" -ge 360 ]; then
      printf 'Another t3 %s installation has held %s for too long.\n' "$T3_ARCHIVE_VERSION" "$T3_LOCK" >&2
      exit 1
    fi
    sleep 1
    T3_LOCK_WAITED=$((T3_LOCK_WAITED + 1))
  done
  trap 'rm -rf "$T3_LOCK"' EXIT
fi
if ! t3_runtime_ready; then
  case "$(uname -s)" in
    Darwin) T3_PLATFORM="darwin" ;;
    Linux) T3_PLATFORM="linux" ;;
    *) printf 'Remote host %s has no t3 release archive.\n' "$(uname -s)" >&2; exit 1 ;;
  esac
  case "$(uname -m)" in
    arm64 | aarch64) T3_ARCH="arm64" ;;
    x86_64 | amd64) T3_ARCH="x64" ;;
    *) printf 'Remote host %s has no t3 release archive.\n' "$(uname -m)" >&2; exit 1 ;;
  esac
  T3_ARCHIVE="t3-$T3_ARCHIVE_VERSION-$T3_PLATFORM-$T3_ARCH.tar.gz"
  T3_STAGING="$(mktemp -d "$HOME/.t3/runtime/versions/.staging-XXXXXX")"
  trap 'rm -rf "$T3_STAGING" "$T3_LOCK"' EXIT
  t3_fetch() {
    if command -v curl >/dev/null 2>&1; then curl -fsSL --connect-timeout 30 --max-time "$3" "$1" -o "$2"
    elif command -v wget >/dev/null 2>&1; then wget -q --timeout=30 --tries=1 "$1" -O "$2"
    else printf 'Remote host needs curl or wget to download %s.\n' "$T3_ARCHIVE" >&2; exit 1
    fi
  }
  t3_fetch "$T3_RELEASE_BASE_URL/v$T3_ARCHIVE_VERSION/SHA256SUMS" "$T3_STAGING/SHA256SUMS" 30
  t3_fetch "$T3_RELEASE_BASE_URL/v$T3_ARCHIVE_VERSION/$T3_ARCHIVE" "$T3_STAGING/$T3_ARCHIVE" 240
  T3_EXPECTED="$(grep " \*\{0,1\}$T3_ARCHIVE$" "$T3_STAGING/SHA256SUMS" | cut -d' ' -f1)"
  if command -v sha256sum >/dev/null 2>&1; then
    T3_ACTUAL="$(sha256sum "$T3_STAGING/$T3_ARCHIVE" | cut -d' ' -f1)"
  else
    T3_ACTUAL="$(shasum -a 256 "$T3_STAGING/$T3_ARCHIVE" | cut -d' ' -f1)"
  fi
  if [ -z "$T3_EXPECTED" ] || [ "$T3_ACTUAL" != "$T3_EXPECTED" ]; then
    printf 'Checksum mismatch for %s.\n' "$T3_ARCHIVE" >&2; exit 1
  fi
  tar -xzf "$T3_STAGING/$T3_ARCHIVE" -C "$T3_STAGING" --strip-components=1
  rm -f "$T3_STAGING/$T3_ARCHIVE" "$T3_STAGING/SHA256SUMS"
  # Prove the binary runs here (libc, arch) before marking it ready, or every
  # later launch would exec a broken install instead of retrying.
  if ! "$T3_STAGING/t3" --version >/dev/null 2>&1; then
    printf 'The t3 %s executable does not run on this host.\n' "$T3_ARCHIVE_VERSION" >&2; exit 1
  fi
  printf '%s\n' "$T3_ARCHIVE_VERSION" > "$T3_STAGING/.install-complete"
  rm -rf "$T3_RUNTIME_DIR"
  mv "$T3_STAGING" "$T3_RUNTIME_DIR"
fi
if [ -n "${T3_LOCK:-}" ]; then
  rm -rf "$T3_LOCK"
  trap - EXIT
fi
exec "$T3_RUNTIME_DIR/t3" "$@""#;

/// Starts the T3 server on the remote, or reuses the one this app started or any server already
/// running on the default home, and prints `{"remotePort":…,"serverKind":…}`. `$1` is the state
/// key. A changed runner (a new version) replaces this app's own running server.
const LAUNCH_SCRIPT: &str = r#"set -eu
STATE_KEY="$1"
STATE_DIR="$HOME/.t3/ssh-launch/$STATE_KEY"
DEFAULT_SERVER_HOME="$HOME/.t3"
DEFAULT_RUNTIME_FILE="$DEFAULT_SERVER_HOME/userdata/server-runtime.json"
PORT_FILE="$STATE_DIR/port"
PID_FILE="$STATE_DIR/pid"
MANAGED_FILE="$STATE_DIR/managed"
LOG_FILE="$STATE_DIR/server.log"
RUNNER_FILE="$STATE_DIR/run-t3.sh"
RUNNER_NEXT="$STATE_DIR/run-t3.next.$$"
mkdir -p "$STATE_DIR"
cleanup_runner_next() {
  rm -f "$RUNNER_NEXT"
}
trap cleanup_runner_next EXIT
cat >"$RUNNER_NEXT" <<'SH'
@@T3_RUNNER_SCRIPT@@
SH
RUNNER_CHANGED=0
if [ ! -f "$RUNNER_FILE" ] || ! cmp -s "$RUNNER_NEXT" "$RUNNER_FILE"; then
  RUNNER_CHANGED=1
fi
mv "$RUNNER_NEXT" "$RUNNER_FILE"
chmod 700 "$RUNNER_FILE"
# The archive ships the helpers below inside the executable; the remote
# needs no Node at all. Resolving the runner once here also downloads the
# archive before the port and readiness probes rely on it.
"$RUNNER_FILE" --version >/dev/null
pick_port() {
  "$RUNNER_FILE" __ssh-helper pick-port "$PORT_FILE" "3773" "200"
}
wait_ready() {
  "$RUNNER_FILE" __ssh-helper wait-ready "$REMOTE_PORT" "$1" "1000"
}
wait_for_pid_exit() {
  PID_TO_WAIT="$1"
  WAIT_COUNT=0
  while kill -0 "$PID_TO_WAIT" 2>/dev/null && [ "$WAIT_COUNT" -lt 20 ]; do
    WAIT_COUNT=$((WAIT_COUNT + 1))
    sleep 0.1
  done
}
# kill -0 also succeeds for a server that exited but was not reaped yet, so
# a zombie does not count as running. Without ps to tell, trust kill -0.
server_running() {
  [ -n "$1" ] && kill -0 "$1" 2>/dev/null || return 1
  case "$(ps -o stat= -p "$1" 2>/dev/null || true)" in
    *Z*) return 1 ;;
  esac
}
resolve_default_runtime_port() {
  "$RUNNER_FILE" __ssh-helper runtime-port "$DEFAULT_RUNTIME_FILE"
}
REMOTE_PID="$(cat "$PID_FILE" 2>/dev/null || true)"
REMOTE_PORT="$(cat "$PORT_FILE" 2>/dev/null || true)"
REMOTE_MANAGED="$(cat "$MANAGED_FILE" 2>/dev/null || true)"
DEFAULT_RUNTIME_INFO="$(resolve_default_runtime_port 2>/dev/null || true)"
DEFAULT_RUNTIME_PID=""
DEFAULT_REMOTE_PORT=""
ADOPTED_PID=""
if [ -n "$DEFAULT_RUNTIME_INFO" ]; then
  DEFAULT_RUNTIME_PID="${DEFAULT_RUNTIME_INFO%% *}"
  DEFAULT_REMOTE_PORT="${DEFAULT_RUNTIME_INFO#* }"
fi
# The managed server runs on the default home, so it writes the default
# runtime file too. When that file names the PID this launcher recorded as
# managed, it describes our own server and its real port, not one to adopt.
if [ -n "$DEFAULT_REMOTE_PORT" ] && [ "$REMOTE_MANAGED" = "managed" ] && [ "$DEFAULT_RUNTIME_PID" = "$REMOTE_PID" ]; then
  REMOTE_PORT="$DEFAULT_REMOTE_PORT"
  printf '%s\n' "$REMOTE_PORT" >"$PORT_FILE"
elif [ -n "$DEFAULT_REMOTE_PORT" ]; then
  REMOTE_PORT="$DEFAULT_REMOTE_PORT"
  if wait_ready "2000"; then
    ADOPTED_PID="$DEFAULT_RUNTIME_PID"
    if [ "$REMOTE_MANAGED" = "managed" ]; then
      if [ -n "$REMOTE_PID" ] && kill -0 "$REMOTE_PID" 2>/dev/null; then
        kill "$REMOTE_PID" 2>/dev/null || true
        wait_for_pid_exit "$REMOTE_PID"
      fi
      REMOTE_PID=""
      REMOTE_PORT="$DEFAULT_REMOTE_PORT"
      REMOTE_MANAGED="external"
      rm -f "$PID_FILE"
      printf '%s\n' "$REMOTE_PORT" >"$PORT_FILE"
      printf 'external\n' >"$MANAGED_FILE"
    else
      printf '%s\n' "$REMOTE_PORT" >"$PORT_FILE"
      printf 'external\n' >"$MANAGED_FILE"
      REMOTE_PID=""
      REMOTE_MANAGED="external"
    fi
  else
    REMOTE_PID="$(cat "$PID_FILE" 2>/dev/null || true)"
    REMOTE_PORT="$(cat "$PORT_FILE" 2>/dev/null || true)"
    REMOTE_MANAGED="$(cat "$MANAGED_FILE" 2>/dev/null || true)"
  fi
fi
if [ "$REMOTE_MANAGED" = "external" ]; then
  # A server adopted above has just answered. While it is still alive, probing
  # it again could catch a momentary stall and launch a second server beside it.
  if server_running "$ADOPTED_PID"; then
    :
  elif [ -z "$REMOTE_PORT" ] || ! wait_ready "2000"; then
    REMOTE_PID=""
    REMOTE_PORT=""
    REMOTE_MANAGED=""
  fi
elif [ -n "$REMOTE_PID" ] && [ -n "$REMOTE_PORT" ] && server_running "$REMOTE_PID"; then
  # Reconnect owns the local tunnel, not the lifetime of an already running
  # server. A changed runner is the update path and still replaces it.
  if [ "$RUNNER_CHANGED" -eq 1 ]; then
    kill "$REMOTE_PID" 2>/dev/null || true
    wait_for_pid_exit "$REMOTE_PID"
    REMOTE_PID=""
    REMOTE_PORT=""
    REMOTE_MANAGED=""
  elif ! wait_ready "60000"; then
    printf 'Remote T3 server is still running but did not become ready. Retry the connection; the server was left running.\n' >&2
    exit 1
  fi
else
  REMOTE_PID=""
  REMOTE_PORT=""
  REMOTE_MANAGED=""
fi
if [ -z "$REMOTE_PORT" ]; then
  REMOTE_PORT="$(pick_port)" || true
  if [ -z "$REMOTE_PORT" ]; then
    printf 'Failed to find an available port on the remote host.\n' >&2
    exit 1
  fi
  nohup env T3CODE_NO_BROWSER=1 "$RUNNER_FILE" serve --host 127.0.0.1 --port "$REMOTE_PORT" --base-dir "$DEFAULT_SERVER_HOME" >>"$LOG_FILE" 2>&1 < /dev/null &
  REMOTE_PID="$!"
  printf '%s\n' "$REMOTE_PID" >"$PID_FILE"
  printf '%s\n' "$REMOTE_PORT" >"$PORT_FILE"
  printf 'managed\n' >"$MANAGED_FILE"
  if ! wait_ready "60000"; then
    printf 'Remote T3 server did not become ready on 127.0.0.1:%s.\n' "$REMOTE_PORT" >&2
    if [ -s "$LOG_FILE" ]; then
      tail -n 80 "$LOG_FILE" >&2 2>/dev/null || true
    else
      printf 'It wrote nothing to %s, so it exited before producing any output.\n' "$LOG_FILE" >&2
    fi
    kill "$REMOTE_PID" 2>/dev/null || true
    wait_for_pid_exit "$REMOTE_PID"
    rm -f "$PID_FILE" "$PORT_FILE" "$MANAGED_FILE"
    exit 1
  fi
fi
printf '{"remotePort":%s,"serverKind":"%s"}\n' "$REMOTE_PORT" "${REMOTE_MANAGED:-managed}"
"#;

/// Mints a pairing credential with the pinned runner against the remote's default home.
const PAIRING_SCRIPT: &str = r#"set -eu
STATE_DIR="$HOME/.t3/ssh-launch/@@T3_STATE_KEY@@"
DEFAULT_SERVER_HOME="$HOME/.t3"
RUNNER_FILE="$STATE_DIR/run-t3.sh"
mkdir -p "$STATE_DIR"
cat >"$RUNNER_FILE" <<'SH'
@@T3_RUNNER_SCRIPT@@
SH
chmod 700 "$RUNNER_FILE"
PAIRING_BASE_DIR="$DEFAULT_SERVER_HOME"
"$RUNNER_FILE" auth pairing create --base-dir "$PAIRING_BASE_DIR" --json
"#;

/// Stops the server this app started; an adopted (external) one is left running.
const STOP_SCRIPT: &str = r#"set -eu
STATE_DIR="$HOME/.t3/ssh-launch/@@T3_STATE_KEY@@"
PID_FILE="$STATE_DIR/pid"
PORT_FILE="$STATE_DIR/port"
MANAGED_FILE="$STATE_DIR/managed"
REMOTE_MANAGED="$(cat "$MANAGED_FILE" 2>/dev/null || true)"
REMOTE_PID="$(cat "$PID_FILE" 2>/dev/null || true)"
if [ "$REMOTE_MANAGED" != "external" ] && [ -n "$REMOTE_PID" ] && kill -0 "$REMOTE_PID" 2>/dev/null; then
  kill "$REMOTE_PID" 2>/dev/null || true
  WAIT_COUNT=0
  while kill -0 "$REMOTE_PID" 2>/dev/null && [ "$WAIT_COUNT" -lt 20 ]; do
    WAIT_COUNT=$((WAIT_COUNT + 1))
    sleep 0.1
  done
  if kill -0 "$REMOTE_PID" 2>/dev/null; then
    printf 'Remote T3 server with PID %s did not stop within 2 seconds. Its ownership files were kept.\n' "$REMOTE_PID" >&2
    exit 1
  fi
fi
rm -f "$PID_FILE" "$PORT_FILE" "$MANAGED_FILE"
printf '{"stopped":true}\n'
"#;

/// `buildRemoteT3RunnerScript` for a release version.
pub fn runner_script(version: &str) -> Result<String, String> {
    let version = version.trim();
    if version.is_empty() {
        return Err("A remote t3 runner needs an archive version or a node script path.".to_owned());
    }
    if !is_exact_archive_version(version) {
        return Err(format!("'{version}' is not an exact t3 version and cannot name a runtime directory."));
    }
    Ok(RUNNER_SCRIPT.replace("@@T3_ARCHIVE_VERSION@@", &single_quote(version)).replace("@@T3_RELEASE_BASE_URL@@", &single_quote(RELEASE_BASE_URL)))
}

/// The launch script, run as `sh -l -s -- <state key>`.
pub fn launch_script(version: &str) -> Result<String, String> {
    Ok(LAUNCH_SCRIPT.replace("@@T3_RUNNER_SCRIPT@@", &runner_script(version)?))
}

pub fn pairing_script(state_key: &str, version: &str) -> Result<String, String> {
    Ok(PAIRING_SCRIPT.replace("@@T3_RUNNER_SCRIPT@@", &runner_script(version)?).replace("@@T3_STATE_KEY@@", state_key))
}

pub fn stop_script(state_key: &str) -> String {
    STOP_SCRIPT.replace("@@T3_STATE_KEY@@", state_key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_runner_pins_an_exact_quoted_version() {
        let runner = runner_script("1.2.3-preview.20260911.4").unwrap();
        assert!(runner.contains("T3_ARCHIVE_VERSION='1.2.3-preview.20260911.4'"));
        assert!(runner.contains("T3_RELEASE_BASE_URL='https://github.com/pingdotgg/t3code/releases/download'"));
        assert!(runner.starts_with("#!/bin/sh\n") && !runner.ends_with('\n'), "the heredoc closes on its own line");
        assert_eq!(runner_script("v1.2.3"), Err("'v1.2.3' is not an exact t3 version and cannot name a runtime directory.".to_owned()));
        assert!(runner_script(" ").is_err());
    }

    #[test]
    fn launch_and_pairing_embed_the_runner_and_the_state_key() {
        let launch = launch_script("0.1.7").unwrap();
        assert!(launch.contains("cat >\"$RUNNER_NEXT\" <<'SH'\n#!/bin/sh\n") && launch.contains("exec \"$T3_RUNTIME_DIR/t3\" \"$@\"\nSH\n"));
        assert!(!launch.contains("@@"));
        let pairing = pairing_script("native-326264c4f08c8a0c", "0.1.7").unwrap();
        assert!(pairing.contains("STATE_DIR=\"$HOME/.t3/ssh-launch/native-326264c4f08c8a0c\"") && !pairing.contains("@@"));
        assert!(pairing.ends_with("auth pairing create --base-dir \"$PAIRING_BASE_DIR\" --json\n"));
        assert!(stop_script("native-x").contains("ssh-launch/native-x\"") && !stop_script("k").contains("@@"));
    }
}
