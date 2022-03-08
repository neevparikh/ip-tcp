use std::collections::HashMap;
use std::io::stdin;
use std::net::Ipv4Addr;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::{anyhow, Result};
use shellwords;

// use super::debug;
use super::ip_packet::IpPacket;
use super::HandlerFunction;
use super::link_layer::LinkLayer;
use super::lnx_config::LnxConfig;
use super::protocol::Protocol;
use crate::{debug, edebug};

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

  fn listen(
    recv_rx: Receiver<IpPacket>,
    send_tx: Sender<IpPacket>,
    handlers: Arc<Mutex<HandlerMap>>,
    ) {
    loop {
      match recv_rx.recv() {
        Ok(packet) => match Node::handle_packet(&handlers, &packet) {
          Ok(Some(response)) => {
            if let Err(_e) = send_tx.send(response) {
              edebug!("Closing Node listen thread");
              return;
            }
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
      let tokens: Vec<String> = match shellwords::split(s) {
        Ok(tokens) => tokens,
        Err(e) => {
          eprintln!("Error: {e}");
          continue;
        }
      };
      debug_assert!(tokens.len() > 0);

      match &*tokens[0] {
        "interfaces" | "li" => {
          let interfaces = self.link_layer.get_interfaces();
          for interface in interfaces.iter() {
            println!("{}", interface);
          }
        }

        "routes" | "lr" => {
          todo!();
        }

        "q" => {
          break;
        }

        "send" => {
          if tokens.len() != 4 {
            println!(
              "Error: '{}' expected 3 arguments received {}",
              tokens[0],
              tokens.len() - 1
              );
            continue;
          }

          let their_ip: Ipv4Addr = match tokens[1].parse::<Ipv4Addr>() {
            Ok(ip) => ip,
            Err(_) => {
              eprintln!("Error: Failed to parse vip");
              continue;
            }
          };

          let protocol: Protocol = match tokens[2].parse::<u8>() {
            Ok(protocol_num) => match Protocol::try_from(protocol_num) {
              Ok(protocol) => protocol,
              Err(e) => {
                eprintln!("Error: Failed to parse protocol, {e}");
                continue;
              }
            },
            Err(_) => {
              eprintln!("Error: Failed to parse protocol, must be u8");
              continue;
            }
          };

          let data: Vec<u8> = tokens[3].as_bytes().to_vec();

          // TODO: get interface number from forwarding, routing table
          // TODO: check status of interface
          let outgoing_interface = 0usize;
          let source_address = self.link_layer.get_our_ip(&outgoing_interface).unwrap();

          // TODO: figure out values for these fields
          let type_of_service = 0u8;
          let time_to_live = 2u8;
          let identifier = 2u16;
          let packet = IpPacket::new(
            source_address,
            their_ip,
            protocol,
            type_of_service,
            time_to_live,
            &data,
            identifier,
            true,
            &[],
            );

          let packet = match packet {
            Ok(packet) => packet,
            Err(e) => {
              eprintln!("Error: Failed to create ip packet, {e}");
              continue;
            }
          };

          if let Err(_) = send_tx.send(packet) {
            eprintln!("LinkLayer closed unexpectedly");
            break;
          }
        }

        "up" | "down" => {
          if tokens.len() != 2 {
            println!(
              "Error: '{}' expected 1 argument received {}",
              tokens[0],
              tokens.len() - 1
              );
            continue;
          }

          let interface_id: usize = match tokens[1].parse() {
            Ok(num) => num,
            Err(_) => {
              println!("Error: interface id must be positive int");
              continue;
            }
          };

          let res = if tokens[0] == "up" {
            self.link_layer.up(&interface_id)
          } else {
            self.link_layer.down(&interface_id)
          };

          match res {
            Ok(_) => (),
            Err(e) => println!("Error: setting interface status failed: {e}"),
          }
        }

        other => {
          eprintln!(
            concat!(
              "Unrecognized command {}, expected one of ",
              "[interfaces | li, routes | lr, q, down INT, ",
              "up INT, send VIP PROTO STRING]"
              ),
              other
              );
        }
      }
    }
    Ok(())
  }

  pub fn register_handler(&mut self, protocol_num: Protocol, handler: HandlerFunction) {
    self.handlers.lock().unwrap().insert(protocol_num, handler);
  }

  fn handle_packet(
    handlers: &Arc<Mutex<HandlerMap>>,
    packet: &IpPacket,
    ) -> Result<Option<IpPacket>> {
    let protocol = packet.protocol();
    debug!("Handling packet with protocol {protocol}");
    let handlers = handlers.lock().unwrap();
    let handler = handlers.get(&protocol);

    match handler {
      None => Err(anyhow!("No handler registered for protocol {protocol}")),
      Some(handler) => Ok(handler(packet)),
    }
  }
}
