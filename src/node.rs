use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::sync::Arc;

use super::interface::Interface;
use super::ip_packet::IpPacket;
use super::lnx_config::LnxConfig;
use super::protocol::Protocol;

struct Node {
  interfaces: Arc<HashMap<Ipv4Addr, Interface>>,
}

impl Node {
  fn new(config: LnxConfig) -> Node {
    let interfaces = HashMap::new();

    for interface in config.interfaces {
      interfaces.insert(interface.their_ip, interface);
    }
  }

  fn run() {
    todo!();
  }

  fn register_handler<F, T>(protocol_num: Protocol, handler: F)
  where
    F: Fn(T) -> IpPacket,
  {
    todo!();
  }

  fn handle_packet(packet: &IpPacket) {
    todo!();
  }
}
