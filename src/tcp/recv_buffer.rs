use std::collections::{BTreeMap, HashSet};
use std::sync::mpsc::Sender;

use anyhow::{anyhow, Result};

use super::ring_buffer::RingBuffer;
use super::tcp_stream::StreamSendThreadMsg;
use super::{MAX_WINDOW_SIZE, TCP_BUF_SIZE};
use crate::{debug, edebug};

#[derive(Debug)]
struct WindowData {
  /// Left window edge (including) SEQ LAND
  left:        u32,
  /// Right window edge (not including) SEQ LAND
  right:       u32,
  /// Reader index (first valid data for reader to read) SEQ LAND
  reader:      u32,
  /// Refers to the current ack (SEQ land)
  current_ack: u32,
  /// Starts of intervals
  starts:      BTreeMap<u32, u32>,
  /// Ends of intervals
  ends:        BTreeMap<u32, u32>,
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
  fn get_interval_between(
    &self,
    requested_start: u32,
    requested_end: u32,
  ) -> (Vec<(u32, u32)>, Vec<(u32, u32)>) {
    let window_left = self.left;
    let window_right = self.right;
    if requested_start > requested_end {
      if window_left <= window_right {
        return (Vec::new(), Vec::new()); // can never have anything within window
      }
      let starts: Vec<_> = self
        .starts
        .iter()
        .filter_map(|(&cur_start, &cur_end)| {
          if cur_start > cur_end {
            if cur_start > requested_start && cur_end < requested_end {
              None
            } else {
              Some((cur_start, cur_end))
            }
          } else {
            let has_cur_start = WindowData::wrapping_within(
              cur_start,
              requested_start,
              requested_end.wrapping_add(1),
            );
            if has_cur_start {
              Some((cur_start, cur_end))
            } else {
              None
            }
          }
        })
        .collect();
      let ends = self
        .ends
        .iter()
        .filter_map(|(&cur_end, &cur_start)| {
          if cur_start > cur_end {
            if cur_start > requested_start && cur_end < requested_end {
              None
            } else {
              Some((cur_end, cur_start))
            }
          } else {
            let has_cur_end =
              WindowData::wrapping_within(cur_end, requested_start, requested_end.wrapping_add(1));
            if has_cur_end {
              Some((cur_end, cur_start))
            } else {
              None
            }
          }
        })
        .collect();
      (starts, ends)
    } else {
      let starts: Vec<_> = self
        .starts
        .range(..=requested_end)
        .into_iter()
        .filter_map(|(&cur_start, &cur_end)| {
          if cur_end < requested_start {
            None
          } else {
            Some((cur_start, cur_end))
          }
        })
        .collect();
      let ends = self
        .ends
        .range(requested_start..)
        .into_iter()
        .filter_map(|(&cur_end, &cur_start)| {
          if cur_start > requested_end {
            None
          } else {
            Some((cur_end, cur_start))
          }
        })
        .collect();
      (starts, ends)
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

      let (l, r) = if s > e {
        let l = if *left_s > s { *left_s } else { s };
        let r = if *right_e < e { *right_e } else { e };
        (l, r)
      } else {
        let l = if *left_s < s { *left_s } else { s };
        let r = if *right_e > e { *right_e } else { e };
        (l, r)
      };

      self.starts.insert(l, r);
      self.ends.insert(r, l);

      (l, r)
    }
  }

  fn wrapping_within(x: u32, l: u32, r: u32) -> bool {
    if l <= r {
      (l..r).contains(&x)
    } else {
      (l..).contains(&x) || (..r).contains(&x)
    }
  }

  fn cleanup_intervals_outside_window(&mut self) {
    let (keep_starts, _) = self.get_interval_between(self.left, self.right);
    let keep_starts: HashSet<(u32, u32)> = HashSet::from_iter(keep_starts.into_iter());
    let all_starts: Vec<_> = self.starts.iter().map(|(&s, &e)| (s, e)).collect();
    for (s, e) in all_starts {
      if keep_starts.contains(&(s, e)) {
        let has_s = WindowData::wrapping_within(s, self.left, self.right);
        let has_e = WindowData::wrapping_within(e, self.left, self.right);

        if !has_s && !has_e {
          self.starts.remove(&s);
          self.ends.remove(&e);
          self.starts.insert(self.left, self.right);
          self.ends.insert(self.right, self.left);
        } else if has_s && !has_e {
          self.starts.insert(s, self.right);
          self.ends.remove(&e);
          self.ends.insert(self.right, s);
        } else if !has_s && has_e {
          self.ends.insert(e, self.left);
          self.starts.remove(&s);
          self.starts.insert(self.left, e);
        }
      } else {
        self.starts.remove(&s);
        self.ends.remove(&e);
      }
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

  pub fn window_size(&self) -> u16 {
    let win = self
      .win
      .as_ref()
      .expect("Fatal: Cannot call handle_seq without initializing with ISN");
    // safe because MAX_WINDOW_SIZE <= u16::max_value
    win.right.wrapping_sub(win.left) as u16
  }

  pub fn handle_seq(&mut self, seq_num: u32, data: &[u8]) -> Result<()> {
    let win = self
      .win
      .as_mut()
      .expect("Fatal: Cannot call handle_seq without initializing with ISN");

    if win.left == win.right {
      debug!("Dropping packet, window size is 0");
      return Ok(());
    }

    let seg_l = seq_num;
    let seg_r = seq_num.wrapping_add(data.len() as u32);
    let has_l = WindowData::wrapping_within(seg_l, win.left, win.right);
    let has_r = WindowData::wrapping_within(seg_r.wrapping_sub(1), win.left, win.right);

    // If within interval, add data to buffer and update intervals
    if let Some((interval_l, interval_r)) = if has_l && has_r {
      // entirely in window
      self
        .buf
        .push_with_offset(&data, (seq_num.wrapping_sub(win.left)) as usize);
      Some(win.handle_interval(seg_l, seg_r))
    } else if has_l && !has_r {
      // start of data in window, right end is outside
      self.buf.push_with_offset(
        &data[..(win.right.wrapping_sub(seg_l)) as usize],
        (seg_l.wrapping_sub(win.left)) as usize,
      );
      Some(win.handle_interval(seg_l, win.right))
    } else if !has_l && has_r {
      // start of data not in window, right end is inside
      self.buf.push_with_offset(
        &data[(win.left.wrapping_sub(seg_l)) as usize..],
        (seg_l.wrapping_sub(win.left)) as usize,
      );
      Some(win.handle_interval(win.left, seg_r))
    } else {
      debug!(
        "Dropping packet {seq_num}, outside of window {}..{}, sending ack {}",
        win.left, win.right, win.current_ack
      );
      if let Err(_) = self.stream_send_tx.send(StreamSendThreadMsg::Ack(
        win.current_ack,
        self.window_size(),
      )) {
        return Err(anyhow!(
          "Could not send message to tcp_stream via stream_send_tx..."
        ));
      } else {
        return Ok(());
      }
    } {
      if interval_l == win.left || WindowData::wrapping_within(win.left, interval_l, interval_r) {
        win.current_ack = win
          .current_ack
          .wrapping_add(interval_r.wrapping_sub(win.left));
        win.left = interval_r;
        self
          .buf
          .move_write_idx((interval_r.wrapping_sub(interval_l)) as usize);
      }
    }

    win.cleanup_intervals_outside_window();
    if let Err(_) = self.stream_send_tx.send(StreamSendThreadMsg::Ack(
      win.current_ack,
      self.window_size(),
    )) {
      Err(anyhow!(
        "Could not send message to tcp_stream via stream_send_tx..."
      ))
    } else {
      Ok(())
    }
  }

  pub fn set_initial_seq_num_data(&mut self, initial_seq: u32) {
    // accounts for SYN increment
    let isn_after_syn = initial_seq.wrapping_add(1);
    self.win = Some(WindowData {
      left:        isn_after_syn,
      right:       (MAX_WINDOW_SIZE as u32).wrapping_add(isn_after_syn),
      reader:      isn_after_syn,
      current_ack: isn_after_syn,
      starts:      BTreeMap::new(),
      ends:        BTreeMap::new(),
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
      let bytes_read = self.buf.pop(data);
      debug_assert_eq!(bytes_read, bytes_asked_and_available);
      win.reader = win.reader.wrapping_add(bytes_asked_and_available as u32);
      win.right = win.right.wrapping_add(bytes_asked_and_available as u32);
      debug_assert_eq!(win.reader.wrapping_add(TCP_BUF_SIZE as u32), win.right);
      if let Err(_) = self
        .stream_send_tx
        .send(StreamSendThreadMsg::UpdateWindowSize(self.window_size()))
      {
        edebug!("Could not send message to tcp_stream via stream_send_tx...");
        return 0;
      }
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
      Ok(StreamSendThreadMsg::Ack(actual_ack, _)) => {
        println!("{actual_ack}");
        assert_eq!(actual_ack, expected_ack)
      }
      Ok(StreamSendThreadMsg::UpdateWindowSize(_)) => (),
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
    if data.len() > 15 {
      print!(
        "Sending seq {seq}: {:?}..., expecting {expected_ack} as ack, got ",
        data[..15].to_vec()
      );
    } else {
      print!(
        "Sending seq {seq}: {:?}, expecting {expected_ack} as ack, got ",
        data
      );
    }
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

  #[test]
  #[timeout(1000)]
  /// Tests receiving data when it wraps around the internal buffer
  fn test_recv_data_wrap_internal_buffer() {
    let (mut buf, rcv_rx) = setup(0);
    send_and_check(
      &mut buf,
      &rcv_rx,
      1,
      vec![1; TCP_BUF_SIZE],
      1 + TCP_BUF_SIZE as u32,
    );
    buf.read_data(&mut vec![0u8; TCP_BUF_SIZE / 2]);
    send_and_check(
      &mut buf,
      &rcv_rx,
      1 + TCP_BUF_SIZE as u32,
      vec![2; TCP_BUF_SIZE / 2],
      1 + TCP_BUF_SIZE as u32 + (TCP_BUF_SIZE / 2) as u32,
    );
    let mut expected = vec![2u8; TCP_BUF_SIZE / 2];
    expected.append(&mut vec![1u8; TCP_BUF_SIZE - (TCP_BUF_SIZE / 2)]);
    assert_eq!(buf.buf._get_raw_buf().clone(), &expected);
  }

  #[test]
  #[timeout(1000)]
  /// Tests receiving ack when sequence number wraps around
  fn test_recv_data_wrap_seq_num() {
    let (mut buf, rcv_rx) = setup(u32::max_value() - 1);
    send_and_check(&mut buf, &rcv_rx, u32::max_value(), vec![1, 1], 1);
    let mut actual = vec![0, 0];
    buf.read_data(&mut actual);
    assert_eq!(actual, vec![1, 1]);
  }

  #[test]
  #[timeout(1000)]
  /// Tests receiving data when sequence number wraps around out of order
  fn test_recv_data_wrap_seq_num_out_of_order() {
    let (mut buf, rcv_rx) = setup(u32::max_value() - 1);
    send_and_check(&mut buf, &rcv_rx, 2, vec![1, 1], u32::max_value());
    send_and_check(&mut buf, &rcv_rx, u32::max_value(), vec![2, 2, 2], 4);
  }
}
