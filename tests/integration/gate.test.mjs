import assert from 'node:assert/strict';
import { chmod, readdir } from 'node:fs/promises';
import { join } from 'node:path';
import { test } from '@ferridriver/test';
import { passed, repo, run, workspace } from './support.mjs';

const executable = join(repo, 'target/debug/ferridriver-gate');

for (const code of [0, 42]) {
  test(`the gate preserves a check's exit status ${code}`, async () => {
    const cwd = await workspace({ bun: `#!/bin/sh\necho gate-probe\nexit ${code}\n` });
    await chmod(join(cwd, 'bun'), 0o700);
    const result = await run(['test', '--only', 'types'], {
      executable, env: { PATH: cwd, FERRIDRIVER_GATE_CACHE_DIR: join(cwd, 'gate-cache') },
    });
    if (code === 0) passed(result);
    else assert.notEqual(result.code, 0, result.text);
    assert.match(result.text, new RegExp(`${code === 0 ? 'PASS' : 'FAIL'} types`));
    assert.doesNotMatch(result.text, /START (build|e2e|bdd)/);
    if (code !== 0) assert.match(result.text, /gate-probe/);
  });
}

test('the gate rejects an unknown check instead of reporting an empty success', async () => {
  const result = await run(['test', '--only', 'missing-check'], { executable });
  assert.notEqual(result.code, 0, result.text);
  assert.match(result.text, /unknown check: missing-check/);
});

test('the gate blocks addon execution when type checking fails', async () => {
  const cwd = await workspace({
    cargo: '#!/bin/sh\nexit 0\n',
    bun: '#!/bin/sh\nif [ "$1" = x ]; then exit 42; fi\nexit 0\n',
  });
  await chmod(join(cwd, 'cargo'), 0o700);
  await chmod(join(cwd, 'bun'), 0o700);
  const result = await run(['test', '--only', 'napi'], {
    executable, env: { PATH: cwd, FERRIDRIVER_GATE_CACHE_DIR: join(cwd, 'gate-cache') },
  });
  assert.notEqual(result.code, 0, result.text);
  assert.match(result.text, /FAIL types/);
  assert.match(result.text, /BLOCKED napi\//);
  assert.doesNotMatch(result.text, /START napi\//);
});

test('gate children are headless even when launched from a desktop session', async () => {
  const cwd = await workspace({ bun: `#!/bin/sh
    [ "$FERRITEST_HEADLESS" = true ] || exit 71
    [ "\${DISPLAY+x}" != x ] || exit 72
    [ "\${WAYLAND_DISPLAY+x}" != x ] || exit 73
  ` });
  await chmod(join(cwd, 'bun'), 0o700);
  const result = await run(['test', '--only', 'types'], {
    executable, env: {
      PATH: cwd, FERRIDRIVER_GATE_CACHE_DIR: join(cwd, 'gate-cache'),
      FERRITEST_HEADLESS: 'false', DISPLAY: ':99', WAYLAND_DISPLAY: 'wayland-test',
    },
  });
  passed(result);
});

test('the gate clears parent Cargo package metadata and preserves build configuration', async () => {
  const cwd = await workspace({ cargo: `#!/bin/sh
    [ -z "$CARGO_PKG_NAME" ] || exit 71
    [ -z "$CARGO_MANIFEST_DIR" ] || exit 72
    [ "$CARGO_BUILD_JOBS" = 3 ] || exit 73
  ` });
  await chmod(join(cwd, 'cargo'), 0o700);
  const result = await run(['test', '--only', 'build'], {
    executable, env: {
      PATH: cwd, FERRIDRIVER_GATE_CACHE_DIR: join(cwd, 'gate-cache'),
      CARGO_PKG_NAME: 'outer-package', CARGO_MANIFEST_DIR: cwd, CARGO_BUILD_JOBS: '3',
    },
  });
  passed(result);
  assert.match(result.text, /PASS build/);
});

test('the addon gate reports each file and preserves failures across the group', async () => {
  const cwd = await workspace({
    cargo: '#!/bin/sh\nexit 0\n',
    bun: `#!/bin/sh
      if [ "$1" = run ]; then exit 0; fi
      if [ "$2" = test/browser.test.ts ]; then exit 42; fi
      exit 0
    `,
  });
  await chmod(join(cwd, 'cargo'), 0o700);
  await chmod(join(cwd, 'bun'), 0o700);
  const result = await run(['test', '--only', 'napi', '--workers', '4', '--jobs', '4'], {
    executable, env: { PATH: cwd, FERRIDRIVER_GATE_CACHE_DIR: join(cwd, 'gate-cache') },
  });
  assert.notEqual(result.code, 0, result.text);
  const files = (await readdir(join(repo, 'crates/ferridriver-node/test')))
    .filter(name => /\.test\.(ts|js|mjs)$/.test(name));
  assert.ok(files.length > 1);
  for (const file of files) {
    assert.ok(result.text.includes(`${file === 'browser.test.ts' ? 'FAIL' : 'PASS'} napi/${file} (`), result.text);
  }
  assert.doesNotMatch(result.text, /START (rust-build|e2e|bdd)/);
});

test('interrupting the gate lets its running check clean up before exiting', async () => {
  const cwd = await workspace({
    bun: `#!/bin/sh
      trap 'echo stopped > "$PROBE_ROOT/stopped"; exit 0' TERM
      echo ready > "$PROBE_ROOT/ready"
      exec 3<> "$PROBE_ROOT/hold"
      read signal <&3
    `,
    'interrupt.sh': `#!/bin/sh
      /bin/mkfifo "$PROBE_ROOT/ready" "$PROBE_ROOT/hold"
      "$1" test --only types > "$PROBE_ROOT/gate.log" 2>&1 &
      gate_pid=$!
      trap 'kill -TERM "$gate_pid" 2>/dev/null || true' EXIT
      read ready < "$PROBE_ROOT/ready"
      [ "$ready" = ready ] || exit 82
      kill -INT "$gate_pid"
      wait "$gate_pid"
      gate_status=$?
      [ "$gate_status" -ne 0 ] || exit 83
      [ -f "$PROBE_ROOT/stopped" ] || exit 84
      /bin/cat "$PROBE_ROOT/gate.log"
    `,
  });
  await chmod(join(cwd, 'bun'), 0o700);
  const result = await run([join(cwd, 'interrupt.sh'), executable], {
    executable: '/bin/sh', env: {
      PATH: cwd, PROBE_ROOT: cwd, FERRIDRIVER_GATE_CACHE_DIR: join(cwd, 'gate-cache'),
    },
  });
  passed(result);
  assert.match(result.text, /gate interrupted; child processes stopped/);
});
