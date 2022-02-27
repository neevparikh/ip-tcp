use anyhow::{anyhow, Result};
use std::collections::HashMap;

use super::ip_packet::IpPacket;
use super::link_layer::LinkLayer;
use super::lnx_config::LnxConfig;
use super::protocol::Protocol;

// TODO: Passing the whole packet might leak too much information
type HandlerFunction = Box<dyn Fn(&IpPacket) -> IpPacket>;
type HandlerMap = HashMap<Protocol, HandlerFunction>;

pub struct Node {
  handlers: HandlerMap,
  link_layer: LinkLayer,
}

impl Node {
  fn new(config: LnxConfig) -> Node {
    Node {
      handlers: HashMap::new(),
      link_layer: LinkLayer::new(config),
    }
  }

  fn run(&mut self) -> Result<()> {
    self.link_layer.run()?;
    todo!();
  }

  fn register_handler<F>(
    &mut self,
    protocol_num: Protocol,
    handler: HandlerFunction,
  ) {
    self.handlers.insert(protocol_num, handler);
  }

  fn handle_packet(&self, packet: &IpPacket) -> Result<IpPacket> {
    let protocol = packet.protocol()?;
    let handler = self.handlers.get(&protocol);

    match handler {
      None => Err(anyhow!("No handler registered for protocol {protocol}")),
      Some(handler) => Ok(handler(packet)),
    }
  }
}
