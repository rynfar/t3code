#!/bin/sh
# Screenshot the native app's frontmost window without shadow. Usage: snap.sh out.png [owner]
set -eu
out="${1:?usage: snap.sh out.png [process-name]}"
owner="${2:-t3-native}"
id=$(swift -e '
import CoreGraphics
let owner = CommandLine.arguments[1]
let list = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID) as! [[String: Any]]
for w in list where (w[kCGWindowOwnerName as String] as? String) == owner && (w[kCGWindowLayer as String] as? Int) == 0 {
  print(w[kCGWindowNumber as String] as! Int); break
}' "$owner")
if [ -z "$id" ]; then
  echo "no on-screen window owned by $owner" >&2
  exit 1
fi
screencapture -x -o -l "$id" "$out"
echo "$out"
