#!/usr/bin/env bash
#
# Refresh the Playwright web apps embedded in the ferridriver binary.
#
# ferridriver serves Playwright's own front-ends: the trace viewer (also the
# UI-mode app) and the recorder/inspector. They are static builds shipped
# inside the playwright-core npm package -- this script pulls
# one pinned version, repacks each app as a zip under
# crates/ferridriver-viewer/assets/, and records the version so the wire
# protocols implemented in Rust can be checked against the matching source.
#
# These zips are the ONLY copies. `ferridriver-test`'s UI-mode server used
# to embed a second trace viewer of its own, one release behind, so the
# same UI arrived at two versions depending on which entry point opened
# it; it now serves `ferridriver_viewer::App::TraceViewer`.
#
# npm is needed HERE ONLY. The zips are committed; building or running
# ferridriver never shells out to node.
#
# Usage: scripts/vendor-playwright-assets.sh [version]

set -euo pipefail

VERSION="${1:-1.62.1}"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ASSETS_DIR="$REPO_ROOT/crates/ferridriver-viewer/assets"
APPS=(traceViewer recorder)

command -v npm >/dev/null || { echo "npm is required to vendor the assets" >&2; exit 1; }
command -v zip >/dev/null || { echo "zip is required to vendor the assets" >&2; exit 1; }

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

echo "fetching playwright-core@$VERSION"
(cd "$WORK" && npm pack "playwright-core@$VERSION" --silent >/dev/null)
tar xzf "$WORK"/playwright-core-*.tgz -C "$WORK"
VITE="$WORK/package/lib/vite"

mkdir -p "$ASSETS_DIR"
for app in "${APPS[@]}"; do
  [ -d "$VITE/$app" ] || { echo "playwright-core@$VERSION has no lib/vite/$app" >&2; exit 1; }
  cp "$WORK/package/LICENSE" "$VITE/$app/LICENSE"
  # Same bytes in, same zip out: sorted entries and a fixed timestamp keep the
  # committed archive from churning on every re-vendor of the same version.
  find "$VITE/$app" -exec touch -t 198510261015 {} +
  target="$ASSETS_DIR/$(echo "$app" | tr '[:upper:]' '[:lower:]').zip"
  rm -f "$target"
  (cd "$VITE/$app" && find . -type f | LC_ALL=C sort | zip -q -X -@ "$target")
  echo "  $(basename "$target")  $(wc -c <"$target" | tr -d ' ') bytes"
done

printf '%s\n' "$VERSION" >"$ASSETS_DIR/PLAYWRIGHT_VERSION"
echo "vendored playwright-core@$VERSION"
