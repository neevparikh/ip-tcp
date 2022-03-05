use super::debug;
use super::interface::{Interface, State};
use super::ip_packet::IpPacket;
use super::lnx_config::LnxConfig;

use std::collections::HashMap;
use std::io::ErrorKind;
use std::net::{Ipv4Addr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, RwLock, RwLockReadGuard};
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, Result};

const MAX_SIZE: usize = 65536;

pub struct LinkLayer {
  interfaces: Arc<RwLock<Vec<Interface>>>,
  addr_to_id: Arc<HashMap<Ipv4Addr, usize>>,
  local_link: UdpSocket,
  closed: Arc<AtomicBool>,
  recv_handle: Option<thread::JoinHandle<Result<()>>>,
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
      closed: Arc::new(AtomicBool::new(false)),
      send_tx: None,
      recv_rx: None,
      recv_handle: None,
    }
  }

  /// Launches recv and send threads, returns closure to wait for threads to exit
  pub fn run(&mut self) {
    let (send_tx, send_rx) = channel();
    let (recv_tx, recv_rx) = channel();

    let local_for_send = self.local_link.try_clone().unwrap();
    let addr_map_send = self.addr_to_id.clone();
    let interface_map_send = self.interfaces.clone();
    let closed_send = self.closed.clone();

    let local_for_recv = self.local_link.try_clone().unwrap();
    let addr_map_recv = self.addr_to_id.clone();
    let interface_map_recv = self.interfaces.clone();
    let closed_recv = self.closed.clone();

    let _send = thread::spawn(move || {
      LinkLayer::send_thread(
        send_rx,
        local_for_send,
        addr_map_send,
        interface_map_send,
        closed_send,
      )
    });
    let recv = thread::spawn(move || {
      LinkLayer::recv_thread(
        recv_tx,
        local_for_recv,
        addr_map_recv,
        interface_map_recv,
        closed_recv,
      )
    });
    self.send_tx = Some(send_tx);
    self.recv_rx = Some(recv_rx);
    self.recv_handle = Some(recv);
  }

  /// Closes the recv/send threads
  pub fn close(&mut self) {
    self.closed.store(true, Ordering::SeqCst);
    if let Some(handle) = self.recv_handle.take() {
      handle
        .join()
        .unwrap_or_else(|e| {
          eprintln!("Got panic in recv thread {:?}", e);
          Ok(())
        })
        .unwrap();
    }
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
    closed: Arc<AtomicBool>,
  ) -> Result<()> {
    loop {
      match send_rx.recv() {
        Ok(packet) => {
          let dst_ip_addr = packet.destination_address();
          let id = addr_to_id[&dst_ip_addr];
          let interfaces = interfaces.read().unwrap();
          let interface = &interfaces[id];
          let dst_socket_addr = interface.outgoing_link.clone();
          let state = interface.state().clone();
          drop(interfaces);
          if state == State::UP {
            local_link.send_to(&packet.pack(), dst_socket_addr)?;
          }
        }
        Err(_e) => {
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
    closed: Arc<AtomicBool>,
  ) -> Result<()> {
    local_link.set_read_timeout(Some(Duration::from_millis(100)))?;
    let mut buf = Vec::new();
    buf.reserve(MAX_SIZE);
    loop {
      match local_link.recv_from(&mut buf) {
        Ok((bytes_read, src)) => {
          let packet = match IpPacket::unpack(&buf[..bytes_read]) {
            Err(_e) => {
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
                  Err(_e) => {
                    debug!("Connection dropped, exiting...");
                    break;
                  }
                }
              }
            }
          }
        }
        Err(e) if e.kind() == ErrorKind::WouldBlock => {
          if closed.load(Ordering::SeqCst) {
            debug!("Connection dropped, exiting...");
            break;
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

impl Drop for LinkLayer {
  fn drop(&mut self) {
    self.close();
  }
}
