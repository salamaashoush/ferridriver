#![allow(clippy::expect_used, clippy::unwrap_used)]
// `std::env::set_var` is `unsafe` in edition 2024; there is no safe way to
// scope env for a test. Confined to this test target.
#![allow(unsafe_code)]
//! Soundness tests for the cross-process bytecode disk cache: store/load
//! round-trip, transitive-input invalidation, and the disable switch.
//! One test fn so the process-global cache dir / env vars aren't raced by
//! parallel tests.

use ferridriver_script::bytecode_cache::{collect_inputs, entry_key, load, store};

#[test]
fn disk_cache_roundtrip_invalidation_and_disable() {
  let cache = tempfile::tempdir().expect("cache dir");
  // SAFETY: set before any cache access in this single-threaded test.
  unsafe { std::env::set_var("FERRIDRIVER_CACHE_DIR", cache.path()) };
  unsafe { std::env::remove_var("FERRIDRIVER_NO_BYTECODE_CACHE") };

  let src = tempfile::tempdir().expect("src dir");
  let entry = src.path().join("a.js");
  let helper = src.path().join("helper.js");
  std::fs::write(&entry, "import './helper.js'; const v = 1;").expect("write entry");
  std::fs::write(&helper, "export const h = 1;").expect("write helper");

  let key = entry_key("bundle", std::slice::from_ref(&entry), 0);
  // Inputs include the transitive helper (collect_inputs would derive
  // this from the source map in production; here we pass both directly).
  let inputs = vec![entry.clone(), helper.clone()];

  // The kind discriminator namespaces consumers: the same entry as a
  // plugin must not share the bundle's slot.
  assert_ne!(
    key,
    entry_key("plugin", std::slice::from_ref(&entry), 0),
    "plugin and bundle keys must not collide for the same file"
  );

  // 1. round-trip.
  store(key, b"BYTECODE-V1", "m.js", None, Some("[{\"name\":\"x\"}]"), &inputs);
  let hit = load(key).expect("entry must load");
  assert_eq!(hit.bytecode, b"BYTECODE-V1");
  assert_eq!(hit.aux.as_deref(), Some("[{\"name\":\"x\"}]"));
  assert!(hit.source_map_json.is_none());

  // 2. editing a TRANSITIVE input (not the entry) must invalidate.
  std::fs::write(&helper, "export const h = 2; // changed").expect("rewrite helper");
  assert!(load(key).is_none(), "edited transitive input must miss");

  // restore + re-store, confirm it hits again.
  store(key, b"BYTECODE-V2", "m.js", None, None, &inputs);
  assert_eq!(load(key).expect("re-store hits").bytecode, b"BYTECODE-V2");

  // 3. a deleted input invalidates.
  std::fs::remove_file(&helper).expect("rm helper");
  assert!(load(key).is_none(), "missing input must miss");

  // 3b. a torn bin/json pair (manifest from one writer, bytecode from
  // another) must miss: corrupt the .bin behind the manifest's back.
  std::fs::write(&helper, "export const h = 2; // changed").expect("restore helper");
  store(key, b"BYTECODE-V3", "m.js", None, None, &inputs);
  assert_eq!(load(key).expect("fresh store hits").bytecode, b"BYTECODE-V3");
  let bin = cache
    .path()
    .join("ferridriver")
    .join("bytecode")
    .join(
      std::fs::read_dir(cache.path().join("ferridriver").join("bytecode"))
        .expect("abi dir")
        .next()
        .expect("one abi dir")
        .expect("entry")
        .file_name(),
    )
    .join(format!("{key:016x}.bin"));
  std::fs::write(&bin, b"TORN-OTHER-WRITER").expect("corrupt bin");
  assert!(load(key).is_none(), "bytecode not matching the manifest hash must miss");

  // 4. disable switch: store no-ops, load returns None.
  unsafe { std::env::set_var("FERRIDRIVER_NO_BYTECODE_CACHE", "1") };
  std::fs::write(&helper, "export const h = 1;").expect("restore helper");
  store(key, b"SHOULD-NOT-PERSIST", "m.js", None, None, &inputs);
  assert!(load(key).is_none(), "disabled cache must not load");
  unsafe { std::env::remove_var("FERRIDRIVER_NO_BYTECODE_CACHE") };

  // collect_inputs at least returns the entry file itself.
  let collected = collect_inputs(std::slice::from_ref(&entry), None, src.path());
  assert!(collected.iter().any(|p| p.ends_with("a.js")), "entry must be an input");
}
