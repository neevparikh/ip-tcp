pub mod ip;
pub mod link;
pub mod misc;
pub mod tcp;

pub use crate::ip::ip_packet::IpPacket;
pub type HandlerFunction = Box<dyn Fn(&IpPacket) + Send>;
pub type InterfaceId = usize;

pub use crate::ip::ip_layer::IpSendMsg;
pub use crate::link::link_layer::{LinkRecvMsg, LinkSendMsg};
