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

#[derive(Debug)]
struct WindowElement {
  num_retries:   u8,
  time_to_retry: Instant,
  size:          u32,
}

#[derive(Debug)]
struct SendWindow {
  pub elems:           VecDeque<WindowElement>,
  /// Refers to the first byte refered to by the first element of the sliding_window
  /// This should in general correspond to the last ack number received
  starting_sequence:   u32,
  /// Number of bytes currently referenced by elements in the window
  pub bytes_in_window: usize,
  /// TODO: when is this increased
  pub max_size:        usize,
  /// Next timeout
  pub next_timeout:    Option<Instant>,
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
        max_size:          MTU as usize, // TODO: what should this be
        next_timeout:      None,
      })),

      wake_send_thread_tx: wake_send_thread_tx.clone(),
    };

    buf.start_send_thread(stream_send_tx, wake_send_thread_rx, wake_timeout_thread_tx);
    buf.start_timeout_thread(wake_timeout_thread_rx, wake_send_thread_tx);
    buf
  }

  /// TODO: write test for off by ones on wrapping stuff
  pub fn handle_ack(&self, ack_num: u32) -> Result<()> {
    let mut window = self.window.write().unwrap();
    let bytes_acked = dbg!(window.handle_ack(ack_num));
    if bytes_acked > 0u32 {
      self.buf.write().unwrap().ack(bytes_acked);
      self.wake_send_thread_tx.send(())?;
    };
    Ok(())
  }

  /// blocks if no space TODO
  pub fn write_data(&self, data: &[u8]) -> Result<()> {
    let mut buf = self.buf.write().unwrap();
    buf.write(data);
    self.wake_send_thread_tx.send(())?;
    Ok(())
  }

  /// TODO: does this really need to be it's own thread
  /// This thread checks what the shortest time in the window currently is, sleeps for that long,
  /// then wakes up the send thread
  pub fn start_timeout_thread(
    &self,
    wake_timeout_thread_rx: Receiver<()>,
    wake_send_thread_tx: Sender<()>,
  ) {
    let _window = self.window.clone();
    thread::spawn(move || loop {
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
          stream_send_tx.send(StreamSendThreadMsg::Outgoing(
            curr_seq,
            Vec::from_iter(buf.read(curr_seq, elem.size as usize).copied()),
          ));
          // TODO: connection should close if this ever gets too high
          elem.num_retries += 1;
          elem.time_to_retry = now + RETRY_INTERVAL;
        }
        curr_seq += elem.size;
      }

      // TODO: we will eventually need to know the receivers window size
      let mut distance_to_end = match buf.dist_to_end_from_seq(curr_seq) {
        Some(d) => d,
        None => continue,
      };

      while window.bytes_in_window <= window.max_size && distance_to_end > 0 {
        debug!("Sending up a level");
        let bytes_left_to_send = window.max_size - window.bytes_in_window;
        let bytes_to_send = bytes_left_to_send.min(MTU).min(buf.data.len());
        stream_send_tx.send(StreamSendThreadMsg::Outgoing(
          curr_seq,
          Vec::from_iter(buf.read(curr_seq, bytes_to_send).copied()),
        ));
        window.bytes_in_window += bytes_to_send;
        curr_seq += bytes_to_send as u32;
        distance_to_end -= bytes_to_send;
        window.elems.push_back(WindowElement {
          num_retries:   0,
          time_to_retry: now + RETRY_INTERVAL,
          size:          bytes_to_send as u32,
        });
      }
      wake_timeout_thread_tx.send(());
    });
  }
}

impl SendWindow {
  /// Returns the number of bytes popped off of the window
  pub fn handle_ack(&mut self, ack_num: u32) -> u32 {
    // TODO: is there a slicker way to do this
    let window_start = self.starting_sequence;
    let window_end = window_start.wrapping_add(self.bytes_in_window.try_into().unwrap());
    let in_window_no_wrapping = dbg!(ack_num) > dbg!(window_start) && ack_num <= dbg!(window_end);
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
        bytes_popped += curr_elem.size;
        debug_assert!(bytes_popped == bytes_acked);
        self.elems.push_front(curr_elem);
        break;
      } else {
        bytes_popped += curr_elem.size;
      }
    }

    self.starting_sequence += bytes_popped;
    return bytes_popped;
  }
}

impl SendData {
  pub fn write(&mut self, data: &[u8]) {
    let left = TCP_BUF_SIZE - self.data.len();
    debug_assert!(data.len() <= left);
    self.data.extend(data);
  }

  /// reads from buffer
  pub fn read<'a>(&'a self, seq_number: u32, bytes_to_read: usize) -> vec_deque::Iter<u8> {
    debug_assert!(self.first_sequnce_number <= seq_number);
    let start = (seq_number - self.first_sequnce_number) as usize;
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
  ///
  /// TODO: handle wrapping
  pub fn dist_to_end_from_seq(&self, seq_number: u32) -> Option<usize> {
    let end = self.first_sequnce_number + (self.data.len() as u32) - 1;
    if seq_number < self.first_sequnce_number || seq_number > end {
      None
    } else {
      Some((end - seq_number + 1) as usize)
    }
  }
}

#[cfg(test)]
mod test {
  use super::*;

  fn setup() -> (SendBuffer, Receiver<StreamSendThreadMsg>) {
    let (send_thread_tx, send_thread_rx) = mpsc::channel();
    (SendBuffer::new(send_thread_tx.clone(), 0), send_thread_rx)
  }

  #[test]
  fn test_send_buffer_simple() {
    let (send_buf, send_thread_rx) = setup();
    let data = vec![1u8, 2u8, 3u8, 4u8];
    send_buf.write_data(&data);
    match send_thread_rx.recv_timeout(Duration::from_millis(100)) {
      Ok(StreamSendThreadMsg::Outgoing(seq_num, recv_data)) => {
        assert_eq!(seq_num, 1u32);
        assert_eq!(recv_data, data);
      }
      Ok(_) => panic!("Unexpected mesg from send buffer"),
      Err(_) => panic!("This should not error"),
    }
  }
}
