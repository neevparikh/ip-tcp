use std::cmp;
use std::collections::BTreeMap;
use std::fmt;
use std::net::Ipv4Addr;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use crate::ip_packet::IpPacket;
use crate::protocol::Protocol;
use crate::rip_message::{RipCommand, RipEntry, RipMsg, INFINITY_COST};
use crate::{debug, edebug, HandlerFunction, InterfaceId, IpSendMsg};

type InternalTable = Arc<Mutex<BTreeMap<Ipv4Addr, RoutingEntry>>>;
type InterfaceTable = Arc<Mutex<BTreeMap<Ipv4Addr, InterfaceId>>>;
const SUBNET_MASK: u32 = u32::from_ne_bytes([255, 255, 255, 255]);

#[derive(Debug, Default)]
pub struct ForwardingTable {
  table: InternalTable,
  neighbors: InterfaceTable,
  our_interfaces: InterfaceTable,
}

impl ForwardingTable {
  pub fn new(
    ip_send_tx: Sender<IpSendMsg>,
    neighbors: Vec<(Ipv4Addr, InterfaceId)>,
    our_interfaces: Vec<(Ipv4Addr, InterfaceId)>,
  ) -> ForwardingTable {
    let forwarding_table = ForwardingTable::default();
    let mut table = forwarding_table.table.lock().unwrap();
    for our_interface in our_interfaces.iter() {
      table.insert(
        our_interface.0,
        RoutingEntry {
          next_hop: our_interface.1,
          cost: 0,
          expiration_time: None,
        },
      );
    }
    drop(table);

    let mut neighbor_table = forwarding_table.neighbors.lock().unwrap();
    *neighbor_table = neighbors.into_iter().collect();
    drop(neighbor_table);

    let mut our_interfaces_table = forwarding_table.our_interfaces.lock().unwrap();
    *our_interfaces_table = our_interfaces.into_iter().collect();
    drop(our_interfaces_table);

    ForwardingTable::start_reaper_thread(forwarding_table.table.clone());

    forwarding_table.start_send_keep_alive_thread(ip_send_tx);
    forwarding_table
  }

  pub fn get_table(&self) -> MutexGuard<BTreeMap<Ipv4Addr, RoutingEntry>> {
    self.table.lock().unwrap()
  }

  /// Returns the handler function which updates the forwarding table based off of
  /// RIP packets
  pub fn get_rip_handler(&self) -> HandlerFunction {
    let table = self.table.clone();
    let neighbors = self.neighbors.clone();
    let our_interfaces = self.our_interfaces.clone();

    Box::new(move |ip_packet| {
      let mut table = table.lock().unwrap();
      let neighbors = neighbors.lock().unwrap();
      let our_interfaces = our_interfaces.lock().unwrap();
      let rip_bytes = ip_packet.data();
      let rip_msg = match RipMsg::unpack(rip_bytes) {
        Ok(msg) => msg,
        Err(e) => {
          edebug!("{e}\nErrored unpacking rip message");
          return;
        }
      };

      // get senders interface number
      let source_interface = match neighbors.get(&ip_packet.source_address()) {
        Some(interface) => *interface,
        // should only receive RIP messages from neighbors
        None => return,
      };

      match rip_msg.command {
        RipCommand::Request => {
          debug_assert!(rip_msg.entries.len() == 0);
          todo!();
        }
        RipCommand::Response => {
          for entry in rip_msg.entries {
            let addr = Ipv4Addr::from(entry.address);
            debug_assert!(entry.cost <= INFINITY_COST);
            let new_cost = cmp::min(entry.cost + 1, INFINITY_COST) as u8;
            let new_routing_entry = RoutingEntry {
              cost: new_cost,
              next_hop: source_interface,
              expiration_time: Some(Instant::now() + Duration::from_secs(12)),
            };

            match table.get_mut(&addr) {
              Some(curr_route) => {
                if !our_interfaces.contains_key(&addr)
                  && (new_cost < curr_route.cost
                    || curr_route.next_hop == new_routing_entry.next_hop)
                {
                  *curr_route = new_routing_entry;
                }
              }
              None => {
                table.insert(addr, new_routing_entry);
              }
            }
          }
        }
      }
    })
  }

  /// Takes in a ip address and returns the interface to send the packet to
  pub fn get_next_hop(&self, ip: Ipv4Addr) -> Option<InterfaceId> {
    let table = self.table.lock().unwrap();
    if let Some(hop) = table.get(&ip) {
      // don't leak the cost value
      return Some(hop.next_hop);
    }
    drop(table);

    // Also check if they are our neighbor
    let neighbors = self.neighbors.lock().unwrap();
    neighbors.get(&ip).cloned()
  }

  pub fn start_send_keep_alive_thread(&self, ip_send_tx: Sender<IpSendMsg>) {
    let table = self.table.clone();
    let neighbors = self.neighbors.clone();
    thread::spawn(move || {
      debug!("Starting keep alive thread");
      loop {
        thread::sleep(Duration::from_secs(5));

        for (destination_address, interface_id) in neighbors.lock().unwrap().iter() {
          let mut rip_msg = RipMsg {
            command: RipCommand::Response,
            entries: Vec::new(),
          };
          // I don't think there is a good way to avoid the double for loop
          for (curr_dest, route_entry) in table.lock().unwrap().iter() {
            // Poison reverse
            let cost = if &route_entry.next_hop == interface_id && route_entry.cost != 0 {
              INFINITY_COST
            } else {
              route_entry.cost as u32
            };
            rip_msg.entries.push(RipEntry {
              cost,
              address: u32::from_be_bytes(curr_dest.octets()),
              mask: SUBNET_MASK,
            });
          }

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
        }
      }
    });
  }

  /// Starts a new thread which goes through all the entries in the forwarding table
  /// and checks if they need to be reaped (i.e. cost to infinity)
  ///
  /// Note: could do something smarter with a priority queue sorted by expiration times
  /// but I think that would be premature optimization.
  fn start_reaper_thread(table: InternalTable) {
    thread::spawn(move || loop {
      thread::sleep(Duration::from_secs(1));
      let mut table = table.lock().unwrap();
      let now = Instant::now();
      table
        .iter_mut()
        .for_each(|(_k, v)| match v.expiration_time {
          Some(t) => {
            if t < now {
              v.cost = INFINITY_COST as u8;
            }
          }
          None => (),
        });
    });
  }
}

#[derive(Debug)]
pub struct RoutingEntry {
  next_hop: InterfaceId,
  cost: u8,
  expiration_time: Option<Instant>,
}

impl fmt::Display for RoutingEntry {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    if cfg!(debug_assertions) {
      let now = Instant::now();
      let time_remaining = match self.expiration_time {
        Some(t) => {
          if t < now {
            Some(0)
          } else {
            Some((t - now).as_secs())
          }
        }
        None => None,
      };
      write!(
        f,
        "next_hop {}, cost {}, time_remaining {:?}",
        self.next_hop, self.cost, time_remaining
      )
    } else {
      write!(f, "next_hop {}, cost {}", self.next_hop, self.cost)
    }
  }
}
