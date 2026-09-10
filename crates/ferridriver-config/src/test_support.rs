use std::net::{Ipv4Addr, SocketAddrV4};

pub fn dead_endpoint() -> std::io::Result<(socket2::Socket, u16)> {
  let socket = socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::STREAM, None)?;
  // Binding without listening rejects connections while preventing another process from taking the port.
  socket.bind(&SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0).into())?;
  let port = socket
    .local_addr()?
    .as_socket_ipv4()
    .ok_or_else(|| std::io::Error::other("not IPv4"))?
    .port();
  Ok((socket, port))
}
