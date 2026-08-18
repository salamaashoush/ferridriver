//! Cross-process disk cache for compiled QuickJS bytecode.
//!
//! Compiling a rolldown bundle to bytecode (`bundle_and_compile`) or a
//! extension file (`compile_and_extract_extensions`) costs ~15 ms cold per
//! process. The in-memory cache only helps within one process; a fresh
//! `ferridriver bdd` / MCP server start pays it again. This persists the
//! bytecode (plus its source map / manifests) to disk so an unchanged
//! source tree skips BOTH rolldown and the QuickJS compile entirely.
//!
//! ## Soundness
//!
//! `Module::load` on bytecode is `unsafe`: it trusts the input was
//! produced by an identical QuickJS build with native endianness. A disk
//! cache crosses process (and machine) boundaries, so every entry lives
//! under an `abi_tag`-named directory folding the QuickJS version
//! (which tracks the on-disk `BC_VERSION`), target arch, endianness, and
//! pointer width. Bytecode is only ever loaded from the directory
//! matching the running toolchain — a mismatched build simply misses and
//! recompiles. Bumping rquickjs changes `JS_GetVersion()` and thus the
//! directory, so stale bytecode is never loaded.
//!
//! ## Freshness
//!
//! A bundle inlines its whole import graph, so the entry file's hash is
//! not enough — an edited (but still-imported) helper must invalidate.
//! Each entry records the content hash of every transitive input (the
//! source map's `sources`); a load re-hashes them all and misses on any
//! change, addition, or deletion.

use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

/// One cached compile: the bytecode plus the auxiliary data each caller
/// needs to reconstruct its result without re-running rolldown.
pub struct CacheEntry {
  pub bytecode: Vec<u8>,
  /// The module name baked into `bytecode`. A caller that registers a
  /// source map has to key it by the same name QuickJS labels the
  /// module's frames with, and only the writer knew it.
  pub module_name: String,
  /// Source-map JSON (BDD bundle) — `None` when the bundle had no map.
  pub source_map_json: Option<String>,
  /// Caller-specific sidecar (extension manifests JSON) — `None` for BDD.
  pub aux: Option<String>,
  /// The input paths this entry's freshness was validated against, so a
  /// caller promoting the entry into an in-process tier can carry the
  /// same set instead of re-deriving it.
  pub inputs: Vec<PathBuf>,
}

#[derive(Serialize, Deserialize)]
struct Manifest {
  module_name: String,
  source_map_json: Option<String>,
  aux: Option<String>,
  /// `(absolute path, content hash)` for every transitive input.
  inputs: Vec<(String, u64)>,
  /// Hash of the `.bin` this manifest was written with. `.bin` and
  /// `.json` are two separate atomic writes; two processes racing a
  /// store after an input edit can interleave so one writer's manifest
  /// pairs with the other's bytecode. Verifying on load rejects the
  /// torn pair (input hashes alone cannot — both writers read the same
  /// new inputs).
  bytecode_hash: u64,
}

fn disabled() -> bool {
  std::env::var_os("FERRIDRIVER_NO_BYTECODE_CACHE").is_some()
}

/// Toolchain fingerprint. Bytecode under one tag is safe to
/// `Module::load` only by an identical toolchain. `fdbc<N>` is our own
/// format version — bump it on any change to the manifest shape, or to
/// anything baked into the bytecode that a reader now depends on.
/// `fdbc4` retired extension bytecode named after the file's position in
/// its batch: the source-map registry keys on the module name, so two
/// extensions compiled in different batches used to collide there.
///
/// Beyond the raw bytecode ABI (QuickJS version, arch, endianness,
/// pointer width) the tag folds in the crate version, as a proxy for
/// the pinned rolldown/oxc bundler (a bundler upgrade alters
/// transpilation/tree-shaking output while every input hash still
/// matches) AND for the native module surface (`ferridriver` /
/// `@cucumber/cucumber` / node-compat are Rust ModuleDefs resolved at
/// link time, not bundled sources).
fn abi_tag() -> &'static str {
  static TAG: OnceLock<String> = OnceLock::new();
  TAG.get_or_init(|| {
    // SAFETY: returns a static C string owned by the linked QuickJS.
    #[allow(unsafe_code)]
    let qjs = unsafe { std::ffi::CStr::from_ptr(rquickjs::qjs::JS_GetVersion()) }
      .to_str()
      .unwrap_or("unknown");
    let endian = if cfg!(target_endian = "big") { "be" } else { "le" };
    format!(
      "fdbc4-v{}-qjs{qjs}-{}-{endian}-p{}",
      env!("CARGO_PKG_VERSION"),
      std::env::consts::ARCH,
      std::mem::size_of::<usize>() * 8,
    )
  })
}

/// `<cache>/ferridriver/bytecode/<abi_tag>/`, created on demand. Honors
/// `FERRIDRIVER_CACHE_DIR`, else the platform user cache dir, else the
/// system temp dir. Returns `None` if no writable base exists.
fn cache_dir() -> Option<&'static Path> {
  static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
  DIR
    .get_or_init(|| {
      let base = std::env::var_os("FERRIDRIVER_CACHE_DIR")
        .map(PathBuf::from)
        .or_else(user_cache_base)
        .unwrap_or_else(std::env::temp_dir);
      let dir = base.join("ferridriver").join("bytecode").join(abi_tag());
      match std::fs::create_dir_all(&dir) {
        Ok(()) => Some(dir),
        Err(_) => None,
      }
    })
    .as_deref()
}

fn user_cache_base() -> Option<PathBuf> {
  if let Some(x) = std::env::var_os("XDG_CACHE_HOME") {
    return Some(PathBuf::from(x));
  }
  #[cfg(target_os = "macos")]
  if let Some(h) = std::env::var_os("HOME") {
    return Some(PathBuf::from(h).join("Library").join("Caches"));
  }
  std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache"))
}

fn hash_bytes(bytes: &[u8]) -> u64 {
  let mut h = std::collections::hash_map::DefaultHasher::new();
  bytes.hash(&mut h);
  h.finish()
}

/// A stable key for a set of entry paths (canonicalized, order-independent).
/// The transitive content check on load is what actually guards freshness;
/// this only needs to be collision-free across distinct bundle requests.
///
/// `kind` namespaces the consumers: the same file compiled as a extension
/// vs. as a BDD steps entry produces different bundles (different cwd,
/// different aux payload), so they must not share one slot. `salt`
/// carries extra pipeline state that changes the output without
/// changing any input file — today the bundler-shims fingerprint
/// (`BundlerEnv::fingerprint`). `cwd` is the directory the bundle is
/// built from, which decides how every bare specifier resolves.
#[must_use]
pub fn entry_key(kind: &str, entry_paths: &[PathBuf], cwd: &Path, salt: u64) -> u64 {
  let mut canon: Vec<String> = entry_paths
    .iter()
    .map(|p| {
      std::fs::canonicalize(p)
        .unwrap_or_else(|_| p.clone())
        .to_string_lossy()
        .into_owned()
    })
    .collect();
  canon.sort();
  let mut h = std::collections::hash_map::DefaultHasher::new();
  abi_tag().hash(&mut h);
  kind.hash(&mut h);
  salt.hash(&mut h);
  // The bundling cwd decides how bare specifiers and `node_modules`
  // resolve, so the same entry files bundled from two directories are
  // two different outputs — and used to share one cache slot.
  std::fs::canonicalize(cwd)
    .unwrap_or_else(|_| cwd.to_path_buf())
    .hash(&mut h);
  canon.hash(&mut h);
  h.finish()
}

/// The transitive input set for a bundle: the entry files plus every
/// module rolldown reported in the chunk's graph, canonicalized and
/// deduped.
///
/// The module graph — not the source map — is the authority. A helper
/// module whose bindings are all inlined leaves no mapping tokens and so
/// never appears in the map's `sources`, which made the input set omit
/// exactly the files an extension author edits most; both cache tiers
/// then answered "unchanged" for a changed tree.
#[must_use]
pub fn input_set(entry_paths: &[PathBuf], modules: &[PathBuf]) -> Vec<PathBuf> {
  let mut out: Vec<PathBuf> = Vec::new();
  let mut push = |p: &Path| {
    let c = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    if !out.contains(&c) {
      out.push(c);
    }
  };
  for e in entry_paths {
    push(e);
  }
  for m in modules {
    if m.is_file() {
      push(m);
    }
  }
  out
}

/// Content fingerprint over a transitive input set, for an in-process
/// cache tier that has to answer the same freshness question [`load`]
/// answers on disk: has ANY input changed, not just the entry file.
///
/// `None` when an input cannot be read — the source moved, so the cached
/// compile must not be reused.
#[must_use]
pub fn inputs_fingerprint(inputs: &[PathBuf]) -> Option<u64> {
  let mut h = std::collections::hash_map::DefaultHasher::new();
  for p in inputs {
    p.hash(&mut h);
    hash_bytes(&std::fs::read(p).ok()?).hash(&mut h);
  }
  Some(h.finish())
}

fn paths(key: u64) -> Option<(PathBuf, PathBuf)> {
  let dir = cache_dir()?;
  let hex = format!("{key:016x}");
  Some((dir.join(format!("{hex}.bin")), dir.join(format!("{hex}.json"))))
}

/// Load a cached compile for `key`, validating that every recorded input
/// still hashes identically. Returns `None` on any miss, mismatch, or IO
/// error (the caller then compiles and [`store`]s).
#[must_use]
pub fn load(key: u64) -> Option<CacheEntry> {
  if disabled() {
    return None;
  }
  let (bin_path, json_path) = paths(key)?;
  let manifest: Manifest = serde_json::from_slice(&std::fs::read(json_path).ok()?).ok()?;
  for (path, want) in &manifest.inputs {
    let bytes = std::fs::read(path).ok()?;
    if hash_bytes(&bytes) != *want {
      return None;
    }
  }
  let bytecode = std::fs::read(bin_path).ok()?;
  if hash_bytes(&bytecode) != manifest.bytecode_hash {
    return None;
  }
  Some(CacheEntry {
    bytecode,
    module_name: manifest.module_name,
    source_map_json: manifest.source_map_json,
    aux: manifest.aux,
    inputs: manifest.inputs.iter().map(|(p, _)| PathBuf::from(p)).collect(),
  })
}

/// Persist a freshly compiled `key` -> bytecode entry. Best-effort: any
/// IO failure is swallowed (the cache is an optimization, never a
/// correctness dependency). Writes are atomic via temp-file + rename so a
/// concurrent or crashed writer never exposes a torn manifest.
pub fn store(
  key: u64,
  bytecode: &[u8],
  module_name: &str,
  source_map_json: Option<&str>,
  aux: Option<&str>,
  inputs: &[PathBuf],
) {
  if disabled() {
    return;
  }
  let Some((bin_path, json_path)) = paths(key) else {
    return;
  };
  let input_hashes: Vec<(String, u64)> = inputs
    .iter()
    .filter_map(|p| {
      let bytes = std::fs::read(p).ok()?;
      Some((p.to_string_lossy().into_owned(), hash_bytes(&bytes)))
    })
    .collect();
  let manifest = Manifest {
    module_name: module_name.to_string(),
    source_map_json: source_map_json.map(str::to_string),
    aux: aux.map(str::to_string),
    inputs: input_hashes,
    bytecode_hash: hash_bytes(bytecode),
  };
  let Ok(json) = serde_json::to_vec(&manifest) else {
    return;
  };
  let _ = atomic_write(&bin_path, bytecode);
  let _ = atomic_write(&json_path, &json);
}

fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
  let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
  std::fs::write(&tmp, bytes)?;
  std::fs::rename(&tmp, path)
}
