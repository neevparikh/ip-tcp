use std::collections::BTreeMap;
use std::net::Ipv4Addr;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::{thread, time};

use crate::ip_packet::IpPacket;
use crate::protocol::Protocol;
use crate::rip_message::{RipCommand, RipEntry, RipMsg, INFINITY_COST};
use crate::{debug, edebug, HandlerFunction, InterfaceId, IpSendMsg};

#[derive(Debug)]
struct RoutingEntry {
  next_hop: InterfaceId,
  cost: u8,
}

type InternalTable = Arc<Mutex<BTreeMap<Ipv4Addr, RoutingEntry>>>;
const SUBNET_MASK: u32 = u32::from_ne_bytes([255, 255, 255, 255]);

#[derive(Debug, Default)]
pub struct ForwardingTable {
  table: InternalTable,
}

impl ForwardingTable {
  pub fn new(
    ip_send_tx: Sender<IpSendMsg>,
    neighbors: Vec<(Ipv4Addr, InterfaceId, u8)>,
  ) -> ForwardingTable {
    let forwarding_table = ForwardingTable::default();
    let mut table = forwarding_table.table.lock().unwrap();
    for neighbor in neighbors {
      table.insert(
        neighbor.0,
        RoutingEntry {
          next_hop: neighbor.1,
          cost: neighbor.2,
        },
      );
    }
    drop(table);
    forwarding_table.start_send_keep_alive_thread(ip_send_tx);
    forwarding_table
  }

  /// Returns the handler function which updates the forwarding table based off of
  /// RIP packets
  pub fn get_rip_handler(&self) -> HandlerFunction {
    let _table_clone = Arc::clone(&self.table);
    Box::new(move |_ip_packet| {
      todo!();
    })
  }

  /// Takes in a ip address and returns the interface to send the packet to
  pub fn get_next_hop(&self, ip: Ipv4Addr) -> Option<InterfaceId> {
    let table = self.table.lock().unwrap();
    // don't leak the cost value
    match table.get(&ip) {
      Some(hop) => Some(hop.next_hop),
      None => None,
    }
  }

  pub fn start_send_keep_alive_thread(&self, ip_send_tx: Sender<IpSendMsg>) {
    let table = self.table.clone();
    thread::spawn(move || {
      debug!("Starting keep alive thread");
      loop {
        thread::sleep(time::Duration::from_secs(5));
        let mut rip_msg = RipMsg {
          command: RipCommand::Response,
          entries: Vec::new(),
        };

        for (dest_addr, route_entry) in table.lock().unwrap().iter() {
          rip_msg.entries.push(RipEntry {
            cost: route_entry.cost as u32,
            address: u32::from_ne_bytes(dest_addr.octets()),
            mask: SUBNET_MASK,
          });
        }

        for (i, (destination_address, route_entry)) in table.lock().unwrap().iter().enumerate() {
          if route_entry.cost == 1 {
            let tmp = rip_msg.entries[i].cost;
            rip_msg.entries[i].cost = INFINITY_COST;
            let data = rip_msg.pack();
            let packet = IpPacket::new_with_defaults(*destination_address, Protocol::RIP, &data);
            let packet = match packet {
              Ok(p) => p,
              Err(e) => {
                edebug!("{:?}\nCannot create IP packet from {:#?}", e, rip_msg);
                continue;
              }
            };
            debug!(
              "Sending packet to destination_address {:?}",
              destination_address
            );
            if let Err(_e) = ip_send_tx.send(packet) {
              debug!("IP layer send channel closed, exiting...");
              return;
            }
            rip_msg.entries[i].cost = tmp;
          }
        }
      }
    });
  }
}
