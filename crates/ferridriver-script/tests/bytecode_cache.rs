#![allow(clippy::expect_used, clippy::unwrap_used)]
// `std::env::set_var` is `unsafe` in edition 2024; there is no safe way to
// scope env for a test. Confined to this test target.
#![allow(unsafe_code)]
//! ferridriver's bytecode cache is the runtime's, rooted where the
//! operator said: `FERRIDRIVER_CACHE_DIR` names the base, and a record
//! stored through it round-trips with its inputs validated. The cache
//! mechanics themselves (invalidation, the ABI tag, the disable switch)
//! are `ferrijs-bundle`'s and tested there.

use ferridriver_script::bundle::bytecode_cache;
use ferrijs_bundle::cache::{entry_key, inputs_fingerprint};

#[test]
fn the_cache_lives_under_the_configured_dir_and_round_trips() {
  let cache_dir = tempfile::tempdir().expect("cache dir");
  // SAFETY: set before any cache access in this single-threaded test.
  unsafe { std::env::set_var("FERRIDRIVER_CACHE_DIR", cache_dir.path()) };
  unsafe { std::env::remove_var("FERRIDRIVER_NO_BYTECODE_CACHE") };

  let cache = bytecode_cache();
  assert!(cache.is_enabled());
  let dir = cache.dir().expect("enabled cache has a dir");
  assert!(
    dir.starts_with(cache_dir.path().join("ferridriver")),
    "cache dir {} must be under the configured base",
    dir.display()
  );
  assert!(
    dir.to_string_lossy().contains("bytecode"),
    "records live under an ABI-tagged bytecode dir: {}",
    dir.display()
  );

  let src = tempfile::tempdir().expect("src dir");
  let entry = src.path().join("a.js");
  let helper = src.path().join("helper.js");
  std::fs::write(&entry, "import './helper.js'; const v = 1;").expect("write entry");
  std::fs::write(&helper, "export const h = 1;").expect("write helper");
  let inputs = vec![entry.clone(), helper.clone()];
  let key = entry_key("bundle", std::slice::from_ref(&entry), src.path(), 0);

  cache.store(key, b"BYTECODE-V1", "m.js", None, Some("[{\"name\":\"x\"}]"), &inputs);
  let hit = cache.load(key).expect("entry must load");
  assert_eq!(hit.bytecode, b"BYTECODE-V1");
  assert_eq!(hit.module_name, "m.js");
  assert_eq!(hit.aux.as_deref(), Some("[{\"name\":\"x\"}]"));
  assert_eq!(inputs_fingerprint(&hit.inputs), inputs_fingerprint(&inputs));

  // A transitive input edited: the record is stale.
  std::thread::sleep(std::time::Duration::from_millis(20));
  std::fs::write(&helper, "export const h = 2;").expect("rewrite helper");
  assert!(cache.load(key).is_none(), "an edited helper must invalidate");
}
