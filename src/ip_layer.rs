use std::collections::{HashMap, HashSet};
use std::io::stdin;
use std::net::Ipv4Addr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, Result};
use shellwords;

use crate::forwarding_table::ForwardingTable;
use crate::ip_packet::IpPacket;
use crate::link_layer::LinkLayer;
use crate::lnx_config::LnxConfig;
use crate::protocol::Protocol;
use crate::HandlerFunction;
use crate::{debug, edebug};

type HandlerMap = HashMap<Protocol, HandlerFunction>;
pub type IpSendMsg = (bool, IpPacket);

pub struct IpLayer {
  handlers: Arc<Mutex<HandlerMap>>,
  link_layer: LinkLayer,
  closed: Arc<AtomicBool>,
  send_handle: Option<thread::JoinHandle<()>>,
  ip_send_tx: Sender<IpSendMsg>,
  table: Arc<ForwardingTable>,
}

impl IpLayer {
  pub fn new(config: LnxConfig) -> IpLayer {
    let (ip_send_tx, ip_send_rx) = channel();
    let mut our_ip_addrs = HashSet::new();
    let link_layer = LinkLayer::new(config);

    for interface in link_layer.get_interfaces().iter() {
      our_ip_addrs.insert(interface.our_ip);
    }

    let mut node = IpLayer {
      handlers: Arc::new(Mutex::new(HashMap::new())),
      link_layer,
      closed: Arc::new(AtomicBool::new(false)),
      send_handle: None,
      table: Arc::new(ForwardingTable::new()),
      ip_send_tx,
    };

    let (link_send_tx, link_recv_rx) = node.link_layer.run();

    let handlers = node.handlers.clone();
    let ip_send_tx = node.ip_send_tx.clone();
    thread::spawn(move || IpLayer::recv_thread(link_recv_rx, handlers, our_ip_addrs, ip_send_tx));

    let closed = node.closed.clone();
    let table = node.table.clone();
    node.send_handle = Some(thread::spawn(move || {
      IpLayer::send_thread(ip_send_rx, link_send_tx, table, closed)
    }));

    node
  }

  fn close(&mut self) {
    self.closed.store(true, Ordering::SeqCst);
    if let Some(handle) = self.send_handle.take() {
      handle.join().unwrap_or_else(|e| {
        eprintln!("Got panic in send thread {:?}", e);
        ()
      })
    }
  }

  fn recv_thread(
    link_recv_rx: Receiver<(usize, IpPacket)>,
    handlers: Arc<Mutex<HandlerMap>>,
    our_ip_addrs: HashSet<Ipv4Addr>,
    ip_send_tx: Sender<IpSendMsg>,
  ) {
    loop {
      match link_recv_rx.recv() {
        Ok((interface, packet)) => {
          if our_ip_addrs.contains(&packet.destination_address()) {
            match IpLayer::handle_packet(&handlers, interface, &packet) {
              Ok(()) => (),
              Err(e) => edebug!("Packet handler errored: {e}"),
            }
          } else {
            debug!(
              "packet dst {:?}, forwarding...",
              packet.destination_address()
            );
            match ip_send_tx.send((false, packet)) {
              Ok(()) => (),
              Err(_e) => {
                edebug!("Connection closed, exiting node listen...");
                return;
              }
            }
          }
        }
        Err(_) => {
          edebug!("Connection closed, exiting node listen...");
          return;
        }
      }
    }
  }

  fn send_thread(
    ip_send_rx: Receiver<IpSendMsg>,
    link_send_tx: Sender<(Option<usize>, IpPacket)>,
    table: Arc<ForwardingTable>,
    closed: Arc<AtomicBool>,
  ) {
    loop {
      match ip_send_rx.recv_timeout(Duration::from_millis(100)) {
        Ok((broadcast, packet)) => {
          if !broadcast {
            let id = match table.get_next_hop(packet.destination_address()) {
              None => {
                edebug!(
                  "{:?}: No next interface for next hop, dropping...",
                  packet.destination_address()
                );
                continue;
              }
              Some(id) => id,
            };
            match link_send_tx.send((Some(id), packet)) {
              Ok(_) => (),
              Err(_e) => edebug!("Connection dropped, exiting node send..."),
            }
          } else {
            todo!();
          }
        }
        Err(RecvTimeoutError::Timeout) => {
          if closed.load(Ordering::SeqCst) {
            debug!("Connection closed, exiting node send...");
            break;
          }
        }
        Err(_) => {
          edebug!("Closing node send...");
          return;
        }
      }
    }
  }

  pub fn run(&mut self) -> Result<()> {
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
      if tokens.len() == 0 {
        continue;
      }

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
              edebug!("Error: Failed to create ip packet, {e}");
              continue;
            }
          };

          if let Err(e) = self.ip_send_tx.send((false, packet)) {
            edebug!("ip send thread closed unexpectedly, {e}");
            break;
          }
        }

        "up" | "down" => {
          if tokens.len() != 2 {
            eprintln!(
              "Error: '{}' expected 1 argument received {}",
              tokens[0],
              tokens.len() - 1
            );
            continue;
          }

          let interface_id: usize = match tokens[1].parse() {
            Ok(num) => num,
            Err(_) => {
              eprintln!("Error: interface id must be positive int");
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
            Err(e) => eprintln!("Error: setting interface status failed: {e}"),
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
    _interface: usize,
    packet: &IpPacket,
  ) -> Result<()> {
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

impl Drop for IpLayer {
  fn drop(&mut self) {
    self.close();
  }
}
