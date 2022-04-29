use std::time::Duration;

use etherparse::{Ipv4Header, TcpHeader};

mod recv_buffer;
mod ring_buffer;
mod send_buffer;
pub mod socket;
mod tcp_internal;
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
const TCP_BUF_SIZE: usize = 10; // u16::max_value() as usize;

// Max number of packets which can be in flight at a given time
const MAX_WINDOW_SIZE: usize = 10; // u16::max_value() as usize;

// See this thread for discussion of MTU choice
// https://stackoverflow.com/questions/2613734/maximum-packet-size-for-a-tcp-connection
const MTU: usize = 1408;

const MAX_SEGMENT_LIFETIME: Duration = Duration::from_secs(1);

#[derive(Debug, Copy, Clone, PartialEq, Hash, Eq)]
pub enum TcpStreamState {
  Closed, // RFC describes CLOSED as a fictitious state, we use it internally for cleanup
  Listen,
  SynReceived,
  SynSent,
  Established,
  FinWait1,
  FinWait2,
  CloseWait,
  TimeWait,
  LastAck,
  Closing,
}

/// Note that these should be thought of as the valid states to retry sending a syn packet, which
/// is while SynSent is in there
const VALID_SYN_STATES: [TcpStreamState; 3] = [
  TcpStreamState::Listen,
  TcpStreamState::SynReceived,
  TcpStreamState::SynSent,
];
/// Again these are states we should be okay retrying a fin
const VALID_FIN_STATES: [TcpStreamState; 6] = [
  TcpStreamState::SynReceived,
  TcpStreamState::Established,
  TcpStreamState::FinWait1, // Note there might be a race condition where FinWait2 happens
  TcpStreamState::Closing,
  TcpStreamState::CloseWait,
  TcpStreamState::LastAck,
];
const VALID_SEND_STATES: [TcpStreamState; 2] =
  [TcpStreamState::Established, TcpStreamState::CloseWait];
const VALID_RECV_STATES: [TcpStreamState; 7] = [
  TcpStreamState::Listen,
  TcpStreamState::SynSent,
  TcpStreamState::SynReceived,
  TcpStreamState::Established,
  TcpStreamState::FinWait1,
  TcpStreamState::FinWait2,
  TcpStreamState::CloseWait,
];
const VALID_ACK_STATES: [TcpStreamState; 5] = [
  TcpStreamState::SynSent,
  TcpStreamState::FinWait1,
  TcpStreamState::FinWait2,
  TcpStreamState::CloseWait,
  TcpStreamState::Established,
];
