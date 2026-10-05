use std::fs::File;
use std::io::Write as _;
use std::path::Path;
#[cfg(not(unix))]
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use base64::Engine as _;
use rustc_hash::FxHashMap;
use serde_json::Value;
use sha2::{Digest as _, Sha256};

use crate::{FerriError, Result};

#[cfg(unix)]
type ProfileKey = (u64, u64);
#[cfg(not(unix))]
type ProfileKey = PathBuf;
type ProfileLocks = FxHashMap<ProfileKey, Weak<tokio::sync::Mutex<()>>>;

fn profile_key(profile: &Path) -> std::io::Result<ProfileKey> {
  #[cfg(unix)]
  {
    use std::os::unix::fs::MetadataExt as _;
    let metadata = profile.metadata()?;
    Ok((metadata.dev(), metadata.ino()))
  }
  #[cfg(not(unix))]
  profile.canonicalize()
}

pub(super) struct ProfileGuard {
  file: Option<File>,
  _claim: File,
  _local: tokio::sync::OwnedMutexGuard<()>,
}

impl ProfileGuard {
  pub(super) fn acquire(profile: &Path) -> Result<Self> {
    static LOCKS: OnceLock<Mutex<ProfileLocks>> = OnceLock::new();
    let key = profile_key(profile)?;
    let local = {
      let mut locks = LOCKS
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
      locks.retain(|_, lock| lock.strong_count() != 0);
      if let Some(lock) = locks.get(&key).and_then(Weak::upgrade) {
        lock
      } else {
        let lock = Arc::new(tokio::sync::Mutex::new(()));
        locks.insert(key, Arc::downgrade(&lock));
        lock
      }
    };
    // Closing any descriptor for this inode releases this process's POSIX locks.
    // Exclude same-process contenders before they can open Firefox's lock file.
    let local = local.try_lock_owned().map_err(|_| busy(profile))?;
    // Darwin's flock and record locks conflict even on the same descriptor.
    let claim = claim_profile(profile)?;
    let file = lock_profile(profile)?;
    Ok(Self {
      file: Some(file),
      _claim: claim,
      _local: local,
    })
  }

  pub(super) fn allow_firefox_start(&mut self) -> Result<()> {
    #[cfg(unix)]
    if let Some(file) = &self.file {
      rustix::fs::fcntl_lock(file, rustix::fs::FlockOperation::Unlock).map_err(std::io::Error::from)?;
    }
    #[cfg(not(unix))]
    self.file.take();
    Ok(())
  }
}

fn busy(profile: &Path) -> FerriError {
  FerriError::invalid_argument(
    "userDataDir",
    format!("Firefox profile {} is already in use", profile.display()),
  )
}

fn claim_profile(profile: &Path) -> Result<File> {
  let path = profile.join(".ferridriver-profile.lock");
  match std::fs::symlink_metadata(&path) {
    Ok(metadata) if !metadata.is_file() || metadata.file_type().is_symlink() => return Err(busy(profile)),
    Ok(_) => {},
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
    Err(error) => return Err(error.into()),
  }
  let file = std::fs::OpenOptions::new()
    .create(true)
    .truncate(false)
    .read(true)
    .write(true)
    .open(path)?;
  file.try_lock().map_err(|error| match error {
    std::fs::TryLockError::WouldBlock => busy(profile),
    std::fs::TryLockError::Error(error) => error.into(),
  })?;
  Ok(file)
}

#[cfg(unix)]
fn lock_profile(profile: &Path) -> Result<File> {
  let path = profile.join(".parentlock");
  match std::fs::symlink_metadata(&path) {
    Ok(metadata) if !metadata.is_file() || metadata.file_type().is_symlink() => return Err(busy(profile)),
    Ok(_) => {},
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
    Err(error) => return Err(error.into()),
  }
  let file = std::fs::OpenOptions::new()
    .create(true)
    .truncate(false)
    .write(true)
    .open(path)?;
  rustix::fs::fcntl_lock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive).map_err(|error| {
    if error == rustix::io::Errno::AGAIN || error == rustix::io::Errno::ACCESS {
      busy(profile)
    } else {
      FerriError::backend(format!("Firefox profile locking failed: {error}"))
    }
  })?;
  // Firefox marks its compatibility symlink with '+' when the record lock is authoritative.
  // Once we hold that lock, Firefox can retire this obsolete symlink on its next launch.
  for name in ["lock", "parent.lock"] {
    match std::fs::symlink_metadata(profile.join(name)) {
      Ok(metadata) if name == "lock" && metadata.file_type().is_symlink() => {
        let target = std::fs::read_link(profile.join(name))?;
        if target
          .to_str()
          .and_then(|target| target.split_once(":+"))
          .is_some_and(|(host, pid)| {
            host.parse::<std::net::Ipv4Addr>().is_ok() && pid.parse::<u32>().is_ok_and(|pid| pid != 0)
          })
        {
          continue;
        }
        return Err(busy(profile));
      },
      Ok(_) => {
        return Err(FerriError::invalid_argument(
          "userDataDir",
          format!(
            "Firefox profile has a legacy {name} lock; close its owner or resolve the stale lock before launching"
          ),
        ));
      },
      Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
      Err(error) => return Err(error.into()),
    }
  }
  Ok(file)
}

#[cfg(windows)]
fn lock_profile(profile: &Path) -> Result<File> {
  use std::os::windows::fs::OpenOptionsExt as _;
  std::fs::OpenOptions::new()
    .create(true)
    .truncate(false)
    .read(true)
    .write(true)
    .share_mode(0)
    .open(profile.join("parent.lock"))
    .map_err(FerriError::from)
}

#[cfg(not(any(unix, windows)))]
fn lock_profile(_profile: &Path) -> Result<File> {
  Err(FerriError::unsupported(
    "Firefox profile locking is unavailable on this platform",
  ))
}

fn preference_number(number: &serde_json::Number) -> Option<i32> {
  if let Some(integer) = number.as_i64() {
    return i32::try_from(integer).ok();
  }
  let value = number.as_f64()?;
  if value.fract().classify() != std::num::FpCategory::Zero {
    return None;
  }
  format!("{value:.0}").parse().ok()
}

pub(super) fn validate_preferences(preferences: Option<&FxHashMap<String, Value>>) -> Result<()> {
  for (name, value) in preferences.into_iter().flatten() {
    let supported = match value {
      Value::String(text) => !text.contains('\0'),
      Value::Bool(_) => true,
      Value::Number(number) => preference_number(number).is_some(),
      _ => false,
    };
    if !supported || name.contains('\0') {
      return Err(FerriError::invalid_argument(
        "firefoxUserPrefs",
        format!("preference {name:?} must be a string, boolean or signed 32-bit integer, without NUL characters"),
      ));
    }
  }
  Ok(())
}

pub(super) fn write_preferences(
  profile: &Path,
  defaults: &[u8],
  preferences: Option<&FxHashMap<String, Value>>,
) -> Result<()> {
  validate_preferences(preferences)?;
  let path = profile.join("user.js");
  let permissions = match std::fs::symlink_metadata(&path) {
    Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => Some(metadata.permissions()),
    Ok(_) => {
      return Err(FerriError::invalid_argument(
        "userDataDir",
        "Firefox user.js must be a regular file",
      ));
    },
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
    Err(error) => return Err(error.into()),
  };
  let existing = if permissions.is_some() {
    std::fs::read(&path)?
  } else {
    Vec::new()
  };
  let (bom, caller) = existing
    .strip_prefix(b"\xef\xbb\xbf")
    .map_or((&b""[..], existing.as_slice()), |caller| (&b"\xef\xbb\xbf"[..], caller));
  let caller = remove_owned_block(&remove_owned_block(caller, "defaults"), "overrides");
  let mut contents = bom.to_vec();
  contents.extend(block(defaults, "defaults"));
  contents.extend_from_slice(&caller);
  if !caller.is_empty() && !caller.ends_with(b"\n") {
    contents.push(b'\n');
  }
  let mut preferences: Vec<_> = preferences.into_iter().flatten().collect();
  preferences.sort_unstable_by_key(|(name, _)| *name);
  let mut overrides = Vec::new();
  for (name, value) in preferences {
    let value = if let Value::Number(number) = value {
      preference_number(number)
        .ok_or_else(|| {
          FerriError::invalid_argument("firefoxUserPrefs", "Firefox preferences require signed 32-bit integers")
        })?
        .to_string()
    } else {
      serde_json::to_string(value)?
    };
    writeln!(overrides, "user_pref({}, {});", serde_json::to_string(name)?, value)?;
  }
  if !overrides.is_empty() {
    contents.extend(block(&overrides, "overrides"));
  }
  if contents == existing {
    return Ok(());
  }
  let mut temporary = tempfile::NamedTempFile::new_in(profile)?;
  if let Some(permissions) = permissions {
    temporary.as_file().set_permissions(permissions)?;
  }
  temporary.write_all(&contents)?;
  temporary.persist(path).map_err(|error| FerriError::from(error.error))?;
  Ok(())
}

fn header(payload: &[u8], label: &str) -> Vec<u8> {
  let hash = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(payload));
  format!("// ferridriver {label} {hash}\n").into_bytes()
}

fn block(payload: &[u8], label: &str) -> Vec<u8> {
  let mut block = header(payload, label);
  block.extend_from_slice(payload);
  block.extend_from_slice(format!("// ferridriver end {label}\n").as_bytes());
  block
}

fn find_bytes(contents: &[u8], needle: &[u8]) -> Option<usize> {
  contents.windows(needle.len()).position(|part| part == needle)
}

fn remove_owned_block(contents: &[u8], label: &str) -> Vec<u8> {
  let prefix = format!("// ferridriver {label} ");
  let suffix = format!("// ferridriver end {label}\n");
  let mut output = Vec::with_capacity(contents.len());
  let mut copied = 0;
  let mut offset = 0;
  while let Some(start) = find_bytes(&contents[offset..], prefix.as_bytes()).map(|index| offset + index) {
    offset = start + prefix.len();
    if start != 0 && contents[start - 1] != b'\n' {
      continue;
    }
    let Some(payload) = contents[offset..]
      .iter()
      .position(|byte| *byte == b'\n')
      .map(|index| offset + index + 1)
    else {
      break;
    };
    let Some(end) = find_bytes(&contents[payload..], suffix.as_bytes()).map(|index| payload + index) else {
      break;
    };
    if contents[start..payload] == header(&contents[payload..end], label) {
      output.extend_from_slice(&contents[copied..start]);
      copied = end + suffix.len();
      offset = copied;
    }
  }
  output.extend_from_slice(&contents[copied..]);
  output
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn preferences_preserve_caller_bytes_and_replace_only_verified_blocks() {
    let profile = tempfile::tempdir().unwrap();
    let caller = b"\xef\xbb\xbf// sashoush: \xff\nuser_pref(\"custom\", 42);";
    let path = profile.path().join("user.js");
    std::fs::write(&path, caller).unwrap();
    let _guard = ProfileGuard::acquire(profile.path()).unwrap();
    let preferences = FxHashMap::from_iter([
      ("custom".into(), serde_json::json!(43.0)),
      ("quoted\"\\key".into(), serde_json::json!("sashoush\n\"value")),
      ("enabled".into(), serde_json::json!(true)),
    ]);
    write_preferences(profile.path(), b"user_pref(\"default\", 1);\n", Some(&preferences)).unwrap();
    let first = std::fs::read(&path).unwrap();
    assert!(first.starts_with(b"\xef\xbb\xbf// ferridriver defaults "));
    assert!(find_bytes(&first, &caller[3..]).is_some());
    assert!(
      find_bytes(&first, b"user_pref(\"custom\", 43);").unwrap()
        > find_bytes(&first, b"user_pref(\"custom\", 42);").unwrap()
    );
    assert!(find_bytes(&first, br#"user_pref("quoted\"\\key", "sashoush\n\"value");"#).is_some());
    write_preferences(profile.path(), b"user_pref(\"default\", 2);\n", Some(&preferences)).unwrap();
    let second = std::fs::read(&path).unwrap();
    assert_eq!(first.len(), second.len());
    assert!(find_bytes(&second, b"user_pref(\"default\", 1);").is_none());
    assert!(find_bytes(&second, b"user_pref(\"default\", 2);").is_some());
    let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
    write_preferences(profile.path(), b"user_pref(\"default\", 2);\n", Some(&preferences)).unwrap();
    assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), modified);
  }

  #[test]
  fn manually_edited_blocks_remain_caller_content() {
    let profile = tempfile::tempdir().unwrap();
    let mut edited = block(b"user_pref(\"custom\", 1);\n", "defaults");
    let index = find_bytes(&edited, b", 1)").unwrap() + 2;
    edited[index] = b'9';
    let path = profile.path().join("user.js");
    std::fs::write(&path, &edited).unwrap();
    let _guard = ProfileGuard::acquire(profile.path()).unwrap();
    write_preferences(profile.path(), b"user_pref(\"custom\", 2);\n", None).unwrap();
    let current = std::fs::read(&path).unwrap();
    assert!(current.ends_with(&edited));
    assert!(
      find_bytes(&current, b"user_pref(\"custom\", 2);").unwrap()
        < find_bytes(&current, b"user_pref(\"custom\", 9);").unwrap()
    );
  }

  #[tokio::test]
  async fn invalid_preferences_fail_before_creating_or_modifying_a_profile() {
    let root = tempfile::tempdir().unwrap();
    let profile = root.path().join("untouched");
    for invalid in [
      Value::Null,
      serde_json::json!([]),
      serde_json::json!({}),
      serde_json::json!(1.5),
      serde_json::json!(2_147_483_648_i64),
      serde_json::json!(-2_147_483_649_i64),
      serde_json::json!("sashoush\0preference"),
    ] {
      let preferences = FxHashMap::from_iter([("custom".into(), invalid)]);
      let result = super::super::session::BidiSession::launch_firefox(
        "missing-firefox",
        &[],
        true,
        &FxHashMap::default(),
        Some(&profile),
        None,
        Some(&preferences),
      )
      .await;
      let Err(error) = result else {
        panic!("invalid preferences launched")
      };
      assert!(error.to_string().contains("firefoxUserPrefs"), "{error}");
      assert!(!profile.exists());
    }
  }

  #[cfg(unix)]
  fn lock_probe(profile: &Path) -> std::process::Child {
    std::process::Command::new("python3")
      .args(["-u", "-c", "import fcntl,sys\nf=open(sys.argv[1], 'a')\ntry:\n fcntl.lockf(f, fcntl.LOCK_EX | fcntl.LOCK_NB)\n print('locked', flush=True)\n sys.stdin.read()\nexcept BlockingIOError:\n print('busy', flush=True)\n"])
      .arg(profile.join(".parentlock"))
      .stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).spawn().unwrap()
  }

  #[cfg(unix)]
  #[test]
  fn profile_guard_coordinates_with_firefox_record_locks_and_same_process_launches() {
    use std::io::BufRead as _;
    let profile = tempfile::tempdir().unwrap();
    let mut guard = ProfileGuard::acquire(profile.path()).unwrap();
    assert!(ProfileGuard::acquire(profile.path()).is_err());
    let aliases = tempfile::tempdir().unwrap();
    let alias = aliases.path().join("same-profile");
    std::os::unix::fs::symlink(profile.path(), &alias).unwrap();
    assert!(ProfileGuard::acquire(&alias).is_err());
    let mut blocked = lock_probe(profile.path());
    let mut response = String::new();
    std::io::BufReader::new(blocked.stdout.take().unwrap())
      .read_line(&mut response)
      .unwrap();
    assert_eq!(response.trim(), "busy");
    assert!(blocked.wait().unwrap().success());
    guard.allow_firefox_start().unwrap();
    let mut firefox = lock_probe(profile.path());
    response.clear();
    std::io::BufReader::new(firefox.stdout.take().unwrap())
      .read_line(&mut response)
      .unwrap();
    assert_eq!(response.trim(), "locked");
    drop(guard);
    assert!(ProfileGuard::acquire(profile.path()).is_err());
    drop(firefox.stdin.take());
    assert!(firefox.wait().unwrap().success());
    assert!(ProfileGuard::acquire(profile.path()).is_ok());
  }

  #[cfg(unix)]
  #[test]
  fn obsolete_firefox_symlinks_remain_intact_while_real_legacy_locks_are_rejected() {
    let profile = tempfile::tempdir().unwrap();
    let lock = profile.path().join("lock");
    let target = "127.0.0.1:+12345";
    std::os::unix::fs::symlink(target, &lock).unwrap();
    let guard = ProfileGuard::acquire(profile.path()).unwrap();
    assert_eq!(std::fs::read_link(&lock).unwrap(), Path::new(target));
    drop(guard);
    std::fs::remove_file(&lock).unwrap();
    std::os::unix::fs::symlink("127.0.0.1:12345", &lock).unwrap();
    assert!(ProfileGuard::acquire(profile.path()).is_err());
    assert_eq!(std::fs::read_link(lock).unwrap(), Path::new("127.0.0.1:12345"));
  }

  #[cfg(unix)]
  #[test]
  fn symlinked_preferences_are_not_replaced() {
    let profile = tempfile::tempdir().unwrap();
    let original = profile.path().join("original");
    std::fs::write(&original, "sashoush sentinel").unwrap();
    std::os::unix::fs::symlink(&original, profile.path().join("user.js")).unwrap();
    let _guard = ProfileGuard::acquire(profile.path()).unwrap();
    assert!(write_preferences(profile.path(), b"user_pref(\"x\", true);\n", None).is_err());
    assert_eq!(std::fs::read_to_string(original).unwrap(), "sashoush sentinel");
    assert!(
      std::fs::symlink_metadata(profile.path().join("user.js"))
        .unwrap()
        .file_type()
        .is_symlink()
    );
  }
}
