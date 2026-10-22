//! A loopback SOCKS5 relay that authenticates to the proxy a browser cannot.
//!
//! Chromium, Firefox and `WebKit` speak SOCKS5 without the username/password
//! method (RFC 1929), which is why Playwright refuses a `socks5://` proxy with
//! credentials. The relay closes that gap: the browser is pointed at it with
//! no credentials, and it opens each connection to the real proxy with them,
//! passing the request through untouched. Names stay unresolved on this side,
//! exactly as the browser sent them, so the proxy still resolves them.
//!
//! It listens on `127.0.0.1` only. Anything on this machine that finds the
//! port can use it, the same exposure Playwright accepts for the loopback
//! proxy behind its client certificates.

use std::net::SocketAddr;
use std::sync::Arc;

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};

use crate::error::{FerriError, Result};
use crate::options::ProxyConfig;

/// A running relay. Stops when dropped.
#[derive(Debug)]
pub struct SocksRelay {
  addr: SocketAddr,
  accept: tokio::task::AbortHandle,
}

impl Drop for SocksRelay {
  fn drop(&mut self) {
    self.accept.abort();
  }
}

/// Where the relay forwards to and how it authenticates there.
struct Upstream {
  address: String,
  username: Vec<u8>,
  password: Vec<u8>,
}

impl SocksRelay {
  /// Start a relay in front of `upstream`, an authenticated SOCKS5 proxy.
  ///
  /// # Errors
  ///
  /// The proxy is not SOCKS5, its credentials do not fit the protocol
  /// (255 bytes each), or no loopback port can be bound.
  pub async fn start(upstream: &ProxyConfig) -> Result<Arc<Self>> {
    let invalid = |message: String| FerriError::invalid_argument("proxy", message);
    let (scheme, rest) = upstream
      .server
      .split_once("://")
      .ok_or_else(|| invalid(format!("{:?} names no scheme", upstream.server)))?;
    if !matches!(scheme, "socks5" | "socks5h") {
      return Err(invalid(format!(
        "only a socks5 proxy can be relayed with credentials, not {scheme}"
      )));
    }
    let credentials = upstream
      .credentials()
      .ok_or_else(|| invalid("a relay is only needed for a proxy with credentials".into()))?;
    let address = rest.trim_end_matches('/').to_string();
    let address = if address
      .rsplit_once(':')
      .is_some_and(|(_, port)| port.parse::<u16>().is_ok())
    {
      address
    } else {
      format!("{address}:1080")
    };
    let upstream = Arc::new(Upstream {
      address,
      username: credentials.username.into_bytes(),
      password: credentials.password.into_bytes(),
    });
    if upstream.username.len() > 255 || upstream.password.len() > 255 {
      return Err(invalid("socks5 credentials are limited to 255 bytes each".into()));
    }

    let listener = TcpListener::bind(("127.0.0.1", 0))
      .await
      .map_err(|e| FerriError::backend(format!("socks relay: {e}")))?;
    let addr = listener
      .local_addr()
      .map_err(|e| FerriError::backend(format!("socks relay: {e}")))?;
    let accept = tokio::spawn(async move {
      while let Ok((client, _)) = listener.accept().await {
        let upstream = Arc::clone(&upstream);
        tokio::spawn(async move {
          if let Err(error) = relay(client, &upstream).await {
            tracing::debug!(target: "ferridriver::socks_relay", %error, "relayed connection failed");
          }
        });
      }
    })
    .abort_handle();
    Ok(Arc::new(Self { addr, accept }))
  }

  /// The proxy to give the browser in place of the one relayed: the
  /// relay's address, no credentials, the same bypass list.
  #[must_use]
  pub fn browser_proxy(&self, original: &ProxyConfig) -> ProxyConfig {
    ProxyConfig {
      server: format!("socks5://{}", self.addr),
      bypass: original.bypass.clone(),
      username: None,
      password: None,
    }
  }
}

const VERSION: u8 = 0x05;
const NO_AUTHENTICATION: u8 = 0x00;
const USERNAME_PASSWORD: u8 = 0x02;
const NO_ACCEPTABLE_METHOD: u8 = 0xff;
const GENERAL_FAILURE: u8 = 0x01;

async fn relay(mut client: TcpStream, upstream: &Upstream) -> std::io::Result<()> {
  // The browser's greeting: it offers methods, and it gets "none".
  let mut greeting = [0u8; 2];
  client.read_exact(&mut greeting).await?;
  if greeting[0] != VERSION {
    return Err(protocol("not a SOCKS5 client"));
  }
  let mut methods = vec![0u8; usize::from(greeting[1])];
  client.read_exact(&mut methods).await?;
  if !methods.contains(&NO_AUTHENTICATION) {
    client.write_all(&[VERSION, NO_ACCEPTABLE_METHOD]).await?;
    return Err(protocol("the client offered no method the relay speaks"));
  }
  client.write_all(&[VERSION, NO_AUTHENTICATION]).await?;

  // The request, kept byte for byte to hand on: VER CMD RSV ATYP ADDR PORT.
  let mut request = vec![0u8; 4];
  client.read_exact(&mut request).await?;
  let address_length = match request[3] {
    0x01 => 4,
    0x04 => 16,
    0x03 => {
      let mut length = [0u8; 1];
      client.read_exact(&mut length).await?;
      request.push(length[0]);
      usize::from(length[0])
    },
    _ => return Err(protocol("unknown address type")),
  };
  let start = request.len();
  request.resize(start + address_length + 2, 0);
  client.read_exact(&mut request[start..]).await?;

  let mut server = match open_upstream(upstream).await {
    Ok(server) => server,
    Err(error) => {
      let _ = client
        .write_all(&[VERSION, GENERAL_FAILURE, 0, 0x01, 0, 0, 0, 0, 0, 0])
        .await;
      return Err(error);
    },
  };
  server.write_all(&request).await?;

  // The proxy's reply, handed back whole: VER REP RSV ATYP BND.ADDR BND.PORT.
  let mut reply = vec![0u8; 4];
  server.read_exact(&mut reply).await?;
  let bound_length = match reply[3] {
    0x01 => 4,
    0x04 => 16,
    0x03 => {
      let mut length = [0u8; 1];
      server.read_exact(&mut length).await?;
      reply.push(length[0]);
      usize::from(length[0])
    },
    _ => return Err(protocol("the proxy replied with an unknown address type")),
  };
  let start = reply.len();
  reply.resize(start + bound_length + 2, 0);
  server.read_exact(&mut reply[start..]).await?;
  client.write_all(&reply).await?;
  if reply[1] != 0 {
    return Ok(());
  }
  tokio::io::copy_bidirectional(&mut client, &mut server).await?;
  Ok(())
}

/// Connect to the proxy and authenticate with RFC 1929.
async fn open_upstream(upstream: &Upstream) -> std::io::Result<TcpStream> {
  let mut server = TcpStream::connect(&upstream.address).await?;
  server.write_all(&[VERSION, 1, USERNAME_PASSWORD]).await?;
  let mut chosen = [0u8; 2];
  server.read_exact(&mut chosen).await?;
  if chosen != [VERSION, USERNAME_PASSWORD] {
    return Err(protocol("the proxy refused username/password authentication"));
  }
  let mut login = Vec::with_capacity(3 + upstream.username.len() + upstream.password.len());
  login.push(0x01);
  login.push(u8::try_from(upstream.username.len()).map_err(|_| protocol("username too long"))?);
  login.extend_from_slice(&upstream.username);
  login.push(u8::try_from(upstream.password.len()).map_err(|_| protocol("password too long"))?);
  login.extend_from_slice(&upstream.password);
  server.write_all(&login).await?;
  let mut status = [0u8; 2];
  server.read_exact(&mut status).await?;
  if status[1] != 0 {
    return Err(protocol("the proxy refused the credentials"));
  }
  Ok(server)
}

fn protocol(message: &str) -> std::io::Error {
  std::io::Error::new(std::io::ErrorKind::InvalidData, message.to_string())
}

#[cfg(test)]
mod tests {
  use super::SocksRelay;
  use crate::options::ProxyConfig;
  use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

  /// A SOCKS5 proxy accepting only `user`/`secret`, answering every CONNECT
  /// by echoing back the host name it was asked for.
  async fn authenticated_proxy() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
      while let Ok((mut socket, _)) = listener.accept().await {
        tokio::spawn(async move {
          let mut greeting = [0u8; 3];
          socket.read_exact(&mut greeting).await.unwrap();
          assert_eq!(greeting, [5, 1, 2], "the relay offers username/password only");
          socket.write_all(&[5, 2]).await.unwrap();
          let mut header = [0u8; 2];
          socket.read_exact(&mut header).await.unwrap();
          let mut user = vec![0u8; usize::from(header[1])];
          socket.read_exact(&mut user).await.unwrap();
          let mut length = [0u8; 1];
          socket.read_exact(&mut length).await.unwrap();
          let mut password = vec![0u8; usize::from(length[0])];
          socket.read_exact(&mut password).await.unwrap();
          let accepted = user == b"user" && password == b"secret";
          socket.write_all(&[1, u8::from(!accepted)]).await.unwrap();
          if !accepted {
            return;
          }
          let mut request = [0u8; 5];
          socket.read_exact(&mut request).await.unwrap();
          assert_eq!(request[3], 3, "the name reaches the proxy unresolved");
          let mut name = vec![0u8; usize::from(request[4])];
          socket.read_exact(&mut name).await.unwrap();
          let mut port = [0u8; 2];
          socket.read_exact(&mut port).await.unwrap();
          socket.write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 80]).await.unwrap();
          socket.write_all(&name).await.unwrap();
        });
      }
    });
    port
  }

  async fn connect_through(relay: &SocksRelay, host: &str) -> Vec<u8> {
    let mut socket = tokio::net::TcpStream::connect(relay.addr).await.unwrap();
    socket.write_all(&[5, 1, 0]).await.unwrap();
    let mut chosen = [0u8; 2];
    socket.read_exact(&mut chosen).await.unwrap();
    assert_eq!(chosen, [5, 0], "the browser is asked for no credentials");
    let mut request = vec![5, 1, 0, 3, u8::try_from(host.len()).unwrap()];
    request.extend_from_slice(host.as_bytes());
    request.extend_from_slice(&443u16.to_be_bytes());
    socket.write_all(&request).await.unwrap();
    let mut reply = [0u8; 10];
    socket.read_exact(&mut reply).await.unwrap();
    let mut body = Vec::new();
    if reply[1] == 0 {
      socket.read_to_end(&mut body).await.unwrap();
    }
    body
  }

  #[tokio::test]
  async fn the_relay_authenticates_for_a_client_that_cannot() {
    let port = authenticated_proxy().await;
    let relay = SocksRelay::start(&ProxyConfig {
      server: format!("socks5://127.0.0.1:{port}"),
      bypass: Some("localhost".into()),
      username: Some("user".into()),
      password: Some("secret".into()),
    })
    .await
    .unwrap();

    assert_eq!(
      connect_through(&relay, "internal.example.com").await,
      b"internal.example.com"
    );
    let browser = relay.browser_proxy(&ProxyConfig {
      server: String::new(),
      bypass: Some("localhost".into()),
      username: Some("user".into()),
      password: Some("secret".into()),
    });
    assert!(browser.server.starts_with("socks5://127.0.0.1:"));
    assert_eq!(browser.credentials(), None);
    assert_eq!(browser.bypass.as_deref(), Some("localhost"));
  }

  #[tokio::test]
  async fn refused_credentials_fail_the_connection_rather_than_hang() {
    let port = authenticated_proxy().await;
    let relay = SocksRelay::start(&ProxyConfig {
      server: format!("socks5://127.0.0.1:{port}"),
      bypass: None,
      username: Some("user".into()),
      password: Some("wrong".into()),
    })
    .await
    .unwrap();

    assert_eq!(connect_through(&relay, "internal.example.com").await, b"");
  }
}
