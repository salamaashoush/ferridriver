//! Children that read C runtime file descriptors past stdio.
//!
//! Playwright's `WebKit` build reads its inspector pipe from descriptors 3
//! and 4 on Windows too (`_get_osfhandle(3)`), which Node hands it through
//! the C runtime's inheritance block in `STARTUPINFO::lpReserved2`. Rust's
//! `Command` cannot fill that block, so the process is created here the way
//! libuv's `uv_spawn` does, with inheritance limited to exactly the handles
//! passed so a concurrent launch cannot pick up this one's pipe ends.

use std::ffi::OsStr;
use std::io;
use std::os::windows::ffi::OsStrExt as _;
use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, IntoRawHandle as _, OwnedHandle};
use std::os::windows::process::ExitStatusExt as _;
use std::path::Path;
use std::process::ExitStatus;

use windows_sys::Win32::Foundation::{
  CloseHandle, HANDLE, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, SetHandleInformation, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::{
  CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, OPEN_EXISTING,
  PIPE_ACCESS_INBOUND, PIPE_ACCESS_OUTBOUND,
};
use windows_sys::Win32::System::Pipes::{CreateNamedPipeW, PIPE_TYPE_BYTE, PIPE_WAIT};
use windows_sys::Win32::System::Threading::{
  CREATE_UNICODE_ENVIRONMENT, CreateProcessW, DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT,
  GetExitCodeProcess, INFINITE, InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
  PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROCESS_INFORMATION, STARTF_USESTDHANDLES, STARTUPINFOEXW, TerminateProcess,
  UpdateProcThreadAttribute, WaitForSingleObject,
};

const GENERIC_READ: u32 = 0x8000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;

/// The C runtime's per-descriptor flags in the inheritance block.
const FOPEN: u8 = 0x01;
const FPIPE: u8 = 0x08;
const FDEV: u8 = 0x40;

fn wide(text: &OsStr) -> Vec<u16> {
  text.encode_wide().chain(std::iter::once(0)).collect()
}

/// Two one-way pipes between this process and a child: the child reads
/// what we write to `ours_write` from `theirs_read`, and we read from
/// `ours_read` what it writes to `theirs_write`. Our ends are overlapped,
/// for tokio; the child's are synchronous, as a C runtime descriptor is.
pub struct PipePair {
  pub ours_read: tokio::net::windows::named_pipe::NamedPipeServer,
  pub ours_write: tokio::net::windows::named_pipe::NamedPipeServer,
  pub theirs_read: OwnedHandle,
  pub theirs_write: OwnedHandle,
}

/// A [`PipePair`] named after `label`, this process, a per-process
/// sequence and the time, so two launches never meet. The time alone did
/// not keep them apart: workers that launch together at the start of a run
/// read the same clock tick, and the second `CreateNamedPipeW` failed with
/// "All pipe instances are busy".
/// # Errors
/// Fails when Windows cannot create or open the pipes.
pub fn pipe_pair(label: &str) -> io::Result<PipePair> {
  static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
  let sequence = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
  let stamp = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .unwrap_or_default()
    .as_nanos();
  let base = format!(
    r"\\.\pipe\ferridriver-{label}-{}-{sequence}-{stamp}",
    std::process::id()
  );
  let to_child = wide(OsStr::new(&format!("{base}-in")));
  let from_child = wide(OsStr::new(&format!("{base}-out")));
  let ours_write = server(&to_child, PIPE_ACCESS_OUTBOUND)?;
  let ours_read = server(&from_child, PIPE_ACCESS_INBOUND)?;
  let theirs_read = client(&to_child, GENERIC_READ)?;
  let theirs_write = client(&from_child, GENERIC_WRITE)?;
  // SAFETY: each handle is a fresh pipe server this value now owns alone.
  #[allow(unsafe_code)]
  let (ours_read, ours_write) = unsafe {
    (
      tokio::net::windows::named_pipe::NamedPipeServer::from_raw_handle(ours_read.into_raw_handle())?,
      tokio::net::windows::named_pipe::NamedPipeServer::from_raw_handle(ours_write.into_raw_handle())?,
    )
  };
  Ok(PipePair {
    ours_read,
    ours_write,
    theirs_read,
    theirs_write,
  })
}

#[allow(unsafe_code)]
fn server(name: &[u16], access: u32) -> io::Result<OwnedHandle> {
  // SAFETY: `name` is NUL-terminated; the handle is checked before it is owned.
  unsafe {
    let handle = CreateNamedPipeW(
      name.as_ptr(),
      access | FILE_FLAG_OVERLAPPED | FILE_FLAG_FIRST_PIPE_INSTANCE,
      PIPE_TYPE_BYTE | PIPE_WAIT,
      1,
      65536,
      65536,
      0,
      std::ptr::null(),
    );
    if handle == INVALID_HANDLE_VALUE {
      return Err(io::Error::last_os_error());
    }
    Ok(OwnedHandle::from_raw_handle(handle))
  }
}

#[allow(unsafe_code)]
fn client(name: &[u16], access: u32) -> io::Result<OwnedHandle> {
  let attributes = SECURITY_ATTRIBUTES {
    nLength: u32::try_from(std::mem::size_of::<SECURITY_ATTRIBUTES>()).map_err(io::Error::other)?,
    lpSecurityDescriptor: std::ptr::null_mut(),
    bInheritHandle: 0,
  };
  // SAFETY: `name` is NUL-terminated and `attributes` outlives the call;
  // the handle is checked before it is owned.
  unsafe {
    let handle = CreateFileW(
      name.as_ptr(),
      access,
      0,
      &raw const attributes,
      OPEN_EXISTING,
      FILE_ATTRIBUTE_NORMAL,
      std::ptr::null_mut(),
    );
    if handle == INVALID_HANDLE_VALUE {
      return Err(io::Error::last_os_error());
    }
    Ok(OwnedHandle::from_raw_handle(handle))
  }
}

/// Let a child spawned through `Command` inherit `handle`.
///
/// # Errors
/// Fails when Windows refuses the flag.
#[allow(unsafe_code)]
pub fn inheritable(handle: &OwnedHandle) -> io::Result<()> {
  // SAFETY: the handle is live for the duration of this call.
  if unsafe {
    SetHandleInformation(
      handle.as_raw_handle() as HANDLE,
      HANDLE_FLAG_INHERIT,
      HANDLE_FLAG_INHERIT,
    )
  } == 0
  {
    return Err(io::Error::last_os_error());
  }
  Ok(())
}

/// What a descriptor the child inherits is, for the C runtime.
#[derive(Clone, Copy)]
pub enum Kind {
  Pipe,
  Device,
}

/// One descriptor the child inherits, numbered by its position.
pub struct Inherited<'a> {
  pub handle: &'a OwnedHandle,
  pub kind: Kind,
}

/// A process this module created: what `tokio::process::Child` is for
/// one `Command` made.
pub struct RawChild {
  process: OwnedHandle,
  pid: u32,
  status: Option<ExitStatus>,
}

impl RawChild {
  /// The pid, until the exit has been observed.
  #[must_use]
  pub fn id(&self) -> Option<u32> {
    self.status.is_none().then_some(self.pid)
  }

  /// # Errors
  /// Fails when Windows cannot report on the process.
  #[allow(unsafe_code)]
  pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
    if let Some(status) = self.status {
      return Ok(Some(status));
    }
    let handle = self.process.as_raw_handle() as HANDLE;
    // SAFETY: the handle is a live process this value owns.
    match unsafe { WaitForSingleObject(handle, 0) } {
      WAIT_OBJECT_0 => {
        let mut code = 0u32;
        // SAFETY: as above; `code` outlives the call.
        if unsafe { GetExitCodeProcess(handle, &raw mut code) } == 0 {
          return Err(io::Error::last_os_error());
        }
        let status = ExitStatus::from_raw(code);
        self.status = Some(status);
        Ok(Some(status))
      },
      WAIT_TIMEOUT => Ok(None),
      _ => Err(io::Error::last_os_error()),
    }
  }

  /// # Errors
  /// Fails when the process is still running and refuses to end.
  #[allow(unsafe_code)]
  pub fn start_kill(&mut self) -> io::Result<()> {
    if self.status.is_some() {
      return Ok(());
    }
    // SAFETY: the handle is a live process this value owns.
    if unsafe { TerminateProcess(self.process.as_raw_handle() as HANDLE, 1) } == 0 {
      let error = io::Error::last_os_error();
      // A process that already exited refuses termination; that is done.
      return if self.try_wait()?.is_some() { Ok(()) } else { Err(error) };
    }
    Ok(())
  }

  /// # Errors
  /// Fails when Windows cannot report on the process.
  #[allow(unsafe_code)]
  pub async fn wait(&mut self) -> io::Result<ExitStatus> {
    if let Some(status) = self.try_wait()? {
      return Ok(status);
    }
    let process = self.process.try_clone()?;
    tokio::task::spawn_blocking(move || {
      // SAFETY: `process` is a duplicate this closure owns until it returns.
      unsafe { WaitForSingleObject(process.as_raw_handle() as HANDLE, INFINITE) }
    })
    .await
    .map_err(io::Error::other)?;
    self
      .try_wait()?
      .ok_or_else(|| io::Error::other("the process was still running after its wait returned"))
  }
}

impl Drop for RawChild {
  fn drop(&mut self) {
    // As `kill_on_drop(true)` does for a tokio child.
    let _ = self.start_kill();
  }
}

/// `program` as a path to run: as given when it names a directory, else
/// found on `PATH` (with `.exe` when it has no extension). Never the
/// current directory, which `CreateProcessW` would search first.
///
/// # Errors
/// Fails when `PATH` has no such program.
pub fn resolve_program(program: &Path) -> io::Result<std::path::PathBuf> {
  if program.components().count() > 1 || program.is_absolute() {
    return Ok(program.to_path_buf());
  }
  let named = if program.extension().is_some() {
    program.to_path_buf()
  } else {
    program.with_extension("exe")
  };
  std::env::var_os("PATH")
    .iter()
    .flat_map(std::env::split_paths)
    .map(|directory| directory.join(&named))
    .find(|candidate| candidate.is_file())
    .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("{} is not on PATH", program.display())))
}

/// Run `program` (see [`resolve_program`]) with `args`, the inherited
/// environment plus `envs`, in `cwd` or this process's directory, and
/// `descriptors` as its C runtime descriptors 0, 1, 2, ... in order.
///
/// # Errors
/// Fails when the program cannot be found or Windows cannot create it.
#[allow(unsafe_code)]
pub fn spawn(
  program: &Path,
  args: &[std::ffi::OsString],
  envs: &[(String, String)],
  cwd: Option<&Path>,
  descriptors: &[Inherited<'_>],
) -> io::Result<RawChild> {
  let program = resolve_program(program)?;
  let program = program.as_path();
  if descriptors.len() < 3 {
    return Err(io::Error::other(
      "a child needs at least its three standard descriptors",
    ));
  }
  let handles: Vec<HANDLE> = descriptors
    .iter()
    .map(|descriptor| descriptor.handle.as_raw_handle() as HANDLE)
    .collect();
  for descriptor in descriptors {
    inheritable(descriptor.handle)?;
  }
  let mut block = crt_block(descriptors, &handles)?;
  let mut command_line = command_line(program, args);
  let mut environment = environment_block(envs);
  let mut inherit: Vec<HANDLE> = Vec::with_capacity(handles.len());
  for handle in &handles {
    if !inherit.contains(handle) {
      inherit.push(*handle);
    }
  }

  // SAFETY: the attribute list is sized by Windows, initialised once,
  // updated with a handle list that outlives `CreateProcessW`, and deleted
  // on every path; every pointer passed refers to a buffer above.
  unsafe {
    let mut size = 0usize;
    InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &raw mut size);
    let mut list_storage = vec![0u8; size];
    let list: LPPROC_THREAD_ATTRIBUTE_LIST = list_storage.as_mut_ptr().cast();
    if InitializeProcThreadAttributeList(list, 1, 0, &raw mut size) == 0 {
      return Err(io::Error::last_os_error());
    }
    let created = (|| {
      if UpdateProcThreadAttribute(
        list,
        0,
        PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
        inherit.as_ptr().cast(),
        inherit.len() * std::mem::size_of::<HANDLE>(),
        std::ptr::null_mut(),
        std::ptr::null(),
      ) == 0
      {
        return Err(io::Error::last_os_error());
      }
      let mut startup: STARTUPINFOEXW = std::mem::zeroed();
      startup.StartupInfo.cb = u32::try_from(std::mem::size_of::<STARTUPINFOEXW>()).map_err(io::Error::other)?;
      startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
      startup.StartupInfo.hStdInput = handles[0];
      startup.StartupInfo.hStdOutput = handles[1];
      startup.StartupInfo.hStdError = handles[2];
      startup.StartupInfo.cbReserved2 = u16::try_from(block.len()).map_err(io::Error::other)?;
      startup.StartupInfo.lpReserved2 = block.as_mut_ptr();
      startup.lpAttributeList = list;
      let mut info: PROCESS_INFORMATION = std::mem::zeroed();
      let application = wide(program.as_os_str());
      let directory = cwd.map(|cwd| wide(cwd.as_os_str()));
      if CreateProcessW(
        application.as_ptr(),
        command_line.as_mut_ptr(),
        std::ptr::null(),
        std::ptr::null(),
        1,
        EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,
        environment.as_mut_ptr().cast(),
        directory.as_ref().map_or(std::ptr::null(), Vec::as_ptr),
        &raw const startup.StartupInfo,
        &raw mut info,
      ) == 0
      {
        return Err(io::Error::last_os_error());
      }
      CloseHandle(info.hThread);
      Ok(RawChild {
        process: OwnedHandle::from_raw_handle(info.hProcess),
        pid: info.dwProcessId,
        status: None,
      })
    })();
    DeleteProcThreadAttributeList(list);
    created
  }
}

/// The C runtime's inheritance block: a count, a flag byte per
/// descriptor, then the handles, packed.
fn crt_block(descriptors: &[Inherited<'_>], handles: &[HANDLE]) -> io::Result<Vec<u8>> {
  let count = i32::try_from(descriptors.len()).map_err(io::Error::other)?;
  let mut block = Vec::with_capacity(4 + descriptors.len() * (1 + std::mem::size_of::<HANDLE>()));
  block.extend_from_slice(&count.to_le_bytes());
  block.extend(descriptors.iter().map(|descriptor| match descriptor.kind {
    Kind::Pipe => FOPEN | FPIPE,
    Kind::Device => FOPEN | FDEV,
  }));
  for handle in handles {
    block.extend_from_slice(&(*handle as usize).to_le_bytes());
  }
  Ok(block)
}

/// The command line as the C runtime parses it back into `argv`: an
/// argument with a space, tab or quote is quoted, and a backslash run is
/// doubled where it precedes a quote.
fn command_line(program: &Path, args: &[std::ffi::OsString]) -> Vec<u16> {
  let mut line = Vec::new();
  append_argument(&mut line, program.as_os_str());
  for arg in args {
    line.push(u16::from(b' '));
    append_argument(&mut line, arg);
  }
  line.push(0);
  line
}

fn append_argument(line: &mut Vec<u16>, arg: &OsStr) {
  let units: Vec<u16> = arg.encode_wide().collect();
  let quote = units.is_empty()
    || units
      .iter()
      .any(|&unit| unit == u16::from(b' ') || unit == u16::from(b'\t') || unit == u16::from(b'"'));
  if quote {
    line.push(u16::from(b'"'));
  }
  let mut backslashes = 0usize;
  for unit in units {
    if unit == u16::from(b'\\') {
      backslashes += 1;
    } else {
      if unit == u16::from(b'"') {
        line.extend(std::iter::repeat_n(u16::from(b'\\'), backslashes + 1));
      }
      backslashes = 0;
    }
    line.push(unit);
  }
  if quote {
    line.extend(std::iter::repeat_n(u16::from(b'\\'), backslashes));
    line.push(u16::from(b'"'));
  }
}

/// This process's environment with `overrides` applied, as the
/// `NAME=value\0...\0\0` block `CreateProcessW` takes. Windows names are
/// case-insensitive, so an override replaces any spelling of its name.
fn environment_block(overrides: &[(String, String)]) -> Vec<u16> {
  let mut variables: Vec<(std::ffi::OsString, std::ffi::OsString)> = std::env::vars_os()
    .filter(|(name, _)| {
      !overrides
        .iter()
        .any(|(key, _)| name.to_string_lossy().eq_ignore_ascii_case(key))
    })
    .collect();
  variables.extend(overrides.iter().map(|(name, value)| (name.into(), value.into())));
  variables.sort_by_key(|(name, _)| name.to_string_lossy().to_uppercase());
  let mut block = Vec::new();
  for (name, value) in variables {
    block.extend(name.encode_wide());
    block.push(u16::from(b'='));
    block.extend(value.encode_wide());
    block.push(0);
  }
  block.push(0);
  block
}

#[cfg(test)]
mod tests {
  use super::*;

  fn line(args: &[&str]) -> String {
    let args: Vec<std::ffi::OsString> = args.iter().map(Into::into).collect();
    let units = command_line(Path::new(r"C:\Program Files\x.exe"), &args);
    String::from_utf16_lossy(&units[..units.len() - 1])
  }

  #[test]
  fn arguments_survive_the_c_runtime_parse() {
    assert_eq!(
      line(&["--inspector-pipe", "a b", "", r"tail\", r#"say "hi""#, r"dir\\"]),
      r#""C:\Program Files\x.exe" --inspector-pipe "a b" "" tail\ "say \"hi\"" dir\\"#
    );
    assert_eq!(line(&[r"with space\"]), r#""C:\Program Files\x.exe" "with space\\""#);
  }

  #[test]
  fn the_inheritance_block_lists_each_descriptor_and_its_handle() {
    let file = std::fs::File::open("NUL").expect("open NUL");
    let handle = OwnedHandle::from(file);
    let descriptors = [
      Inherited {
        handle: &handle,
        kind: Kind::Device,
      },
      Inherited {
        handle: &handle,
        kind: Kind::Pipe,
      },
    ];
    let raw = handle.as_raw_handle() as HANDLE;
    let block = crt_block(&descriptors, &[raw, raw]).expect("block");
    assert_eq!(&block[..4], &2i32.to_le_bytes());
    assert_eq!(&block[4..6], &[FOPEN | FDEV, FOPEN | FPIPE]);
    assert_eq!(
      &block[6..6 + std::mem::size_of::<HANDLE>()],
      &(raw as usize).to_le_bytes()
    );
  }

  /// A child reads what we write on its descriptor 3 and answers on 4,
  /// through the C runtime, as `WebKit` does. PowerShell reaches the
  /// descriptors through the handles `_get_osfhandle` would return, so
  /// the round trip proves the block, the handle list and the pipes.
  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn a_child_talks_over_descriptors_three_and_four() {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let pair = pipe_pair("spawn-test").expect("pipes");
    let null = OwnedHandle::from(std::fs::File::open("NUL").expect("NUL"));
    let mut ours_write = pair.ours_write;
    let mut ours_read = pair.ours_read;
    let script = "$i = [System.IO.FileStream]::new([Microsoft.Win32.SafeHandles.SafeFileHandle]::new([IntPtr][long]$env:FD3, $false), 'Read'); \
       $o = [System.IO.FileStream]::new([Microsoft.Win32.SafeHandles.SafeFileHandle]::new([IntPtr][long]$env:FD4, $false), 'Write'); \
       $b = New-Object byte[] 4; $n = $i.Read($b, 0, 4); $o.Write($b, 0, $n); $o.Flush()";
    let envs = [
      (
        "FD3".to_string(),
        (pair.theirs_read.as_raw_handle() as usize).to_string(),
      ),
      (
        "FD4".to_string(),
        (pair.theirs_write.as_raw_handle() as usize).to_string(),
      ),
    ];
    let powershell = std::path::PathBuf::from(std::env::var("SystemRoot").expect("SystemRoot"))
      .join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
    let mut child = spawn(
      &powershell,
      &["-NoProfile".into(), "-Command".into(), script.into()],
      &envs,
      None,
      &[
        Inherited {
          handle: &null,
          kind: Kind::Device,
        },
        Inherited {
          handle: &null,
          kind: Kind::Device,
        },
        Inherited {
          handle: &null,
          kind: Kind::Device,
        },
        Inherited {
          handle: &pair.theirs_read,
          kind: Kind::Pipe,
        },
        Inherited {
          handle: &pair.theirs_write,
          kind: Kind::Pipe,
        },
      ],
    )
    .expect("spawn");
    drop(pair.theirs_read);
    drop(pair.theirs_write);
    ours_write.write_all(b"ping").await.expect("write");
    let mut answer = [0u8; 4];
    ours_read.read_exact(&mut answer).await.expect("read");
    assert_eq!(&answer, b"ping");
    assert!(child.wait().await.expect("wait").success());
  }
}
