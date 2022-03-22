use std::collections::HashMap;
use std::io::ErrorKind;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, RwLock, RwLockReadGuard};
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, Result};

use crate::interface::{Interface, State};
use crate::ip_packet::IpPacket;
use crate::lnx_config::LnxConfig;
use crate::{debug, edebug, InterfaceId};

const MAX_SIZE: usize = 65536;

pub type LinkSendMsg = (Option<InterfaceId>, IpPacket);
pub type LinkRecvMsg = (InterfaceId, IpPacket);

pub struct LinkLayer {
  interfaces: Arc<RwLock<Vec<Interface>>>,
  addr_to_id: Arc<HashMap<SocketAddr, InterfaceId>>,
  local_link: UdpSocket,
  closed: Arc<AtomicBool>,
  recv_handle: Option<thread::JoinHandle<Result<()>>>,
}

impl LinkLayer {
  pub fn get_interfaces(&self) -> RwLockReadGuard<Vec<Interface>> {
    self.interfaces.read().unwrap()
  }

  pub fn get_interfaces_clone(&self) -> Arc<RwLock<Vec<Interface>>> {
    self.interfaces.clone()
  }

  /// Builds up internal interface map
  pub fn new(config: LnxConfig) -> LinkLayer {
    // build interface map
    let mut interface_id_by_their_addr = HashMap::new();

    for (id, interface) in config.interfaces.iter().enumerate() {
      let their_socket_addr = interface.outgoing_link_addr.clone();
      interface_id_by_their_addr.insert(their_socket_addr, id);
    }

    LinkLayer {
      interfaces: Arc::new(RwLock::new(config.interfaces)),
      addr_to_id: Arc::new(interface_id_by_their_addr),
      local_link: config.local_link,
      closed: Arc::new(AtomicBool::new(false)),
      recv_handle: None,
    }
  }

  /// Launches recv and send threads, returns channels which can be used to send and receive
  /// Note that if the interface_id is None in the send, the message will be broadcast on all
  /// interfaces which are currently set to up
  pub fn run(&mut self) -> (Sender<LinkSendMsg>, Receiver<LinkRecvMsg>) {
    let (link_send_tx, link_send_rx) = channel();
    let (link_recv_tx, link_recv_rx) = channel();

    let local_for_send = self.local_link.try_clone().unwrap();
    let interfaces_send = self.interfaces.clone();

    let local_for_recv = self.local_link.try_clone().unwrap();
    let addr_map_recv = self.addr_to_id.clone();
    let interfaces_recv = self.interfaces.clone();
    let closed_recv = self.closed.clone();

    let _send =
      thread::spawn(move || LinkLayer::send_thread(link_send_rx, local_for_send, interfaces_send));
    let recv = thread::spawn(move || {
      LinkLayer::recv_thread(
        link_recv_tx,
        local_for_recv,
        addr_map_recv,
        interfaces_recv,
        closed_recv,
      )
    });

    self.recv_handle = Some(recv);
    (link_send_tx, link_recv_rx)
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

  /// Sets the specified interface up
  pub fn up(&self, interface_id: InterfaceId) -> Result<()> {
    let mut interfaces = self.interfaces.write().unwrap();
    if interface_id >= interfaces.len() {
      Err(anyhow!("Unknown interface id"))
    } else {
      interfaces[interface_id].up();
      Ok(())
    }
  }

  /// Sets the specified interface down
  pub fn down(&self, interface_id: InterfaceId) -> Result<()> {
    let mut interfaces = self.interfaces.write().unwrap();
    if interface_id >= interfaces.len() {
      Err(anyhow!("Unknown interface id"))
    } else {
      interfaces[interface_id].down();
      Ok(())
    }
  }

  /// Returns the locked state of the specified interface
  pub fn get_state(&self, interface_id: InterfaceId) -> Result<State> {
    let interfaces = self.get_interfaces();
    if interface_id >= interfaces.len() {
      Err(anyhow!("Unknown interface id"))
    } else {
      Ok(interfaces[interface_id].state())
    }
  }

  /// Returns the locked state of the specified interface
  pub fn get_our_ip(&self, interface_id: InterfaceId) -> Result<Ipv4Addr> {
    let interfaces = self.get_interfaces();
    if interface_id >= interfaces.len() {
      Err(anyhow!("Unknown interface id"))
    } else {
      Ok(interfaces[interface_id].our_ip)
    }
  }

  fn send_thread(
    link_send_rx: Receiver<LinkSendMsg>,
    local_link: UdpSocket,
    interfaces: Arc<RwLock<Vec<Interface>>>,
  ) -> Result<()> {
    loop {
      match link_send_rx.recv() {
        Ok((id_opt, packet)) => {
          let interfaces = interfaces.read().unwrap();
          let send_packet_to_id = |interface: &Interface| -> Result<()> {
            let dst_socket_addr = interface.outgoing_link_addr.clone();
            let state = interface.state().clone();
            if state == State::UP {
              local_link.send_to(&packet.pack(), dst_socket_addr)?;
            }
            Ok(())
          };

          match id_opt {
            Some(id) => send_packet_to_id(&interfaces[id])?,
            None => {
              for interface in interfaces.iter() {
                send_packet_to_id(interface)?;
              }
            }
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
    link_recv_tx: Sender<LinkRecvMsg>,
    local_link: UdpSocket,
    addr_to_id: Arc<HashMap<SocketAddr, InterfaceId>>,
    interfaces: Arc<RwLock<Vec<Interface>>>,
    closed: Arc<AtomicBool>,
  ) -> Result<()> {
    local_link.set_read_timeout(Some(Duration::from_millis(100)))?;
    let mut buf = [0u8; MAX_SIZE];
    loop {
      match local_link.recv_from(&mut buf) {
        Ok((bytes_read, src)) => {
          let packet = match IpPacket::unpack(&buf[..bytes_read]) {
            Err(e) => {
              edebug!("{}", e);
              debug!("Warning: Malformed packet from {}, dropping...", src);
              continue;
            }
            Ok(p) => p,
          };
          match addr_to_id.get(&src) {
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
                match link_recv_tx.send((id, packet)) {
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
