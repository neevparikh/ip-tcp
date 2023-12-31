use std::collections::BTreeMap;
use std::net::Ipv4Addr;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, MutexGuard, RwLock};
use std::time::{Duration, Instant};
use std::{cmp, fmt, thread};

use anyhow::Result;

use super::ip_packet::IpPacket;
use super::protocol::Protocol;
use super::rip_message::{RipCommand, RipEntry, RipMsg, INFINITY_COST};
use super::HandlerFunction;
use crate::link::interface::{Interface, InterfaceId, State};
use crate::link::link_layer::SharedInterfaces;
use crate::{debug, edebug};

type PartialTable = BTreeMap<Ipv4Addr, RoutingEntry>;
type InternalTable = Arc<Mutex<PartialTable>>;
type SharedInterfaceTable = Arc<BTreeMap<Ipv4Addr, InterfaceId>>;
const SUBNET_MASK: u32 = u32::from_ne_bytes([255, 255, 255, 255]);

#[derive(Debug)]
pub struct ForwardingTable {
  table:          InternalTable,
  neighbors:      SharedInterfaceTable,
  our_interfaces: SharedInterfaceTable,
  interface_map:  SharedInterfaces,
}

#[derive(Debug, Copy, Clone)]
pub struct RoutingEntry {
  pub next_hop:    InterfaceId,
  pub cost:        u8,
  expiration_time: Option<Instant>,
}

impl ForwardingTable {
  pub fn new(
    ip_send_tx: Sender<IpPacket>,
    interface_map: Arc<RwLock<Vec<Interface>>>,
  ) -> (HandlerFunction, ForwardingTable) {
    let unlocked_interfaces = interface_map.read().unwrap();
    let table: InternalTable = Arc::new(Mutex::new(
      unlocked_interfaces
        .iter()
        .map(|interface| {
          (
            interface.our_ip,
            RoutingEntry {
              next_hop:        interface.id,
              cost:            0,
              expiration_time: None,
            },
          )
        })
        .collect(),
    ));

    let neighbors: SharedInterfaceTable = Arc::new(
      unlocked_interfaces
        .iter()
        .map(|interface| (interface.their_ip, interface.id))
        .collect(),
    );

    let our_interfaces: SharedInterfaceTable = Arc::new(
      unlocked_interfaces
        .iter()
        .map(|interface| (interface.our_ip, interface.id))
        .collect(),
    );
    drop(unlocked_interfaces);

    let forwarding_table = ForwardingTable {
      table,
      neighbors,
      our_interfaces,
      interface_map,
    };

    forwarding_table.start_reaper_thread(ip_send_tx.clone());
    forwarding_table.start_send_keep_alive_thread(ip_send_tx.clone());

    if let Err(_e) =
      ForwardingTable::make_request_and_send_to_all(&ip_send_tx, forwarding_table.neighbors.clone())
    {
      edebug!("Error in make_and_send_to_all");
    }

    (
      forwarding_table.get_rip_handler(ip_send_tx.clone()),
      forwarding_table,
    )
  }

  pub fn get_table(&self) -> MutexGuard<BTreeMap<Ipv4Addr, RoutingEntry>> {
    self.table.lock().unwrap()
  }

  /// Returns the handler function which updates the forwarding table based off of
  /// RIP packets
  pub fn get_rip_handler(&self, ip_send_tx: Sender<IpPacket>) -> HandlerFunction {
    let table = self.table.clone();
    let neighbors = self.neighbors.clone();
    let our_interfaces = self.our_interfaces.clone();

    Box::new(move |ip_packet| {
      let mut table = table.lock().unwrap();
      let rip_bytes = ip_packet.data();
      let rip_msg = match RipMsg::unpack(rip_bytes) {
        Ok(msg) => msg,
        Err(e) => {
          edebug!("{e}");
          edebug!("Errored unpacking rip message");
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
          let table = table.clone();
          if let Err(e) = ForwardingTable::make_response_and_send_to_all(
            &ip_send_tx,
            &neighbors,
            table,
            &our_interfaces,
          ) {
            edebug!("{}", e);
            edebug!("Error in sending to all in rip handler");
          }
        }
        RipCommand::Response => {
          let mut updated_entries: PartialTable = PartialTable::new();

          for entry in rip_msg.entries {
            let addr = Ipv4Addr::from(entry.address);
            debug_assert!(entry.cost <= INFINITY_COST);
            let new_cost = cmp::min(entry.cost + 1, INFINITY_COST);
            let new_routing_entry = RoutingEntry {
              cost:            new_cost,
              next_hop:        source_interface,
              expiration_time: Some(Instant::now() + Duration::from_secs(12)),
            };

            match table.get_mut(&addr) {
              Some(curr_route) => {
                let different_cost = new_cost != curr_route.cost;
                let not_us = !our_interfaces.contains_key(&addr);
                let better_cost = new_cost < curr_route.cost;
                let same_next_hop = curr_route.next_hop == new_routing_entry.next_hop;
                debug_assert!(!(!not_us && better_cost));
                if better_cost || (not_us && same_next_hop) {
                  *curr_route = new_routing_entry;
                  if different_cost {
                    updated_entries.insert(addr, new_routing_entry);
                  }
                }
              }
              None => {
                table.insert(addr, new_routing_entry);
                updated_entries.insert(addr, new_routing_entry);
              }
            }
          }

          if updated_entries.len() > 0 {
            if let Err(e) = ForwardingTable::make_response_and_send_to_all(
              &ip_send_tx,
              &neighbors,
              updated_entries,
              &our_interfaces,
            ) {
              edebug!("{}", e);
              edebug!("Error in sending to all in rip handler");
            }
          }
        }
      }
    })
  }

  /// Takes in a ip address and returns the interface to send the packet to
  pub fn get_next_hop(&self, ip: Ipv4Addr) -> Option<InterfaceId> {
    match self.neighbors.get(&ip) {
      Some(&interface) => {
        // This avoids the issue of not being able to rediscover a revived link
        let unlocked_interfaces = self.interface_map.read().unwrap();
        if unlocked_interfaces[interface].state() == State::UP {
          Some(interface)
        } else {
          None
        }
      }
      None => {
        let table = self.table.lock().unwrap();
        match table.get(&ip) {
          // don't leak the cost value
          Some(hop) if hop.cost < INFINITY_COST => Some(hop.next_hop),
          _ => None,
        }
      }
    }
  }

  pub fn start_send_keep_alive_thread(&self, ip_send_tx: Sender<IpPacket>) {
    let table = self.table.clone();
    let neighbors = self.neighbors.clone();
    let our_interfaces = self.our_interfaces.clone();
    thread::spawn(move || {
      // debug!("Starting keep alive thread");
      loop {
        let table = table.lock().unwrap().clone();
        if let Err(e) = ForwardingTable::make_response_and_send_to_all(
          &ip_send_tx,
          &neighbors,
          table,
          &our_interfaces,
        ) {
          edebug!("{}", e);
          debug!("IP layer send channel closed, exiting...");
          return;
        }
        thread::sleep(Duration::from_secs(5));
      }
    });
  }

  /// Starts a new thread which goes through all the entries in the forwarding table
  /// and checks if they need to be reaped (i.e. cost to infinity)
  ///
  /// Note: could do something smarter with a priority queue sorted by expiration times
  /// but I think that would be premature optimization.
  fn start_reaper_thread(&self, ip_send_tx: Sender<IpPacket>) {
    let table = self.table.clone();
    let interface_map = self.interface_map.clone();
    let neighbors = self.neighbors.clone();
    let our_interfaces = self.our_interfaces.clone();

    thread::spawn(move || loop {
      thread::sleep(Duration::from_millis(20));
      let interfaces = interface_map.read().unwrap();
      let mut table = table.lock().unwrap();
      let now = Instant::now();
      let mut updated_entries = PartialTable::new();
      table
        .iter_mut()
        .for_each(|(addr, route_entry)| match our_interfaces.get(&addr) {
          Some(&interface_id) => {
            debug_assert!(interface_id == route_entry.next_hop);
            let old_cost = route_entry.cost;
            if interfaces[interface_id].state() == State::UP {
              route_entry.cost = 0;
            } else {
              route_entry.cost = INFINITY_COST;
            }
            if old_cost != route_entry.cost {
              updated_entries.insert(*addr, *route_entry);
            }
          }
          None => {
            // It is safe to unwrap because only routes to ourselves don't expire
            if route_entry.expiration_time.unwrap() < now && route_entry.cost < INFINITY_COST {
              route_entry.cost = INFINITY_COST;
              updated_entries.insert(*addr, *route_entry);
            }
          }
        });
      if updated_entries.len() > 0 {
        if let Err(e) = ForwardingTable::make_response_and_send_to_all(
          &ip_send_tx,
          &neighbors,
          updated_entries,
          &our_interfaces,
        ) {
          edebug!("{}", e);
          debug!("ip layer closed, exiting...");
          break;
        }
      }
    });
  }

  fn make_request_and_send_to_all(
    ip_send_tx: &Sender<IpPacket>,
    neighbors: SharedInterfaceTable,
  ) -> Result<()> {
    for (destination_address, _interface_id) in neighbors.iter() {
      let rip_msg = RipMsg {
        command: RipCommand::Request,
        entries: Vec::new(),
      };
      // debug!(
      //   "Sending request to destination_address {:?}",
      //   destination_address
      // );
      ForwardingTable::make_and_send(rip_msg, &ip_send_tx, destination_address.clone())?
    }
    Ok(())
  }

  fn make_response_and_send_to_all(
    ip_send_tx: &Sender<IpPacket>,
    neighbors: &SharedInterfaceTable,
    table: PartialTable,
    our_interfaces: &SharedInterfaceTable,
  ) -> Result<()> {
    for (destination_address, interface_id) in neighbors.iter() {
      let mut rip_msg = RipMsg {
        command: RipCommand::Response,
        entries: Vec::new(),
      };
      // I don't think there is a good way to avoid the double for loop
      for (curr_dest, route_entry) in table.iter() {
        // Poison reverse
        let is_our_interface = our_interfaces.contains_key(&curr_dest);
        let cost = if &route_entry.next_hop == interface_id && !is_our_interface {
          INFINITY_COST
        } else {
          route_entry.cost
        };
        rip_msg.entries.push(RipEntry {
          cost,
          address: u32::from_be_bytes(curr_dest.octets()),
          mask: SUBNET_MASK,
        });
      }
      // debug!(
      //   "Sending response to destination_address {:?}",
      //   destination_address
      // );

      ForwardingTable::make_and_send(rip_msg, &ip_send_tx, destination_address.clone())?
    }
    Ok(())
  }

  fn make_and_send(
    rip_msg: RipMsg,
    ip_send_tx: &Sender<IpPacket>,
    destination_address: Ipv4Addr,
  ) -> Result<()> {
    let data = rip_msg.pack();
    let packet = IpPacket::new_with_defaults(destination_address, Protocol::RIP, &data);
    let packet = match packet {
      Ok(p) => p,
      Err(e) => {
        edebug!("{:?}\nCannot create IP packet from {:#?}", e, rip_msg);
        return Ok(());
      }
    };
    ip_send_tx.send(packet)?;
    Ok(())
  }
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
