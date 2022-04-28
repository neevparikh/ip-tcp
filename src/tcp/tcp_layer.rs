use std::collections::{BTreeMap, BTreeSet};
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

type StreamMap = Arc<RwLock<BTreeMap<SocketId, Arc<TcpStream>>>>;

#[derive(Debug)]
pub enum LayerStreamMsg {
  /// Clean up stream
  Closed(SocketId),
}

pub struct TcpLayer {
  ip_send_tx:  Sender<IpPacket>,
  tcp_recv_tx: Sender<IpTcpPacket>,

  used_ports:     BTreeSet<Port>,
  next_socket_id: SocketId,

  streams: StreamMap,
}

impl TcpLayer {
  pub fn new(ip_send_tx: Sender<IpPacket>) -> TcpLayer {
    let (tcp_recv_tx, tcp_recv_rx) = channel();
    let tcp_layer = TcpLayer {
      ip_send_tx,
      tcp_recv_tx,
      used_ports: BTreeSet::new(),
      next_socket_id: 0,
      streams: Arc::new(RwLock::new(BTreeMap::new())),
    };
    tcp_layer.start_stream_dispatcher(tcp_recv_rx);
    tcp_layer
  }

  fn make_cleanup_callback(&self, socket_id: SocketId) -> Box<dyn Fn() + Send> {
    let stream_map = self.streams.clone();
    Box::new(move || {
      debug!("Waiting on stream map lock");
      let mut sm = stream_map.write().unwrap();
      debug!("Got stream map lock");
      sm.remove(&socket_id);
      debug!("Removed");
    })
  }

  fn get_new_socket(&mut self) -> SocketId {
    let s = self.next_socket_id;
    self.next_socket_id += 1;
    s
  }

  fn get_new_port(&mut self) -> Port {
    if self.used_ports.len() == 0 {
      self.used_ports.insert(1024);
      1024
    } else {
      let mut prev_port = 1023;
      for &port in self.used_ports.iter() {
        if prev_port + 1 < port {
          self.used_ports.insert(prev_port + 1);
          return prev_port + 1;
        }
        prev_port += 1;
      }
      self.used_ports.insert(prev_port);
      return prev_port;
    }
  }

  /// Starts thread that handles all incoming TcpPackets and routes them appropriately
  pub fn start_stream_dispatcher(&self, tcp_recv_rx: Receiver<IpTcpPacket>) {
    let streams = self.streams.clone();
    thread::spawn(move || loop {
      match tcp_recv_rx.recv() {
        Ok((ip_header, tcp_header, data)) => {
          let dst_port = tcp_header.destination_port;
          let streams = streams.read().unwrap();
          let stream: Vec<_> = streams
            .iter()
            .filter_map(|(_, s)| {
              if s.source_port() == dst_port {
                Some(s)
              } else {
                None
              }
            })
            .collect();
          if stream.len() < 1 {
            debug!("No matching source_port, dropping packet");
          } else if stream.len() == 1 {
            debug_assert_eq!(stream.len(), 1);
            match stream[0].process((ip_header, tcp_header, data)) {
              Ok(()) => (),
              Err(_e) => {
                debug!("Exiting...");
                break;
              }
            }
          } else {
            panic!("Fatal: Duplicate streams for the same port!");
          }
        }
        Err(_e) => {
          debug!("Exiting...");
          break;
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
    for (i, stream) in streams.iter() {
      println!(
        "Socket {i} - src: {:?}:{}, dst: {:?}, state: {:?}",
        stream.source_ip(),
        stream.source_port(),
        stream.destination(),
        stream.state()
      )
    }
  }

  pub fn print_window(&self, socket_id: SocketId) {
    let streams = self.streams.read().unwrap();
    match streams.get(&socket_id) {
      Some(stream) => println!("Window size: {}", stream.get_window_size()),
      None => eprintln!("Unknown socket_id: {socket_id}"),
    }
  }

  pub fn accept(&mut self, port: Port) {
    if self.used_ports.contains(&port) {
      eprintln!("Error: port {port} in use");
      return;
    }
    let new_socket = self.get_new_socket();
    let stream = TcpStream::listen(
      port,
      self.ip_send_tx.clone(),
      self.make_cleanup_callback(new_socket),
    );
    self
      .streams
      .write()
      .unwrap()
      .insert(new_socket, Arc::new(stream));
    self.used_ports.insert(port);
  }

  pub fn connect(&mut self, src_ip: Ipv4Addr, dst_ip: Ipv4Addr, dst_port: Port) {
    let src_port = self.get_new_port();
    let ip_send_tx = self.ip_send_tx.clone();
    let new_socket = self.get_new_socket();
    let stream = TcpStream::connect(
      src_ip,
      src_port,
      dst_ip,
      dst_port,
      ip_send_tx,
      self.make_cleanup_callback(new_socket),
    )
    .unwrap();
    self
      .streams
      .write()
      .unwrap()
      .insert(new_socket, Arc::new(stream));
  }

  pub fn send(&self, socket_id: SocketId, data: Vec<u8>) -> Result<()> {
    // TODO: check state of stream
    let streams = self.streams.read().unwrap();
    match streams.get(&socket_id) {
      Some(stream) => {
        stream.send(&data)?;
        Ok(())
      }
      None => return Err(anyhow!("Unknown socket_id: {socket_id}")),
    }
  }

  pub fn recv(&self, socket_id: SocketId, numbytes: usize, should_block: bool) -> Result<Vec<u8>> {
    let streams = self.streams.read().unwrap();
    match streams.get(&socket_id) {
      Some(stream) => Ok(stream.recv(numbytes, should_block)?),
      None => Err(anyhow!("Unknown socket_id: {socket_id}")),
    }
  }

  pub fn shutdown(&mut self, socket_id: SocketId, shutdown_method: SocketSide) {
    let streams = self.streams.read().unwrap();
    let stream = match streams.get(&socket_id) {
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

  /// This is v_close, not CLOSE in RFC
  pub fn close(&mut self, socket_id: SocketId) {
    let streams = self.streams.read().unwrap();
    let stream = match streams.get(&socket_id) {
      Some(stream) => stream,
      None => {
        debug!("Unknown socket_id: {socket_id}");
        return;
      }
    };

    if let Err(e) = stream.close() {
      debug!("Error: {e}")
    }
  }

  pub fn send_file(&self, filename: String, ip: Ipv4Addr, port: Port) {
    todo!()
  }

  pub fn recv_file(&self, filename: String, port: Port) {
    todo!()
  }
}
