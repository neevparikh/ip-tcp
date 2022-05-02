mod forwarding_table;
mod ip_layer;
mod ip_packet;
mod protocol;
mod rip_message;

pub type HandlerFunction = Box<dyn Fn(&ip_packet::IpPacket) + Send>;
pub use ip_layer::IpLayer;
pub use ip_packet::IpPacket;
pub use protocol::Protocol;
