pub mod interface;
pub mod ip_packet;
pub mod link_layer;
pub mod lnx_config;
pub mod node;
pub mod protocol;
pub mod utils;
pub mod driver;
pub mod forwarding_table;

use std::collections::HashMap;
use ip_packet::IpPacket;
use protocol::Protocol;
pub type HandlerFunction = Box<dyn (Fn(&IpPacket) -> Option<IpPacket>) + Send>;
type HandlerMap = HashMap<Protocol, HandlerFunction>;
