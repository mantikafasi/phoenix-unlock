#!/bin/sh
# Installs Sunbird for Desktop Phoenix on Linux: a launcher that preloads the
# loader into Discord, and a desktop entry for it. Discord's own files stay
# untouched, so its updates never undo Sunbird.
#
#   curl -fsSL https://assets.sunbird.tools/plugins/sunbird-linux.sh | sh
#   curl -fsSL https://assets.sunbird.tools/plugins/sunbird-linux.sh | sh -s -- /path/to/Discord.AppImage
#   curl -fsSL https://assets.sunbird.tools/plugins/sunbird-linux.sh | sh -s -- --no-runtime
#   curl -fsSL https://assets.sunbird.tools/plugins/sunbird-linux.sh | sh -s -- --drover
#   curl -fsSL https://assets.sunbird.tools/plugins/sunbird-linux.sh | sh -s -- --uninstall
#
# SUNBIRD_SITE points it at a local server for testing.
set -eu

site=${SUNBIRD_SITE:-https://assets.sunbird.tools}
data=${XDG_DATA_HOME:-$HOME/.local/share}
home=$data/sunbird
entry=$data/applications/sunbird.desktop

if [ "${1:-}" = --uninstall ]; then
   rm -rf "$home" "$entry"
   echo "Sunbird is uninstalled. Discord itself was not changed."
   exit 0
fi

# --drover turns on Discord Drover's voice unblock, for networks that block
# Discord voice.
no_runtime= drover=
while true; do
   case ${1:-} in
      --no-runtime | --gate-only) no_runtime=1 ;;
      --drover) drover=1 ;;
      *) break ;;
   esac
   shift
done

# What the site serves is loaded into Discord, so only https, or plain http to
# this machine for testing.
case $site in
   https://*) proto==https ;;
   http://127.0.0.1 | http://127.0.0.1[:/]* | http://localhost | http://localhost[:/]*) proto==http ;;
   *)
      echo "SUNBIRD_SITE must be https:// or a localhost server, not $site" >&2
      exit 1
      ;;
esac

# LD_PRELOAD splits on spaces and colons, and the desktop entry's Exec= would
# need escaping for the rest.
case $home in
   *[[:space:]:\"\$\`\\\']*)
      echo "Sunbird can't be installed under $home: set XDG_DATA_HOME to a path without spaces, colons or quotes." >&2
      exit 1
      ;;
esac

script="curl -fsSL $site/plugins/sunbird-linux.sh | sh -s --"
discord=${1:-}
if [ -n "$discord" ] && [ ! -f "$discord" ]; then
   echo "No file at $discord. Pass the path of Discord Phoenix's AppImage:" >&2
   echo "  $script /path/to/discord-desktop.AppImage" >&2
   exit 1
fi
if [ -z "$discord" ]; then
   discord=$(command -v discord-phoenix 2>/dev/null || true)
fi
if [ -z "$discord" ]; then
   # The newest AppImage wins when an older download is still around.
   discord=$(ls -t "$PWD"/*[Dd]iscord*.AppImage "$HOME"/Applications/*[Dd]iscord*.AppImage \
      "$HOME"/Downloads/*[Dd]iscord*.AppImage "$HOME"/Desktop/*[Dd]iscord*.AppImage \
      2>/dev/null | head -n 1 || true)
fi
if [ -z "$discord" ]; then
   cat >&2 <<EOF
Couldn't find Discord Phoenix. Looked for discord-phoenix on PATH and for a
Discord AppImage in $PWD, ~/Applications, ~/Downloads and ~/Desktop.

Download Discord Phoenix's AppImage, then run this again with its path:
  $script /path/to/discord-desktop.AppImage
EOF
   exit 1
fi
discord=$(realpath "$discord")
[ -x "$discord" ] || chmod +x "$discord"
echo "Using Discord Phoenix at $discord"

# Single-quotes a value for the generated launcher.
q() { printf "'%s'" "$(printf %s "$1" | sed "s/'/'\\\\''/g")"; }

mkdir -p "$home" "$data/applications"
# The launcher brings the loader up to the site's version before each start,
# and keeps the one it has when offline. Each run downloads to its own file, so
# two starts at once never load a half-written one.
cat >"$home/sunbird" <<EOF
#!/bin/sh
home=$(q "$home")
site=$(q "$site")
fetch() { curl -fsSL --proto $proto --proto-redir =https "\$@" 2>/dev/null; }
version=\$(fetch --max-time 5 "\$site/version.json" |
   sed -n 's/.*"loader": *"\([^"]*\)".*/\1/p')
if [ -n "\$version" ] && [ "\$version" != "\$(cat "\$home/version" 2>/dev/null)" ]; then
   tmp=\$(mktemp "\$home/libphoenix.so.XXXXXX")
   if fetch --max-time 60 -o "\$tmp" "\$site/plugins/libphoenix-linux-x86_64.so"; then
      mv "\$tmp" "\$home/libphoenix.so"
      echo "\$version" >"\$home/version"
   else
      rm -f "\$tmp"
   fi
fi
[ "\${1:-}" = --update ] && exit 0
while true; do
   case \${1:-} in
      --no-runtime | --gate-only) export SUNBIRD_GATE_ONLY=1 ;;
      --drover) export SUNBIRD_DROVER=1 ;;
      *) break ;;
   esac
   shift
done
# A second Discord hands off to the running one and exits, so one running
# without Sunbird, or started with other flags, is restarted with these.
discord=$(q "$discord")
flags() { grep -E '^SUNBIRD_(GATE_ONLY|DROVER)=' | sort | tr '\\n' ' '; }
wanted=\$(env | flags)
pids= sunbird= seen= running=
for p in /proc/[0-9]*; do
   if [ "\$(readlink "\$p/exe" 2>/dev/null)" = "\$discord" ] ||
      tr '\\0' '\\n' <"\$p/environ" 2>/dev/null | grep -qxF "APPIMAGE=\$discord"; then
      pids="\$pids \${p#/proc/}"
      grep -q libphoenix "\$p/maps" 2>/dev/null && sunbird=1
      [ -n "\$seen" ] || { seen=1; running=\$(tr '\\0' '\\n' <"\$p/environ" 2>/dev/null | flags); }
   fi
done
if [ -n "\$pids" ] && { [ -z "\$sunbird" ] || [ "\$running" != "\$wanted" ]; }; then
   kill \$pids 2>/dev/null
   for _ in 1 2 3 4 5 6 7 8 9 10; do
      alive=
      for pid in \$pids; do kill -0 "\$pid" 2>/dev/null && alive=1; done
      [ -z "\$alive" ] && break
      sleep 0.5
   done
fi
[ -f "\$home/libphoenix.so" ] &&
   export LD_PRELOAD="\$home/libphoenix.so\${LD_PRELOAD:+:\$LD_PRELOAD}"
exec "\$discord" "\$@"
EOF
chmod +x "$home/sunbird"

exec_args=
[ -n "$no_runtime" ] && exec_args=" --no-runtime"
[ -n "$drover" ] && exec_args="$exec_args --drover"
cat >"$entry" <<EOF
[Desktop Entry]
Type=Application
Name=Sunbird
Comment=Discord with Sunbird
Exec="$home/sunbird"$exec_args %U
Icon=discord
Terminal=false
Categories=Network;InstantMessaging;
MimeType=x-scheme-handler/discord;
EOF
command -v update-desktop-database >/dev/null && update-desktop-database "$data/applications" || true

# First download now, so a missing network shows up here rather than as a
# Discord without Sunbird.
echo "Downloading Sunbird..."
rm -f "$home/version"
"$home/sunbird" --update
if [ -f "$home/libphoenix.so" ]; then
   echo "Sunbird is installed. Open \"Sunbird\" from your app menu to start Discord with it."
else
   echo "Installed, but downloading Sunbird failed. It will retry each time you open it." >&2
fi
echo "It runs Discord from $discord: if you move or replace that file, run this again."
