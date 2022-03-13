pub mod forwarding_table;
pub mod interface;
pub mod ip_layer;
pub mod ip_packet;
pub mod link_layer;
pub mod lnx_config;
pub mod protocol;
pub mod utils;

use ip_packet::IpPacket;
pub type HandlerFunction = Box<dyn Fn(&IpPacket) + Send>;
pub type InterfaceId = usize;

pub use ip_layer::IpSendMsg;
pub use link_layer::LinkRecvMsg;
pub use link_layer::LinkSendMsg;
