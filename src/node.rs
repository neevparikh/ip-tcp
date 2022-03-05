use std::collections::HashMap;
use std::io::stdin;
use std::sync::mpsc::{Sender, Receiver};
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::{anyhow, Result};

use crate::edebug;

// use super::debug;
use super::ip_packet::IpPacket;
use super::link_layer::LinkLayer;
use super::lnx_config::LnxConfig;
use super::protocol::Protocol;

// TODO: Passing the whole packet might leak too much information
type HandlerFunction = Box<dyn (Fn(&IpPacket) -> Option<IpPacket>) + Send>;
type HandlerMap = HashMap<Protocol, HandlerFunction>;

pub struct Node {
  handlers: Arc<Mutex<HandlerMap>>,
  link_layer: LinkLayer,
}

impl Node {
  pub fn new(config: LnxConfig) -> Node {
    Node {
      handlers: Arc::new(Mutex::new(HashMap::new())),
      link_layer: LinkLayer::new(config),
    }
  }

  fn listen(recv_rx: Receiver<IpPacket>, send_tx: Sender<IpPacket>, handlers: Arc<Mutex<HandlerMap>>) {
    loop {
      match recv_rx.recv() {
        Ok(packet) => match Node::handle_packet(&handlers, &packet) {
          Ok(Some(response)) => if let Err(e) = send_tx.send(response) {
            edebug!("Closing Node listen thread");
            return;
          }
          Ok(None) => (),
          Err(e) => eprintln!("Packet handler errored: {e}"),
        },
        Err(_) => {
          edebug!("Closing Node listen thread");
          return;
        }
      }
    }
  }

  pub fn run(&mut self) -> Result<()> {
    let (send_tx, recv_rx) = self.link_layer.run();
    let send_tx_clone = send_tx.clone();
    let handlers_clone = Arc::clone(&self.handlers);
    thread::spawn(move || {
      Node::listen(recv_rx, send_tx_clone, handlers_clone);
    });

    loop {
      let mut buf = String::new();
      stdin().read_line(&mut buf)?;

      let s = buf.as_str().trim();
      let tokens: Vec<&str> = s.split(" ").collect();
      debug_assert!(tokens.len() > 0);

      if tokens[0] == "interfaces" || tokens[0] == "li" {
        let interfaces = self.link_layer.get_interfaces();
        for interface in interfaces.iter() {
          println!("{}", interface);
        }
      } else if tokens[0] == ("routes") || tokens[0] == "lr" {
        todo!();
      } else if tokens[0] == "q" {
        break;
      } else if tokens[0] == "send" {
        todo!();
      } else if tokens[0] == "up" || tokens[0] == "down" {
        if tokens.len() != 2 {
          println!("Error: '{}' expected one argument received {}", tokens[0], tokens.len() - 1);
          continue;
        }

        let interface_id: usize = match tokens[1].parse() {
          Ok(num) => num,
          Err(_) => {
            println!("Error: interface id must be positive int");
            continue;
          },
        };

        let res = if tokens[0] == "up" {
          self.link_layer.up(&interface_id)
        } else {
          self.link_layer.down(&interface_id)
        };

        match res {
          Ok(_) =>  (),
          Err(e) => println!("Error: setting interface status failed: {e}"),
        }
      } else {
        eprintln!(
          concat!(
            "Unrecognized command {}, expected one of ",
            "[interfaces | li, routes | lr, q, down INT, ",
            "up INT, send VIP PROTO STRING]"
            ),
            tokens[0]
            );
      }
    }
    Ok(())
  }

  fn register_handler<F>(&mut self, protocol_num: Protocol, handler: HandlerFunction) {
    self.handlers.lock().unwrap().insert(protocol_num, handler);
  }

  fn handle_packet(handlers: &Arc<Mutex<HandlerMap>>, packet: &IpPacket) -> Result<Option<IpPacket>> {
    let protocol = packet.protocol();
    let handlers = handlers.lock().unwrap();
    let handler = handlers.get(&protocol);

    match handler {
      None => Err(anyhow!("No handler registered for protocol {protocol}")),
      Some(handler) => Ok(handler(packet)),
    }
  }
}
