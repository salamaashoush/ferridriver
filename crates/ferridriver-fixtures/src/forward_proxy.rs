//! Two proxies that forward for real and want credentials: an HTTP proxy
//! (absolute-form requests and `CONNECT`, `Proxy-Authorization: Basic`) and a
//! SOCKS5 proxy (username/password, RFC 1929).
//!
//! Every host under `.test` resolves to loopback here and nowhere else, so a
//! page or request that reaches `http://<key>.proxied.test:<port>/` proves it
//! went through one of these: the name means nothing to the machine's own
//! resolver. Each connection is logged with its kind and target, the key in
//! the host name keeping concurrent tests' entries apart.

use std::sync::{Arc, Mutex};

use base64::Engine as _;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};

pub const PROXY_USER: &str = "fixture-user";
pub const PROXY_PASSWORD: &str = "fixture-pass";

#[derive(Default)]
pub struct ForwardLog {
  lines: Mutex<Vec<String>>,
}

impl ForwardLog {
  fn push(&self, line: String) {
    self
      .lines
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .push(line);
  }

  /// The entries naming `key`, removing them when `take` is set.
  pub fn matching(&self, key: &str, take: bool) -> Vec<String> {
    let mut lines = self.lines.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let found: Vec<String> = lines.iter().filter(|line| line.contains(key)).cloned().collect();
    if take {
      lines.retain(|line| !line.contains(key));
    }
    found
  }
}

/// Where a target host is reached: `.test` names at loopback, loopback as
/// itself, anything else nowhere (a test must not reach the internet).
fn resolve(host: &str) -> Option<&str> {
  let bare = host.trim_start_matches('[').trim_end_matches(']');
  if bare
    .rsplit_once('.')
    .is_some_and(|(_, tld)| tld.eq_ignore_ascii_case("test"))
  {
    return Some("127.0.0.1");
  }
  matches!(bare, "127.0.0.1" | "localhost" | "::1").then_some(bare)
}

async fn read_head(socket: &mut TcpStream) -> Option<String> {
  let mut head = Vec::new();
  let mut byte = [0u8; 1];
  while !head.ends_with(b"\r\n\r\n") {
    if socket.read(&mut byte).await.ok()? == 0 || head.len() > 64 * 1024 {
      return None;
    }
    head.push(byte[0]);
  }
  Some(String::from_utf8_lossy(&head).into_owned())
}

fn header<'a>(head: &'a str, name: &str) -> Option<&'a str> {
  head.lines().find_map(|line| {
    let (key, value) = line.split_once(':')?;
    key.trim().eq_ignore_ascii_case(name).then(|| value.trim())
  })
}

fn expected_authorization() -> String {
  let pair = format!("{PROXY_USER}:{PROXY_PASSWORD}");
  format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(pair))
}

pub async fn run_http(listener: TcpListener, log: Arc<ForwardLog>) {
  while let Ok((socket, _)) = listener.accept().await {
    let log = Arc::clone(&log);
    tokio::spawn(async move {
      let _ = http_connection(socket, &log).await;
    });
  }
}

async fn http_connection(mut socket: TcpStream, log: &ForwardLog) -> std::io::Result<()> {
  let Some(head) = read_head(&mut socket).await else {
    return Ok(());
  };
  if header(&head, "proxy-authorization") != Some(expected_authorization().as_str()) {
    socket
      .write_all(
        b"HTTP/1.1 407 Proxy Authentication Required\r\nProxy-Authenticate: Basic realm=\"fixtures\"\r\n\
          Content-Length: 0\r\nConnection: close\r\n\r\n",
      )
      .await?;
    return Ok(());
  }
  let mut words = head.split_whitespace();
  let method = words.next().unwrap_or_default().to_string();
  let target = words.next().unwrap_or_default().to_string();

  if method.eq_ignore_ascii_case("CONNECT") {
    let (host, port) = target.rsplit_once(':').unwrap_or((&target, "443"));
    let Some(address) = resolve(host) else {
      socket
        .write_all(b"HTTP/1.1 502 Bad Gateway\r\nConnection: close\r\n\r\n")
        .await?;
      return Ok(());
    };
    log.push(format!("http CONNECT {target}"));
    let mut upstream = TcpStream::connect((address, port.parse::<u16>().unwrap_or(443))).await?;
    socket.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n").await?;
    tokio::io::copy_bidirectional(&mut socket, &mut upstream).await?;
    return Ok(());
  }

  let Ok(url) = parse_absolute(&target) else {
    socket
      .write_all(b"HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\n")
      .await?;
    return Ok(());
  };
  let Some(address) = resolve(&url.host) else {
    socket
      .write_all(b"HTTP/1.1 502 Bad Gateway\r\nConnection: close\r\n\r\n")
      .await?;
    return Ok(());
  };
  log.push(format!("http {method} {target}"));
  let mut upstream = TcpStream::connect((address, url.port)).await?;
  // Origin-form for the server, and one request per connection: a browser
  // keeps a proxy connection alive across hosts, which a byte pipe to one
  // upstream cannot honour.
  let mut forwarded = format!("{method} {} HTTP/1.1\r\n", url.path_and_query);
  for line in head.lines().skip(1) {
    let name = line.split_once(':').map_or("", |(name, _)| name.trim());
    if line.is_empty()
      || name.eq_ignore_ascii_case("proxy-authorization")
      || name.eq_ignore_ascii_case("proxy-connection")
      || name.eq_ignore_ascii_case("connection")
      || name.eq_ignore_ascii_case("keep-alive")
    {
      continue;
    }
    forwarded.push_str(line);
    forwarded.push_str("\r\n");
  }
  forwarded.push_str("Connection: close\r\n\r\n");
  upstream.write_all(forwarded.as_bytes()).await?;
  // The server closes after its response (`Connection: close` above), which
  // ends the pipe once the client hangs up in turn.
  tokio::io::copy_bidirectional(&mut socket, &mut upstream).await?;
  Ok(())
}

struct Target {
  host: String,
  port: u16,
  path_and_query: String,
}

/// Split an absolute-form `http://host[:port]/path?query` target.
fn parse_absolute(target: &str) -> Result<Target, ()> {
  let rest = target.strip_prefix("http://").ok_or(())?;
  let (authority, path) = rest.find('/').map_or((rest, "/"), |i| (&rest[..i], &rest[i..]));
  let (host, port) = match authority.rsplit_once(':') {
    Some((host, port)) if !host.ends_with(']') || authority.starts_with('[') => {
      (host.to_string(), port.parse::<u16>().map_err(|_| ())?)
    },
    _ => (authority.to_string(), 80),
  };
  Ok(Target {
    host,
    port,
    path_and_query: path.to_string(),
  })
}

pub async fn run_socks5(listener: TcpListener, log: Arc<ForwardLog>) {
  while let Ok((socket, _)) = listener.accept().await {
    let log = Arc::clone(&log);
    tokio::spawn(async move {
      let _ = socks5_connection(socket, &log).await;
    });
  }
}

async fn socks5_connection(mut socket: TcpStream, log: &ForwardLog) -> std::io::Result<()> {
  let mut greeting = [0u8; 2];
  socket.read_exact(&mut greeting).await?;
  let mut methods = vec![0u8; usize::from(greeting[1])];
  socket.read_exact(&mut methods).await?;
  if greeting[0] != 5 || !methods.contains(&0x02) {
    socket.write_all(&[5, 0xff]).await?;
    return Ok(());
  }
  socket.write_all(&[5, 0x02]).await?;
  let mut header = [0u8; 2];
  socket.read_exact(&mut header).await?;
  let mut user = vec![0u8; usize::from(header[1])];
  socket.read_exact(&mut user).await?;
  let mut length = [0u8; 1];
  socket.read_exact(&mut length).await?;
  let mut password = vec![0u8; usize::from(length[0])];
  socket.read_exact(&mut password).await?;
  if user != PROXY_USER.as_bytes() || password != PROXY_PASSWORD.as_bytes() {
    socket.write_all(&[1, 1]).await?;
    return Ok(());
  }
  socket.write_all(&[1, 0]).await?;

  let mut request = [0u8; 4];
  socket.read_exact(&mut request).await?;
  let host = match request[3] {
    0x03 => {
      let mut length = [0u8; 1];
      socket.read_exact(&mut length).await?;
      let mut name = vec![0u8; usize::from(length[0])];
      socket.read_exact(&mut name).await?;
      String::from_utf8_lossy(&name).into_owned()
    },
    0x01 => {
      let mut octets = [0u8; 4];
      socket.read_exact(&mut octets).await?;
      std::net::Ipv4Addr::from(octets).to_string()
    },
    0x04 => {
      let mut octets = [0u8; 16];
      socket.read_exact(&mut octets).await?;
      std::net::Ipv6Addr::from(octets).to_string()
    },
    _ => return Ok(()),
  };
  let mut port = [0u8; 2];
  socket.read_exact(&mut port).await?;
  let port = u16::from_be_bytes(port);
  let Some(address) = resolve(&host) else {
    socket.write_all(&[5, 0x04, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
    return Ok(());
  };
  log.push(format!("socks {host}:{port}"));
  let mut upstream = TcpStream::connect((address, port)).await?;
  socket.write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0]).await?;
  tokio::io::copy_bidirectional(&mut socket, &mut upstream).await?;
  Ok(())
}
