use etherparse::{Ipv4Header, TcpHeader};

pub mod recv_buffer;
mod ring_buffer;
pub mod send_buffer;
pub mod socket;
pub mod tcp_layer;
pub mod tcp_listener;
pub mod tcp_stream;

pub type Port = u16;
pub type TcpPacket = (TcpHeader, Vec<u8>);
pub type IpTcpPacket = (Ipv4Header, TcpHeader, Vec<u8>);

// https://www.ibm.com/docs/en/was-zos/8.5.5?topic=SS7K4U_8.5.5/com.ibm.websphere.nd.multiplatform.doc/ae/tprf_tunetcpip.html
// "The default buffer size is 8 KB. The maximum size is 8 MB (8096 KB). The optimal buffer size
// depends on several network environment factors including types of switches and systems,
// acknowledgment timing, error rates and network topology, memory size, and data transfer size.
// When data transfer size is extremely large, you might want to set the buffer sizes up to the
// maximum value to improve throughput, reduce the occurrence of flow control, and reduce CPU
// cost."
const TCP_BUF_SIZE: usize = u16::max_value() as usize;

// Max number of packets which can be in flight at a given time
const MAX_WINDOW_SIZE: usize = u16::max_value() as usize;

// See this thread for discussion of MTU choice
// https://stackoverflow.com/questions/2613734/maximum-packet-size-for-a-tcp-connection
const MTU: usize = 1408;
