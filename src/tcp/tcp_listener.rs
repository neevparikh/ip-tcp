use std::sync::Arc;

use anyhow::{anyhow, Result};

use super::tcp_layer::TcpLayerInfo;
use super::tcp_stream::TcpStream;
use super::{Port, TcpStreamState};

pub struct TcpListener {
  port: Port,
  info: TcpLayerInfo,
}

impl TcpListener {
  pub fn bind(source_port: Port, info: TcpLayerInfo) -> Result<TcpListener> {
    let socket_port = info.socket_port.lock().unwrap();
    if socket_port.contains_port(source_port) {
      Err(anyhow!("Error: port {source_port} in use"))
    } else {
      drop(socket_port);
      Ok(TcpListener {
        port: source_port,
        info,
      })
    }
  }

  pub fn accept(&mut self) -> TcpStream {
    self.spawn()
  }

  fn spawn(&self) -> TcpStream {
    let mut socket_port = self.info.socket_port.lock().unwrap();
    let new_socket = socket_port.get_new_socket();
    let stream = TcpStream::new(
      None,
      self.port,
      None,
      None,
      TcpStreamState::Listen,
      self.info.ip_send_tx.clone(),
      self.info.make_cleanup_callback(new_socket),
    );

    self
      .info
      .streams
      .write()
      .unwrap()
      .insert(new_socket, stream.clone());
    socket_port.add_port(self.port);
    stream
  }
}
