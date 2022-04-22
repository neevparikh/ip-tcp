use std::collections::{vec_deque, VecDeque};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;

use super::tcp_stream::StreamSendThreadMsg;
use super::{MAX_WINDOW_SIZE, MTU, TCP_BUF_SIZE};
use crate::{debug, edebug, IpPacket};

const RETRY_INTERVAL: Duration = Duration::from_secs(2);
const MAX_RETRIES: u8 = 4;

#[derive(Debug)]
struct WindowElement {
  num_retries:   u8,
  time_to_retry: Instant,
  size:          u32,
}

#[derive(Debug)]
struct SendWindow {
  pub elems:            VecDeque<WindowElement>,
  /// Refers to the first byte refered to by the first element of the sliding_window
  /// This should in general correspond to the last ack number received
  starting_sequence:    u32,
  /// Number of bytes currently referenced by elements in the window
  pub bytes_in_window:  usize,
  /// max_size is the max we are willing to send
  pub max_size:         usize,
  /// recv_window_size is the max they are willing to recveive
  pub recv_window_size: u16,
  /// Next timeout
  pub next_timeout:     Option<Instant>,
}

#[derive(Debug)]
struct SendData {
  pub data: VecDeque<u8>,

  /// sequence number of the byte returned by data.pop()
  pub first_sequnce_number: u32,
}

#[derive(Debug)]
pub(super) struct SendBuffer {
  buf: Arc<RwLock<SendData>>,

  /// Fields for sliding window
  window:              Arc<RwLock<SendWindow>>,
  wake_send_thread_tx: Sender<()>,
}

impl SendBuffer {
  pub fn new(
    stream_send_tx: Sender<StreamSendThreadMsg>,
    initial_sequence_number: u32,
  ) -> SendBuffer {
    let (wake_send_thread_tx, wake_send_thread_rx) = mpsc::channel();
    let (wake_timeout_thread_tx, wake_timeout_thread_rx) = mpsc::channel();
    let buf = SendBuffer {
      buf:    Arc::new(RwLock::new(SendData {
        data:                 VecDeque::with_capacity(TCP_BUF_SIZE),
        first_sequnce_number: initial_sequence_number + 1,
      })),
      window: Arc::new(RwLock::new(SendWindow {
        starting_sequence: initial_sequence_number + 1,
        elems:             VecDeque::with_capacity(MAX_WINDOW_SIZE),
        bytes_in_window:   0usize,
        max_size:          TCP_BUF_SIZE, // TODO: what should this be to start
        recv_window_size:  0,            // This will be initialized when we receive SYNACK/ACK
        next_timeout:      None,
      })),

      wake_send_thread_tx: wake_send_thread_tx.clone(),
    };

    buf.start_send_thread(stream_send_tx, wake_send_thread_rx, wake_timeout_thread_tx);
    buf.start_timeout_thread(wake_timeout_thread_rx, wake_send_thread_tx);
    buf
  }

  pub fn handle_ack(&self, ack_num: u32, window_size: u16) -> Result<()> {
    let mut window = self.window.write().unwrap();
    let bytes_acked = window.handle_ack(ack_num, window_size);
    if bytes_acked > 0u32 {
      self.buf.write().unwrap().ack(bytes_acked);
      self.wake_send_thread_tx.send(())?;
    };
    Ok(())
  }

  /// Returns number of bytes written
  pub fn write_data(&self, data: &[u8]) -> Result<usize> {
    let mut buf = self.buf.write().unwrap();
    let bytes_written = buf.write(data);
    self.wake_send_thread_tx.send(())?;
    Ok(bytes_written)
  }

  /// This thread checks what the shortest time in the window currently is, sleeps for that long,
  /// then wakes up the send thread
  pub fn start_timeout_thread(
    &self,
    wake_timeout_thread_rx: Receiver<()>,
    wake_send_thread_tx: Sender<()>,
  ) {
    let window = self.window.clone();
    thread::spawn(move || loop {
      let window = window.read().unwrap();
      let most_urgent_elem = window.elems.iter().reduce(|acc, item| {
        if acc.time_to_retry < item.time_to_retry {
          acc
        } else {
          item
        }
      });
      let time_to_retry = if let Some(elem) = most_urgent_elem {
        Some(elem.time_to_retry)
      } else {
        None
      };
      drop(window);

      if let Some(time_to_retry) = time_to_retry {
        let now = Instant::now();
        if time_to_retry > now {
          thread::sleep(time_to_retry.duration_since(now));
        }

        if wake_send_thread_tx.send(()).is_err() {
          edebug!("timeout thread died");
          break;
        }
      }

      match wake_timeout_thread_rx.recv() {
        Ok(()) => debug!("timeout thread woken"),
        Err(_) => {
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
  ) {
    let window = self.window.clone();
    let buf = self.buf.clone();
    thread::spawn(move || loop {
      if wake_send_thread_rx.recv().is_err() {
        break;
      }

      let mut window = window.write().unwrap();
      let buf = buf.read().unwrap();

      // handle timeouts
      let now = Instant::now();
      let mut curr_seq = window.starting_sequence;
      for elem in window.elems.iter_mut() {
        if elem.time_to_retry < now {
          debug!("Sending retry");
          let send_res = stream_send_tx.send(StreamSendThreadMsg::Outgoing(
            curr_seq,
            Vec::from_iter(buf.read(curr_seq, elem.size as usize).copied()),
          ));

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

          elem.time_to_retry = now + RETRY_INTERVAL;
        }
        curr_seq = curr_seq.wrapping_add(elem.size);
      }

      let mut distance_to_end = match buf.dist_to_end_from_seq(curr_seq) {
        Some(d) => d,
        None => continue,
      };

      let max_window_size = window.max_size.min(window.recv_window_size as usize);

      while dbg!(window.bytes_in_window) < dbg!(max_window_size) && distance_to_end > 0 {
        debug!("Sending up a level");
        let bytes_left_to_send = max_window_size - window.bytes_in_window;
        let bytes_to_send = bytes_left_to_send.min(MTU).min(distance_to_end);
        let send_res = stream_send_tx.send(StreamSendThreadMsg::Outgoing(
          curr_seq,
          Vec::from_iter(buf.read(curr_seq, bytes_to_send).copied()),
        ));

        if send_res.is_err() {
          debug!("Sending to stream failed in SendBuffer, closing");
          return;
        }

        window.bytes_in_window += bytes_to_send;
        window.recv_window_size -= bytes_to_send as u16;
        curr_seq = curr_seq.wrapping_add(bytes_to_send as u32);
        distance_to_end -= bytes_to_send;
        window.elems.push_back(WindowElement {
          num_retries:   0,
          time_to_retry: now + RETRY_INTERVAL,
          size:          bytes_to_send as u32,
        });
      }

      if wake_timeout_thread_tx.send(()).is_err() {
        // ignore error since we are already closing
        debug!("timeout thread is dead, closing send_thread, sending Shutdown to stream");
        let _ = stream_send_tx.send(StreamSendThreadMsg::Shutdown);
        return;
      }
    });
  }
}

impl SendWindow {
  /// Returns the number of bytes popped off of the window
  pub fn handle_ack(&mut self, ack_num: u32, window_size: u16) -> u32 {
    self.recv_window_size = window_size;
    let window_start = self.starting_sequence;
    let window_end = window_start.wrapping_add(self.bytes_in_window.try_into().unwrap());
    let in_window_no_wrapping = ack_num > window_start && ack_num <= window_end;
    let in_window_wrapping = window_end < window_start && ack_num <= window_end;
    if !(in_window_wrapping || in_window_no_wrapping) {
      return 0u32;
    }

    let bytes_acked = ack_num.wrapping_sub(window_start);
    debug_assert!(bytes_acked as usize <= self.bytes_in_window);
    let mut bytes_popped = 0u32;

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
        let bytes_to_ack_from_curr = bytes_acked - bytes_popped;
        debug_assert!(bytes_to_ack_from_curr < curr_elem.size);
        let mut curr_elem = curr_elem;
        curr_elem.size -= bytes_to_ack_from_curr;
        bytes_popped += bytes_to_ack_from_curr;
        debug_assert_eq!(bytes_popped, bytes_acked);
        self.elems.push_front(curr_elem);
        break;
      } else {
        bytes_popped += curr_elem.size;
      }
    }

    self.starting_sequence += bytes_popped;
    self.bytes_in_window -= bytes_popped as usize;
    return bytes_popped;
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
    self.first_sequnce_number += bytes_acked;
  }

  /// Returns none if the data associated with seq_number is not currently in the buffer, otherwise
  /// returns the number of bytes after the point inclusice of seq_number
  pub fn dist_to_end_from_seq(&self, seq_number: u32) -> Option<usize> {
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

  use super::*;

  fn setup() -> (SendBuffer, Receiver<StreamSendThreadMsg>) {
    let (send_thread_tx, send_thread_rx) = mpsc::channel();
    let buf = SendBuffer::new(send_thread_tx.clone(), 0);
    let _ = buf.handle_ack(0, u16::max_value());
    (buf, send_thread_rx)
  }

  fn setup_initial_seq(initial_seq: u32) -> (SendBuffer, Receiver<StreamSendThreadMsg>) {
    let (send_thread_tx, send_thread_rx) = mpsc::channel();
    let buf = SendBuffer::new(send_thread_tx.clone(), initial_seq);
    buf.handle_ack(initial_seq, u16::max_value());
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

  fn recv_retry(rx: &Receiver<StreamSendThreadMsg>, expected_seq: u32, expected_data: &[u8]) {
    match rx.recv_timeout(RETRY_INTERVAL * 2) {
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

  fn recv_no_retry(rx: &Receiver<StreamSendThreadMsg>) {
    match rx.recv_timeout(RETRY_INTERVAL * 2) {
      Ok(StreamSendThreadMsg::Shutdown) => panic!("Received unexpected shutdown"),
      Ok(_) => panic!("Retransmitted despite ack"),
      Err(RecvTimeoutError::Timeout) => (),
      Err(RecvTimeoutError::Disconnected) => panic!("Channel closed"),
    }
  }

  #[test]
  fn test_send_buffer_simple() {
    let (send_buf, send_thread_rx) = setup();
    let data = vec![1u8, 2u8, 3u8, 4u8];
    send_buf.write_data(&data).unwrap();
    recv_data(&send_thread_rx, 1, &data)
  }

  #[test]
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
    recv_retry(&send_thread_rx, 1u32, &data);
  }

  /// Tests that acked bytes aren't resent
  #[test]
  fn test_single_ack() {
    let (send_buf, send_thread_rx) = setup();
    let data = vec![1u8, 2u8, 3u8, 4u8];
    send_buf.write_data(&data).unwrap();
    recv_data(&send_thread_rx, 1u32, &data);

    send_buf.handle_ack(5, u16::max_value()).unwrap();

    recv_no_retry(&send_thread_rx);
  }

  #[test]
  fn test_partial_ack() {
    let (send_buf, send_thread_rx) = setup();
    let data = vec![1u8, 2u8, 3u8, 4u8];
    send_buf.write_data(&data).unwrap();
    recv_data(&send_thread_rx, 1u32, &data);

    send_buf.handle_ack(2, u16::max_value()).unwrap();
    recv_retry(&send_thread_rx, 2u32, &[2u8, 3u8, 4u8]);
  }

  #[test]
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
  fn test_write_too_much() {
    let (send_buf, _) = setup();
    let data = [0u8; 1 << 17];
    let bytes_writen = send_buf.write_data(&data).unwrap();
    assert!(bytes_writen == TCP_BUF_SIZE);
  }

  #[test]
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
    recv_retry(&send_thread_rx, 1, &data[0..MTU]);
    assert!(send_buf.handle_ack((MTU as u32) + 1, MTU as u16).is_ok());
    recv_data(&send_thread_rx, (MTU as u32) + 1, &data[MTU..2 * MTU]);
  }
}
