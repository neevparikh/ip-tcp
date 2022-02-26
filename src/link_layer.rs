use anyhow::Result;
use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::sync::{Arc, RwLock, RwLockReadGuard};

use super::interface::{Interface, State};
use super::ip_packet::IpPacket;
use super::lnx_config::LnxConfig;

pub struct LinkLayer {
  /// TODO: Should this map use id's as keys for easier setting up and down?
  interfaces: Arc<RwLock<HashMap<Ipv4Addr, Interface>>>,
}

impl LinkLayer {
  /// Builds up internal interface map
  pub fn new(config: LnxConfig) -> LinkLayer {
    // build interface map
    let mut interfaces = HashMap::new();

    for interface in config.interfaces {
      interfaces.insert(interface.their_ip.clone(), interface);
    }
    LinkLayer {
      interfaces: Arc::new(RwLock::new(interfaces)),
    }
  }

  pub fn run(&self) -> Result<()> {
    todo!();
  }

  pub fn send(&self, packet: IpPacket) {
    todo!();
  }

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

  fn start_send_thread() -> Result<()> {
    todo!();
  }

  fn start_recv_thread() -> Result<()> {
    todo!();
  }
}
