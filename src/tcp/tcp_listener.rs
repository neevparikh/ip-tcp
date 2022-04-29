use std::sync::Arc;

use anyhow::{anyhow, Result};

use super::tcp_layer::TcpLayerInfo;
use super::tcp_stream::TcpStream;
use super::{Port, TcpStreamState};

pub struct TcpListener {
  port:   Port,
  info:   TcpLayerInfo,
  stream: Arc<TcpStream>,
}

impl TcpListener {
  pub fn bind(src_port: Port, info: TcpLayerInfo) -> Result<TcpListener> {
    let sockets_and_ports = info.sockets_and_ports.lock().unwrap();
    if sockets_and_ports.contains_port(src_port) {
      Err(anyhow!("Error: port {src_port} in use"))
    } else {
      let stream = TcpListener::create_stream(&info, src_port, TcpStreamState::Listen);
      drop(sockets_and_ports);
      Ok(TcpListener {
        port: src_port,
        info,
        stream,
      })
    }
  }

  pub fn accept(&mut self) -> Arc<TcpStream> {
    self.spawn()
  }

  fn spawn(&mut self) -> Arc<TcpStream> {
    TcpListener::create_stream(&self.info, self.port, TcpStreamState::SynReceived)
  }

  fn create_stream(info: &TcpLayerInfo, port: Port, state: TcpStreamState) -> Arc<TcpStream> {
    let mut sockets_and_ports = info.sockets_and_ports.lock().unwrap();
    let new_socket = sockets_and_ports.get_new_socket();
    let stream = Arc::new(TcpStream::new(
      None,
      port,
      None,
      None,
      state,
      info.ip_send_tx.clone(),
      info.make_cleanup_callback(new_socket),
    ));

    info
      .streams
      .write()
      .unwrap()
      .insert(new_socket, stream.clone());
    sockets_and_ports.add_port(port);
    stream
  }
}
