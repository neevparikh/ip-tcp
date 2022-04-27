use std::collections::BTreeMap;
use std::sync::mpsc::Sender;

use anyhow::{anyhow, Result};

use super::tcp_stream::StreamSendThreadMsg;
use super::TCP_BUF_SIZE;
use crate::edebug;
use crate::tcp::{MAX_WINDOW_SIZE, MTU};

#[derive(Debug)]
struct RecvWindow {
  /// Refers to the first valid initial_sequence_number (SEQ land)
  initial_sequence_number: Option<u32>,
  /// Refers to the current ack (SEQ land)
  current_ack:             Option<u32>,
  /// Refers to the index which represents the left edge of window (BUF land)
  left_index:              u32,
  /// Refers to the index which represents the right edge of window (BUF land)
  right_index:             u32,
  /// Refers to the index where the reader is (BUF land)
  reader_index:            u32,
  /// Data structure of starting indices for intervals (BUF land)
  starts:                  BTreeMap<u32, u32>,
  /// Data structure of ending indices for intervals (BUF land)
  ends:                    BTreeMap<u32, u32>,
}

#[derive(Debug)]
pub(super) struct RecvBuffer {
  buf:            [u8; TCP_BUF_SIZE],
  win:            RecvWindow,
  stream_send_tx: Sender<StreamSendThreadMsg>,
}

impl RecvBuffer {
  pub fn new(stream_send_tx: Sender<StreamSendThreadMsg>) -> RecvBuffer {
    // this is so that we can never overflow u32 when trying to add new data to our buffer
    debug_assert!((TCP_BUF_SIZE + MTU) < u32::max_value() as usize);
    debug_assert!(MAX_WINDOW_SIZE <= TCP_BUF_SIZE);
    RecvBuffer {
      buf: [0u8; TCP_BUF_SIZE],
      stream_send_tx,
      win: RecvWindow {
        initial_sequence_number: None,
        current_ack:             None,
        left_index:              0,
        right_index:             MAX_WINDOW_SIZE as u32,
        reader_index:            0,
        starts:                  BTreeMap::new(),
        ends:                    BTreeMap::new(),
      },
    }
  }

  /// Read data into buffer, until buffer is full.
  pub fn read_data(&mut self, data: &mut [u8]) -> usize {
    let bytes_available = (self.win.left_index - self.win.reader_index) as usize;

    if bytes_available > 0 {
      let data = &mut data[..bytes_available];
      let mut ready_slice = self.buf
        [self.win.reader_index as usize..(self.win.reader_index as usize) + bytes_available]
        .to_vec();
      data.swap_with_slice(&mut ready_slice);
      self.win.reader_index = self.win.reader_index.wrapping_add(bytes_available as u32);
    }

    bytes_available
  }

  pub fn set_initial_seq_num(&mut self, initial_seq: u32) {
    // accounts for SYN increment
    self.win.initial_sequence_number = Some(initial_seq.wrapping_add(1));
    self.win.current_ack = Some(initial_seq.wrapping_add(1));
  }

  fn get_interval_between(&self, s: u32, e: u32) -> (Vec<(u32, u32)>, Vec<(u32, u32)>) {
    let starts: Vec<_> = self
      .win
      .starts
      .range(s..=e)
      .into_iter()
      .filter_map(|(&st, &en)| {
        let l = self.win.left_index;
        let r = self.win.right_index;
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
      .win
      .ends
      .range(s..e)
      .into_iter()
      .filter_map(|(&en, &st)| {
        let l = self.win.left_index;
        let r = self.win.right_index;
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
    let remove: Vec<_> = if self.win.right_index <= self.win.left_index {
      self
        .win
        .starts
        .range(self.win.right_index..self.win.left_index)
        .into_iter()
        .filter_map(|(&s, &e)| {
          if (self.win.right_index..=self.win.left_index).contains(&e) {
            Some((s, e))
          } else {
            None
          }
        })
        .collect()
    } else {
      self
        .win
        .starts
        .range(0..self.win.left_index)
        .into_iter()
        .chain(self.win.starts.range(self.win.right_index..).into_iter())
        .filter_map(|(&s, &e)| {
          if (0..=self.win.left_index).contains(&e) || (self.win.right_index..).contains(&e) {
            Some((s, e))
          } else {
            None
          }
        })
        .collect()
    };
    for (s, e) in remove {
      self.win.starts.remove(&s);
      self.win.ends.remove(&e);
    }
  }

  fn handle_interval(
    &mut self,
    s: u32,
    e: u32,
    starts: Vec<(u32, u32)>,
    ends: Vec<(u32, u32)>,
    mut data: Vec<u8>,
  ) -> (u32, u32) {
    if starts.len() == 0 && ends.len() == 0 {
      // disjoint
      self.win.starts.insert(s, e);
      self.win.ends.insert(e, s);

      debug_assert_eq!((e - s) as usize, data.len());
      self.buf[s as usize..e as usize].swap_with_slice(&mut data);

      (s, e)
    } else if starts.len() > 0 && ends.len() == 0 {
      // overlap left
      debug_assert!(starts.len() == 1);
      let (existing_s, existing_e) = starts[0];

      self.win.starts.remove(&existing_s);
      self.win.ends.remove(&existing_e);

      self.win.starts.insert(s, existing_e);
      self.win.ends.insert(existing_e, s);

      debug_assert!(((existing_s - s) as usize) <= data.len());
      self.buf[s as usize..existing_s as usize]
        .swap_with_slice(&mut data[..(existing_s - s) as usize]);

      (s, existing_e)
    } else if starts.len() == 0 && ends.len() > 0 {
      // overlap right
      debug_assert!(ends.len() == 1);
      let (existing_e, existing_s) = ends[0];

      self.win.starts.remove(&existing_s);
      self.win.ends.remove(&existing_e);

      self.win.starts.insert(existing_s, e);
      self.win.ends.insert(e, existing_s);

      debug_assert!(((e - existing_e) as usize) <= data.len());
      self.buf[existing_e as usize..e as usize]
        .swap_with_slice(&mut data[(existing_e - s) as usize..]);

      (existing_s, e)
    } else {
      // general case of arbitrary intersections in the middle
      let (_, right_e) = starts.last().unwrap();
      let (_, left_s) = ends.first().unwrap();

      for (s, e) in &starts {
        self.win.starts.remove(&s);
        self.win.ends.remove(&e);
      }
      for (e, s) in &ends {
        self.win.starts.remove(&s);
        self.win.ends.remove(&e);
      }

      let l = if *left_s < s { *left_s } else { s };
      let r = if *right_e > e { *right_e } else { e };

      self.win.starts.insert(l, r);
      self.win.ends.insert(r, l);

      self.buf[s as usize..e as usize].swap_with_slice(&mut data);

      (l, r)
    }
  }

  /// Handle incoming packet, with a seq_num and data, moving window appropriately.
  pub fn handle_seq(&mut self, seq_num: u32, data: Vec<u8>) -> Result<()> {
    let size: u32 = data.len().try_into().unwrap();
    if seq_num >= self.win.initial_sequence_number.unwrap() {
      let s = seq_num - self.win.initial_sequence_number.unwrap(); // can't overflow u32

      let e = s + size; // this cannot overflow u32, because size is max data per packet.
      let overflow = e > TCP_BUF_SIZE as u32;

      if overflow {
        let (starts, ends) = self.get_interval_between(s, TCP_BUF_SIZE as u32);
        let pre = TCP_BUF_SIZE - s as usize;
        let (l, _) =
          self.handle_interval(s, TCP_BUF_SIZE as u32, starts, ends, data[..pre].to_vec());
        debug_assert!(l < TCP_BUF_SIZE as u32);
        let e = e - TCP_BUF_SIZE as u32;
        let (starts, ends) = self.get_interval_between(0, e);
        let (_, r) = self.handle_interval(0, e, starts, ends, data[pre..].to_vec());

        if self.win.left_index == l
          || (l..TCP_BUF_SIZE as u32).contains(&self.win.left_index)
          || (0..r).contains(&self.win.left_index)
        {
          self.win.current_ack = Some(
            self
              .win
              .current_ack
              .unwrap()
              .wrapping_add(TCP_BUF_SIZE as u32 - l + r), // TODO: check for ack overflow
          );
          self.win.left_index = r;
        }
      } else {
        let (starts, ends) = self.get_interval_between(s, e);
        let (l, r) = self.handle_interval(s, e, starts, ends, data);

        if self.win.left_index == l || (l..r).contains(&self.win.left_index) {
          self.win.current_ack = Some(
            self
              .win
              .current_ack
              .unwrap()
              .wrapping_add(r - self.win.left_index), // TODO check for ack overflow
          );
          self.win.left_index = r;
        }
      }

      self.cleanup_intervals_outside_window();

      if let Err(_) = self
        .stream_send_tx
        .send(StreamSendThreadMsg::Ack(self.win.current_ack.unwrap()))
      {
        edebug!("Could not send message to tcp_stream via stream_send_tx...");
        Err(anyhow!(
          "Could not send message to tcp_stream via stream_send_tx..."
        ))
      } else {
        Ok(())
      }
    } else {
      Err(anyhow!("Out of order seq_num - TODO: accept wrapping"))
    }
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
    rb.set_initial_seq_num(initial_seq);
    (rb, recv_thread_rx)
  }

  #[test]
  fn test_adding() {
    let (mut buf, _rcv) = setup(0);
    buf.handle_seq(1, vec![0u8, 1u8]).unwrap();
    debug_assert_eq!(buf.win.left_index, 2);
    buf.handle_seq(3, vec![2u8, 3u8]).unwrap();
    debug_assert_eq!(buf.win.left_index, 4);

    assert_eq!(buf.buf.clone()[0..4], vec![0u8, 1u8, 2u8, 3u8]);
  }

  #[test]
  fn test_disjoint() {
    let (mut buf, _rcv) = setup(0);
    buf.handle_seq(1, vec![0u8, 1u8]).unwrap();
    debug_assert_eq!(buf.win.left_index, 2);
    buf.handle_seq(8, vec![7u8, 8u8]).unwrap();
    debug_assert_eq!(buf.win.left_index, 2);

    assert_eq!(
      buf.buf.clone()[0..9],
      vec![0u8, 1u8, 0u8, 0u8, 0u8, 0u8, 0u8, 7u8, 8u8]
    );

    buf.handle_seq(4, vec![3u8, 4u8]).unwrap();

    assert_eq!(
      buf.buf.clone()[0..9],
      vec![0u8, 1u8, 0u8, 3u8, 4u8, 0u8, 0u8, 7u8, 8u8]
    );
    debug_assert_eq!(buf.win.left_index, 2);
  }

  #[test]
  fn test_exact_interval() {
    let (mut buf, _rcv) = setup(0);
    buf.handle_seq(1, vec![0u8, 1u8]).unwrap();
    debug_assert_eq!(buf.win.left_index, 2);
    buf.handle_seq(5, vec![4u8, 5u8]).unwrap();
    debug_assert_eq!(buf.win.left_index, 2);

    assert_eq!(buf.buf.clone()[0..6], vec![0u8, 1u8, 0u8, 0u8, 4u8, 5u8]);

    buf.handle_seq(3, vec![2u8, 3u8]).unwrap();

    assert_eq!(buf.buf.clone()[0..6], vec![0u8, 1u8, 2u8, 3u8, 4u8, 5u8]);
    debug_assert_eq!(buf.win.left_index, 6);
  }

  #[test]
  fn test_overlap_left() {
    let (mut buf, _rcv) = setup(0);
    buf.handle_seq(1, vec![0u8, 1u8, 2u8]).unwrap();
    buf.handle_seq(5, vec![4u8, 5u8]).unwrap();

    assert_eq!(buf.buf.clone()[0..6], vec![0u8, 1u8, 2u8, 0u8, 4u8, 5u8]);

    buf.handle_seq(3, vec![2u8, 3u8]).unwrap();

    assert_eq!(buf.buf.clone()[0..6], vec![0u8, 1u8, 2u8, 3u8, 4u8, 5u8]);
    debug_assert_eq!(buf.win.left_index, 6);
  }

  #[test]
  fn test_overlap_right() {
    let (mut buf, _rcv) = setup(0);
    buf.handle_seq(1, vec![0u8, 1u8]).unwrap();
    buf.handle_seq(4, vec![3u8, 4u8, 5u8]).unwrap();

    assert_eq!(buf.buf.clone()[0..6], vec![0u8, 1u8, 0u8, 3u8, 4u8, 5u8]);

    buf.handle_seq(3, vec![2u8, 3u8]).unwrap();

    assert_eq!(buf.buf.clone()[0..6], vec![0u8, 1u8, 2u8, 3u8, 4u8, 5u8]);
    debug_assert_eq!(buf.win.left_index, 6);
  }

  #[test]
  /// Consumes an entire existing interval, without intersecting others
  fn test_subsume_no_intersection() {
    let (mut buf, _rcv) = setup(0);
    buf.handle_seq(3, vec![2u8, 3u8]).unwrap();

    assert_eq!(buf.buf.clone()[0..6], vec![0u8, 0u8, 2u8, 3u8, 0u8, 0u8]);

    buf.handle_seq(2, vec![1u8, 2u8, 3u8, 4u8]).unwrap();

    assert_eq!(buf.buf.clone()[0..6], vec![0u8, 1u8, 2u8, 3u8, 4u8, 0u8]);
    debug_assert_eq!(buf.win.left_index, 0);
  }

  #[test]
  /// Consumes an entire existing interval, with left intersection
  fn test_subsume_left_intersection() {
    let (mut buf, _rcv) = setup(0);

    buf.handle_seq(1, vec![0u8, 1u8]).unwrap();

    dbg!(&buf.win.starts);
    dbg!(&buf.win.ends);

    buf.handle_seq(4, vec![3u8, 4u8]).unwrap();

    dbg!(&buf.win.starts);
    dbg!(&buf.win.ends);

    debug_assert!(buf.win.starts.contains_key(&3u32));
    debug_assert!(buf.win.ends.contains_key(&5u32));
    debug_assert_eq!(buf.win.starts[&3u32], 5u32);
    debug_assert_eq!(buf.win.ends[&5u32], 3u32);

    assert_eq!(
      buf.buf.clone()[0..9],
      vec![0u8, 1u8, 0u8, 3u8, 4u8, 0u8, 0u8, 0u8, 0u8]
    );

    buf.handle_seq(2, vec![1u8, 2u8, 3u8, 4u8, 5u8]).unwrap();

    assert_eq!(
      buf.buf.clone()[0..9],
      vec![0u8, 1u8, 2u8, 3u8, 4u8, 5u8, 0u8, 0u8, 0u8]
    );
    debug_assert_eq!(buf.win.left_index, 6);

    dbg!(&buf.win.starts);
    dbg!(&buf.win.ends);
    debug_assert_eq!(buf.win.starts.len(), 0);
    debug_assert_eq!(buf.win.ends.len(), 0);
  }

  #[test]
  /// Consumes an entire existing interval, with right intersection
  fn test_subsume_right_intersection() {
    let (mut buf, _rcv) = setup(0);

    buf.handle_seq(8, vec![7u8, 8u8]).unwrap();
    buf.handle_seq(4, vec![3u8, 4u8]).unwrap();

    assert_eq!(
      buf.buf.clone()[0..9],
      vec![0u8, 0u8, 0u8, 3u8, 4u8, 0u8, 0u8, 7u8, 8u8]
    );

    buf
      .handle_seq(3, vec![2u8, 3u8, 4u8, 5u8, 6u8, 7u8])
      .unwrap();

    assert_eq!(
      buf.buf.clone()[0..9],
      vec![0u8, 0u8, 2u8, 3u8, 4u8, 5u8, 6u8, 7u8, 8u8]
    );
    debug_assert_eq!(buf.win.left_index, 0);

    dbg!(&buf.win.starts);
    dbg!(&buf.win.ends);
    debug_assert!(buf.win.starts.contains_key(&2u32));
    debug_assert!(buf.win.ends.contains_key(&9u32));
    debug_assert_eq!(buf.win.starts[&2u32], 9u32);
    debug_assert_eq!(buf.win.ends[&9u32], 2u32);
  }

  #[test]
  /// Consumes an entire existing interval, with both intersection
  fn test_subsume_both_intersection() {
    let (mut buf, _rcv) = setup(0);

    buf.handle_seq(1, vec![0u8, 1u8]).unwrap();
    buf.handle_seq(4, vec![3u8, 4u8]).unwrap();
    buf.handle_seq(8, vec![7u8, 8u8]).unwrap();

    assert_eq!(
      buf.buf.clone()[0..9],
      vec![0u8, 1u8, 0u8, 3u8, 4u8, 0u8, 0u8, 7u8, 8u8]
    );

    buf
      .handle_seq(2, vec![1u8, 2u8, 3u8, 4u8, 5u8, 6u8, 7u8])
      .unwrap();

    assert_eq!(
      buf.buf.clone()[0..9],
      vec![0u8, 1u8, 2u8, 3u8, 4u8, 5u8, 6u8, 7u8, 8u8]
    );
    debug_assert_eq!(buf.win.left_index, 9);
  }

  #[test]
  /// Overflow test on writing past the end
  fn test_overflow_interval() {
    let (mut buf, _rcv) = setup(0);
    buf.handle_seq(1, vec![1u8; TCP_BUF_SIZE - 1]).unwrap();
    assert_eq!(
      buf.buf.clone()[0..TCP_BUF_SIZE - 1],
      vec![1u8; TCP_BUF_SIZE - 1]
    );
    buf
      .handle_seq((TCP_BUF_SIZE) as u32, vec![2u8, 2u8])
      .unwrap();
    assert_eq!(buf.buf.clone()[TCP_BUF_SIZE - 1], 2u8);
    assert_eq!(buf.buf.clone()[0], 2u8);
  }

  // #[test]
  /// Overflow test on seq num
  fn test_overflow_seq_num() {
    let (mut buf, _rcv) = setup(u32::max_value() - 1);
    buf.handle_seq(0, vec![2u8, 1u8]).unwrap();
    assert_eq!(buf.buf.clone()[0..2], vec![2u8, 1u8]);
  }

  // not typing out Xu8 for brevity

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
    buf.handle_seq(seq, data).unwrap();
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
