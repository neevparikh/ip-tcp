use std::sync::mpsc::Receiver;
use std::sync::{mpsc, Arc};

use anyhow::{anyhow, Result};

use super::tcp_layer::TcpLayerInfo;
use super::tcp_stream::TcpStream;
use super::{Port, SocketId};

pub struct TcpListener {
  info:      TcpLayerInfo,
  socket_id: SocketId,
  conn_rx:   Receiver<Arc<TcpStream>>,
}

impl TcpListener {
  pub fn bind(src_port: Port, info: TcpLayerInfo) -> Result<TcpListener> {
    let stream = TcpStream::listen(src_port, info.clone())?;
    let socket_id = stream.socket_id();
    let mut queues = info.queued_streams.lock().unwrap();
    let (conn_tx, conn_rx) = mpsc::channel();
    queues.insert(socket_id, (conn_tx, info.get_our_ip_addrs()));
    drop(queues);
    Ok(TcpListener {
      info,
      socket_id,
      conn_rx,
    })
  }

  pub fn accept(&mut self) -> Result<Arc<TcpStream>> {
    match self.conn_rx.recv() {
      Ok(stream) => {
        let socket_id = stream.socket_id();
        self
          .info
          .streams
          .write()
          .unwrap()
          .insert(socket_id, stream.clone());
        Ok(stream)
      }
      Err(e) => Err(anyhow!(
        "Got error {e}, during accept for {}, exiting...",
        self.socket_id
      )),
    }
  }
}
