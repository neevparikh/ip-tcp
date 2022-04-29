use std::collections::{HashMap, HashSet};
use std::net::Ipv4Addr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, RwLock};
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, Result};

use super::forwarding_table::ForwardingTable;
use super::ip_packet::IpPacket;
use super::protocol::Protocol;
use super::rip_message::INFINITY_COST;
use super::HandlerFunction;
use crate::link::interface::{InterfaceId, State};
use crate::link::link_layer::{LinkLayer, LinkRecvMsg, LinkSendMsg};
use crate::misc::lnx_config::LnxConfig;
use crate::{debug, edebug};

type HandlerMap = HashMap<Protocol, HandlerFunction>;

pub struct IpLayer {
  handlers:    Arc<Mutex<HandlerMap>>,
  link_layer:  Arc<RwLock<LinkLayer>>,
  closed:      Arc<AtomicBool>,
  send_handle: Option<thread::JoinHandle<()>>,
  ip_send_tx:  Sender<IpPacket>,
  table:       Arc<ForwardingTable>,
}

impl IpLayer {
  pub fn new(config: LnxConfig) -> IpLayer {
    let (ip_send_tx, ip_send_rx) = channel();
    let mut our_ip_addrs = HashSet::new();
    let link_layer = Arc::new(RwLock::new(LinkLayer::new(config)));

    for interface in link_layer.read().unwrap().get_interfaces().iter() {
      our_ip_addrs.insert(interface.our_ip);
    }

    let (rip_handler, table) = ForwardingTable::new(
      ip_send_tx.clone(),
      link_layer.read().unwrap().get_interfaces_clone(),
    );
    let table = Arc::new(table);

    let mut node = IpLayer {
      handlers: Arc::new(Mutex::new(HashMap::new())),
      link_layer,
      closed: Arc::new(AtomicBool::new(false)),
      send_handle: None,
      table,
      ip_send_tx,
    };

    node.register_handler(Protocol::RIP, rip_handler);

    let (link_send_tx, link_recv_rx) = node.link_layer.write().unwrap().run();

    let handlers = node.handlers.clone();
    let ip_send_tx = node.ip_send_tx.clone();
    thread::spawn(move || IpLayer::recv_thread(link_recv_rx, handlers, our_ip_addrs, ip_send_tx));

    let closed = node.closed.clone();
    let table = node.table.clone();
    let link_layer = node.link_layer.clone();
    node.send_handle = Some(thread::spawn(move || {
      IpLayer::send_thread(ip_send_rx, link_send_tx, table, closed, link_layer)
    }));

    node
  }

  /// Returns a channel which can be used by other protocols to pass messages down to the IP level
  pub fn get_ip_send_tx(&self) -> Sender<IpPacket> {
    self.ip_send_tx.clone()
  }

  fn close(&mut self) {
    self.closed.store(true, Ordering::SeqCst);
    if let Some(handle) = self.send_handle.take() {
      handle.join().unwrap_or_else(|e| {
        eprintln!("Got panic in send thread {:?}", e);
        ()
      })
    }
  }

  fn recv_thread(
    link_recv_rx: Receiver<LinkRecvMsg>,
    handlers: Arc<Mutex<HandlerMap>>,
    our_ip_addrs: HashSet<Ipv4Addr>,
    ip_send_tx: Sender<IpPacket>,
  ) {
    loop {
      match link_recv_rx.recv() {
        Ok((interface, bytes)) => {
          let mut packet = match IpPacket::unpack(&bytes) {
            Err(e) => {
              edebug!("{}", e);
              debug!("Warning: Malformed packet, dropping...");
              continue;
            }
            Ok(p) => p,
          };

          if our_ip_addrs.contains(&packet.destination_address()) {
            // debug!(
            //   "packet dst {:?} ours, calling handle_packet...",
            //   packet.destination_address()
            // );
            match IpLayer::handle_packet(&handlers, interface, &packet) {
              Ok(()) => (),
              Err(e) => edebug!("Packet handler errored: {e}"),
            }
          } else if packet.time_to_live() == 0 {
            debug!(
              "packet dst {:?} expired, dropping...",
              packet.destination_address()
            );
          } else {
            packet.set_time_to_live(packet.time_to_live() - 1);
            debug!(
              "packet dst {:?}, forwarding...",
              packet.destination_address()
            );
            if let Err(_e) = ip_send_tx.send(packet) {
              edebug!("Connection closed, exiting node listen...");
              return;
            }
          }
        }
        Err(_) => {
          edebug!("Connection closed, exiting node listen...");
          return;
        }
      }
    }
  }

  fn send_thread(
    ip_send_rx: Receiver<IpPacket>,
    link_send_tx: Sender<LinkSendMsg>,
    table: Arc<ForwardingTable>,
    closed: Arc<AtomicBool>,
    link_layer: Arc<RwLock<LinkLayer>>,
  ) {
    loop {
      match ip_send_rx.recv_timeout(Duration::from_millis(100)) {
        Ok(mut packet) => {
          let id = match table.get_next_hop(packet.destination_address()) {
            None => {
              edebug!(
                "{:?}: No next interface for next hop, dropping...",
                packet.destination_address()
              );
              continue;
            }
            Some(id) => id,
          };
          if !packet.source_address_set() {
            packet.set_source_address(link_layer.read().unwrap().get_our_ip(id).unwrap());
          }
          if let Err(_e) = link_send_tx.send((Some(id), packet.pack())) {
            edebug!("Connection dropped, exiting node send...");
            break;
          }
        }
        Err(RecvTimeoutError::Timeout) => {
          if closed.load(Ordering::SeqCst) {
            debug!("Connection closed, exiting node send...");
            break;
          }
        }
        Err(_) => {
          edebug!("Closing node send...");
          break;
        }
      }
    }
  }

  pub fn toggle_interface(&mut self, id: InterfaceId, state: State) -> Result<()> {
    match state {
      State::UP => self.link_layer.read().unwrap().up(id),
      State::DOWN => self.link_layer.read().unwrap().down(id),
    }
  }

  pub fn send(&mut self, their_ip: Ipv4Addr, protocol: Protocol, data: Vec<u8>) -> Result<()> {
    let packet = match IpPacket::new_with_defaults(their_ip, protocol, &data) {
      Ok(packet) => packet,
      Err(e) => {
        edebug!("Failed to create ip packet, {e}");
        return Ok(());
      }
    };

    if let Err(e) = self.ip_send_tx.send(packet) {
      Err(anyhow!("ip send thread closed unexpectedly, {e}"))
    } else {
      Ok(())
    }
  }

  pub fn print_interfaces(&self) {
    let layer_read = self.link_layer.read().unwrap();
    let interfaces = layer_read.get_interfaces();
    for interface in interfaces.iter() {
      println!("{}", interface);
    }
  }

  pub fn print_routes(&self) {
    let routes = self.table.get_table();
    for (dest, route) in routes.iter() {
      if route.cost < INFINITY_COST {
        println!("{}: {}", dest, route);
      }
    }
  }

  pub fn get_src_from_dst(&self, dst_ip: Ipv4Addr) -> Option<Ipv4Addr> {
    let link_layer = self.link_layer.read().unwrap();
    let interfaces = link_layer.get_interfaces();
    let next_hop = match self.table.get_next_hop(dst_ip) {
      Some(next_hop) => next_hop,
      None => return None,
    };

    match interfaces.get(next_hop) {
      Some(interface) => Some(interface.our_ip),
      None => None,
    }
  }

  pub fn register_handler(&mut self, protocol_num: Protocol, handler: HandlerFunction) {
    self.handlers.lock().unwrap().insert(protocol_num, handler);
  }

  fn handle_packet(
    handlers: &Arc<Mutex<HandlerMap>>,
    _interface: usize,
    packet: &IpPacket,
  ) -> Result<()> {
    let protocol = packet.protocol();
    // debug!("Handling packet with protocol {protocol}");
    let handlers = handlers.lock().unwrap();
    let handler = handlers.get(&protocol);

    match handler {
      None => Err(anyhow!("No handler registered for protocol {protocol}")),
      Some(handler) => Ok(handler(packet)),
    }
  }
}

impl Drop for IpLayer {
  fn drop(&mut self) {
    self.close();
  }
}
