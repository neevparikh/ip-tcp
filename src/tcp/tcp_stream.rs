use std::sync::mpsc::{self, Receiver, Sender}
use std::net::Ipv4Addr;

use etherparse::TcpHeader;
use rand::random;

use super::{Port, TcpPacket};
use crate::ip::protocol::Protocol;
use crate::{IpSendMsg, IpPacket};

const TCP_BUF_SIZE: usize = u16::max_value() as usize;
const MAX_WINDOW_SIZE: u16 = u16::max_value();

enum TcpStreamState {
  /// TODO: should we have this RFC describes CLOSED as a fictitious state
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
  source_port:             u16,
  /// This should only be none if we are in the LISTEN state
  destination_ip:          Option<Ipv4Addr>,
  destination_port:        Option<u16>,
  initial_sequence_number: u32,
  /// This is set equal to the initial_sequence_number of the other node
  initial_ack:             Option<u32>,

  /// state
  state: TcpStreamState,

  /// TODO: what do we need to manage the circular buffers
  /// need to know what is in flight, what has been acked, what to send next,
  /// how many resends have been sent
  ///
  /// Idea: maybe have a window struct which is a smaller circular buffer which contains meta data
  /// about the current window?

  /// data
  recv_buffer: [u8; TCP_BUF_SIZE],
  send_buffer: [u8; TCP_BUF_SIZE],

  /// TODO: how should commands like SHUTDOWN and CLOSE be passed
  recv_rx:    Receiver<TcpPacket>,
  ip_send_tx: Sender<IpSendMsg>,
}

impl TcpStream {
  fn new(
    source_port: Port,
    destination_ip: Option<Ipv4Addr>,
    destination_port: Option<Port>,
    initial_state: TcpStreamState,
    ip_send_tx: Sender<IpSendMsg>,
  ) -> (TcpStream, Sender<TcpPacket>) {
    let (recv_tx, recv_rx) = mpsc::channel();
    (
      TcpStream {
        source_port,
        destination_ip,
        destination_port,
        initial_sequence_number: random(),
        initial_ack: None,
        state: TcpStreamState::Listen,
        recv_buffer: [0u8; TCP_BUF_SIZE],
        send_buffer: [0u8; TCP_BUF_SIZE],
        recv_rx,
        ip_send_tx,
      },
      recv_tx,
    )
  }
  pub fn new_listener(
    source_port: Port,
    ip_send_tx: Sender<IpSendMsg>,
  ) -> (TcpStream, Sender<TcpPacket>) {
    TcpStream::new(source_port, None, None, TcpStreamState::Listen, ip_send_tx)
  }

  pub fn new_connect(
    source_port: Port,
    destination_ip: Ipv4Addr,
    destination_port: Port,
    ip_send_tx: Sender<IpSendMsg>,
  ) -> (TcpStream, Sender<TcpPacket>) {
    let (new_stream, recv_tx) = TcpStream::new(
      source_port,
      Some(destination_ip),
      Some(destination_port),
      TcpStreamState::Closed,
      ip_send_tx,
    );

    new_stream.send_syn(destination_ip, destination_port);
    (new_stream, recv_tx)
  }

  fn send_syn(&self, destination_ip: Ipv4Addr, destination_port: Port) -> Result<()> {
    debug_assert_state_valid(
      &self.state,
      vec![TcpStreamState::Closed, TcpStreamState::Listen],
    );

    self.set_destination_or_check(destination_ip, destination_port);

    let msg = self.get_default_tcp_header();
    msg.syn = true;

    let ip_msg = IpPacket::new_with_defaults(destination_ip, Protocol::Tcp, &[])?;
    self.ip_send_tx.send(ip_msg)?;
    self.state = TcpStreamState::SynSent;
    Ok(())
  }

  fn send_syn_ack(&self, destination_ip: Ipv4Addr, destination_port: Port) -> Result<()> {
    debug_assert_state_valid(
      &self.state,
      vec![TcpStreamState::Listen, TcpStreamState::SynSent],
    );

    self.set_destination_or_check(destination_ip, destination_port);

    let msg = self.get_default_tcp_header();
    msg.syn = true;
    msg.ack = true;
    msg.sequence_number = self.initial_sequence_number;
    msg.acknowledgment_number = self.initial_ack.unwrap() + 1;

    self.send_tcp_packet(msg, &[]);
    self.state = TcpStreamState::SynSent;
    Ok(())
  }

  fn send_tcp_packet(&self, header: TcpHeader, data: &[u8]) -> Result<()> {
    let ip_msg = IpPacket::new_with_defaults(destination_ip, Protocol::Tcp, data)?;
    self.ip_send_tx.send(ip_msg)?;
    Ok(())
  }

  fn get_default_tcp_header(&self) -> TcpHeader {
    debug_assert!(self.destination_port.is_some());
    TcpHeader::new(
      self.source_port,
      self.destination_port.unwrap(),
      self.initial_sequence_number,
      MAX_WINDOW_SIZE,
    )
  }

  /// If destination_ip is None then we set both fields, otherwise assert that
  /// the values were already set correctly
  fn set_destination_or_check(&self, destination_ip: Ipv4Addr, destination_port: Port) {
    match self.destination_ip {
      Some(ip) => {
        debug_assert!(ip == destination_ip);
        debug_assert!(self.destination_port.is_some());
        debug_assert!(self.destination_port.unwrap() == destination_port);
      }
      None => {
        debug_assert!(self.destination_port.is_none());
        self.destination_ip = Some(destination_ip);
        self.destination_port = Some(destination_port);
      }
    }

  }
}

/// Helper function for validates valid state transitions, should be called at the
/// top of all (non-contructor) public methods
fn debug_assert_state_valid(curr_state: &TcpStreamState, valid_states: Vec<TcpStreamState>) {
  debug_assert!(valid_states.contains(curr_state))
}
