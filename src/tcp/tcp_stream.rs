use std::net::{Ipv4Addr, SocketAddr};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{mpsc, Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;
use std::{thread, vec};

use anyhow::{anyhow, Result};
use etherparse::{Ipv4Header, TcpHeader};

use super::recv_buffer::RecvBuffer;
use super::send_buffer::SendBuffer;
use super::{IpTcpPacket, Port, MAX_WINDOW_SIZE};
use crate::ip::protocol::Protocol;
use crate::{debug, edebug, IpPacket};

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
const VALID_ACK_STATES: [TcpStreamState; 5] = [
  TcpStreamState::SynSent,
  TcpStreamState::FinWait1,
  TcpStreamState::FinWait2,
  TcpStreamState::CloseWait,
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

  /// Used to set sequence number on acks and ack number of data packets
  last_ack:         Option<u32>,
  /// Last window size
  last_window_size: u16,
  next_seq:         u32,

  /// state
  state:      TcpStreamState,
  ip_send_tx: Sender<IpPacket>,
}

#[derive(Debug)]
pub(super) enum StreamSendThreadMsg {
  /// sequence number, data
  Syn,
  Outgoing(u32, Vec<u8>),
  /// ack num and window size
  Ack(u32, u16),

  /// new window size after read
  UpdateWindowSize(u16),
  /// includes seq num
  Fin(u32),

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
      last_ack: None,
      last_window_size: MAX_WINDOW_SIZE as u16,
      next_seq: initial_sequence_number.wrapping_add(1),
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
      TcpStreamState::SynSent,
      ip_send_tx,
    );

    new_stream.send_buffer.send_syn()?;

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

  /// get window size
  pub fn get_window_size(&self) -> u16 {
    self.recv_buffer.lock().unwrap().get_window_size()
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

  /// Close is a overloaded term when dealing with sockets. This function corresponds to the
  /// sending the CLOSE command as described in the RFC. This corresponds to calling shutdown with
  /// the method set to write.
  pub fn close(&self) -> Result<()> {
    let mut internal = self.internal.lock().unwrap();
    match internal.state {
      TcpStreamState::Listen => {
        // TODO: Any outstanding RECEIVEs are returned with "error:  closing"
        // responses.  Delete TCB, enter CLOSED state, and return.
        internal.state = TcpStreamState::Closed;
        Ok(())
      }
      TcpStreamState::SynSent => {
        // TODO: delete the TCB (this probably need to be handled a level up?)
        internal.state = TcpStreamState::Closed;
        Ok(())
      }
      TcpStreamState::SynReceived => {
        // TODO: If no SENDs have been issued and there is no pending data to send,
        // then form a FIN segment and send it, and enter FIN-WAIT-1 state;
        // otherwise queue for processing after entering ESTABLISHED state.
        Ok(())
      }
      TcpStreamState::Established => {
        // TODO: Queue this request until all preceding SENDs have been
        // segmentized; then send a FIN segment, enter CLOSING state.
        self.send_buffer.send_fin()?;
        internal.state = TcpStreamState::FinWait1;
        Ok(())
      }
      TcpStreamState::FinWait1 => Err(anyhow!("connection closing")),
      TcpStreamState::FinWait2 => Err(anyhow!("connection closing")),
      TcpStreamState::CloseWait => {
        // TODO: Contradiction between flow chart and page 61 of RFC which says
        // Queue this request until all preceding SENDs have been
        // segmentized; then send a FIN segment, enter CLOSING state.
        // current implementation follows chart
        self.send_buffer.send_fin()?;
        internal.state = TcpStreamState::LastAck;
        Ok(())
      }
      TcpStreamState::Closing => Err(anyhow!("connection closing")),
      TcpStreamState::LastAck => Err(anyhow!("connection closing")),
      TcpStreamState::TimeWait => Err(anyhow!("connection closing")),
      TcpStreamState::Closed => Err(anyhow!("connection closing")),
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
        Ok(StreamSendThreadMsg::Syn) => {
          let mut stream = stream.lock().unwrap();
          match stream.state {
            TcpStreamState::Listen | TcpStreamState::SynSent => {
              stream.send_syn();
            }
            TcpStreamState::SynReceived => {
              stream.send_syn_ack();
            }
            other => (),
          }
        }
        Ok(StreamSendThreadMsg::Outgoing(seq_num, data)) => {
          let mut stream = stream.lock().unwrap();
          if VALID_SEND_STATES.contains(&stream.state) {
            if seq_num >= stream.next_seq {
              stream.next_seq = seq_num + data.len() as u32;
            }
            if let Err(_e) = stream.send_data(seq_num, data) {
              edebug!("Failed to send data with seq_num {seq_num}");
              break;
            }
          }
        }
        Ok(StreamSendThreadMsg::Ack(ack_num, window_size)) => {
          let mut stream = stream.lock().unwrap();
          stream.last_window_size = window_size;
          stream.last_ack = Some(ack_num);
          if VALID_ACK_STATES.contains(&stream.state) {
            if let Err(_e) = stream.send_ack(ack_num) {
              edebug!("Failed to send {ack_num} for data");
              break;
            }
          }
        }
        Ok(StreamSendThreadMsg::UpdateWindowSize(window_size)) => {
          let mut stream = stream.lock().unwrap();
          stream.last_window_size = window_size;
        }
        Ok(StreamSendThreadMsg::Fin(seq_num)) => {
          let mut stream = stream.lock().unwrap();
          if VALID_FIN_STATES.contains(&stream.state) {
            if seq_num >= stream.next_seq {
              stream.next_seq = seq_num + 1;
            }
            stream.send_fin(seq_num);
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
    thread::spawn(move || {
      let handle_incoming_ack_data =
        |tcp_header: &TcpHeader, data: Vec<u8>, stream: &mut MutexGuard<TcpStreamInternal>| {
          if tcp_header.ack {
            if let Err(e) =
              send_buffer.handle_ack(tcp_header.acknowledgment_number, tcp_header.window_size)
            {
              stream.state = TcpStreamState::Closed;
              return Err(anyhow!("Failed to handle ack {e}, closing..."));
            }
          }

          if data.len() > 0 {
            if let Err(e) = recv_buffer
              .lock()
              .unwrap()
              .handle_seq(tcp_header.sequence_number, &data)
            {
              stream.state = TcpStreamState::Closed;
              return Err(anyhow!("Failed to handle seq {e}, closing..."));
            }
            recv_buffer_cond.notify_one();
          }

          Ok(())
        };

      let handle_fin =
        |tcp_header: &TcpHeader, stream: &mut MutexGuard<TcpStreamInternal>, first_fin: bool| {
          if first_fin {
            debug!("Received fin");
          } else {
            debug!("Received duplicate of fin");
          }
          let ack_num = tcp_header.sequence_number.wrapping_add(1);
          if let Err(e) = stream.send_ack(ack_num) {
            edebug!("Failed to send ack of fin {e}, closing...");
            stream.state = TcpStreamState::Closed;
            return Err(anyhow!("Failed to send ack of fin {e}, closing..."));
          }
          Ok(())
        };

      let handle_ack_of_fin = |_tcp_header: &TcpHeader, send_buffer: &SendBuffer| {
        debug!("Received ack of fin");
        send_buffer.handle_ack_of_fin()
      };

      loop {
        match stream_rx.recv_timeout(Duration::from_millis(10)) {
          Ok((ip_header, tcp_header, data)) => {
            let mut stream = stream.lock().unwrap();
            match dbg!(stream.state) {
              TcpStreamState::Listen => {
                if tcp_header.syn {
                  let ip: Ipv4Addr = ip_header.source.into();
                  let port = tcp_header.source_port;
                  stream.set_destination_or_check(ip, port);

                  stream.set_source_ip(ip_header.destination.into());
                  recv_buffer
                    .lock()
                    .unwrap()
                    .set_initial_seq_num_data(tcp_header.sequence_number);
                  stream.set_initial_ack(tcp_header.sequence_number.wrapping_add(1));

                  // Note ack will be sent on the syn since we are in SynReceived
                  match send_buffer.send_syn() {
                    Ok(()) => {
                      stream.state = TcpStreamState::SynReceived;
                    }
                    Err(e) => {
                      edebug!("Failed to send syn_ack {e}, closing");
                      stream.state = TcpStreamState::Closed;
                    }
                  }
                }
              }
              TcpStreamState::SynReceived => {
                if tcp_header.ack {
                  let window_size = tcp_header.window_size;
                  if let Err(e) = send_buffer.handle_ack_of_syn(window_size) {
                    edebug!("Failed to handle ack of syn {e}, closing...");
                    // Don't stop here, this could be because of a duplicate syn ack send
                  }
                  stream.state = TcpStreamState::Established;
                }
              }
              TcpStreamState::SynSent => {
                let ip: Ipv4Addr = ip_header.source.into();
                let port = tcp_header.source_port;
                stream.set_destination_or_check(ip, port);
                let initial_ack = tcp_header.sequence_number.wrapping_add(1);
                stream.set_initial_ack(tcp_header.sequence_number.wrapping_add(1));
                if tcp_header.ack && tcp_header.syn {
                  recv_buffer
                    .lock()
                    .unwrap()
                    .set_initial_seq_num_data(tcp_header.sequence_number);

                  let window_size = tcp_header.window_size;
                  if let Err(e) = send_buffer.handle_ack_of_syn(window_size) {
                    edebug!("Failed to handle ack of syn {e}, closing...");
                    // Don't stop here, this could be because of a duplicate syn ack send
                  }

                  match stream.send_ack(initial_ack) {
                    Ok(()) => {
                      stream.state = TcpStreamState::Established;
                    }
                    Err(e) => edebug!("could not send ack? {e}"),
                  }
                } else if tcp_header.syn && !tcp_header.ack {
                  recv_buffer
                    .lock()
                    .unwrap()
                    .set_initial_seq_num_data(tcp_header.sequence_number);
                  stream.set_source_ip(ip_header.destination.into());
                  match stream.send_ack(initial_ack) {
                    Ok(()) => {
                      stream.state = TcpStreamState::SynReceived;
                    }
                    Err(e) => {
                      edebug!("Failed to send initial ack {e}, closing...");
                      stream.state = TcpStreamState::Closed;
                    }
                  }
                }
              }
              TcpStreamState::Established => {
                if tcp_header.fin {
                  if let Err(e) = handle_fin(&tcp_header, &mut stream, true) {
                    edebug!("{}", e);
                    return;
                  }
                  stream.state = TcpStreamState::CloseWait;
                }

                if let Err(e) = handle_incoming_ack_data(&tcp_header, data, &mut stream) {
                  edebug!("{}", e);
                  return;
                }
              }
              TcpStreamState::Closed => {
                edebug!(
                  "Packet received in Closed state: This should never probably never happen?"
                );
              }
              TcpStreamState::FinWait1 => {
                if tcp_header.fin {
                  if let Err(e) = handle_fin(&tcp_header, &mut stream, true) {
                    edebug!("{}", e);
                    return;
                  }
                  stream.state = TcpStreamState::Closing;
                } else if tcp_header.ack && !tcp_header.fin {
                  if tcp_header.acknowledgment_number == stream.next_seq {
                    handle_ack_of_fin(&tcp_header, &*send_buffer);
                    stream.state = TcpStreamState::FinWait2;
                  }
                }
                if let Err(e) = handle_incoming_ack_data(&tcp_header, data, &mut stream) {
                  edebug!("{}", e);
                  return;
                }
              }
              TcpStreamState::FinWait2 => {
                if tcp_header.fin {
                  if let Err(e) = handle_fin(&tcp_header, &mut stream, true) {
                    edebug!("{}", e);
                    return;
                  }
                  stream.state = TcpStreamState::TimeWait;
                }
                if let Err(e) = handle_incoming_ack_data(&tcp_header, data, &mut stream) {
                  edebug!("{}", e);
                  return;
                }
              }
              TcpStreamState::CloseWait => {
                // handle dup fin
                if tcp_header.fin {
                  if let Err(e) = handle_fin(&tcp_header, &mut stream, false) {
                    edebug!("{}", e);
                    return;
                  }
                }

                if tcp_header.ack {
                  if let Err(e) =
                    send_buffer.handle_ack(tcp_header.acknowledgment_number, tcp_header.window_size)
                  {
                    edebug!("Failed to handle ack {e}, closing...");
                    stream.state = TcpStreamState::Closed;
                    return;
                  }
                }
              }
              TcpStreamState::Closing => {
                // handle dup fin
                if tcp_header.fin {
                  if let Err(e) = handle_fin(&tcp_header, &mut stream, false) {
                    edebug!("{}", e);
                    return;
                  }
                }

                if tcp_header.ack {
                  if tcp_header.acknowledgment_number == stream.next_seq {
                    handle_ack_of_fin(&tcp_header, &*send_buffer);
                    stream.state = TcpStreamState::TimeWait;
                  }
                }
              }
              TcpStreamState::TimeWait => {
                edebug!("Received packet in TimeWait");
              }
              TcpStreamState::LastAck => {
                // handle dup fin
                if tcp_header.fin {
                  if let Err(e) = handle_fin(&tcp_header, &mut stream, false) {
                    edebug!("{}", e);
                    return;
                  }
                }

                if tcp_header.ack {
                  if tcp_header.acknowledgment_number == stream.next_seq {
                    handle_ack_of_fin(&tcp_header, &*send_buffer);
                    stream.state = TcpStreamState::Closed;
                  }
                }
              }
            }
          }
          Err(RecvTimeoutError::Timeout) => (),
          Err(RecvTimeoutError::Disconnected) => {
            let mut stream = stream.lock().unwrap();
            stream.state = TcpStreamState::Closed;
            debug!("Listen stream_rx closed, closing...");
            break;
          }
        }
      }
    });
  }

  fn set_state(&mut self, state: TcpStreamState) {
    self.internal.lock().unwrap().state = state;
  }
}

impl TcpStreamInternal {
  fn send_syn(&mut self) -> Result<()> {
    debug_assert_state_valid(&self.state, &VALID_SYN_STATES);

    debug_assert!(self.destination_ip.is_some());
    debug_assert!(self.destination_port.is_some());

    let mut msg = self.make_default_tcp_header();
    msg.syn = true;
    msg.sequence_number = self.initial_sequence_number;

    self.send_tcp_packet(msg, &[])?;
    Ok(())
  }

  fn send_syn_ack(&mut self) -> Result<()> {
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

  fn send_ack(&mut self, ack: u32) -> Result<()> {
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

  fn send_fin(&mut self, seq_num: u32) -> Result<()> {
    debug_assert_state_valid(&self.state, &VALID_FIN_STATES);

    let mut msg = self.make_default_tcp_header();
    msg.fin = true;
    msg.sequence_number = seq_num;

    self.send_tcp_packet(msg, &[])?;
    Ok(())
  }

  fn send_data(&mut self, seq_num: u32, data: Vec<u8>) -> Result<()> {
    debug_assert_state_valid(&self.state, &VALID_SEND_STATES);

    let mut msg = self.make_default_tcp_header();
    msg.ack = true;
    msg.sequence_number = seq_num;
    msg.acknowledgment_number = self.last_ack.unwrap();

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

  /// Close is a overloaded term when dealing with sockets. This function corresponds to the
  /// sending the CLOSE command as described in the RFC. This corresponds to calling shutdown with
  /// the method set to write.
  pub fn close(&self) {}

  fn make_default_tcp_header(&self) -> TcpHeader {
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
    self.last_ack = Some(ack);
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
fn debug_assert_state_valid(curr_state: &TcpStreamState, valid_states: &[TcpStreamState]) {
  debug_assert!(valid_states.contains(curr_state))
}

impl Drop for TcpStream {
  fn drop(&mut self) {
    self.set_state(TcpStreamState::Closed);
  }
}
