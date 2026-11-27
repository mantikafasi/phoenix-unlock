#!/bin/sh
set -eu

REPO=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
TARGET="${CARGO_TARGET_DIR:-$REPO/target}"
SUNBIRD="$HOME/Library/Application Support/Sunbird"
COPY="$SUNBIRD/Sunbird.app"
DATA="${LIBDISCORD_DATA_DIR:-$HOME/Library/Application Support/libdiscord}/sunbird"
DISCORD_APP="${DISCORD_APP:-/Applications/Discord Phoenix.app}"

api() {
   tok=$(cat "$DATA/debug-shell-token" 2>/dev/null || true)
   curl -fsS -H "X-Sunbird-Token: $tok" "$@"
}

require_copy() {
   if [ ! -d "$COPY" ]; then
      echo "No $COPY. Install Sunbird once: $0 install-launcher" >&2
      exit 1
   fi
}

build() {
   if [ "$1" = release ]; then
      (cd "$REPO" && cargo build --release -p phoenix-loader)
   else
      (cd "$REPO" && cargo build --profile dev-fast -p phoenix-loader)
   fi
}

plugins() {
   (cd "$REPO" && node web/tools/bundle-plugins.ts)
   mkdir -p "$DATA/plugins"
   node -e 'for (const p of require(process.argv[1])) if (p.name !== "example") console.log(p.name)' \
      "$REPO/web/plugins/index.json" |
      while read -r name; do
         cp "$REPO/web/plugins/$name.js" "$DATA/plugins/"
      done
   echo "Copied plugins to $DATA/plugins"
}

launch() {
   require_copy
   lib="$TARGET/$1/libphoenix.dylib"
   if [ ! -f "$lib" ]; then
      echo "Missing $lib (run '$0 run' or '$0 release' first)" >&2
      exit 1
   fi
   pkill -f "$COPY/Contents/MacOS/" 2>/dev/null || true
   sleep 0.6
   # A new inode, so a process still mapping the old dylib keeps a valid signature.
   cp "$lib" "$SUNBIRD/libphoenix.dylib.new"
   mv -f "$SUNBIRD/libphoenix.dylib.new" "$SUNBIRD/libphoenix.dylib"
   open -n --env SUNBIRD_DEVTOOLS=1 -a "$COPY" --args -ApplePersistenceIgnoreState YES
   echo "Launched $COPY ($1) with $lib"
}

cmd="${1:-run}"
case "$cmd" in
   run | release)
      profile=dev-fast
      [ "$cmd" = release ] && profile=release
      build "$profile"
      plugins
      launch "$profile"
      ;;
   launch)
      launch "${2:-dev-fast}"
      ;;
   plugins)
      plugins
      ;;
   hot)
      name="${2:?usage: $0 hot <plugin>}"
      tmp=$(mktemp -d)
      (cd "$REPO" && node web/tools/bundle-plugins.ts --out "$tmp" "$name")
      api -X PUT -H "Content-Type: text/javascript; charset=utf-8" \
         --data-binary "@$tmp/$name.js" "http://127.0.0.1:4242/plugins/$name"
      echo
      rm -rf "$tmp"
      ;;
   eval)
      api -X POST -H "Content-Type: text/plain" --data-binary "${2:?usage: $0 eval '<js>'}" \
         http://127.0.0.1:4242/eval
      echo
      ;;
   logs)
      api http://127.0.0.1:4242/logs | tail -n "${2:-40}"
      ;;
   events)
      if [ -n "${2:-}" ]; then
         api -G --data-urlencode "match=$2" http://127.0.0.1:4242/events
      else
         api http://127.0.0.1:4242/events
      fi
      echo
      ;;
   health)
      api http://127.0.0.1:4242/health
      echo
      ;;
   clear)
      api -X POST http://127.0.0.1:4242/clear
      echo
      ;;
   stream)
      /usr/bin/log stream --predicate 'eventMessage CONTAINS "phoenix:"'
      ;;
   reset)
      rm -f "$DATA/launcher.conf"
      echo "Removed $DATA/launcher.conf"
      launch "${2:-dev-fast}"
      ;;
   sigscan)
      shift
      (cd "$REPO" && cargo build -q --release -p sigscan)
      "$TARGET/release/sigscan" "$DISCORD_APP/Contents/MacOS/Discord" "$@"
      ;;
   test)
      (cd "$REPO" && cargo test -p phoenix-ui-desktop -p phoenix-runtime -p phoenix-runtime-desktop)
      ;;
   install-launcher)
      sh "$REPO/scripts/sunbird-macos.sh" "$DISCORD_APP"
      ;;
   -h | --help | help)
      cat <<HELP
Usage: $0 <command>

  run               Build dev-fast, copy plugins, and relaunch Sunbird (default)
  release           Same with the release profile
  launch [profile]  Relaunch with an existing build (default: dev-fast)
  plugins           Bundle plugins and copy them to Sunbird's home
  hot <plugin>      Bundle one plugin and reload it in the running app
  eval '<js>'       Run JS in the plugin runtime
  logs [n]          Last n runtime log lines (default: 40)
  events [filter]   Gateway events, API requests and native hook hits
  health            GET /health
  clear             Empty the debug shell's buffers
  stream            Follow phoenix: lines in the unified log
  reset [profile]   Delete launcher.conf (re-triggers onboarding) and relaunch
  sigscan <args>    sigscan against Discord Phoenix (check, strings, refs, dis, ...)
  test              Run Rust unit tests
  install-launcher  Install Sunbird.app from the local installer

Launching opens $COPY directly, so the launcher's
loader update never replaces the dylib this script copies in.

Environment:
  DISCORD_APP       Discord Phoenix.app (default: /Applications/Discord Phoenix.app)
  CARGO_TARGET_DIR  Cargo target directory (default: <repo>/target)
  ESBUILD           esbuild binary, when it is not on PATH
HELP
      ;;
   *)
      echo "Unknown command: $cmd (try '$0 help')" >&2
      exit 1
      ;;
esac
