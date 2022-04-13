use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::mpsc::Sender;

use anyhow::Result;

use super::tcp_stream::{LockedTcpStream, TcpStream};
use super::{Port, TcpPacket};
use crate::IpSendMsg;

pub struct TcpListener {
  stream:       LockedTcpStream,
  port:         Port,
  ip_send_tx:   Sender<IpSendMsg>,
  _tcp_send_tx: Sender<TcpPacket>,
}

impl TcpListener {
  pub fn bind(port: Port, ip_send_tx: Sender<IpSendMsg>) -> TcpListener {
    // TODO return a Result<> and don't accept when already bound
    let (stream, _tcp_send_tx) = TcpStream::listen(port, ip_send_tx.clone());
    TcpListener {
      stream,
      port,
      ip_send_tx,
      _tcp_send_tx,
    }
  }

  pub fn accept(&self) -> Result<(TcpStream, SocketAddr)> {
    // TODO still WIP
    todo!();
  }
}
