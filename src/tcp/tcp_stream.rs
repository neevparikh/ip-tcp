use std::sync::mpsc::{Receiver, Sender};

use etherparse::TcpHeader;

const TCP_BUF_SIZE: usize = u16::max_value() as usize;

enum TcpStreamState {
  Closed,
  Listen,
  SynReceived,
  SynSent,
  Established,
  FinWait1,
  FinWait2,
  CloseWait,
  Closing,
}

pub struct TcpStream {
  /// Constant properties of the stream
  source_port:      u16,
  destination_port: u16,

  /// state
  current_state: TcpStreamState,
  /// TODO: what do we need to manage the circular buffers
  /// need to know what is in flight, what has been acked, what to send next,
  /// how many resends have been sent
  ///
  /// Idea: maybe have a window struct which is a smaller circular buffer which contains meta data
  /// about the current window?

  /// data
  recv_buffer:   [u8; TCP_BUF_SIZE],
  send_buffer:   [u8; TCP_BUF_SIZE],

  recv_rx: Receiver<(TcpHeader, Vec<u8>)>,
  send_tx: Sender<(TcpHeader, Vec<u8>)>,
}
