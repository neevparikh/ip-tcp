use std::net::{Ipv4Addr, SocketAddr};
use std::sync::mpsc::Sender;

use anyhow::Result;
use etherparse::{Ipv4Header, TcpHeader};

use super::{
  Port, SocketId, TcpStreamState, VALID_ACK_STATES, VALID_FIN_STATES, VALID_SEND_STATES,
  VALID_SYN_STATES,
};
use crate::ip::{IpPacket, Protocol};

#[derive(Debug)]
pub struct TcpStreamInternal {
  pub source_ip:               Option<Ipv4Addr>,
  /// Constant properties of the stream
  pub source_port:             u16,
  /// This should only be none if we are in the LISTEN state
  pub destination_ip:          Option<Ipv4Addr>,
  pub destination_port:        Option<u16>,
  pub initial_sequence_number: u32,
  /// This is set equal to the initial_sequence_number of the other node + 1
  pub initial_ack:             Option<u32>,

  /// Used to set sequence number on acks and ack number of data packets
  pub last_ack:         Option<u32>,
  /// Last window size
  pub last_window_size: u16,
  pub next_seq:         u32,

  /// state
  pub state:      TcpStreamState,
  pub socket_id:  SocketId,
  pub ip_send_tx: Sender<IpPacket>,
}

/// Helper function for validates valid state transitions, should be called at the
/// top of all (non-contructor) public methods
pub fn debug_assert_state_valid(curr_state: &TcpStreamState, valid_states: &[TcpStreamState]) {
  debug_assert!(valid_states.contains(curr_state))
}

impl TcpStreamInternal {
  pub fn send_syn(&mut self) -> Result<()> {
    debug_assert_state_valid(&self.state, &VALID_SYN_STATES);

    debug_assert!(self.destination_ip.is_some());
    debug_assert!(self.destination_port.is_some());

    let mut msg = self.make_default_tcp_header();
    msg.syn = true;
    msg.sequence_number = self.initial_sequence_number;

    self.send_tcp_packet(msg, &[])?;
    Ok(())
  }

  pub fn send_syn_ack(&mut self) -> Result<()> {
    debug_assert_state_valid(&self.state, &VALID_SYN_STATES);

    debug_assert!(self.destination_ip.is_some());
    debug_assert!(self.destination_port.is_some());

    let mut msg = self.make_default_tcp_header();
    msg.syn = true;
    msg.ack = true;
    msg.sequence_number = self.initial_sequence_number;
    msg.acknowledgment_number = self.initial_ack.unwrap();

    self.send_tcp_packet(msg, &[])?;
    Ok(())
  }

  pub fn send_ack(&mut self, ack: u32) -> Result<()> {
    debug_assert_state_valid(&self.state, &VALID_ACK_STATES);
    debug_assert!(self.destination_ip.is_some());
    debug_assert!(self.destination_port.is_some());
    let mut msg = self.make_default_tcp_header();
    msg.ack = true;
    msg.sequence_number = self.next_seq;
    msg.acknowledgment_number = ack;

    self.send_tcp_packet(msg, &[])?;
    Ok(())
  }

  pub fn send_fin(&mut self, seq_num: u32) -> Result<()> {
    debug_assert_state_valid(&self.state, &VALID_FIN_STATES);

    let mut msg = self.make_default_tcp_header();
    msg.fin = true;
    msg.sequence_number = seq_num;

    self.send_tcp_packet(msg, &[])?;
    Ok(())
  }

  pub fn send_data(&mut self, seq_num: u32, data: Vec<u8>) -> Result<()> {
    debug_assert_state_valid(&self.state, &VALID_SEND_STATES);

    let mut msg = self.make_default_tcp_header();
    msg.ack = true;
    msg.sequence_number = seq_num;
    msg.acknowledgment_number = self.last_ack.unwrap();

    self.send_tcp_packet(msg, &data)?;
    Ok(())
  }

  pub fn send_tcp_packet(&self, mut header: TcpHeader, data: &[u8]) -> Result<()> {
    debug_assert!(self.source_ip.is_some());
    debug_assert!(self.destination_ip.is_some());
    let mut ip_msg = IpPacket::new_with_defaults(self.destination_ip.unwrap(), Protocol::TCP, &[])?;
    ip_msg.set_source_address(self.source_ip.unwrap());
    let etherparse_header = Ipv4Header::from_slice(&ip_msg.pack()).unwrap().0;
    header.checksum = header.calc_checksum_ipv4(&etherparse_header, data).unwrap();

    let mut header_buf = Vec::new();
    header.write(&mut header_buf)?;
    let data = [&header_buf, data].concat();
    let mut ip_msg =
      IpPacket::new_with_defaults(self.destination_ip.unwrap(), Protocol::TCP, &data)?;
    ip_msg.set_source_address(self.source_ip.unwrap());

    self.ip_send_tx.send(ip_msg)?;
    Ok(())
  }

  pub fn make_default_tcp_header(&self) -> TcpHeader {
    debug_assert!(self.destination_port.is_some());
    TcpHeader::new(
      self.source_port,
      self.destination_port.unwrap(),
      self.initial_sequence_number,
      self.last_window_size,
    )
  }

  pub fn destination(&self) -> Option<SocketAddr> {
    match self.destination_ip.zip(self.destination_port) {
      Some(ip_port) => Some(ip_port.into()),
      None => None,
    }
  }

  pub fn source_ip(&self) -> Option<Ipv4Addr> {
    self.source_ip
  }

  pub fn source_port(&self) -> Port {
    self.source_port
  }

  /// If destination_ip is None then we set both fields, otherwise assert that
  /// the values were already set correctly
  pub fn set_destination_or_check(&mut self, destination_ip: Ipv4Addr, destination_port: Port) {
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

  /// set initial_ack based on syn
  pub fn set_initial_ack(&mut self, ack: u32) {
    self.initial_ack = Some(ack);
    self.last_ack = Some(ack);
  }

  /// set source_ip based on syn
  pub fn set_source_ip(&mut self, source_ip: Ipv4Addr) {
    self.source_ip = Some(source_ip);
  }

  /// get stream state
  pub fn state(&self) -> TcpStreamState {
    self.state
  }
}
