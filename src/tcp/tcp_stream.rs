use std::fmt::Debug;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{mpsc, Arc, Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Result};
use etherparse::TcpHeader;

use super::recv_buffer::RecvBuffer;
use super::send_buffer::{CongestionControlStrategy, SendBuffer};
use super::tcp_internal::TcpStreamInternal;
use super::{
  IpTcpPacket, Port, SocketId, TcpLayerInfo, TcpStreamState, MAX_SEGMENT_LIFETIME, MAX_WINDOW_SIZE,
  VALID_ACK_STATES, VALID_FIN_STATES, VALID_RECV_STATES, VALID_SEND_STATES,
};
use crate::ip::IpPacket;
use crate::{debug, edebug};

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

#[derive(Clone, Debug)]
pub struct TcpStream {
  pub(super) internal: Arc<Mutex<TcpStreamInternal>>,

  /// data
  recv_buffer:      Arc<Mutex<RecvBuffer>>,
  recv_buffer_cond: Arc<Condvar>,
  send_buffer:      Arc<SendBuffer>,

  // thing for others to send this stream a packet
  pub(super) stream_tx: Arc<Mutex<Sender<IpTcpPacket>>>,
}

impl TcpStream {
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

  /// get stream socket_id
  pub fn socket_id(&self) -> SocketId {
    self.internal.lock().unwrap().socket_id
  }

  fn set_state(&mut self, state: TcpStreamState) {
    self.internal.lock().unwrap().state = state;
  }

  /// get window size, recv, and send
  pub fn window_size(&self) -> Option<(u16, u16, u16)> {
    match self.state() {
      TcpStreamState::Listen => None,
      _ => Some((
        self.recv_buffer.lock().unwrap().window_size(),
        self.send_buffer.their_recv_window_size(),
        self.send_buffer.window_size(),
      )),
    }
  }
}

impl TcpStream {
  fn new(
    source_ip: Option<Ipv4Addr>,
    source_port: Port,
    destination_ip: Option<Ipv4Addr>,
    destination_port: Option<Port>,
    initial_state: TcpStreamState,
    ip_send_tx: Sender<IpPacket>,
    cleanup: Box<dyn Fn() + Send>,
    socket_id: SocketId,
    info: TcpLayerInfo,
    congestion_control: CongestionControlStrategy,
  ) -> TcpStream {
    let (stream_tx, stream_rx) = mpsc::channel();
    let (send_thread_tx, send_thread_rx) = mpsc::channel();

    let initial_sequence_number = SystemTime::now()
      .duration_since(UNIX_EPOCH)
      .map_or(0u32, |t| t.as_millis() as u32);
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
      socket_id,
    }));

    let recv_buffer = Arc::new(Mutex::new(RecvBuffer::new(send_thread_tx.clone())));
    let recv_buffer_cond = Arc::new(Condvar::new());

    let send_buffer = Arc::new(SendBuffer::new(
      send_thread_tx.clone(),
      initial_sequence_number,
      congestion_control,
    ));

    TcpStream::start_listen_thread(
      internal.clone(),
      recv_buffer.clone(),
      recv_buffer_cond.clone(),
      send_buffer.clone(),
      stream_rx,
      cleanup,
      info,
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

  pub(super) fn spawn_and_queue(
    info: &TcpLayerInfo,
    src_port: Port,
    listener_socket: SocketId,
    dst_ip: Ipv4Addr,
    dst_port: Port,
    sequence_number: u32,
  ) -> Result<()> {
    let mut queues = info.queued_streams.lock().unwrap();
    match queues.get_mut(&listener_socket) {
      None => Err(anyhow!("Unknown listener socket {listener_socket}")),
      Some((queue, available_addrs)) => {
        let mut sockets_and_ports = info.sockets_and_ports.lock().unwrap();
        let new_socket = sockets_and_ports.get_new_socket();
        let src_ip = available_addrs.iter().next().cloned();
        if src_ip.is_none() {
          panic!("No ip addresses available!");
        }
        let stream = Arc::new(TcpStream::new(
          src_ip,
          src_port,
          None,
          None,
          TcpStreamState::Spawning,
          info.ip_send_tx.clone(),
          info.make_cleanup_callback(new_socket),
          new_socket,
          info.clone(),
          CongestionControlStrategy::No, /* for now we only want congestion control on the
                                          * client side */
        ));
        let mut internal = stream.internal.lock().unwrap();
        internal.set_destination_or_check(dst_ip, dst_port);
        stream
          .recv_buffer
          .lock()
          .unwrap()
          .set_initial_seq_num_data(sequence_number);
        internal.set_initial_ack(sequence_number.wrapping_add(1));
        match stream.send_buffer.send_syn() {
          Ok(()) => {
            internal.state = TcpStreamState::SynReceived;
          }
          Err(e) => {
            edebug!("Failed to send syn_ack {e}, closing");
            internal.state = TcpStreamState::Closed;
          }
        }
        drop(internal);
        sockets_and_ports.add_port(src_port);
        queue.send(stream)?;
        Ok(())
      }
    }
  }

  /// Create a listener socket, as a result of bind
  pub(super) fn listen(src_port: Port, info: TcpLayerInfo) -> Result<Arc<TcpStream>> {
    let mut sockets_and_ports = info.sockets_and_ports.lock().unwrap();
    if sockets_and_ports.contains_port(src_port) {
      Err(anyhow!("Error: port {src_port} in use"))
    } else {
      let new_socket = sockets_and_ports.get_new_socket();
      drop(sockets_and_ports);
      let stream = Arc::new(TcpStream::new(
        None,
        src_port,
        None,
        None,
        TcpStreamState::Listen,
        info.ip_send_tx.clone(),
        info.make_cleanup_callback(new_socket),
        new_socket,
        info.clone(),
        CongestionControlStrategy::No,
      ));
      info
        .streams
        .write()
        .unwrap()
        .insert(new_socket, stream.clone());

      Ok(stream)
    }
  }

  /// Create a regular socket, connecting to a dst_ip:dst_port addr
  pub fn connect(
    info: TcpLayerInfo,
    dst_ip: Ipv4Addr,
    dst_port: Port,
    congestion_control: CongestionControlStrategy,
  ) -> Result<Arc<TcpStream>> {
    let mut sockets_and_ports = info.sockets_and_ports.lock().unwrap();
    let src_port = sockets_and_ports.get_new_port();
    let ip_send_tx = info.ip_send_tx.clone();
    let new_socket = sockets_and_ports.get_new_socket();
    let src_ip = info.get_our_ip_addrs().iter().next().cloned();
    if src_ip.is_none() {
      panic!("No ip addresses!");
    }
    let stream = Arc::new(TcpStream::new(
      src_ip,
      src_port,
      Some(dst_ip),
      Some(dst_port),
      TcpStreamState::SynSent,
      ip_send_tx,
      info.make_cleanup_callback(new_socket),
      new_socket,
      info.clone(),
      congestion_control,
    ));

    info
      .streams
      .write()
      .unwrap()
      .insert(new_socket, stream.clone());

    stream.send_buffer.send_syn()?;

    Ok(stream)
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
  pub fn recv(&self, data: &mut [u8], should_block: bool) -> Result<usize> {
    let num_bytes = data.len();
    let mut bytes_read = 0;
    if should_block {
      loop {
        // lock order
        let state = self.state();
        let mut buf = self.recv_buffer.lock().unwrap();
        let n = buf.read_data(&mut data[bytes_read..]);
        bytes_read += n;
        if !(bytes_read < num_bytes && VALID_RECV_STATES.contains(&state)) {
          break;
        }
        // Close wait is a valid recv state, however we know that no more data is coming
        if state == TcpStreamState::CloseWait {
          if bytes_read > 0 {
            return Ok(bytes_read);
          } else {
            break;
          }
        }

        let _ = self.recv_buffer_cond.wait(buf);
      }

      let state = self.state();
      if !VALID_RECV_STATES.contains(&state) {
        Err(anyhow!("Error: connection closing"))
      } else if bytes_read == num_bytes {
        Ok(bytes_read)
      } else if state == TcpStreamState::CloseWait {
        Err(anyhow!("Error: connection closing"))
      } else {
        Err(anyhow!("Error: failure occured during blocking read"))
      }
    } else {
      let mut buf = self.recv_buffer.lock().unwrap();
      let bytes_read = buf.read_data(data);
      debug_assert!(bytes_read <= num_bytes);
      Ok(bytes_read)
    }
  }

  /// Close is a overloaded term when dealing with sockets. This function corresponds to the
  /// sending the CLOSE command as described in the RFC. This corresponds to calling shutdown with
  /// the method set to write.
  pub fn close(&self) -> Result<()> {
    let mut internal = self.internal.lock().unwrap();
    match internal.state {
      TcpStreamState::Spawning => {
        debug!("Warning: told to close in spawning state...");
        internal.state = TcpStreamState::Closed;
        Ok(())
      }
      TcpStreamState::Listen => {
        // Any outstanding RECEIVEs are returned with "error:  closing"
        // responses. Delete TCB, enter CLOSED state, and return.
        internal.state = TcpStreamState::Closed;
        Ok(())
      }
      TcpStreamState::SynSent => {
        // Delete the TCB (this probably need to be handled a level up?)
        internal.state = TcpStreamState::Closed;
        Ok(())
      }
      TcpStreamState::SynReceived => {
        // If no SENDs have been issued and there is no pending data to send,
        // then form a FIN segment and send it, and enter FIN-WAIT-1 state;
        // otherwise queue for processing after entering ESTABLISHED state.
        drop(internal);
        self.send_buffer.send_fin()?;
        Ok(())
      }
      TcpStreamState::Established => {
        // Queue this request until all preceding SENDs have been
        // segmentized; then send a FIN segment, enter CLOSING state.
        drop(internal);
        self.send_buffer.send_fin()?;
        // Don't set state to fin wait until fin is actually sent out
        Ok(())
      }
      TcpStreamState::FinWait1 => Err(anyhow!("connection closing")),
      TcpStreamState::FinWait2 => Err(anyhow!("connection closing")),
      TcpStreamState::CloseWait => {
        // NOTE: Contradiction between flow chart and page 61 of RFC which says
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
}

impl TcpStream {
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
            TcpStreamState::Spawning | TcpStreamState::Listen | TcpStreamState::SynSent => {
              if let Err(_e) = stream.send_syn() {
                edebug!("Failed to send syn");
                break;
              }
            }
            TcpStreamState::SynReceived => {
              if let Err(_e) = stream.send_syn_ack() {
                edebug!("Failed to send syn ack");
                break;
              }
            }
            other => panic!("Unexpected syn stream_msg, state {:?}", other),
          }
        }
        Ok(StreamSendThreadMsg::Outgoing(seq_num, data)) => {
          let mut stream = stream.lock().unwrap();
          if VALID_SEND_STATES.contains(&stream.state) {
            if seq_num == stream.next_seq {
              stream.next_seq = seq_num.wrapping_add(data.len() as u32);
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
          debug!("Sending Fin");
          let mut stream = stream.lock().unwrap();
          if VALID_FIN_STATES.contains(&stream.state) {
            if stream.state == TcpStreamState::Established {
              stream.state = TcpStreamState::FinWait1;
            }
            if seq_num >= stream.next_seq {
              debug_assert_eq!(stream.next_seq, seq_num);
              stream.next_seq = seq_num + 1;
            }
            if let Err(_e) = stream.send_fin(seq_num) {
              edebug!("Failed to send fin for seq {seq_num}");
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
    cleanup: Box<dyn Fn() + Send>,
    info: TcpLayerInfo,
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

      let mut timewait_timeout = None;

      loop {
        match stream_rx.recv_timeout(Duration::from_millis(10)) {
          Ok((ip_header, tcp_header, data)) => {
            let mut stream = stream.lock().unwrap();
            match stream.state {
              TcpStreamState::Spawning => {
                debug!("Got packet in spawning state, {}", stream.socket_id);
              }
              TcpStreamState::Listen => {
                if tcp_header.syn {
                  let ip: Ipv4Addr = ip_header.source.into();
                  let port = tcp_header.source_port;

                  TcpStream::spawn_and_queue(
                    &info,
                    stream.source_port(),
                    stream.socket_id,
                    ip,
                    port,
                    tcp_header.sequence_number,
                  )
                  .unwrap_or_else(|e| {
                    edebug!(
                      "Failed to spawn new connection on listener socket {}, {e}",
                      stream.socket_id
                    )
                  });
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
                    break;
                  }
                  stream.state = TcpStreamState::CloseWait;
                  recv_buffer_cond.notify_all();
                }

                if let Err(e) = handle_incoming_ack_data(&tcp_header, data, &mut stream) {
                  edebug!("{}", e);
                  break;
                }
              }
              TcpStreamState::Closed => {
                edebug!(
                  "Packet received in Closed state: This should never probably never happen?"
                );
                break;
              }
              TcpStreamState::FinWait1 => {
                if tcp_header.fin {
                  if let Err(e) = handle_fin(&tcp_header, &mut stream, true) {
                    edebug!("{}", e);
                    break;
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
                  break;
                }
              }
              TcpStreamState::FinWait2 => {
                if tcp_header.fin {
                  if let Err(e) = handle_fin(&tcp_header, &mut stream, true) {
                    edebug!("{}", e);
                    break;
                  }
                  stream.state = TcpStreamState::TimeWait;
                  timewait_timeout = Some(Instant::now() + MAX_SEGMENT_LIFETIME);
                  recv_buffer_cond.notify_all();
                }
                if let Err(e) = handle_incoming_ack_data(&tcp_header, data, &mut stream) {
                  edebug!("{}", e);
                  break;
                }
              }
              TcpStreamState::CloseWait => {
                // handle dup fin
                if tcp_header.fin {
                  if let Err(e) = handle_fin(&tcp_header, &mut stream, false) {
                    edebug!("{}", e);
                    break;
                  }
                }

                if tcp_header.ack {
                  if let Err(e) =
                    send_buffer.handle_ack(tcp_header.acknowledgment_number, tcp_header.window_size)
                  {
                    edebug!("Failed to handle ack {e}, closing...");
                    stream.state = TcpStreamState::Closed;
                    break;
                  }
                }
              }
              TcpStreamState::Closing => {
                // handle dup fin
                if tcp_header.fin {
                  if let Err(e) = handle_fin(&tcp_header, &mut stream, false) {
                    edebug!("{}", e);
                    break;
                  }
                }

                if tcp_header.ack {
                  if tcp_header.acknowledgment_number == stream.next_seq {
                    handle_ack_of_fin(&tcp_header, &*send_buffer);
                    stream.state = TcpStreamState::TimeWait;
                    timewait_timeout = Some(Instant::now() + MAX_SEGMENT_LIFETIME);
                    recv_buffer_cond.notify_all();
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
                    break;
                  }
                }

                if tcp_header.ack {
                  if tcp_header.acknowledgment_number == stream.next_seq {
                    handle_ack_of_fin(&tcp_header, &*send_buffer);
                    stream.state = TcpStreamState::Closed;
                    break;
                  }
                }
              }
            }
          }
          Err(RecvTimeoutError::Timeout) => {
            let mut stream = stream.lock().unwrap();
            if stream.state == TcpStreamState::Closed {
              debug!("State is CLOSED, closing...");
              break;
            }
            if let Some(timeout) = timewait_timeout {
              if Instant::now() > timeout
                && [TcpStreamState::FinWait1, TcpStreamState::Closing].contains(&stream.state)
              {
                debug!("StreamSendThread woken");
                debug!("MSL timed out, state is now CLOSED, closing...");
                stream.state = TcpStreamState::Closed;
                break;
              }
            }
          }
          Err(RecvTimeoutError::Disconnected) => {
            let mut stream = stream.lock().unwrap();
            stream.state = TcpStreamState::Closed;
            debug!(
              "Listen for {} stream_rx closed, closing...",
              stream.socket_id
            );
            break;
          }
        }
      }
      debug!("StreamSendThread died");

      recv_buffer_cond.notify_all();
      cleanup();
    });
  }
}

impl Drop for TcpStream {
  fn drop(&mut self) {
    self.set_state(TcpStreamState::Closed);
  }
}
