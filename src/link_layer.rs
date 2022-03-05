use super::debug;
use super::interface::{Interface, State};
use super::ip_packet::IpPacket;
use super::lnx_config::LnxConfig;

use std::collections::HashMap;
use std::net::{Ipv4Addr, UdpSocket};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, RwLock, RwLockReadGuard};
use std::thread;

use anyhow::{anyhow, Result};

const MAX_SIZE: usize = 65536;

pub struct LinkLayer {
  interfaces: Arc<RwLock<Vec<Interface>>>,
  addr_to_id: Arc<HashMap<Ipv4Addr, usize>>,
  local_link: UdpSocket,
  send_tx: Option<Sender<IpPacket>>,
  recv_rx: Option<Receiver<IpPacket>>,
}

impl LinkLayer {
  pub fn get_interface_map(&self) -> RwLockReadGuard<Vec<Interface>> {
    self.interfaces.read().unwrap()
  }

  /// Builds up internal interface map
  pub fn new(config: LnxConfig) -> LinkLayer {
    // build interface map
    let mut interface_id_by_their_addr = HashMap::new();

    for (id, interface) in config.interfaces.iter().enumerate() {
      let their_ip = interface.their_ip.clone();
      interface_id_by_their_addr.insert(their_ip, id);
    }

    LinkLayer {
      interfaces: Arc::new(RwLock::new(config.interfaces)),
      addr_to_id: Arc::new(interface_id_by_their_addr),
      local_link: config.local_link,
      send_tx: None,
      recv_rx: None,
    }
  }

  pub fn run(&mut self) {
    let (send_tx, send_rx) = channel();
    let (recv_tx, recv_rx) = channel();

    let local_for_send = self.local_link.try_clone().unwrap();
    let addr_map_send = self.addr_to_id.clone();
    let interface_map_send = self.interfaces.clone();

    let local_for_recv = self.local_link.try_clone().unwrap();
    let addr_map_recv = self.addr_to_id.clone();
    let interface_map_recv = self.interfaces.clone();

    thread::spawn(move || {
      LinkLayer::send_thread(send_rx, local_for_send, addr_map_send, interface_map_send)
    });
    thread::spawn(move || {
      LinkLayer::recv_thread(recv_tx, local_for_recv, addr_map_recv, interface_map_recv)
    });
    self.send_tx = Some(send_tx);
    self.recv_rx = Some(recv_rx);
  }

  pub fn close(&self) -> Result<()> {
    todo!();
  }

  /// Send packet to send_thread via channel
  pub fn send(&self, packet: IpPacket) {
    todo!();
  }

  /// Get packet from recv_thread via channel
  pub fn recv(&self) -> Result<IpPacket> {
    todo!();
  }

  /// Sets the specified interface up
  pub fn up(interface_id: &usize) -> Result<()> {
    todo!();
  }

  /// Sets the specified interface down
  pub fn down(interface_id: &usize) -> Result<()> {
    todo!();
  }

  /// Returns the locked state of the specified interface
  pub fn get_state(interface_id: &usize) -> RwLockReadGuard<State> {
    todo!();
  }

  fn send_thread(
    send_rx: Receiver<IpPacket>,
    local_link: UdpSocket,
    addr_to_id: Arc<HashMap<Ipv4Addr, usize>>,
    interfaces: Arc<RwLock<Vec<Interface>>>,
  ) -> Result<()> {
    loop {
      match send_rx.recv() {
        Ok(packet) => {
          let dst_ip_addr = packet.destination_address();
          let id = addr_to_id[&dst_ip_addr];
          let interfaces = interfaces.read().unwrap();
          let interface = &interfaces[id];
          let dst_socket_addr = interface.outgoing_link;
          let state = interface.state().clone();
          drop(interfaces);
          if state == State::UP {
            local_link.send_to(&packet.pack(), dst_socket_addr)?;
          }
        }
        Err(e) => {
          debug!("Connection dropped, exiting...");
          break;
        }
      }
    }
    Ok(())
  }

  fn recv_thread(
    read_tx: Sender<IpPacket>,
    local_link: UdpSocket,
    addr_to_id: Arc<HashMap<Ipv4Addr, usize>>,
    interfaces: Arc<RwLock<Vec<Interface>>>,
  ) -> Result<()> {
    let mut buf = Vec::new();
    buf.reserve(MAX_SIZE);
    loop {
      match local_link.recv_from(&mut buf) {
        Ok((bytes_read, src)) => {
          let packet = match IpPacket::unpack(&buf[..bytes_read]) {
            Err(e) => {
              debug!("Warning: Malformed packet from {}, dropping...", src);
              continue;
            }
            Ok(p) => p,
          };
          match addr_to_id.get(&packet.source_address()) {
            None => {
              debug!(
                "Warning: Unknown source addr {}, local: {}, dropping...",
                packet.source_address(),
                src
              );
            }
            Some(&id) => {
              let interfaces = interfaces.read().unwrap();
              let interface = &interfaces[id];
              let state = interface.state().clone();
              drop(interfaces);
              if state == State::UP {
                match read_tx.send(packet) {
                  Ok(_) => (),
                  Err(e) => {
                    debug!("Connection dropped, exiting...");
                    break;
                  }
                }
              }
            }
          }
        }
        Err(e) => {
          return Err(anyhow!("Error in receiving from local link").context(format!("{}", e)));
        }
      }
    }
    Ok(())
  }
}
