use std::net::Ipv4Addr;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, RwLock};
use std::thread;

use anyhow::{anyhow, Result};
use etherparse::{Ipv4Header, TcpHeader};

use super::socket::{SocketId, SocketSide};
use super::tcp_stream::TcpStream;
use super::{IpTcpPacket, Port};
use crate::{debug, edebug, HandlerFunction, IpPacket};

type StreamMap = Arc<RwLock<Vec<Arc<TcpStream>>>>;

#[derive(Debug)]
pub struct TcpLayer {
  ip_send_tx:  Sender<IpPacket>,
  tcp_recv_tx: Sender<IpTcpPacket>,

  /// TODO: this should probably be a map but idk what the keys should be
  streams: StreamMap,
}

impl TcpLayer {
  pub fn new(ip_send_tx: Sender<IpPacket>) -> TcpLayer {
    let (tcp_recv_tx, tcp_recv_rx) = channel();
    let tcp_layer = TcpLayer {
      ip_send_tx,
      tcp_recv_tx,
      streams: Arc::new(RwLock::new(Vec::new())),
    };
    tcp_layer.start_stream_dispatcher(tcp_recv_rx);
    tcp_layer
  }

  /// Starts thread that handles all incoming TcpPackets and routes them appropriately
  pub fn start_stream_dispatcher(&self, tcp_recv_rx: Receiver<IpTcpPacket>) {
    let streams = self.streams.clone();
    thread::spawn(move || {
      // TODO add cleanup
      loop {
        // TODO: should this be timeout
        match tcp_recv_rx.recv() {
          Ok((ip_header, tcp_header, data)) => {
            let dst_port = tcp_header.destination_port;
            let streams = streams.read().unwrap();
            let stream: Vec<_> = streams
              .iter()
              .filter(|&s| {
                let src_port = s.source_port();
                src_port == dst_port
              })
              .collect();
            // TODO handle this better -> what if there are no streams that match
            debug_assert_eq!(stream.len(), 1);
            match stream[0].process((ip_header, tcp_header, data)) {
              Ok(()) => (),
              Err(_e) => {
                debug!("Exiting...");
                break;
              }
            }
          }
          Err(_e) => {
            debug!("Exiting...");
            break;
          }
        }
      }
    });
  }

  /// Passes packets to channel, sending to thread that handles packets
  pub fn get_tcp_handler(&self) -> HandlerFunction {
    let tcp_recv_tx = self.tcp_recv_tx.clone();

    Box::new(move |ip_packet| {
      let (ip_header, _) = Ipv4Header::from_slice(ip_packet.header()).unwrap();
      let data = ip_packet.data();
      match TcpHeader::from_slice(data) {
        Ok((tcp_header, data)) => match tcp_recv_tx.send((ip_header, tcp_header, data.to_vec())) {
          Ok(()) => (),
          Err(e) => {
            edebug!("Error parsing TCP packet: {e}");
            return;
          }
        },
        // Drop in the case of a parsing error
        Err(e) => {
          edebug!("Error parsing TCP packet: {e}");
          return;
        }
      };
    })
  }

  pub fn print_sockets(&self) {
    let streams = self.streams.read().unwrap();
    for (i, stream) in streams.iter().enumerate() {
      println!(
        "Socket {i} - src: {:?}:{}, dst: {:?}, state: {:?}",
        stream.source_ip(),
        stream.source_port(),
        stream.destination(),
        stream.state()
      )
    }
  }

  pub fn print_window(&self, socket: SocketId) {
    let streams = self.streams.read().unwrap();
    let num_streams = streams.len();
    if !(0..num_streams).contains(&socket) {
      eprintln!("Invalid socket_id {socket}, must be within 0..{num_streams}");
    } else {
      let stream = &streams[socket];
      println!("Window size: {}", stream.get_window_size());
    }
  }

  pub fn accept(&self, port: Port) {
    let stream = TcpStream::listen(port, self.ip_send_tx.clone());
    self.streams.write().unwrap().push(Arc::new(stream));
  }

  pub fn connect(&self, src_ip: Ipv4Addr, dst_ip: Ipv4Addr, dst_port: Port) {
    // TODO: should keep track of available ports and assign source port from there
    let source_ip = 1024;
    // TODO: pass error back up?
    let stream =
      TcpStream::connect(src_ip, source_ip, dst_ip, dst_port, self.ip_send_tx.clone()).unwrap();
    self.streams.write().unwrap().push(Arc::new(stream));
  }

  pub fn send(&self, socket_id: SocketId, data: Vec<u8>) -> Result<()> {
    // TODO: check state of stream
    let streams = self.streams.read().unwrap();
    match streams.get(socket_id) {
      Some(stream) => {
        stream.send(&data)?;
        Ok(())
      }
      None => return Err(anyhow!("Unknown socket_id: {socket_id}")),
    }
  }

  pub fn recv(&self, socket_id: SocketId, numbytes: usize, should_block: bool) -> Result<Vec<u8>> {
    // TODO: check state of stream
    let streams = self.streams.read().unwrap();
    match streams.get(socket_id) {
      Some(stream) => Ok(stream.recv(numbytes, should_block)?),
      None => Err(anyhow!("Unknown socket_id: {socket_id}")),
    }
  }

  pub fn shutdown(&self, socket_id: SocketId, shutdown_method: SocketSide) {
    let streams = self.streams.read().unwrap();
    let stream = match streams.get(socket_id) {
      Some(stream) => stream,
      None => {
        debug!("Unknown socket_id: {socket_id}");
        return;
      }
    };

    match shutdown_method {
      SocketSide::Write => {
        if let Err(e) = stream.close() {
          debug!("Error: {e}")
        }
      }
      SocketSide::Read => todo!(),
      SocketSide::Both => todo!(),
    }
  }

  pub fn close(&self, socket_id: SocketId) {
    todo!()
  }

  pub fn send_file(&self, filename: String, ip: Ipv4Addr, port: Port) {
    todo!()
  }

  pub fn recv_file(&self, filename: String, port: Port) {
    todo!()
  }
}
