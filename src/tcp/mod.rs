use etherparse::TcpHeader;

pub mod socket;
pub mod tcp_layer;
pub mod tcp_listener;
pub mod tcp_stream;

pub type Port = u16;
pub type TcpPacket = (TcpHeader, Vec<u8>);
