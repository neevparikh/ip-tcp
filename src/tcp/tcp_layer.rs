use std::net::Ipv4Addr;
use std::sync::mpsc::Sender;
use std::sync::{Arc, RwLock};

use super::socket::{SocketId, SocketSide};
use super::tcp_stream::TcpStream;
use super::{Port, TcpPacket};
use crate::{HandlerFunction, IpSendMsg};

type StreamMap = Arc<RwLock<Vec<(TcpStream, Sender<TcpPacket>)>>>;

pub struct TcpLayer {
  ip_send_tx: Sender<IpSendMsg>,

  /// TODO: this should probably be a map but idk what the keys should be
  streams: StreamMap,
}

impl TcpLayer {
  pub fn new(ip_send_tx: Sender<IpSendMsg>) -> TcpLayer {
    TcpLayer {
      ip_send_tx,
      streams: vec![],
    }
  }

  pub fn get_tcp_handler(&self) -> HandlerFunction {
    let ip_send_tx = self.ip_send_tx.clone();

    Box::new(move |ip_packet| {})
  }

  pub fn print_sockets() {
    todo!()
  }

  pub fn print_window(&self, socket: SocketId) {
    todo!()
  }

  pub fn accept(&self, port: Port) {
    let source_port = 1024;
    let stream = TcpStream::new_listener(port, self.ip_send_tx.clone());
    self.streams.write().unwrap().push(stream);
  }

  pub fn connect(&self, ip: Ipv4Addr, port: Port) {
    // TODO: should keep track of availible ports and assign source port from there
    let source_port = 1024;
    let stream = TcpStream::new_connect(source_port, ip, port, self.ip_send_tx.clone());
    self.streams.write().unwrap().push(stream);
  }

  pub fn send(&self, socket_id: SocketId, data: Vec<u8>) {
    todo!()
  }

  pub fn recv(&self, socket_id: SocketId, numbytes: usize, should_block: bool) {
    todo!()
  }

  pub fn shutdown(&self, socket_id: SocketId, shutdown_method: SocketSide) {
    todo!()
  }

  pub fn close(&self, socket_id: SocketId) {
    todo!()
  }

  pub fn send_file(&self) {
    todo!()
  }

  pub fn recv_file(&self) {
    todo!()
  }
}
