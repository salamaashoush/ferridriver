# Vendored from Playwright

`deviceDescriptorsSource.json` is a **verbatim copy** of
`packages/isomorphic/deviceDescriptorsSource.json`. Like the vendored
sources under `src/injected/`, it must stay byte-identical to upstream:
a re-sync is `cp`, never a merge. Anything ferridriver needs that the
file does not carry belongs in `mod.rs`.

## Current revision

| | |
|---|---|
| upstream | `microsoft/playwright` |
| version | 1.63.0-next |
| commit | `51023b4` |
| synced | 2026-08-19 |
| sha256 | `f4c07bb3f3abf58439a7db5d1eac5f205b3934bc3f1a9838498842905ee54c02` |
| devices | 207 |

## Attribution

Copyright (c) Microsoft Corporation, licensed under the Apache License,
Version 2.0 — the license the rest of the vendored Playwright sources in
this crate carry (`src/injected/VENDOR.md`).

## Re-sync recipe

```bash
# 1. refresh the clone (the compat harness keeps one at /tmp/playwright)
git -C /tmp/playwright fetch --all && git -C /tmp/playwright checkout <rev>

# 2. copy the file and record what arrived
cp /tmp/playwright/packages/isomorphic/deviceDescriptorsSource.json \
   crates/ferridriver/src/devices/deviceDescriptorsSource.json
shasum -a 256 crates/ferridriver/src/devices/deviceDescriptorsSource.json

# 3. rebuild — build.rs re-parses the file, so a descriptor that gained
#    or lost a field fails HERE rather than at run time
cargo test -p ferridriver --lib devices::
```

Step 3 is the point of generating the table in `build.rs`: the parse is
`deny_unknown_fields`, so an upstream schema change is a build error
naming the device. Update the table above and the `207` in
`the_table_holds_every_vendored_device` when the count moves.
