use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::net::Ipv4Addr;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, RwLock};
use std::thread;

use anyhow::{anyhow, Result};
use etherparse::{Ipv4Header, TcpHeader};

use super::socket::{SocketId, SocketSide};
use super::tcp_stream::TcpStream;
use super::{IpTcpPacket, Port, TcpStreamState};
use crate::ip::{HandlerFunction, IpPacket};
use crate::{debug, edebug};

type StreamMap = Arc<RwLock<BTreeMap<SocketId, Arc<TcpStream>>>>;
type QueuedStreamMap = Arc<Mutex<BTreeMap<SocketId, (Sender<Arc<TcpStream>>, HashSet<Ipv4Addr>)>>>;

const START_PORT: Port = 10000;

#[derive(Debug)]
pub struct SocketPortAvailablity {
  used_ports:     BTreeSet<Port>,
  next_socket_id: SocketId,
}

pub(super) type LockedSocketPort = Arc<Mutex<SocketPortAvailablity>>;

impl SocketPortAvailablity {
  pub(super) fn get_new_socket(&mut self) -> SocketId {
    let s = self.next_socket_id;
    self.next_socket_id += 1;
    s
  }

  pub(super) fn get_new_port(&mut self) -> Port {
    if self.used_ports.len() == 0 {
      self.used_ports.insert(START_PORT);
      START_PORT
    } else {
      let mut prev_port = START_PORT;
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

  pub(super) fn contains_port(&self, port: Port) -> bool {
    self.used_ports.contains(&port)
  }

  pub(super) fn add_port(&mut self, port: Port) -> bool {
    self.used_ports.insert(port)
  }

  pub(super) fn remove_port(&mut self, port: Port) -> bool {
    self.used_ports.remove(&port)
  }
}

#[derive(Clone)]
pub struct TcpLayerInfo {
  pub sockets_and_ports:     LockedSocketPort,
  pub(super) ip_send_tx:     Sender<IpPacket>,
  pub(super) streams:        StreamMap,
  pub(super) queued_streams: QueuedStreamMap,
  pub(super) our_ip_addrs:   HashSet<Ipv4Addr>,
}

impl TcpLayerInfo {
  pub(super) fn make_cleanup_callback(&self, socket_id: SocketId) -> Box<dyn Fn() + Send> {
    let stream_map = self.streams.clone();
    let sockets_and_ports = self.sockets_and_ports.clone();
    Box::new(move || {
      let mut sm = stream_map.write().unwrap();
      let stream = sm.remove(&socket_id);
      drop(sm);
      if let Some(stream) = stream {
        sockets_and_ports
          .lock()
          .unwrap()
          .remove_port(stream.source_port());
      }
    })
  }

  pub fn get_our_ip_addrs(&self) -> HashSet<Ipv4Addr> {
    self.our_ip_addrs.clone()
  }
}

pub struct TcpLayer {
  tcp_recv_tx: Sender<IpTcpPacket>,
  info:        TcpLayerInfo,
}

impl TcpLayer {
  pub fn get_info(&self) -> TcpLayerInfo {
    self.info.clone()
  }
}

impl TcpLayer {
  pub fn new(ip_send_tx: Sender<IpPacket>, our_ip_addrs: HashSet<Ipv4Addr>) -> TcpLayer {
    let (tcp_recv_tx, tcp_recv_rx) = mpsc::channel(); // magic number

    let info = SocketPortAvailablity {
      next_socket_id: 0,
      used_ports:     BTreeSet::new(),
    };

    let tcp_layer = TcpLayer {
      tcp_recv_tx,
      info: TcpLayerInfo {
        sockets_and_ports: Arc::new(Mutex::new(info)),
        ip_send_tx: ip_send_tx.clone(),
        streams: Arc::new(RwLock::new(BTreeMap::new())),
        queued_streams: Arc::new(Mutex::new(BTreeMap::new())),
        our_ip_addrs,
      },
    };
    tcp_layer.start_stream_dispatcher(tcp_recv_rx);
    tcp_layer
  }

  /// Starts thread that handles all incoming TcpPackets and routes them appropriately
  pub fn start_stream_dispatcher(&self, tcp_recv_rx: Receiver<IpTcpPacket>) {
    let streams = self.info.streams.clone();
    thread::spawn(move || loop {
      match tcp_recv_rx.recv() {
        Ok((ip_header, tcp_header, data)) => {
          let their_src_ip: Ipv4Addr = ip_header.source.into();
          let their_dst_ip: Ipv4Addr = ip_header.destination.into();
          let their_src_port = tcp_header.source_port;
          let their_dst_port = tcp_header.destination_port;

          let streams = streams.read().unwrap();

          let mut stream = None;

          for (_, cur_stream) in streams.iter() {
            let our_src_ip = cur_stream.source_ip();
            let our_src_port = cur_stream.source_port();
            let our_dst_ip = cur_stream.destination().map(|a| a.ip());
            let our_dst_port = cur_stream.destination().map(|a| a.port());

            if our_src_port == their_dst_port && our_dst_ip.is_some() {
              let our_dst_ip = our_dst_ip.unwrap();
              let our_dst_port = our_dst_port.unwrap();
              let our_src_ip = our_src_ip.unwrap();

              if our_src_ip == their_dst_ip
                && our_dst_port == their_src_port
                && our_dst_ip == their_src_ip
              {
                stream = Some(cur_stream);
                break;
              }
            } else if our_dst_ip.is_none() {
              stream = Some(cur_stream)
            }
          }

          if let Some(stream) = stream {
            match stream.process((ip_header, tcp_header, data)) {
              Ok(()) => (),
              Err(e) => {
                debug!(
                  "Stream {} process failed with {e}, closing stream...",
                  stream.socket_id()
                );
                stream.close().unwrap_or_else(|e| debug!("{e}"));
              }
            }
          } else {
            debug!("No matching source_port, dropping packet");
          }
        }
        Err(_e) => {
          debug!("Exiting stream dispatcher...");
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
            eprintln!("Error parsing TCP packet: {e}");
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
    let streams = self.info.streams.read().unwrap();
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
    let streams = self.info.streams.read().unwrap();
    match streams.get(&socket_id) {
      Some(stream) => match stream.window_size() {
        Some((our_recv_window, their_recv_window, send_window)) => {
          println!(
            "our_recv: {our_recv_window} | their_recv: {their_recv_window} | our_send: \
             {send_window}"
          )
        }
        None => println!("N/A for listener sockets"),
      },
      None => eprintln!("Unknown socket_id: {socket_id}"),
    }
  }

  pub fn send(&self, socket_id: SocketId, data: Vec<u8>) -> Result<()> {
    let streams = self.info.streams.read().unwrap();
    match streams.get(&socket_id) {
      Some(stream) => match stream.state() {
        TcpStreamState::Listen => Err(anyhow!("Operation not supported on listener socket")),
        _ => Ok(stream.send(&data)?),
      },
      None => Err(anyhow!("Unknown socket_id: {socket_id}")),
    }
  }

  pub fn recv(&self, socket_id: SocketId, data: &mut [u8], should_block: bool) -> Result<usize> {
    let streams = self.info.streams.read().unwrap();
    match streams.get(&socket_id) {
      Some(stream) => match stream.state() {
        TcpStreamState::Listen => Err(anyhow!("Operation not supported on listener socket")),
        _ => Ok(stream.recv(data, should_block)?),
      },
      None => Err(anyhow!("Unknown socket_id: {socket_id}")),
    }
  }

  pub fn shutdown(&mut self, socket_id: SocketId, shutdown_method: SocketSide) {
    let streams = self.info.streams.read().unwrap();
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
    let streams = self.info.streams.read().unwrap();
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
}

impl Drop for TcpLayer {
  fn drop(&mut self) {
    let streams = self.info.streams.read().unwrap();
    for (socket_id, stream) in streams.iter() {
      stream
        .close()
        .unwrap_or_else(|e| edebug!("Failed to close stream {socket_id}, {e}"));
    }
  }
}
