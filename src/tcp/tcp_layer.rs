use std::net::Ipv4Addr;

use super::socket::{SocketId, SocketSide};
use super::Port;

pub struct TcpLayer {}

impl TcpLayer {
  pub fn new() -> TcpLayer {
    TcpLayer {}
  }

  pub fn print_sockets() {
    todo!()
  }

  pub fn print_window(socket: SocketId) {
    todo!()
  }

  pub fn accept(port: Port) {
    todo!()
  }

  pub fn connect(ip: Ipv4Addr, port: Port) {
    todo!()
  }

  pub fn send(socket_id: SocketId, data: Vec<u8>) {
    todo!()
  }

  pub fn recv(socket_id: SocketId, numbytes: usize, should_block: bool) {
    todo!()
  }

  pub fn shutdown(socket_id: SocketId, shutdown_method: SocketSide) {
    todo!()
  }

  pub fn close(socket_id: SocketId) {
    todo!()
  }

  pub fn send_file() {
    todo!()
  }

  pub fn recv_file() {
    todo!()
  }
}
