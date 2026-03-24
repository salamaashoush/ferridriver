//! IPC between parent and WebKit host subprocess.
//!
//! Binary frame protocol over Unix socketpair (ported from Bun's ipc_protocol.h):
//!   Frame = { u32 len, u32 req_id, u8 op } (9 bytes LE) + payload[len]
//!   Strings = u32 len (LE) + UTF-8 bytes
//!
//! Child: single-threaded, nonblocking socket, NSRunLoop for AppKit callbacks.
//!        NO background threads. NO mpsc. NO socket cloning.
//!        Matches Bun's host_main.cpp: read() to EAGAIN, parse frames, dispatch.
//!
//! Parent: std blocking sockets, background reader thread, oneshot channels.
//!         std::process::Command spawn (NOT tokio).

use rustc_hash::FxHashMap;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::oneshot;

// ─── Binary frame protocol ──────────────────────────────────────────────────

const FRAME_HDR: usize = 9;

#[repr(u8)]
#[derive(Debug, Clone, Copy)]
pub enum Op {
    CreateView = 1, Navigate = 2, Evaluate = 3, Screenshot = 4,
    Close = 5, GoBack = 7, GoForward = 8, Reload = 9,
    Click = 10, Type = 11, PressKey = 12,
    GetUrl = 20, GetTitle = 21, ListViews = 22, SetUserAgent = 30,
    WaitNav = 40, SetFileInput = 50, SetViewport = 51,
    GetCookies = 60, SetCookie = 61, DeleteCookie = 62, ClearCookies = 63,
    LoadHtml = 64, AddInitScript = 65, MouseEvent = 66,
    SetLocale = 67, SetTimezone = 68, EmulateMedia = 69,
    Shutdown = 255,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy)]
pub enum Rep {
    Ok = 1, Error = 2, Value = 3, ViewCreated = 4, ViewList = 5, Binary = 6,
}

fn frame_write(w: &mut impl Write, req_id: u32, op: u8, payload: &[u8]) {
    let len = payload.len() as u32;
    let mut h = [0u8; FRAME_HDR];
    h[0..4].copy_from_slice(&len.to_le_bytes());
    h[4..8].copy_from_slice(&req_id.to_le_bytes());
    h[8] = op;
    let _ = w.write_all(&h);
    if !payload.is_empty() { let _ = w.write_all(payload); }
    let _ = w.flush();
}

pub fn str_encode(buf: &mut Vec<u8>, s: &str) {
    buf.extend_from_slice(&(s.len() as u32).to_le_bytes());
    buf.extend_from_slice(s.as_bytes());
}

fn str_decode(data: &[u8], off: &mut usize) -> String {
    if *off + 4 > data.len() { return String::new(); }
    let n = u32::from_le_bytes([data[*off], data[*off+1], data[*off+2], data[*off+3]]) as usize;
    *off += 4;
    if *off + n > data.len() { *off = data.len(); return String::new(); }
    let s = String::from_utf8_lossy(&data[*off..*off + n]).to_string();
    *off += n;
    s
}

fn u64_decode(data: &[u8], off: &mut usize) -> u64 {
    if *off + 8 > data.len() { return 0; }
    let v = u64::from_le_bytes([data[*off],data[*off+1],data[*off+2],data[*off+3],
                                data[*off+4],data[*off+5],data[*off+6],data[*off+7]]);
    *off += 8;
    v
}

// ─── IPC Response ───────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum IpcResponse {
    Ok,
    Error(String),
    Value(serde_json::Value),
    ViewCreated(u64),
    ViewList(Vec<u64>),
    Binary(Vec<u8>),
}

// ─── Parent-side client ─────────────────────────────────────────────────────

pub struct IpcClient {
    writer: StdMutex<std::os::unix::net::UnixStream>,
    pending: Arc<StdMutex<FxHashMap<u64, oneshot::Sender<IpcResponse>>>>,
    next_id: AtomicU64,
    /// Console messages pushed by the host via REP_CONSOLE_EVENT (no polling).
    pub console_log: Arc<StdMutex<Vec<(String, String)>>>,
    /// Dialog events pushed by the host via REP_DIALOG_EVENT (no polling).
    pub dialog_log: Arc<StdMutex<Vec<(String, String, String)>>>,
    /// Network events pushed by the host via REP_NET_EVENT (no polling).
    pub network_log: Arc<StdMutex<Vec<(String, String, String, String)>>>,
}

/// Path to the host binary baked in at compile time by build.rs.
#[cfg(target_os = "macos")]
static HOST_BINARY_PATH: &str = concat!(env!("OUT_DIR"), "/fd_webkit_host");

impl IpcClient {
    /// Resolve the path to the WebKit host binary.
    ///
    /// Priority:
    /// 1. `FERRIDRIVER_WEBKIT_HOST` env var (explicit override)
    /// 2. Compile-time path from build.rs (baked into the binary)
    #[cfg(target_os = "macos")]
    fn resolve_host_binary() -> Result<std::path::PathBuf, String> {
        if let Ok(path) = std::env::var("FERRIDRIVER_WEBKIT_HOST") {
            let p = std::path::PathBuf::from(&path);
            if p.exists() {
                return Ok(p);
            }
            return Err(format!("FERRIDRIVER_WEBKIT_HOST={path} does not exist"));
        }

        let p = std::path::PathBuf::from(HOST_BINARY_PATH);
        if p.exists() {
            return Ok(p);
        }

        Err(format!(
            "WebKit host binary not found at {HOST_BINARY_PATH}. \
             Set FERRIDRIVER_WEBKIT_HOST to the path of fd_webkit_host."
        ))
    }

    pub async fn spawn() -> Result<(Self, std::process::Child), String> {
        use std::os::unix::io::IntoRawFd;

        let (parent_sock, child_sock) =
            std::os::unix::net::UnixStream::pair().map_err(|e| format!("socketpair: {e}"))?;
        let child_fd = child_sock.into_raw_fd();
        let exe = Self::resolve_host_binary()?;

        let child = unsafe {
            use std::os::unix::process::CommandExt;
            let mut cmd = std::process::Command::new(&exe);
            cmd.stdin(std::process::Stdio::null())
               .stdout(std::process::Stdio::null())
               .stderr(std::process::Stdio::inherit());
            cmd.pre_exec(move || {
                let flags = libc::fcntl(child_fd, libc::F_GETFD);
                if flags != -1 { libc::fcntl(child_fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC); }
                if child_fd != 3 {
                    if libc::dup2(child_fd, 3) == -1 { return Err(std::io::Error::last_os_error()); }
                    libc::close(child_fd);
                }
                Ok(())
            });
            cmd.spawn().map_err(|e| format!("spawn webkit host ({exe:?}): {e}"))?
        };

        let read_sock = parent_sock.try_clone().map_err(|e| format!("clone: {e}"))?;
        let write_sock = parent_sock;

        let pending: Arc<StdMutex<FxHashMap<u64, oneshot::Sender<IpcResponse>>>> =
            Arc::new(StdMutex::new(FxHashMap::default()));
        let pending2 = pending.clone();
        let console_log: Arc<StdMutex<Vec<(String, String)>>> =
            Arc::new(StdMutex::new(Vec::new()));
        let console_log2 = console_log.clone();
        let dialog_log: Arc<StdMutex<Vec<(String, String, String)>>> =
            Arc::new(StdMutex::new(Vec::new()));
        let dialog_log2 = dialog_log.clone();
        let network_log: Arc<StdMutex<Vec<(String, String, String, String)>>> =
            Arc::new(StdMutex::new(Vec::new()));
        let network_log2 = network_log.clone();

        // Reader thread: blocking reads of binary frames
        std::thread::spawn(move || {
            let mut s = read_sock;
            let mut h = [0u8; FRAME_HDR];
            loop {
                if s.read_exact(&mut h).is_err() { return; }
                let len = u32::from_le_bytes([h[0],h[1],h[2],h[3]]) as usize;
                let rid = u32::from_le_bytes([h[4],h[5],h[6],h[7]]) as u64;
                let rep = h[8];
                let mut payload = vec![0u8; len];
                if len > 0 && s.read_exact(&mut payload).is_err() { return; }

                let resp = match rep {
                    1 => IpcResponse::Ok,
                    2 => { let mut o = 0; IpcResponse::Error(str_decode(&payload, &mut o)) }
                    3 => {
                        let mut o = 0;
                        let raw = str_decode(&payload, &mut o);
                        let v = serde_json::from_str(&raw).unwrap_or(serde_json::Value::String(raw));
                        IpcResponse::Value(v)
                    }
                    4 => {
                        let vid = if payload.len() >= 8 {
                            u64::from_le_bytes([payload[0],payload[1],payload[2],payload[3],
                                               payload[4],payload[5],payload[6],payload[7]])
                        } else { 0 };
                        IpcResponse::ViewCreated(vid)
                    }
                    5 => {
                        let cnt = if payload.len() >= 4 {
                            u32::from_le_bytes([payload[0],payload[1],payload[2],payload[3]]) as usize
                        } else { 0 };
                        let mut ids = Vec::with_capacity(cnt);
                        for i in 0..cnt {
                            let o = 4 + i * 8;
                            if o + 8 <= payload.len() {
                                ids.push(u64::from_le_bytes([
                                    payload[o],payload[o+1],payload[o+2],payload[o+3],
                                    payload[o+4],payload[o+5],payload[o+6],payload[o+7]]));
                            }
                        }
                        IpcResponse::ViewList(ids)
                    }
                    6 => IpcResponse::Binary(payload),
                    7 => {
                        // REP_SHM_SCREENSHOT: payload = u32 nameLen + name + u32 pngLen
                        // Open shared memory, read PNG bytes, unlink. Zero-copy from child.
                        let mut o = 0;
                        let shm_name = str_decode(&payload, &mut o);
                        let png_len = if o + 4 <= payload.len() {
                            u32::from_le_bytes([payload[o],payload[o+1],payload[o+2],payload[o+3]]) as usize
                        } else { 0 };

                        let bytes = (|| -> Result<Vec<u8>, String> {
                            use std::ffi::CString;
                            let c_name = CString::new(shm_name.as_bytes())
                                .map_err(|e| format!("CString: {e}"))?;
                            let fd = unsafe { libc::shm_open(c_name.as_ptr(), libc::O_RDONLY, 0) };
                            if fd < 0 { return Err("shm_open failed".into()); }
                            let map = unsafe {
                                libc::mmap(std::ptr::null_mut(), png_len, libc::PROT_READ,
                                           libc::MAP_SHARED, fd, 0)
                            };
                            unsafe { libc::close(fd); }
                            if map == libc::MAP_FAILED {
                                unsafe { libc::shm_unlink(c_name.as_ptr()); }
                                return Err("mmap failed".into());
                            }
                            let data = unsafe {
                                std::slice::from_raw_parts(map as *const u8, png_len)
                            }.to_vec();
                            unsafe {
                                libc::munmap(map, png_len);
                                libc::shm_unlink(c_name.as_ptr());
                            }
                            Ok(data)
                        })();

                        match bytes {
                            Ok(data) => IpcResponse::Binary(data),
                            Err(e) => IpcResponse::Error(e),
                        }
                    }
                    8 => {
                        // REP_CONSOLE_EVENT — unsolicited, pushed by WKScriptMessageHandler.
                        let mut o = 0;
                        let level = str_decode(&payload, &mut o);
                        let text = str_decode(&payload, &mut o);
                        console_log2.lock().unwrap().push((level, text));
                        continue;
                    }
                    9 => {
                        // REP_DIALOG_EVENT — unsolicited dialog event.
                        let mut o = 0;
                        let dtype = str_decode(&payload, &mut o);
                        let message = str_decode(&payload, &mut o);
                        let action = str_decode(&payload, &mut o);
                        dialog_log2.lock().unwrap().push((dtype, message, action));
                        continue;
                    }
                    10 => {
                        // REP_NET_EVENT — unsolicited network event.
                        let mut o = 0;
                        let id = str_decode(&payload, &mut o);
                        let method = str_decode(&payload, &mut o);
                        let url = str_decode(&payload, &mut o);
                        let res_type = str_decode(&payload, &mut o);
                        network_log2.lock().unwrap().push((id, method, url, res_type));
                        continue;
                    }
                    _ => IpcResponse::Error(format!("unknown rep {rep}")),
                };

                if let Some(tx) = pending2.lock().unwrap().remove(&rid) {
                    let _ = tx.send(resp);
                }
            }
        });

        let client = Self { writer: StdMutex::new(write_sock), pending, next_id: AtomicU64::new(1), console_log, dialog_log, network_log };

        // Probe subprocess readiness — send ListViews, retry until it responds.
        for _ in 0..200 {
            match tokio::time::timeout(
                std::time::Duration::from_millis(50),
                client.send_empty(Op::ListViews),
            ).await {
                Ok(Ok(_)) => return Ok((client, child)),
                _ => tokio::time::sleep(std::time::Duration::from_millis(5)).await,
            }
        }
        Ok((client, child))
    }

    pub async fn send(&self, op: Op, payload: &[u8]) -> Result<IpcResponse, String> {
        let rid = self.next_id.fetch_add(1, Ordering::Relaxed) as u32;
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(rid as u64, tx);
        { let mut w = self.writer.lock().unwrap(); frame_write(&mut *w, rid, op as u8, payload); }
        match tokio::time::timeout(std::time::Duration::from_secs(30), rx).await {
            Ok(Ok(r)) => Ok(r),
            Ok(Err(_)) => Err("dropped".into()),
            Err(_) => { self.pending.lock().unwrap().remove(&(rid as u64)); Err("timeout".into()) }
        }
    }

    pub async fn send_str(&self, op: Op, s: &str) -> Result<IpcResponse, String> {
        let mut p = Vec::new(); str_encode(&mut p, s); self.send(op, &p).await
    }

    pub async fn send_str_vid(&self, op: Op, s: &str, vid: u64) -> Result<IpcResponse, String> {
        let mut p = Vec::new();
        str_encode(&mut p, s);
        p.extend_from_slice(&vid.to_le_bytes());
        self.send(op, &p).await
    }

    pub async fn send_vid(&self, op: Op, vid: u64) -> Result<IpcResponse, String> {
        self.send(op, &vid.to_le_bytes()).await
    }

    pub async fn send_empty(&self, op: Op) -> Result<IpcResponse, String> {
        self.send(op, &[]).await
    }
}

