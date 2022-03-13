use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::str::FromStr;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use crate::ip_packet::IpPacket;
use crate::{HandlerFunction, InterfaceId};

#[derive(Debug, Default)]
pub struct ForwardingTable {
  pub table: Arc<Mutex<HashMap<Ipv4Addr, (InterfaceId, u8)>>>,
}

impl ForwardingTable {
  pub fn new() -> ForwardingTable {
    let forwarding_table = ForwardingTable::default();
    {
      let mut table = forwarding_table.table.lock().unwrap();
      table.insert(Ipv4Addr::from_str("192.168.0.4").unwrap(), (1, 0)); // For B
    }
    forwarding_table
  }

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
    // don't leak the cost value
    match table.get(&ip) {
      Some(hop) => Some(hop.0),
      None => None,
    }
  }

  pub fn start_send_keep_alive_thread(send_tx: Sender<(Option<InterfaceId>, IpPacket)>) {}
}
