use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::io::stdin;

// use super::debug;
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
  pub fn new(config: LnxConfig) -> Node {
    Node {
      handlers: HashMap::new(),
      link_layer: LinkLayer::new(config),
    }
  }

  fn listen(&mut self) -> Result<()> {
    todo!();
  }

  pub fn run(&mut self) -> Result<()> {
    self.link_layer.run();
    loop {
      let mut buf = String::new();
      stdin().read_line(&mut buf)?;

      let s = buf.as_str().trim();
      if s.starts_with("interfaces") || s.starts_with("li") {
        let interfaces = self.link_layer.get_interface_map();
        for interface in interfaces.iter() {
          println!("{}", interface);
        }
      } else if s.starts_with("routes") || s.starts_with("lr") {
        todo!();
      } else if s.starts_with("q") {
        todo!();
      } else if s.starts_with("send") {
        todo!();
      } else if s.starts_with("up") {
        todo!();
      } else if s.starts_with("down") {
        todo!();
      } else {
        eprintln!(
          concat!(
            "Unrecognized command {}, expected one of ",
            "[interfaces | li, routes | lr, q, down INT, ",
            "up INT, send VIP PROTO STRING]"
          ),
          s
        );
      }
    }
  }

  fn register_handler<F>(&mut self, protocol_num: Protocol, handler: HandlerFunction) {
    self.handlers.insert(protocol_num, handler);
  }

  fn handle_packet(&self, packet: &IpPacket) -> Result<IpPacket> {
    let protocol = packet.protocol();
    let handler = self.handlers.get(&protocol);

    match handler {
      None => Err(anyhow!("No handler registered for protocol {protocol}")),
      Some(handler) => Ok(handler(packet)),
    }
  }
}
