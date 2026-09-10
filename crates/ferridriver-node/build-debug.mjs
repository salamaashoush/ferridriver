import { dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

// The source patch keeps host builds in Cargo's workspace artifact directory.
const { buildProject } = await import(new URL('./src/api/build.ts', import.meta.resolve('@napi-rs/cli/package.json')));
const build = await buildProject({
  cwd: dirname(fileURLToPath(import.meta.url)),
  platform: true,
  hostTarget: !process.env.CARGO_BUILD_TARGET,
  cargoOptions: ['--locked', '--workspace', '--lib'],
});
await build.task;
