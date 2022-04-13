use std::net::SocketAddr;
use std::sync::mpsc::Sender;

use anyhow::Result;

use super::tcp_stream::{LockedTcpStream, TcpStream};
use super::Port;
use crate::IpPacket;

pub struct TcpListener {
  stream:     LockedTcpStream,
  port:       Port,
  ip_send_tx: Sender<IpPacket>,
}

impl TcpListener {
  pub fn bind(port: Port, ip_send_tx: Sender<IpPacket>) -> TcpListener {
    // TODO return a Result<> and don't accept when already bound
    let stream = TcpStream::listen(port, ip_send_tx.clone());
    TcpListener {
      stream,
      port,
      ip_send_tx,
    }
  }

  pub fn accept(&self) -> Result<(TcpStream, SocketAddr)> {
    // TODO still WIP
    todo!();
  }
}
