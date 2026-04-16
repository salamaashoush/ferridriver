#!/usr/bin/env node
// Copy fd_webkit_host to the ferridriver cache directory on macOS.
// This ensures the WebKit backend can discover the host binary
// regardless of how the npm package was installed.

const fs = require('fs');
const path = require('path');
const os = require('os');

if (os.platform() !== 'darwin') {
  process.exit(0);
}

const src = path.join(__dirname, 'fd_webkit_host');
if (!fs.existsSync(src)) {
  // Not present in this platform package (e.g. linux), skip silently.
  process.exit(0);
}

const cacheDir = path.join(os.homedir(), 'Library', 'Caches', 'ferridriver');
try {
  fs.mkdirSync(cacheDir, { recursive: true });
  const dest = path.join(cacheDir, 'fd_webkit_host');
  fs.copyFileSync(src, dest);
  fs.chmodSync(dest, 0o755);
} catch (e) {
  // Best-effort: don't fail the install if we can't copy.
  // The binary can still be found via other discovery paths.
  console.warn(`ferridriver: could not copy webkit host to cache: ${e.message}`);
}
