use std::net::{Ipv4Addr, SocketAddr};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::time::Duration;
use std::{thread, vec};

use anyhow::{anyhow, Result};
use etherparse::{Ipv4Header, TcpHeader};

use super::recv_buffer::RecvBuffer;
use super::send_buffer::SendBuffer;
use super::{IpTcpPacket, Port, TCP_BUF_SIZE};
use crate::ip::protocol::Protocol;
use crate::{debug, edebug, IpPacket};

const VALID_SEND_STATES: [TcpStreamState; 2] =
  [TcpStreamState::Established, TcpStreamState::CloseWait];
const VALID_ACK_STATES: [TcpStreamState; 3] = [
  TcpStreamState::FinWait1,
  TcpStreamState::FinWait2,
  TcpStreamState::Established,
];

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

#[derive(Debug)]
struct TcpStreamInternal {
  source_ip:               Option<Ipv4Addr>,
  /// Constant properties of the stream
  source_port:             u16,
  /// TODO: switch to net::SocketAddr
  /// This should only be none if we are in the LISTEN state
  destination_ip:          Option<Ipv4Addr>,
  destination_port:        Option<u16>,
  initial_sequence_number: u32,
  /// This is set equal to the initial_sequence_number of the other node + 1
  initial_ack:             Option<u32>,

  /// state
  state:      TcpStreamState,
  ip_send_tx: Sender<IpPacket>,
}

#[derive(Debug)]
pub(super) enum StreamSendThreadMsg {
  /// sequence number, data
  Outgoing(u32, Vec<u8>),
  Ack(u32),

  /// Sent in the case the stream has shutdown (i.e. we've sent too many retries with no acks)
  Shutdown,
}

#[derive(Debug)]
pub struct TcpStream {
  internal: Arc<Mutex<TcpStreamInternal>>,

  /// data
  recv_buffer:      Arc<Mutex<RecvBuffer>>,
  recv_buffer_cond: Arc<Condvar>,
  send_buffer:      Arc<SendBuffer>,

  /// TODO: how should commands like SHUTDOWN and CLOSE be passed
  /// Selecting???
  stream_tx: Arc<Mutex<Sender<IpTcpPacket>>>, // thing for others to send this stream a packet
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
  ) -> TcpStream {
    let (stream_tx, stream_rx) = mpsc::channel();
    let (send_thread_tx, send_thread_rx) = mpsc::channel();

    let initial_sequence_number = 0; // TODO: switch to ISN being clock time
    let internal = Arc::new(Mutex::new(TcpStreamInternal {
      source_ip,
      source_port,
      destination_ip,
      destination_port,
      initial_sequence_number,
      initial_ack: None,
      state: initial_state,
      ip_send_tx: ip_send_tx.clone(),
    }));

    let recv_buffer = Arc::new(Mutex::new(RecvBuffer::new(send_thread_tx.clone())));
    let recv_buffer_cond = Arc::new(Condvar::new());

    let send_buffer = Arc::new(SendBuffer::new(
      send_thread_tx.clone(),
      initial_sequence_number,
    ));

    TcpStream::start_listen_thread(
      internal.clone(),
      recv_buffer.clone(),
      recv_buffer_cond.clone(),
      send_buffer.clone(),
      stream_rx,
    );
    TcpStream::start_send_thread(internal.clone(), send_thread_rx);

    TcpStream {
      internal,
      recv_buffer,
      recv_buffer_cond,
      send_buffer,
      stream_tx: Arc::new(Mutex::new(stream_tx)),
    }
  }

  pub fn listen(source_port: Port, ip_send_tx: Sender<IpPacket>) -> TcpStream {
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
  ) -> Result<TcpStream> {
    let new_stream = TcpStream::new(
      Some(source_ip),
      source_port,
      Some(destination_ip),
      Some(destination_port),
      TcpStreamState::Closed,
      ip_send_tx,
    );

    new_stream
      .internal
      .lock()
      .unwrap()
      .send_syn(destination_ip, destination_port)?;

    Ok(new_stream)
  }

  pub fn destination(&self) -> Option<SocketAddr> {
    self.internal.lock().unwrap().destination()
  }

  pub fn source_ip(&self) -> Option<Ipv4Addr> {
    self.internal.lock().unwrap().source_ip()
  }

  pub fn source_port(&self) -> Port {
    self.internal.lock().unwrap().source_port()
  }

  /// get stream state
  pub fn state(&self) -> TcpStreamState {
    self.internal.lock().unwrap().state()
  }

  /// Called by TcpLayer when it receives a packet for this stream, sends to listen thread to
  /// process
  pub fn process(&self, packet: IpTcpPacket) -> Result<()> {
    self.stream_tx.lock().unwrap().send(packet)?;
    Ok(())
  }

  /// Called by TcpLayer
  pub fn send(&self, data: &[u8]) -> Result<()> {
    self.send_buffer.write_data(data)?;
    Ok(())
  }

  /// Called by TcpLayer
  /// TODO: what is the behavior if the stream dies while blocked: ans: return err
  pub fn recv(&self, num_bytes: usize, should_block: bool) -> Result<Vec<u8>> {
    let mut data = vec![0u8; num_bytes];
    let mut bytes_read = 0;
    if should_block {
      let buf = self.recv_buffer.lock().unwrap();
      let _ = self
        .recv_buffer_cond
        .wait_while(buf, |buf| {
          let n = buf.read_data(&mut data[bytes_read..]);
          bytes_read += n;
          bytes_read < num_bytes
        })
        .unwrap();

      if bytes_read == num_bytes {
        Ok(data)
      } else {
        Err(anyhow!("Error occured during blocking read"))
      }
    } else {
      let mut buf = self.recv_buffer.lock().unwrap();
      let bytes_read = buf.read_data(&mut data);
      debug_assert!(bytes_read <= num_bytes);
      data.resize(bytes_read, 0u8);
      Ok(data)
    }
  }

  // TODO maybe better name?
  /// Handles sending packets from send buffer (to send data out) and handles sending acks for
  /// recieved data from recv buffer.
  fn start_send_thread(
    stream: Arc<Mutex<TcpStreamInternal>>,
    send_thread_rx: Receiver<StreamSendThreadMsg>,
  ) {
    thread::spawn(move || loop {
      {
        let stream = stream.lock().unwrap();
        if let TcpStreamState::Closed = stream.state {
          edebug!("Closing send thread, and send_thread_rx, which tells send_buffer to close");
          break;
        }
      }
      match send_thread_rx.recv_timeout(Duration::from_millis(10)) {
        Ok(StreamSendThreadMsg::Outgoing(seq_num, data)) => {
          if VALID_SEND_STATES.contains(&stream.lock().unwrap().state) {
            if let Err(_e) = stream.lock().unwrap().send_data(seq_num, data) {
              edebug!("Failed to send data with seq_num {seq_num}");
              break;
            }
          }
        }
        Ok(StreamSendThreadMsg::Ack(ack_num)) => {
          if VALID_ACK_STATES.contains(&stream.lock().unwrap().state) {
            if let Err(_e) = stream.lock().unwrap().send_ack_for_data(ack_num) {
              edebug!("Failed to send {ack_num} for data");
              break;
            }
          }
        }
        Ok(StreamSendThreadMsg::Shutdown) => {
          edebug!("Closing streams send thread due to Shutdown");
          stream.lock().unwrap().state = TcpStreamState::Closed;
          break;
        }
        Err(RecvTimeoutError::Timeout) => (),
        Err(RecvTimeoutError::Disconnected) => {
          edebug!("Closing stream's send thread");
          stream.lock().unwrap().state = TcpStreamState::Closed;
          break;
        }
      }
    });
  }

  fn start_listen_thread(
    stream: Arc<Mutex<TcpStreamInternal>>,
    recv_buffer: Arc<Mutex<RecvBuffer>>,
    recv_buffer_cond: Arc<Condvar>,
    send_buffer: Arc<SendBuffer>,
    stream_rx: Receiver<IpTcpPacket>,
  ) {
    thread::spawn(move || loop {
      match stream_rx.recv_timeout(Duration::from_millis(10)) {
        Ok((ip_header, tcp_header, data)) => {
          let mut stream = stream.lock().unwrap();
          match stream.state {
            TcpStreamState::Listen => {
              if tcp_header.syn {
                let ip: Ipv4Addr = ip_header.source.into();
                let port = tcp_header.source_port;

                stream.set_source_ip(ip_header.destination.into());
                recv_buffer
                  .lock()
                  .unwrap()
                  .set_initial_seq_num(tcp_header.sequence_number);
                stream.set_initial_ack(tcp_header.sequence_number.wrapping_add(1));

                match stream.send_syn_ack(ip, port) {
                  Ok(()) => {
                    stream.state = TcpStreamState::SynReceived;
                  }
                  Err(e) => {
                    edebug!("Failed to send syn_ack {e}, closing");
                    stream.close();
                  }
                }
              }
            }
            TcpStreamState::SynReceived => {
              if tcp_header.ack {
                let ack_num = tcp_header.acknowledgment_number;
                let window_size = tcp_header.window_size;
                if let Err(e) = send_buffer.handle_ack(ack_num, window_size) {
                  edebug!("Failed to handle ack {e}, closing...");
                  stream.close();
                }
                stream.state = TcpStreamState::Established;
              }
            }
            TcpStreamState::SynSent => {
              let ip: Ipv4Addr = ip_header.source.into();
              let port = tcp_header.source_port;
              if tcp_header.ack && tcp_header.syn {
                recv_buffer
                  .lock()
                  .unwrap()
                  .set_initial_seq_num(tcp_header.sequence_number);
                stream.set_initial_ack(tcp_header.sequence_number.wrapping_add(1));

                let ack_num = tcp_header.acknowledgment_number;
                let window_size = tcp_header.window_size;
                if let Err(e) = send_buffer.handle_ack(ack_num, window_size) {
                  edebug!("Failed to handle ack {e}, closing...");
                  stream.close();
                }

                match stream.send_initial_ack(ip, port) {
                  Ok(()) => {
                    stream.state = TcpStreamState::Established;
                  }
                  Err(e) => edebug!("could not send ack? {e}"),
                }
              } else if tcp_header.syn && !tcp_header.ack {
                recv_buffer
                  .lock()
                  .unwrap()
                  .set_initial_seq_num(tcp_header.sequence_number);
                stream.set_initial_ack(tcp_header.sequence_number.wrapping_add(1));
                stream.set_source_ip(ip_header.destination.into());
                match stream.send_initial_ack(ip, port) {
                  Ok(()) => {
                    stream.state = TcpStreamState::SynReceived;
                  }
                  Err(e) => {
                    edebug!("Failed to send initial ack {e}, closing...");
                    stream.close();
                  }
                }
              }
            }
            TcpStreamState::Established => {
              if tcp_header.ack {
                if let Err(e) =
                  send_buffer.handle_ack(tcp_header.acknowledgment_number, tcp_header.window_size)
                {
                  edebug!("Failed to handle ack {e}, closing...");
                  stream.close();
                  return;
                }
              }

              if data.len() > 0 {
                recv_buffer
                  .lock()
                  .unwrap()
                  .handle_seq(tcp_header.sequence_number, data); // TODO handle result
                recv_buffer_cond.notify_one();
              }
            }
            TcpStreamState::Closed => {
              edebug!("Packet received in Closed state: This should never probably never happen?");
            }
            TcpStreamState::FinWait1 => todo!(),
            TcpStreamState::FinWait2 => todo!(),
            TcpStreamState::CloseWait => todo!(),
            TcpStreamState::Closing => todo!(),
            TcpStreamState::TimeWait => todo!(),
            TcpStreamState::LastAck => todo!(),
          }
        }
        Err(RecvTimeoutError::Timeout) => (),
        Err(RecvTimeoutError::Disconnected) => {
          let mut stream = stream.lock().unwrap();
          stream.close();
          debug!("Listen stream_rx closed, closing...");
          break;
        }
      }
    });
  }

  fn close(&mut self) {
    self.internal.lock().unwrap().close();
  }
}

impl TcpStreamInternal {
  fn send_syn(&mut self, destination_ip: Ipv4Addr, destination_port: Port) -> Result<()> {
    debug_assert_state_valid(
      &self.state,
      &[TcpStreamState::Closed, TcpStreamState::Listen],
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
      &[TcpStreamState::Listen, TcpStreamState::SynSent],
    );

    self.set_destination_or_check(destination_ip, destination_port);

    let mut msg = self.make_default_tcp_header();
    msg.syn = true;
    msg.ack = true;
    msg.sequence_number = self.initial_sequence_number;
    msg.acknowledgment_number = self.initial_ack.unwrap();

    self.send_tcp_packet(msg, &[])?;
    self.state = TcpStreamState::SynSent;
    Ok(())
  }

  fn send_ack_for_data(&mut self, ack: u32) -> Result<()> {
    debug_assert_state_valid(&self.state, &VALID_ACK_STATES);
    let mut msg = self.make_default_tcp_header();
    msg.ack = true;
    // TODO: fix this, this is wrong
    msg.sequence_number = self.initial_sequence_number.wrapping_add(1);
    msg.acknowledgment_number = ack;

    self.send_tcp_packet(msg, &[])?;
    Ok(())
  }

  fn send_initial_ack(&mut self, destination_ip: Ipv4Addr, destination_port: Port) -> Result<()> {
    debug_assert_state_valid(
      &self.state,
      &[
        TcpStreamState::Established,
        TcpStreamState::SynSent,
        TcpStreamState::FinWait1,
        TcpStreamState::FinWait2,
      ],
    );

    self.set_destination_or_check(destination_ip, destination_port);

    let mut msg = self.make_default_tcp_header();
    msg.ack = true;
    msg.sequence_number = self.initial_sequence_number;
    msg.acknowledgment_number = self.initial_ack.unwrap();

    self.send_tcp_packet(msg, &[])?;
    Ok(())
  }

  fn send_data(&mut self, seq_num: u32, data: Vec<u8>) -> Result<()> {
    debug_assert_state_valid(&self.state, &VALID_SEND_STATES);

    let mut msg = self.make_default_tcp_header();
    msg.sequence_number = seq_num;

    self.send_tcp_packet(msg, &data)?;
    Ok(())
  }

  fn send_tcp_packet(&self, mut header: TcpHeader, data: &[u8]) -> Result<()> {
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

  fn make_default_tcp_header(&self) -> TcpHeader {
    debug_assert!(self.destination_port.is_some());
    TcpHeader::new(
      self.source_port,
      self.destination_port.unwrap(),
      self.initial_sequence_number,
      TCP_BUF_SIZE as u16, // TODO see slow start
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

  fn close(&mut self) {
    self.state = TcpStreamState::Closed;
  }
}

/// Helper function for validates valid state transitions, should be called at the
/// top of all (non-contructor) public methods
fn debug_assert_state_valid(curr_state: &TcpStreamState, valid_states: &[TcpStreamState]) {
  debug_assert!(valid_states.contains(curr_state))
}

impl Drop for TcpStream {
  fn drop(&mut self) {
    self.close();
  }
}
