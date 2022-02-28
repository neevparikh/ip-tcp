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
  interfaces: Arc<RwLock<HashMap<usize, Interface>>>,
  addr_to_id: Arc<HashMap<Ipv4Addr, usize>>,
  local_link: UdpSocket,
  send_tx: Option<Sender<IpPacket>>,
  read_rx: Option<Receiver<IpPacket>>,
}

impl LinkLayer {
  /// Builds up internal interface map
  pub fn new(config: LnxConfig) -> LinkLayer {
    // build interface map
    let mut interfaces = HashMap::new();
    let mut interface_id_by_their_addr = HashMap::new();

    for (id, interface) in config.interfaces.into_iter().enumerate() {
      interfaces.insert(id, interface);
      interface_id_by_their_addr.insert(interface.their_ip.clone(), id);
    }

    LinkLayer {
      interfaces: Arc::new(RwLock::new(interfaces)),
      addr_to_id: Arc::new(interface_id_by_their_addr),
      local_link: config.local_link,
      send_tx: None,
      read_rx: None,
    }
  }

  pub fn run(&self) {
    let (send_tx, send_rx) = channel();
    let (recv_tx, recv_rx) = channel();
    thread::spawn(move || {
      LinkLayer::send_thread(send_rx);
    });
    thread::spawn(move || {
      LinkLayer::recv_thread(recv_tx);
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
    id_to_interface: Arc<RwLock<HashMap<usize, Interface>>>,
  ) -> Result<()> {
    loop {
      match send_rx.recv() {
        Ok(packet) => {
          let dst_ip_addr = packet.destination_addr();
          let id = addr_to_id[&dst_ip_addr];
          let map = id_to_interface.read().unwrap();
          let interface = map[&id];
          let dst_socket_addr = interface.outgoing_link;
          drop(map);
          local_link.send_to(&packet.pack(), dst_socket_addr)?;
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
    id_to_interface: Arc<RwLock<HashMap<usize, Interface>>>,
  ) -> Result<()> {
    let mut buf = Vec::new();
    buf.reserve(MAX_SIZE);
    loop {
      match local_link.recv_from(&mut buf) {
        Ok((bytes_read, src)) => {
          let packet = IpPacket::unpack(&buf[..bytes_read]);
          match addr_to_id.get(packet.source_addr()) {
            None => debug!(
              "Warning: Unknown source addr {}, local: {}, dropping...",
              packet.source_addr(),
              src
            ),
            Some(id) => {
              let map = id_to_interface.read().unwrap();
              let interface = map[&id];
              let state = interface.state();
              drop(map);
              if *state == State::UP {
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
