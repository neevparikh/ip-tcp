use std::net::{Ipv4Addr, SocketAddr};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::{thread, vec};

use anyhow::Result;
use etherparse::{Ipv4Header, TcpHeader};
use rand::random;

use super::recv_buffer::RecvBuffer;
use super::send_buffer::SendBuffer;
use super::{IpTcpPacket, Port};
use crate::ip::protocol::Protocol;
use crate::{debug, edebug, IpPacket};

pub type LockedTcpStream = Arc<Mutex<TcpStream>>;

#[derive(Debug, Copy, Clone, PartialEq)]
pub enum TcpStreamState {
  /// TODO: should we have this, RFC describes CLOSED as a fictitious state
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

pub(super) enum StreamSendThreadMsg {
  /// sequence number, data
  Outgoing(u32, Vec<u8>),
  Ack(u32),
}

#[derive(Debug)]
pub struct TcpStream {
  /// TODO: how should we be actually setting this
  source_ip:               Option<Ipv4Addr>,
  /// Constant properties of the stream
  source_port:             u16,
  /// TODO: switch to net::SocketAddr
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
  recv_buffer: RecvBuffer,
  send_buffer: SendBuffer,

  /// TODO: how should commands like SHUTDOWN and CLOSE be passed
  /// Selecting???
  stream_tx:  Sender<IpTcpPacket>, // thing for others to send this stream a packet
  ip_send_tx: Sender<IpPacket>,
}

impl TcpStream {
  fn new(
    // TODO, do we need this? source_ip: Ipv4Addr,
    source_ip: Option<Ipv4Addr>,
    source_port: Port,
    destination_ip: Option<Ipv4Addr>,
    destination_port: Option<Port>,
    initial_state: TcpStreamState,
    ip_send_tx: Sender<IpPacket>,
  ) -> LockedTcpStream {
    let (stream_tx, stream_rx) = mpsc::channel();
    let (send_thread_tx, send_thread_rx) = mpsc::channel();

    let initial_sequence_number = random(); // TODO: switch to ISN being clock time
    let stream = Arc::new(Mutex::new(TcpStream {
      source_ip,
      source_port,
      destination_ip,
      destination_port,
      initial_sequence_number,
      initial_ack: None,
      state: initial_state,
      recv_buffer: RecvBuffer::new(send_thread_tx.clone(), initial_sequence_number),
      send_buffer: SendBuffer::new(send_thread_tx.clone(), initial_sequence_number),
      stream_tx,
      ip_send_tx,
    }));

    TcpStream::start_listen_thread(stream.clone(), stream_rx);
    TcpStream::start_send_thread(stream.clone(), send_thread_rx);
    stream
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

  pub fn listen(source_port: Port, ip_send_tx: Sender<IpPacket>) -> LockedTcpStream {
    TcpStream::new(
      None,
      source_port,
      None,
      None,
      TcpStreamState::Listen,
      ip_send_tx,
    )
  }

  pub fn connect(
    source_ip: Ipv4Addr,
    source_port: Port,
    destination_ip: Ipv4Addr,
    destination_port: Port,
    ip_send_tx: Sender<IpPacket>,
  ) -> Result<LockedTcpStream> {
    let new_stream = TcpStream::new(
      Some(source_ip),
      source_port,
      Some(destination_ip),
      Some(destination_port),
      TcpStreamState::Closed,
      ip_send_tx,
    );

    new_stream
      .lock()
      .unwrap()
      .send_syn(destination_ip, destination_port)?;

    Ok(new_stream)
  }

  /// Called by TcpLayer when it receives a packet for this stream, sends to listen thread to
  /// process
  pub fn process(&mut self, packet: IpTcpPacket) -> Result<()> {
    self.stream_tx.send(packet)?;
    Ok(())
  }

  /// Called by TcpLayer
  pub fn send(&self, data: &[u8]) -> Result<()> {
    self.send_buffer.write_data(data)?;
    Ok(())
  }

  /// Called by TcpLayer
  pub fn recv(&mut self, num_bytes: usize) -> Result<Vec<u8>> {
    let mut data = vec![0u8; num_bytes];
    self.recv_buffer.read_data(&mut data)?;
    Ok(data)
  }

  /// TODO: I'm not the biggest fan of this design because we get a huge amount of contention when
  /// we are sending and receiving at the same time
  fn start_send_thread(tcp_stream: LockedTcpStream, send_thread_rx: Receiver<StreamSendThreadMsg>) {
    thread::spawn(move || loop {
      match send_thread_rx.recv() {
        Ok(StreamSendThreadMsg::Outgoing(seq_num, data)) => {
          if let Err(e) = tcp_stream.lock().unwrap().send_data(seq_num, data) {
            edebug!("Failed to send data with seq_num {seq_num}");
            break;
          }
        }
        Ok(StreamSendThreadMsg::Ack(ack_num)) => {
          if let Err(e) = tcp_stream.lock().unwrap().send_ack_for_data(ack_num) {
            edebug!("Failed to send {ack_num} for data");
            break;
          }
        }
        Err(_) => {
          edebug!("Closing streams send thread");
          break;
        }
      }
    });
  }

  fn start_listen_thread(tcp_stream: LockedTcpStream, stream_rx: Receiver<IpTcpPacket>) {
    thread::spawn(move || loop {
      // TODO: should this be timeout
      match stream_rx.recv() {
        Ok((ip_header, tcp_header, data)) => {
          let mut stream = tcp_stream.lock().unwrap();
          match stream.state {
            TcpStreamState::Listen => {
              if tcp_header.syn {
                // TODO: or should this be packet.destination_ip?
                let ip: Ipv4Addr = ip_header.source.into();
                let port = tcp_header.source_port;

                stream.set_initial_ack(tcp_header.sequence_number);
                stream.set_source_ip(ip_header.destination.into());
                match stream.send_syn_ack(ip, port) {
                  Ok(()) => {
                    stream.state = TcpStreamState::SynReceived;
                  }
                  // TODO: what to do in the case that you can't send the syn_ack??
                  Err(e) => edebug!("Could not send syn_ack? {e}"),
                }
              }
            }
            TcpStreamState::SynReceived => {
              // TODO: check packet.acknowledgment_number and where to keep track of current ack?
              // Needs sliding window??
              if tcp_header.ack {
                stream.state = TcpStreamState::Established;
              }
            }
            TcpStreamState::SynSent => {
              // TODO: or should this be packet.destination_ip?
              // TODO: what to do in the case that you can't send the syn_ack??
              let ip: Ipv4Addr = ip_header.source.into();
              let port = tcp_header.source_port;
              if tcp_header.ack && tcp_header.syn {
                stream.set_initial_ack(tcp_header.sequence_number);
                stream.set_source_ip(ip_header.destination.into());
                match stream.send_ack(ip, port) {
                  Ok(()) => {
                    stream.state = TcpStreamState::Established;
                  }
                  Err(e) => edebug!("could not send ack? {e}"),
                }
              } else if tcp_header.syn && !tcp_header.ack {
                stream.set_initial_ack(tcp_header.sequence_number);
                stream.set_source_ip(ip_header.destination.into());
                match stream.send_ack(ip, port) {
                  Ok(()) => {
                    stream.state = TcpStreamState::SynReceived;
                  }
                  Err(e) => edebug!("could not send ack? {e}"),
                }
              }
            }
            TcpStreamState::Established => {
              if tcp_header.ack {
                if let Err(_) = stream
                  .send_buffer
                  .handle_ack(tcp_header.acknowledgment_number)
                {
                  edebug!("Failed to handle ack");
                }
              }
              stream
                .recv_buffer
                .handle_seq(tcp_header.sequence_number, data);
            }
            TcpStreamState::Closed => {
              edebug!("Packet received in Closed state: This should never probably never happen?");
            }
            TcpStreamState::FinWait1 => todo!(),
            TcpStreamState::FinWait2 => todo!(),
            TcpStreamState::CloseWait => todo!(),
            TcpStreamState::Closing => todo!(),
          }
        }
        Err(_e) => {
          debug!("Exiting...");
          break;
        }
      }
    });
  }

  fn send_syn(&mut self, destination_ip: Ipv4Addr, destination_port: Port) -> Result<()> {
    debug_assert_state_valid(
      &self.state,
      vec![TcpStreamState::Closed, TcpStreamState::Listen],
    );

    self.set_destination_or_check(destination_ip, destination_port);

    let mut msg = self.make_default_tcp_header();
    msg.syn = true;

    self.send_tcp_packet(msg, &[])?;
    self.state = TcpStreamState::SynSent;
    Ok(())
  }

  fn send_syn_ack(&mut self, destination_ip: Ipv4Addr, destination_port: Port) -> Result<()> {
    debug_assert_state_valid(
      &self.state,
      vec![TcpStreamState::Listen, TcpStreamState::SynSent],
    );

    self.set_destination_or_check(destination_ip, destination_port);

    let mut msg = self.make_default_tcp_header();
    msg.syn = true;
    msg.ack = true;
    msg.sequence_number = self.initial_sequence_number;
    msg.acknowledgment_number = self.initial_ack.unwrap() + 1;

    self.send_tcp_packet(msg, &[])?;
    self.state = TcpStreamState::SynSent;
    Ok(())
  }

  // TODO make this happen when sending other data or something like this?
  fn send_ack_for_data(&mut self, ack: u32) -> Result<()> {
    debug_assert_state_valid(&self.state, vec![TcpStreamState::Established]); // TODO check this
    let mut msg = self.make_default_tcp_header();
    msg.ack = true;
    // TODO: fix this, this is wrong
    msg.sequence_number = self.initial_sequence_number;
    msg.acknowledgment_number = ack;

    self.send_tcp_packet(msg, &[])?;
    Ok(())
  }

  fn send_ack(&mut self, destination_ip: Ipv4Addr, destination_port: Port) -> Result<()> {
    debug_assert_state_valid(
      &self.state,
      vec![
        TcpStreamState::Established,
        TcpStreamState::SynSent,
        TcpStreamState::FinWait1,
        TcpStreamState::FinWait2,
      ],
    );

    self.set_destination_or_check(destination_ip, destination_port);

    let mut msg = self.make_default_tcp_header();
    msg.ack = true;
    // TODO: fix this, this is wrong
    msg.sequence_number = self.initial_sequence_number;
    msg.acknowledgment_number = self.initial_ack.unwrap().wrapping_add(1);

    self.send_tcp_packet(msg, &[])?;
    Ok(())
  }

  fn send_data(&mut self, seq_num: u32, data: Vec<u8>) -> Result<()> {
    debug_assert_state_valid(&self.state, vec![TcpStreamState::Established]);

    let mut msg = self.make_default_tcp_header();
    // TODO: fix this, this is wrong
    msg.sequence_number = seq_num;

    self.send_tcp_packet(msg, &data)?;
    Ok(())
  }

  fn send_tcp_packet(&self, mut header: TcpHeader, data: &[u8]) -> Result<()> {
    debug_assert!(self.source_ip.is_some());
    debug_assert!(self.destination_ip.is_some());
    // TODO: figure out best way to pack header (making sure that checksum is calculated)
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

  fn make_default_tcp_header(&self) -> TcpHeader {
    debug_assert!(self.destination_port.is_some());
    TcpHeader::new(
      self.source_port,
      self.destination_port.unwrap(),
      self.initial_sequence_number,
      0, // TODO
    )
  }

  /// If destination_ip is None then we set both fields, otherwise assert that
  /// the values were already set correctly
  fn set_destination_or_check(&mut self, destination_ip: Ipv4Addr, destination_port: Port) {
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
  fn set_initial_ack(&mut self, ack: u32) {
    self.initial_ack = Some(ack);
  }

  /// set source_ip based on syn
  fn set_source_ip(&mut self, source_ip: Ipv4Addr) {
    self.source_ip = Some(source_ip);
  }

  /// get stream state
  pub fn state(&self) -> TcpStreamState {
    self.state
  }
}

/// Helper function for validates valid state transitions, should be called at the
/// top of all (non-contructor) public methods
fn debug_assert_state_valid(curr_state: &TcpStreamState, valid_states: Vec<TcpStreamState>) {
  debug_assert!(valid_states.contains(curr_state))
}
