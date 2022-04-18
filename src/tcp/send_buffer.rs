use std::collections::VecDeque;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, RwLock};
use std::time::Instant;

use super::{MAX_WINDOW_SIZE, TCP_BUF_SIZE};
use crate::{debug, edebug, IpPacket};

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

  /// Last sequence number written in by application
  pub last_sequence_number: u32,
}

#[derive(Debug)]
pub(super) struct SendBuffer {
  buf: Arc<RwLock<SendData>>,

  /// Fields for sliding window
  window:                 Arc<RwLock<SendWindow>>,
  wake_up_send_thread_tx: Sender<()>,
}

impl SendBuffer {
  pub fn new(ip_send_tx: Sender<IpPacket>, initial_sequence_number: u32) -> SendBuffer {
    let (wake_up_send_thread_tx, wake_up_send_thread_rx) = mpsc::channel();
    let buf = SendBuffer {
      buf: Arc::new(RwLock::new(SendData {
        data:                 VecDeque::with_capacity(TCP_BUF_SIZE),
        last_sequence_number: initial_sequence_number,
      })),
      window: Arc::new(RwLock::new(SendWindow {
        starting_sequence: initial_sequence_number + 1,
        elems:             VecDeque::with_capacity(MAX_WINDOW_SIZE),
        bytes_in_window:   0usize,
        max_size:          2usize,
        next_timeout:      None,
      })),
      wake_up_send_thread_tx,
    };

    buf.start_send_thread(wake_up_send_thread_rx);
    buf
  }

  /// TODO: write test for off by ones on wrapping stuff
  pub fn handle_ack(&self, ack_num: u32) {
    let mut window = self.window.write().unwrap();
    window.handle_ack(ack_num);
  }

  /// blocks if no space
  pub fn write_data(&self, data: &[u8]) {
    let mut buf = self.buf.write().unwrap();
    buf.write(data);
  }

  /// TODO: does this really need to be it's own thread
  /// This thread checks what the shortest time in the window currently is, sleeps for that long,
  /// then wakes up the send thread
  pub fn start_timeout_thread(&self) {}

  /// Starts thread which owns ip_send_tx and is in charge of sending messages
  ///
  /// Wakes up when it
  fn start_send_thread(&self, wake_up_send_thread_rx: Receiver<()>) {
    todo!();
  }
}

impl SendWindow {
  /// Returns the number of bytes popped off of the window
  pub fn handle_ack(&mut self, ack_num: u32) -> u32 {
    // TODO: is there a slicker way to do this
    let window_start = self.starting_sequence;
    let window_end = window_start.wrapping_add(self.bytes_in_window.try_into().unwrap());
    let in_window_no_wrapping = ack_num > window_start && ack_num < window_end;
    let in_window_wrapping = window_end < window_start && ack_num < window_end;
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
}
