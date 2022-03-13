use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use ipnet::Ipv4Net;

use crate::ip_packet::IpPacket;
use crate::{HandlerFunction, InterfaceId};

#[derive(Debug, Default)]
pub struct ForwardingTable {
  pub table: Arc<Mutex<HashMap<Ipv4Net, (InterfaceId, u8)>>>,
}

impl ForwardingTable {
  /// Returns the handler function which updates the forwarding table based off of
  /// RIP packets
  pub fn get_rip_handler(&self) -> HandlerFunction {
    let table_clone = Arc::clone(&self.table);
    Box::new(move |ip_packet| {
      todo!();
    })
  }

  /// Takes in a ip address and returns the interface to send the packet to
  pub fn get_next_hop(&self, ip: Ipv4Addr) -> Option<InterfaceId> {
    let table = self.table.lock().unwrap();

    // TODO: there has to be a better way to do this:
    let mut next_hop = None;
    for prefix_len in (0..=32).rev() {
      let ip_net = Ipv4Net::new(ip, prefix_len).unwrap();
      if let Some(&hop) = table.get(&ip_net) {
        next_hop = Some(hop);
      }
    }

    // don't leak the cost value
    match next_hop {
      Some(hop) => Some(hop.0),
      None => None,
    }
  }

  pub fn start_send_keep_alive_thread(send_tx: Sender<(Option<InterfaceId>, IpPacket)>) {}
}
