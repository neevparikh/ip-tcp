use std::net::{Ipv4Addr, SocketAddr};

use super::socket::SocketId;
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

  pub fn send() {
    todo!()
  }

  pub fn recv() {
    todo!()
  }

  pub fn shutdown() {
    todo!()
  }

  pub fn close() {
    todo!()
  }

  pub fn send_file() {
    todo!()
  }

  pub fn recv_file() {
    todo!()
  }
}
