use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::sync::Arc;

use super::interface::Interface;
use super::lnx_config::LnxConfig;

struct Node {
  interfaces: Arc<HashMap<Ipv4Addr, Interface>>,
}

impl Node {
  fn new(config: LnxConfig) -> Node {
    todo!();
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
}
