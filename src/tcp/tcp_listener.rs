use std::sync::Arc;

use super::tcp_layer::TcpLayerInfo;
use super::tcp_stream::TcpStream;
use super::{Port, TcpStreamState};

pub struct TcpListener {
  port: Port,
  info: TcpLayerInfo,
}

impl TcpListener {
  pub fn bind(source_port: Port, info: TcpLayerInfo) -> TcpListener {
    TcpListener {
      port: source_port,
      info,
    }
  }

  pub fn accept(&mut self, port: Port) {
    let mut socket_port = self.info.socket_port.lock().unwrap();
    if socket_port.contains_port(port) {
      eprintln!("Error: port {port} in use");
      return;
    }
    let new_socket = socket_port.get_new_socket();
    let stream = self.spawn(port, self.info.make_cleanup_callback(new_socket));
    self
      .info
      .streams
      .write()
      .unwrap()
      .insert(new_socket, Arc::new(stream));
    socket_port.add_port(port);
  }

  fn spawn(&self, source_port: Port, cleanup: Box<dyn Fn() + Send>) -> TcpStream {
    TcpStream::new(
      None,
      source_port,
      None,
      None,
      TcpStreamState::Listen,
      self.info.ip_send_tx.clone(),
      cleanup,
    )
  }
}
