use std::collections::BTreeMap;
use std::sync::mpsc::Sender;

use anyhow::{anyhow, Result};

use super::ring_buffer::RingBuffer;
use super::tcp_stream::StreamSendThreadMsg;
use super::{MAX_WINDOW_SIZE, TCP_BUF_SIZE};
use crate::{debug, edebug};

#[derive(Debug)]
struct WindowData {
  /// Left window edge (including) SEQ LAND
  left:                    u32,
  /// Right window edge (not including) SEQ LAND
  right:                   u32,
  /// Reader index (first valid data for reader to read) SEQ LAND
  reader:                  u32,
  /// Refers to the first valid initial_sequence_number (SEQ land)
  initial_sequence_number: u32,
  /// Refers to the current ack (SEQ land)
  current_ack:             u32,
  /// Starts of intervals
  starts:                  BTreeMap<u32, u32>,
  /// Ends of intervals
  ends:                    BTreeMap<u32, u32>,
}

#[derive(Debug)]
pub(super) struct RecvBuffer {
  /// Ring buffer for TCP read storage
  buf:            RingBuffer,
  /// Window data
  win:            Option<WindowData>,
  /// Channel to send messages via tcp_stream
  stream_send_tx: Sender<StreamSendThreadMsg>,
}

impl WindowData {
  fn get_interval_between(&self, s: u32, e: u32) -> (Vec<(u32, u32)>, Vec<(u32, u32)>) {
    let starts: Vec<_> = self
      .starts
      .range(s..=e)
      .into_iter()
      .filter_map(|(&st, &en)| {
        let l = self.left;
        let r = self.right;
        if en <= l || st > r {
          None
        } else if st <= l && en > l {
          Some((l, en))
        } else if st < r && en >= r {
          Some((st, r))
        } else {
          Some((st, en))
        }
      })
      .collect();
    let ends = self
      .ends
      .range(s..e)
      .into_iter()
      .filter_map(|(&en, &st)| {
        let l = self.left;
        let r = self.right;
        if en <= l || st > r {
          None
        } else if st <= l && en > l {
          Some((en, l))
        } else if st < r && en >= r {
          Some((r, st))
        } else {
          Some((en, st))
        }
      })
      .collect();
    (starts, ends)
  }

  fn cleanup_intervals_outside_window(&mut self) {
    let remove: Vec<_> = if self.right <= self.left {
      self
        .starts
        .range(self.right..self.left)
        .into_iter()
        .filter_map(|(&s, &e)| {
          if (self.right..=self.left).contains(&e) {
            Some((s, e))
          } else {
            None
          }
        })
        .collect()
    } else {
      self
        .starts
        .range(0..self.left)
        .into_iter()
        .chain(self.starts.range(self.right..).into_iter())
        .filter_map(|(&s, &e)| {
          if (0..=self.left).contains(&e) || (self.right..).contains(&e) {
            Some((s, e))
          } else {
            None
          }
        })
        .collect()
    };
    for (s, e) in remove {
      self.starts.remove(&s);
      self.ends.remove(&e);
    }
  }

  fn handle_interval(&mut self, s: u32, e: u32) -> (u32, u32) {
    let (starts, ends) = self.get_interval_between(s, e);
    if starts.len() == 0 && ends.len() == 0 {
      // disjoint
      self.starts.insert(s, e);
      self.ends.insert(e, s);

      (s, e)
    } else if starts.len() > 0 && ends.len() == 0 {
      // overlap left
      debug_assert!(starts.len() == 1);
      let (existing_s, existing_e) = starts[0];

      self.starts.remove(&existing_s);
      self.ends.remove(&existing_e);

      self.starts.insert(s, existing_e);
      self.ends.insert(existing_e, s);

      (s, existing_e)
    } else if starts.len() == 0 && ends.len() > 0 {
      // overlap right
      debug_assert!(ends.len() == 1);
      let (existing_e, existing_s) = ends[0];

      self.starts.remove(&existing_s);
      self.ends.remove(&existing_e);

      self.starts.insert(existing_s, e);
      self.ends.insert(e, existing_s);

      (existing_s, e)
    } else {
      // general case of arbitrary intersections in the middle
      let (_, right_e) = starts.last().unwrap();
      let (_, left_s) = ends.first().unwrap();

      for (s, e) in &starts {
        self.starts.remove(&s);
        self.ends.remove(&e);
      }
      for (e, s) in &ends {
        self.starts.remove(&s);
        self.ends.remove(&e);
      }

      let l = if *left_s < s { *left_s } else { s };
      let r = if *right_e > e { *right_e } else { e };

      self.starts.insert(l, r);
      self.ends.insert(r, l);

      (l, r)
    }
  }
}

impl RecvBuffer {
  pub fn new(stream_send_tx: Sender<StreamSendThreadMsg>) -> RecvBuffer {
    debug_assert!(MAX_WINDOW_SIZE == TCP_BUF_SIZE);
    RecvBuffer {
      buf: RingBuffer::new(TCP_BUF_SIZE),
      win: None,
      stream_send_tx,
    }
  }

  pub fn handle_seq(&mut self, seq_num: u32, data: &[u8]) -> Result<()> {
    let win = self
      .win
      .as_mut()
      .expect("Fatal: Cannot call handle_seq without initializing with ISN");

    if win.left == win.right {
      todo!();
    } else if win.left < win.right {
      // no wrapping
      let seg_l = seq_num;
      let seg_r = seq_num + data.len() as u32;
      let has_l = (win.left..win.right).contains(&seg_l);
      let has_r = (win.left..win.right).contains(&seg_r);

      // If within interval, add data to buffer and update intervals
      if let Some((interval_l, interval_r)) = if has_l && has_r {
        // entirely in window
        self
          .buf
          .push_with_offset(&data, (seq_num - win.left) as usize);
        Some(win.handle_interval(seg_l, seg_r))
      } else if has_l && !has_r {
        // start of data in window, right end is outside
        self.buf.push_with_offset(
          &data[..(win.right - seg_l) as usize],
          (seg_l - win.left) as usize,
        );
        Some(win.handle_interval(seg_l, win.right))
      } else if !has_l && has_r {
        // start of data not in window, right end is inside
        self.buf.push_with_offset(
          &data[(win.left - seg_l) as usize..],
          (seg_l - win.left) as usize,
        );
        Some(win.handle_interval(win.left, seg_r))
      } else {
        debug!("Dropping packet, outside of window");
        None
      } {
        if interval_l == win.left || (interval_l..interval_r).contains(&win.left) {
          debug_assert!(interval_r > win.left);
          win.current_ack = win.current_ack.wrapping_add(interval_r - win.left);
          win.left = interval_r;
          self.buf.move_write_idx((interval_r - interval_l) as usize);
        }
      }

      win.cleanup_intervals_outside_window();
      if let Err(_) = self
        .stream_send_tx
        .send(StreamSendThreadMsg::Ack(win.current_ack))
      {
        edebug!("Could not send message to tcp_stream via stream_send_tx...");
        Err(anyhow!(
          "Could not send message to tcp_stream via stream_send_tx..."
        ))
      } else {
        Ok(())
      }
    } else {
      // wrapping
      todo!();
    }
  }

  pub fn set_initial_seq_num_data(&mut self, initial_seq: u32) {
    // accounts for SYN increment
    let isn_after_syn = initial_seq.wrapping_add(1);
    self.win = Some(WindowData {
      left:                    isn_after_syn,
      right:                   (MAX_WINDOW_SIZE as u32).wrapping_add(isn_after_syn),
      reader:                  isn_after_syn,
      initial_sequence_number: isn_after_syn,
      current_ack:             isn_after_syn,
      starts:                  BTreeMap::new(),
      ends:                    BTreeMap::new(),
    });
  }

  /// Read data into buffer, until buffer is full.
  pub fn read_data(&mut self, data: &mut [u8]) -> usize {
    let win = self
      .win
      .as_mut()
      .expect("Fatal: Cannot call handle_seq without initializing with ISN");

    let bytes_asked_and_available = (win.left.wrapping_sub(win.reader) as usize).min(data.len());

    if bytes_asked_and_available > 0 {
      let mut ready_slice = self.buf.pop(bytes_asked_and_available);
      debug_assert_eq!(ready_slice.len(), bytes_asked_and_available);
      data[..bytes_asked_and_available].copy_from_slice(&mut ready_slice);
      win.reader = win.reader.wrapping_add(bytes_asked_and_available as u32);
      win.right = win.reader.wrapping_add(TCP_BUF_SIZE as u32);
    }

    bytes_asked_and_available
  }
}

#[cfg(test)]
mod test {
  use std::sync::mpsc::{channel, Receiver};

  use ntest::timeout;

  use super::*;

  fn setup(initial_seq: u32) -> (RecvBuffer, Receiver<StreamSendThreadMsg>) {
    let (recv_thread_tx, recv_thread_rx) = channel();
    let mut rb = RecvBuffer::new(recv_thread_tx.clone());
    rb.set_initial_seq_num_data(initial_seq);
    (rb, recv_thread_rx)
  }

  fn check_ack(rx: &Receiver<StreamSendThreadMsg>, expected_ack: u32) {
    match rx.recv() {
      Ok(StreamSendThreadMsg::Ack(actual_ack)) => {
        println!("{actual_ack}");
        assert_eq!(actual_ack, expected_ack)
      }
      _ => panic!("Unexpected stream_msg"),
    }
  }

  fn send_and_check(
    buf: &mut RecvBuffer,
    rcv_rx: &Receiver<StreamSendThreadMsg>,
    seq: u32,
    data: Vec<u8>,
    expected_ack: u32,
  ) {
    print!(
      "Sending seq {seq}: {:?}, expecting {expected_ack} as ack, got ",
      data
    );
    buf.handle_seq(seq, &data).unwrap();
    check_ack(&rcv_rx, expected_ack);
  }

  #[test]
  #[timeout(1000)]
  /// Tests receiving data, with correct acks
  fn test_recv_data() {
    let (mut buf, rcv_rx) = setup(0);
    send_and_check(&mut buf, &rcv_rx, 1, vec![1, 2, 3, 4, 5, 6, 7], 8);
  }

  #[test]
  #[timeout(1000)]
  /// Tests receiving data out of order, with correct acks
  fn test_recv_data_out_of_order() {
    let (mut buf, rcv_rx) = setup(0);
    send_and_check(&mut buf, &rcv_rx, 5, vec![4, 5, 6, 7], 1);
    send_and_check(&mut buf, &rcv_rx, 3, vec![2, 3], 1);
    send_and_check(&mut buf, &rcv_rx, 2, vec![1], 1);
    // ack should go up by 8 bytes, since we wrote 8 bytes
    send_and_check(&mut buf, &rcv_rx, 1, vec![0], 9);
  }
}
