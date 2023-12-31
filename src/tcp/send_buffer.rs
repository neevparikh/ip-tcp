use std::collections::{vec_deque, VecDeque};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SendError, Sender};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};

use super::tcp_stream::StreamSendThreadMsg;
use super::{MAX_WINDOW_SIZE, MTU, TCP_BUF_SIZE};
use crate::{debug, edebug};

/// Note: in a weird implementation quirk Syn and Fin messages are retried 1 fewer times
const MAX_RETRIES: u8 = 4;
const RTO_LBOUND: Duration = Duration::from_millis(20);
const RTO_UBOUND: Duration = Duration::from_secs(60);
const RTO_BETA: f32 = 2f32;
const SRTT_ALPHA: f32 = 0.8f32;
const INITIAL_SRTT: Duration = Duration::from_millis(1500);
const ZERO_WINDOW_PROBE_TIMEOUT: Duration = Duration::from_secs(1);
const MAX_RETRY_TIMEOUT: Duration = Duration::from_secs(1);
const SEND_BUFFER_MAX_WINDOW_SIZE: usize = 32 * MTU;

#[derive(Debug, PartialEq)]
enum WindowElementType {
  Data,
  Syn,
  Fin,
}

#[derive(Debug)]
pub enum CongestionControlStrategy {
  No,
  Reno(CongestionControlInfo),
}

#[derive(Debug)]
pub struct CongestionControlInfo {
  pub curr_start:          u32,
  pub curr_window_size:    usize,
  /// used for detecting dup acks
  pub last_ack:            u32,
  pub dup_ack_count:       u8,
  pub ssthresh:            usize,
  pub drop_in_curr_window: bool,
}

impl CongestionControlInfo {
  pub fn new() -> CongestionControlInfo {
    CongestionControlInfo {
      /// This is initialized in SendBuffer::new()
      curr_start:          0u32,
      curr_window_size:    MTU,
      last_ack:            0u32,
      dup_ack_count:       0u8,
      ssthresh:            usize::max_value(),
      drop_in_curr_window: false,
    }
  }
}

#[derive(Debug, PartialEq)]
enum SendBufferState {
  /// Initial state. Transitions to SynSent when syn is sent
  Init,
  /// Syn has not been acked yet, data should be queued. Transitions to Ready when syn is acked
  SynSent,
  /// Syn has been acked and data can be sent. Transitions to Sending while messages are being
  /// queued. Transitions to Closing when Fin is queued
  Ready,
  /// Data is in the send buffer which is not in the window. Transitions to Ready when
  Queueing,
  /// Fin has been sent. Transitions to closed when Fin is acked or hits max retries
  FinSent,
  /// SendBuffer is dead
  _Closed,
}

#[derive(Debug)]
struct WindowElement {
  msg_type:       WindowElementType,
  num_retries:    u8,
  last_time_sent: Instant,
  time_to_retry:  Instant,
  size:           u32,
}

#[derive(Debug)]
struct SendWindow {
  pub elems:               VecDeque<WindowElement>,
  /// Refers to the first byte refered to by the first element of the sliding_window
  /// This should in general correspond to the last ack number received
  starting_sequence:       u32,
  /// Number of bytes currently referenced by elements in the window
  pub bytes_in_window:     usize,
  /// max_size is the max we are willing to send
  pub max_size:            usize,
  pub congestion_control:  CongestionControlStrategy,
  /// recv_window_size is the max they are willing to recveive
  pub recv_window_size:    u16,
  pub zero_window_probing: bool,
  /// Smoothed Round Trip Time
  srtt:                    Duration,
}

#[derive(Debug)]
struct SendData {
  pub data: VecDeque<u8>,

  /// sequence number of the byte returned by data.pop()
  pub first_sequnce_number: u32,
}

#[derive(Debug)]
pub(super) struct SendBuffer {
  state_pair: Arc<(Mutex<SendBufferState>, Condvar)>,

  buf:      Arc<Mutex<SendData>>,
  buf_cond: Arc<Condvar>,

  /// Fields for sliding window
  window:              Arc<RwLock<SendWindow>>,
  wake_send_thread_tx: Arc<Mutex<Sender<()>>>,
}

impl SendBuffer {
  pub fn new(
    stream_send_tx: Sender<StreamSendThreadMsg>,
    initial_sequence_number: u32,
    mut congestion_control: CongestionControlStrategy,
  ) -> SendBuffer {
    let (wake_send_thread_tx, wake_send_thread_rx) = mpsc::channel();
    let (wake_timeout_thread_tx, wake_timeout_thread_rx) = mpsc::channel();
    let first_seq = initial_sequence_number.wrapping_add(1);
    let state_pair = Arc::new((Mutex::new(SendBufferState::Init), Condvar::new()));
    let initial_max_size = match &mut congestion_control {
      CongestionControlStrategy::No => MTU * 5,
      CongestionControlStrategy::Reno(info) => {
        info.last_ack = initial_sequence_number;
        info.curr_start = initial_sequence_number;
        MTU
      }
    };
    let buf = SendBuffer {
      state_pair: state_pair.clone(),

      buf: Arc::new(Mutex::new(SendData {
        data:                 VecDeque::with_capacity(TCP_BUF_SIZE),
        first_sequnce_number: first_seq,
      })),

      buf_cond: Arc::new(Condvar::new()),

      window: Arc::new(RwLock::new(SendWindow {
        starting_sequence: first_seq,
        elems: VecDeque::with_capacity(MAX_WINDOW_SIZE),
        bytes_in_window: 0usize,
        max_size: initial_max_size,
        congestion_control,

        recv_window_size: 0, /* This will be initialized when we receive
                              * SYNACK/ACK */
        zero_window_probing: false,
        srtt: INITIAL_SRTT,
      })),

      wake_send_thread_tx: Arc::new(Mutex::new(wake_send_thread_tx.clone())),
    };

    buf.start_send_thread(
      stream_send_tx,
      wake_send_thread_rx,
      wake_timeout_thread_tx,
      state_pair,
    );
    buf.start_timeout_thread(wake_timeout_thread_rx, wake_send_thread_tx);
    buf
  }

  pub fn handle_ack_of_syn(&self, window_size: u16) -> Result<()> {
    let mut state = self.state_pair.0.lock().unwrap();
    match *state {
      SendBufferState::SynSent => {
        let mut window = self.window.write().unwrap();
        window.handle_ack_of_syn(window_size);
        *state = SendBufferState::Ready;
        self.state_pair.1.notify_all();
        Ok(())
      }
      _ => Err(anyhow!("Unexpected ack of syn")),
    }
  }

  pub fn handle_ack_of_fin(&self) {
    let state = self.state_pair.0.lock().unwrap();
    match *state {
      SendBufferState::FinSent => {
        let mut window = self.window.write().unwrap();
        window.handle_ack_of_fin();
        self.state_pair.1.notify_all();
      }
      _ => edebug!("Unexpected ack of syn"),
    }
  }

  pub fn handle_ack(&self, ack_num: u32, window_size: u16) -> Result<()> {
    let mut window = self.window.write().unwrap();
    let (bytes_acked, fast_retry) = window.handle_ack(ack_num, window_size);

    if bytes_acked > 0u32 {
      self.buf.lock().unwrap().ack(bytes_acked);
      self.buf_cond.notify_all();
      self.wake_send_thread()?;
    } else if fast_retry {
      self.wake_send_thread()?;
    };
    Ok(())
  }

  /// Blocks until all bytes written
  /// TODO: what is stream shuts down before we finish?
  /// NOTE: Should never have multiple threads calling this function
  pub fn write_data(&self, data: &[u8]) -> Result<usize> {
    let mut bytes_written = 0;
    let state = self.state_pair.0.lock().unwrap();
    let mut state = self
      .state_pair
      .1
      .wait_while(state, |state| match *state {
        SendBufferState::Init => true,
        SendBufferState::SynSent => true,
        _ => false,
      })
      .unwrap();

    match *state {
      SendBufferState::Ready => (),    // all good
      SendBufferState::Queueing => (), // all good
      SendBufferState::Init => panic!("Can never happen because of wait_while above"),
      SendBufferState::SynSent => panic!("Can never happen because of wait_while above"),
      _ => return Err(anyhow!("connection closing")),
    }

    *state = SendBufferState::Queueing;
    self.state_pair.1.notify_all();
    drop(state);

    while bytes_written < data.len() {
      let buf = self.buf.lock().unwrap();
      let mut buf = self
        .buf_cond
        .wait_while(buf, |buf| buf.remaining_capacity() == 0)
        .unwrap();
      bytes_written += buf.write(&data[bytes_written..]);
      self.wake_send_thread()?;
    }

    self.state_pair.1.notify_all();

    Ok(bytes_written)
  }

  pub fn window_size(&self) -> u16 {
    let win = self.window.read().unwrap();
    win.max_size.saturating_sub(win.bytes_in_window) as u16
  }

  pub fn their_recv_window_size(&self) -> u16 {
    self.window.read().unwrap().recv_window_size
  }

  pub fn send_syn(&self) -> Result<()> {
    let mut state = self.state_pair.0.lock().unwrap();
    match *state {
      SendBufferState::Init => {
        self.window.write().unwrap().send_syn();
        *state = SendBufferState::SynSent;
        self.state_pair.1.notify_all();
        self.wake_send_thread()?;
        Ok(())
      }
      SendBufferState::SynSent => {
        edebug!("send_syn called more than once, ignoring...");
        Ok(())
      }
      _ => Err(anyhow!(
        "sending syn after connection is already established/shut down"
      )),
    }
  }

  pub fn send_fin(&self) -> Result<()> {
    let state = self.state_pair.0.lock().unwrap();
    match *state {
      SendBufferState::Init => {
        panic!("Impossible since close is called by the user, which they can't do until syn_sent");
      }
      SendBufferState::Ready | SendBufferState::SynSent | SendBufferState::Queueing => {
        // if it's SynSent wait until ready
        let mut state = self
          .state_pair
          .1
          .wait_while(state, |state| {
            *state == SendBufferState::SynSent || *state == SendBufferState::Queueing
          })
          .unwrap();

        if *state != SendBufferState::Ready {
          debug!("Closed while waiting to send fin");
          return Err(anyhow!(""));
        }

        *state = SendBufferState::FinSent;
        let mut window = self.window.write().unwrap();
        self.state_pair.1.notify_all();
        window.send_fin();
        self.wake_send_thread()?;
        Ok(())
      }
      SendBufferState::FinSent => {
        debug!("send_fin called more than once, ignoring...");
        Err(anyhow!("connection closing"))
      }
      SendBufferState::_Closed => Err(anyhow!("connection closing")),
    }
  }

  pub fn _srtt(&self) -> Duration {
    self.window.read().unwrap().srtt
  }

  pub fn _rto(&self) -> Duration {
    self.window.read().unwrap().rto()
  }

  /// This thread checks what the shortest time in the window currently is, sleeps for that long,
  /// then wakes up the send thread
  fn start_timeout_thread(
    &self,
    wake_timeout_thread_rx: Receiver<()>,
    wake_send_thread_tx: Sender<()>,
  ) {
    let window = self.window.clone();
    thread::spawn(move || loop {
      let window = window.read().unwrap();
      // clear out dup wakeups so that we don't wake up a bunch of extra times
      while let Ok(()) = wake_timeout_thread_rx.try_recv() {}

      let most_urgent_elem = window.elems.iter().reduce(|acc, item| {
        if acc.time_to_retry < item.time_to_retry {
          acc
        } else {
          item
        }
      });
      let now = Instant::now();
      let mut retry_timeout = if let Some(elem) = most_urgent_elem {
        let time_to_retry = elem.time_to_retry;
        if time_to_retry < now {
          if wake_send_thread_tx.send(()).is_err() {
            edebug!("timeout thread died");
            break;
          }
          Duration::ZERO
        } else {
          time_to_retry.duration_since(now)
        }
      } else {
        MAX_RETRY_TIMEOUT
      };
      if window.zero_window_probing {
        if wake_send_thread_tx.send(()).is_err() {
          edebug!("timeout thread died");
          break;
        }
        // ingore first wakeup during zero_window_probing
        let _ = wake_timeout_thread_rx.try_recv();
        retry_timeout = ZERO_WINDOW_PROBE_TIMEOUT;
      }
      drop(window);

      match wake_timeout_thread_rx.recv_timeout(retry_timeout) {
        Ok(()) => (),                         // debug!("timeout thread woken"),
        Err(RecvTimeoutError::Timeout) => (), // debug!("timeout thread timeout"),
        Err(RecvTimeoutError::Disconnected) => {
          edebug!("timeout thread died");
          break;
        }
      }
    });
  }

  /// Starts thread which owns ip_send_tx and is in charge of sending messages
  ///
  /// Wakes up when it
  fn start_send_thread(
    &self,
    stream_send_tx: Sender<StreamSendThreadMsg>,
    wake_send_thread_rx: Receiver<()>,
    wake_timeout_thread_tx: Sender<()>,
    state_pair: Arc<(Mutex<SendBufferState>, Condvar)>,
  ) {
    let window = self.window.clone();
    let buf = self.buf.clone();
    thread::spawn(move || loop {
      if wake_send_thread_rx.recv().is_err() {
        break;
      }

      let mut state = state_pair.0.lock().unwrap();
      let mut window = window.write().unwrap();
      let buf = buf.lock().unwrap();

      // handle timeouts
      let now = Instant::now();
      let mut curr_seq = window.starting_sequence;
      let rto = window.rto();

      let mut sent_retry = false;
      let mut retried = false;
      for elem in window.elems.iter_mut() {
        if elem.time_to_retry <= now && !sent_retry {
          sent_retry = true;
          let send_res = match elem.msg_type {
            WindowElementType::Syn => stream_send_tx.send(StreamSendThreadMsg::Syn),
            WindowElementType::Data => {
              debug!("Sending retry");
              retried = true;
              stream_send_tx.send(StreamSendThreadMsg::Outgoing(
                curr_seq,
                Vec::from_iter(buf.read(curr_seq, elem.size as usize).copied()),
              ))
            }
            WindowElementType::Fin => stream_send_tx.send(StreamSendThreadMsg::Fin(curr_seq)),
          };

          if send_res.is_err() {
            debug!("Sending to stream failed in SendBuffer, closing");
            return;
          }

          elem.num_retries += 1;
          if elem.num_retries > MAX_RETRIES {
            // ignore error since we are already closing
            debug!("MAX_RETRIES exceeded, closing SendBuffer");
            let _ = stream_send_tx.send(StreamSendThreadMsg::Shutdown);
            return;
          }

          elem.last_time_sent = Instant::now();
          elem.time_to_retry = now + (rto * 2u32.pow(elem.num_retries as u32));
        }
        curr_seq = curr_seq.wrapping_add(elem.size);
      }

      match &mut window.congestion_control {
        CongestionControlStrategy::No => (),
        CongestionControlStrategy::Reno(info) => {
          info.drop_in_curr_window |= retried;
        }
      }

      let mut distance_to_end = match buf.dist_to_end_from_seq(curr_seq) {
        Some(d) => d,
        None => 0,
      };

      let max_window_size = window.max_size.min(window.recv_window_size as usize);
      let zero_window_probe = window.recv_window_size == 0 && distance_to_end > 0;

      if zero_window_probe {
        debug!("Zero Window Probing");
        window.zero_window_probing = true;
        let send_res = stream_send_tx.send(StreamSendThreadMsg::Outgoing(
          curr_seq,
          Vec::from_iter(buf.read(curr_seq, 1).copied()),
        ));

        if send_res.is_err() {
          debug!("Sending to stream failed in SendBuffer, closing");
          return;
        }
      }

      while window.bytes_in_window < max_window_size && distance_to_end > 0 {
        let bytes_left_to_send = max_window_size - window.bytes_in_window;

        let bytes_to_send = bytes_left_to_send.min(MTU).min(distance_to_end);
        let data = Vec::from_iter(buf.read(curr_seq, bytes_to_send).copied());
        let send_res = stream_send_tx.send(StreamSendThreadMsg::Outgoing(curr_seq, data));

        if send_res.is_err() {
          debug!("Sending to stream failed in SendBuffer, closing");
          return;
        }

        window.bytes_in_window += bytes_to_send;
        window.recv_window_size -= bytes_to_send as u16;
        curr_seq = curr_seq.wrapping_add(bytes_to_send as u32);
        distance_to_end -= bytes_to_send;
        window.elems.push_back(WindowElement {
          msg_type:       WindowElementType::Data,
          num_retries:    0,
          last_time_sent: now,
          time_to_retry:  now + rto,
          size:           bytes_to_send as u32,
        });
      }

      if window.bytes_in_window == buf.data.len() && *state == SendBufferState::Queueing {
        *state = SendBufferState::Ready;
        state_pair.1.notify_all()
      }

      if wake_timeout_thread_tx.send(()).is_err() {
        // ignore error since we are already closing
        debug!("timeout thread is dead, closing send_thread, sending Shutdown to stream");
        let _ = stream_send_tx.send(StreamSendThreadMsg::Shutdown);
        return;
      }
    });
  }

  fn wake_send_thread(&self) -> Result<(), SendError<()>> {
    self.wake_send_thread_tx.lock().unwrap().send(())
  }
}

impl SendWindow {
  pub fn rto(&self) -> Duration {
    RTO_UBOUND.min(RTO_LBOUND.max(self.srtt.mul_f32(RTO_BETA)))
  }

  pub fn send_syn(&mut self) {
    let now = Instant::now();
    self.elems.push_back(WindowElement {
      msg_type:       WindowElementType::Syn,
      num_retries:    0u8,
      last_time_sent: now,
      time_to_retry:  now,
      size:           0u32,
    })
  }

  pub fn send_fin(&mut self) {
    let now = Instant::now();
    self.elems.push_back(WindowElement {
      msg_type:       WindowElementType::Fin,
      num_retries:    0u8,
      last_time_sent: now,
      time_to_retry:  now,
      size:           1u32,
    })
  }

  pub fn handle_ack_of_syn(&mut self, window_size: u16) {
    self.recv_window_size = window_size;
    debug_assert!(self.elems.len() == 1);
    let elem = self.elems.pop_front();
    debug_assert!(elem.unwrap().msg_type == WindowElementType::Syn);
  }

  pub fn handle_ack_of_fin(&mut self) {
    // Pop from the back because we might have old messages we are still retrying
    let elem = self.elems.pop_back();
    debug_assert!(elem.unwrap().msg_type == WindowElementType::Fin);
  }

  /// Returns the number of bytes popped off of the window
  /// bool is whether to fast retry
  pub fn handle_ack(&mut self, ack_num: u32, window_size: u16) -> (u32, bool) {
    let starting_sequence = self.starting_sequence;
    let mut fast_retry = false;
    match &mut self.congestion_control {
      CongestionControlStrategy::No => (),
      CongestionControlStrategy::Reno(info) => {
        if ack_num == info.last_ack {
          if info.dup_ack_count == 3 {
            info.curr_window_size /= 2;
            info.curr_window_size = info.curr_window_size.max(MTU);
            fast_retry = true;
            info.dup_ack_count += 1;
          } else {
            info.dup_ack_count += 1;
          }
        } else {
          info.last_ack = ack_num;
          info.dup_ack_count = 0;
        }

        if ack_num >= info.curr_start.wrapping_add(info.curr_window_size as u32) {
          info.curr_start = ack_num;
          if info.drop_in_curr_window {
            if info.curr_window_size != MTU {
              info.ssthresh = info.curr_window_size / 2;
            }
            info.curr_window_size = MTU;
          } else {
            if info.curr_window_size < info.ssthresh {
              info.curr_window_size *= 2;
              info.curr_window_size = info.curr_window_size.min(SEND_BUFFER_MAX_WINDOW_SIZE);
              self.max_size = info.curr_window_size;
            } else {
              info.curr_window_size += MTU;
              self.max_size += MTU;
            }
          }
          info.drop_in_curr_window = false;
        }

        self.max_size = info
          .curr_start
          .wrapping_add(info.curr_window_size as u32)
          .wrapping_sub(starting_sequence) as usize;
      }
    }

    if fast_retry {
      match self.elems.get_mut(0) {
        Some(elem) => {
          self.starting_sequence;
          elem.time_to_retry = Instant::now() - Duration::from_secs(10);
        }
        None => (),
      }
    }

    self.recv_window_size = window_size;
    let window_start = self.starting_sequence;
    let window_end = window_start.wrapping_add(self.bytes_in_window.try_into().unwrap());
    let in_window_no_wrapping = ack_num > window_start && ack_num <= window_end;
    let in_window_wrapping = window_end < window_start && ack_num <= window_end;
    if !(in_window_wrapping || in_window_no_wrapping) {
      if self.zero_window_probing {
        self.zero_window_probing = false;
        // zero window probe byte not in window
        self.starting_sequence += 1;
        return (1u32, fast_retry);
      }
      return (0u32, fast_retry);
    }

    let bytes_acked = ack_num.wrapping_sub(window_start);
    debug_assert!(bytes_acked as usize <= self.bytes_in_window);
    let mut bytes_popped = 0u32;

    let now = Instant::now();
    while bytes_popped < bytes_acked {
      let curr_elem = match self.elems.pop_front() {
        Some(elem) => elem,
        None => {
          edebug!("This should be caught by the assert above");
          panic!("This is a bug, we have invalid state in the send sliding window");
        }
      };

      // handles case where ack a part of a packet
      if bytes_popped + curr_elem.size > bytes_acked {
        // We don't update srtt here since something weird happened
        let bytes_to_ack_from_curr = bytes_acked - bytes_popped;
        debug_assert!(bytes_to_ack_from_curr < curr_elem.size);
        let mut curr_elem = curr_elem;
        curr_elem.size -= bytes_to_ack_from_curr;
        bytes_popped += bytes_to_ack_from_curr;
        debug_assert_eq!(bytes_popped, bytes_acked);
        self.elems.push_front(curr_elem);
        break;
      } else {
        let rtt = now - curr_elem.last_time_sent;
        self.srtt = self.srtt.mul_f32(SRTT_ALPHA) + rtt.mul_f32(SRTT_ALPHA);
        bytes_popped += curr_elem.size;
      }
    }

    self.starting_sequence = self.starting_sequence.wrapping_add(bytes_popped);
    self.bytes_in_window = self.bytes_in_window.wrapping_sub(bytes_popped as usize);
    return (bytes_popped, fast_retry);
  }
}

impl SendData {
  pub fn write(&mut self, data: &[u8]) -> usize {
    let left = self.remaining_capacity();
    if data.len() <= left {
      self.data.extend(data);
    } else {
      self.data.extend(&data[0..left]);
    }
    return left.min(data.len());
  }

  /// reads from buffer
  pub fn read(&self, seq_number: u32, bytes_to_read: usize) -> vec_deque::Iter<u8> {
    let start = seq_number.wrapping_sub(self.first_sequnce_number) as usize;
    let end = start + bytes_to_read;
    self.data.range(start..end)
  }

  pub fn ack(&mut self, bytes_acked: u32) {
    debug_assert!(bytes_acked <= self.data.len() as u32);
    // I really want a better function to do this, should be O(1)
    for _ in 0..bytes_acked {
      self.data.pop_front();
    }
    self.first_sequnce_number = self.first_sequnce_number.wrapping_add(bytes_acked);
  }

  /// Returns none if the data associated with seq_number is not currently in the buffer, otherwise
  /// returns the number of bytes after the point inclusice of seq_number
  pub fn dist_to_end_from_seq(&self, seq_number: u32) -> Option<usize> {
    if self.data.len() == 0 {
      return None;
    }
    let start = self.first_sequnce_number;
    let end = start.wrapping_add(self.data.len() as u32);
    let in_no_wrapping = seq_number >= start && (seq_number <= end || end <= start);
    let in_wrapping = seq_number <= end && end < start;
    if !(in_wrapping || in_no_wrapping) {
      None
    } else {
      Some(end.wrapping_sub(seq_number) as usize)
    }
  }

  pub fn remaining_capacity(&self) -> usize {
    TCP_BUF_SIZE - self.data.len()
  }
}

#[cfg(test)]
mod test {
  use std::sync::mpsc::RecvTimeoutError;

  use ntest::timeout;

  use super::*;

  fn setup() -> (SendBuffer, Receiver<StreamSendThreadMsg>) {
    let (send_thread_tx, send_thread_rx) = mpsc::channel();
    let buf = SendBuffer::new(send_thread_tx.clone(), 0, CongestionControlStrategy::No);
    *buf.state_pair.0.lock().unwrap() = SendBufferState::Ready;
    let _ = buf.handle_ack(0, u16::max_value());
    (buf, send_thread_rx)
  }

  fn setup_initial_seq(initial_seq: u32) -> (SendBuffer, Receiver<StreamSendThreadMsg>) {
    let (send_thread_tx, send_thread_rx) = mpsc::channel();
    let buf = SendBuffer::new(
      send_thread_tx.clone(),
      initial_seq,
      CongestionControlStrategy::No,
    );
    *buf.state_pair.0.lock().unwrap() = SendBufferState::Ready;
    buf.handle_ack(initial_seq, u16::max_value()).unwrap();
    (buf, send_thread_rx)
  }

  fn recv_data(rx: &Receiver<StreamSendThreadMsg>, expected_seq: u32, expected_data: &[u8]) {
    match rx.recv_timeout(Duration::from_millis(100)) {
      Ok(StreamSendThreadMsg::Outgoing(seq_num, recv_data)) => {
        assert_eq!(seq_num, expected_seq);
        assert_eq!(recv_data, expected_data);
      }
      Ok(_) => panic!("Unexpected mesg from send buffer"),
      Err(RecvTimeoutError::Timeout) => panic!("Didn't receive expected msg"),
      Err(RecvTimeoutError::Disconnected) => panic!("This should not error"),
    }
  }

  /// This is the same as recv data except that we are allowed to recv only a subset of data
  fn recv_data_any_size(
    rx: &Receiver<StreamSendThreadMsg>,
    expected_seq: u32,
    expected_data: &[u8],
  ) -> usize {
    match rx.recv_timeout(Duration::from_millis(100)) {
      Ok(StreamSendThreadMsg::Outgoing(seq_num, recv_data)) => {
        assert_eq!(seq_num, expected_seq);
        assert!(recv_data.len() <= expected_data.len());
        assert_eq!(recv_data, expected_data[..recv_data.len()]);
        recv_data.len()
      }
      Ok(_) => panic!("Unexpected mesg from send buffer"),
      Err(RecvTimeoutError::Timeout) => panic!("Didn't receive expected msg"),
      Err(RecvTimeoutError::Disconnected) => panic!("This should not error"),
    }
  }

  fn recv_retry(
    rx: &Receiver<StreamSendThreadMsg>,
    expected_seq: u32,
    expected_data: &[u8],
    rto: Duration,
  ) {
    match rx.recv_timeout(rto * 2) {
      Ok(StreamSendThreadMsg::Shutdown) => panic!("Received unexpected shutdown"),
      Ok(StreamSendThreadMsg::Outgoing(seq_num, recv_data)) => {
        assert_eq!(seq_num, expected_seq);
        assert_eq!(recv_data, expected_data);
      }
      Ok(_) => panic!("Unexpected mesg from send buffer"),
      Err(RecvTimeoutError::Timeout) => panic!("Didn't receive expected retry"),
      Err(RecvTimeoutError::Disconnected) => panic!("This should not error"),
    }
  }

  fn recv_no_retry(rx: &Receiver<StreamSendThreadMsg>, rto: Duration) {
    match rx.recv_timeout(rto * 2) {
      Ok(StreamSendThreadMsg::Shutdown) => panic!("Received unexpected shutdown"),
      Ok(_) => panic!("Retransmitted despite ack"),
      Err(RecvTimeoutError::Timeout) => (),
      Err(RecvTimeoutError::Disconnected) => panic!("Channel closed"),
    }
  }

  #[test]
  #[timeout(1000)]
  fn test_send_buffer_simple() {
    let (send_buf, send_thread_rx) = setup();
    let data = vec![1u8, 2u8, 3u8, 4u8];
    send_buf.write_data(&data).unwrap();
    recv_data(&send_thread_rx, 1, &data)
  }

  #[test]
  #[timeout(1000)]
  fn test_multiple_sends() {
    let (send_buf, send_thread_rx) = setup();
    let data = vec![1u8, 2u8, 3u8, 4u8];
    send_buf.write_data(&data).unwrap();
    recv_data(&send_thread_rx, 1, &data);

    send_buf.write_data(&data).unwrap();
    recv_data(&send_thread_rx, 5u32, &data);
  }

  #[test]
  fn test_single_retry() {
    let (send_buf, send_thread_rx) = setup();
    let data = vec![1u8, 2u8, 3u8, 4u8];
    send_buf.write_data(&data).unwrap();
    recv_data(&send_thread_rx, 1u32, &data);
    recv_retry(&send_thread_rx, 1u32, &data, send_buf._rto());
  }

  /// Tests that acked bytes aren't resent
  #[test]
  fn test_single_ack() {
    let (send_buf, send_thread_rx) = setup();
    let data = vec![1u8, 2u8, 3u8, 4u8];
    send_buf.write_data(&data).unwrap();
    recv_data(&send_thread_rx, 1u32, &data);

    send_buf.handle_ack(5, u16::max_value()).unwrap();

    recv_no_retry(&send_thread_rx, send_buf._rto());
  }

  #[test]
  fn test_partial_ack() {
    let (send_buf, send_thread_rx) = setup();
    let data = vec![1u8, 2u8, 3u8, 4u8];
    send_buf.write_data(&data).unwrap();
    recv_data(&send_thread_rx, 1u32, &data);

    send_buf.handle_ack(2, u16::max_value()).unwrap();
    recv_retry(&send_thread_rx, 2u32, &[2u8, 3u8, 4u8], send_buf._rto());
  }

  #[test]
  #[timeout(1000)]
  fn test_overflow() {
    let (send_buf, send_thread_rx) = setup_initial_seq(u32::max_value() - 1);
    let data = vec![1u8];
    send_buf.write_data(&data).unwrap();
    recv_data(&send_thread_rx, u32::max_value(), &data);

    let data = vec![2u8];
    send_buf.write_data(&data).unwrap();
    recv_data(&send_thread_rx, 0, &data);

    let data = vec![3u8];
    send_buf.write_data(&data).unwrap();
    recv_data(&send_thread_rx, 1, &data);
  }

  #[test]
  #[timeout(1000)]
  fn test_send_big() {
    let (send_buf, send_thread_rx) = setup();
    let data = [0u8; MTU * 3 + 4];
    send_buf.write_data(&data).unwrap();
    recv_data(&send_thread_rx, 1, &data[0..MTU]);
    recv_data(&send_thread_rx, (MTU as u32) + 1, &data[MTU..2 * MTU]);
    recv_data(
      &send_thread_rx,
      (2 * MTU) as u32 + 1,
      &data[2 * MTU..3 * MTU],
    );
    recv_data(&send_thread_rx, (3 * MTU) as u32 + 1, &data[3 * MTU..]);
  }

  #[test]
  fn test_send_big_small_window() {
    let (send_buf, send_thread_rx) = setup();
    assert!(send_buf.handle_ack(1, MTU as u16).is_ok());
    let data = [0u8; MTU * 2];
    send_buf.write_data(&data).unwrap();
    recv_data(&send_thread_rx, 1, &data[0..MTU]);
    recv_retry(&send_thread_rx, 1, &data[0..MTU], send_buf._rto());
    assert!(send_buf.handle_ack((MTU as u32) + 1, MTU as u16).is_ok());
    // NOTE: can't really test that the next chunk is appropriately sent due to zero window
    // probing
  }

  #[test]
  #[timeout(1000)]
  fn test_send_blocking_write() {
    let (send_buf, send_thread_rx) = setup();
    let send_buf = Arc::new(send_buf);
    let send_buf_clone = send_buf.clone();
    const DATA_SIZE: usize = TCP_BUF_SIZE * 2 + 3;
    let data = [0u8; DATA_SIZE];
    let data_clone = data.clone();
    let send_handle = thread::spawn(move || {
      send_buf.write_data(&data).unwrap();
    });
    let recv_handle = thread::spawn(move || {
      let mut bytes_received = 0;
      let mut ack = 1;
      while bytes_received < DATA_SIZE {
        let n = recv_data_any_size(&send_thread_rx, ack, &data_clone[0..MTU]);
        bytes_received += n;
        ack = (bytes_received + 1) as u32;
        assert!(send_buf_clone.handle_ack(ack, u16::max_value()).is_ok());
      }
    });
    assert!(send_handle.join().is_ok());
    assert!(recv_handle.join().is_ok());
  }
}
