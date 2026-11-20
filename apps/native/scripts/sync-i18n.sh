#!/bin/sh
# Vendors the 10 web and 10 desktop i18n JSON translation catalogs into
# apps/native/crates/core/src/i18n/translations/{web,desktop}/ so apps/native
# compiles cleanly without monorepo-relative include_str! paths.
#
# Usage:
#   sh apps/native/scripts/sync-i18n.sh          Copy catalogs from apps/{web,desktop}
#   sh apps/native/scripts/sync-i18n.sh --check  Verify vendored catalogs match sources

set -eu

LOCALES="en de es fr ja ko pt-BR ru sv zh"

MODE="sync"
case "${1:-}" in
  "")
    ;;
  --check)
    if [ "$#" -gt 1 ]; then
      echo "usage: sh apps/native/scripts/sync-i18n.sh [--check]" >&2
      exit 2
    fi
    MODE="check"
    ;;
  -h|--help)
    echo "usage: sh apps/native/scripts/sync-i18n.sh [--check]"
    exit 0
    ;;
  *)
    echo "unknown argument: $1" >&2
    echo "usage: sh apps/native/scripts/sync-i18n.sh [--check]" >&2
    exit 2
    ;;
esac

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
NATIVE_DIR="$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)"
APPS_DIR="$(CDPATH= cd -- "$NATIVE_DIR/.." && pwd)"

WEB_SRC="$APPS_DIR/web/src/i18n/translations"
DESKTOP_SRC="$APPS_DIR/desktop/src/i18n/translations"
DEST_ROOT="$NATIVE_DIR/crates/core/src/i18n/translations"

HAS_WEB=0
HAS_DESKTOP=0
[ -d "$WEB_SRC" ] && HAS_WEB=1
[ -d "$DESKTOP_SRC" ] && HAS_DESKTOP=1

if [ "$MODE" = "check" ]; then
  if [ "$HAS_WEB" -eq 0 ] && [ "$HAS_DESKTOP" -eq 0 ]; then
    missing=0
    for surface in web desktop; do
      for locale in $LOCALES; do
        dst="$DEST_ROOT/$surface/$locale.json"
        if [ ! -s "$dst" ]; then
          echo "missing or empty vendored catalog: $dst" >&2
          missing=1
        fi
      done
    done
    if [ "$missing" -ne 0 ]; then
      exit 1
    fi
    echo "Standalone checkout detected (monorepo i18n sources absent); all 20 vendored catalogs are present."
    exit 0
  fi

  if [ "$HAS_WEB" -eq 0 ] || [ "$HAS_DESKTOP" -eq 0 ]; then
    [ "$HAS_WEB" -eq 0 ] && echo "missing monorepo source dir: $WEB_SRC" >&2
    [ "$HAS_DESKTOP" -eq 0 ] && echo "missing monorepo source dir: $DESKTOP_SRC" >&2
    exit 1
  fi

  drifted=0
  for surface in web desktop; do
    if [ "$surface" = "web" ]; then
      src_dir="$WEB_SRC"
    else
      src_dir="$DESKTOP_SRC"
    fi
    dst_dir="$DEST_ROOT/$surface"
    for locale in $LOCALES; do
      src="$src_dir/$locale.json"
      dst="$dst_dir/$locale.json"
      if [ ! -f "$src" ]; then
        echo "missing source catalog: $src" >&2
        drifted=1
      elif [ ! -f "$dst" ]; then
        echo "missing vendored catalog: $dst" >&2
        drifted=1
      elif ! cmp -s "$src" "$dst"; then
        echo "drifted vendored catalog: crates/core/src/i18n/translations/$surface/$locale.json" >&2
        drifted=1
      fi
    done
  done

  if [ "$drifted" -ne 0 ]; then
    echo "Vendored i18n catalogs are stale. Run: sh apps/native/scripts/sync-i18n.sh" >&2
    exit 1
  fi

  echo "Vendored i18n catalogs are up to date (20 files checked)."
  exit 0
fi

# Sync mode
if [ "$HAS_WEB" -eq 0 ] || [ "$HAS_DESKTOP" -eq 0 ]; then
  [ "$HAS_WEB" -eq 0 ] && echo "missing monorepo source dir: $WEB_SRC" >&2
  [ "$HAS_DESKTOP" -eq 0 ] && echo "missing monorepo source dir: $DESKTOP_SRC" >&2
  echo "Cannot sync without monorepo web and desktop translation directories." >&2
  exit 1
fi

mkdir -p "$DEST_ROOT/web" "$DEST_ROOT/desktop"

for surface in web desktop; do
  if [ "$surface" = "web" ]; then
    src_dir="$WEB_SRC"
  else
    src_dir="$DESKTOP_SRC"
  fi
  dst_dir="$DEST_ROOT/$surface"
  for locale in $LOCALES; do
    src="$src_dir/$locale.json"
    if [ ! -f "$src" ]; then
      echo "missing source catalog: $src" >&2
      exit 1
    fi
    cp "$src" "$dst_dir/$locale.json"
  done
done

echo "Synced 10 web and 10 desktop translation catalogs into $DEST_ROOT"
