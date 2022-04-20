use std::collections::{BTreeMap, VecDeque};
use std::sync::mpsc::Sender;
use std::sync::{Arc, RwLock};

use anyhow::Result;

use super::tcp_stream::StreamSendThreadMsg;
use super::TCP_BUF_SIZE;

#[derive(Debug)]
struct RecvWindow {
  /// Refers to the first valid initial_sequence_number
  initial_sequence_number: u32,
  /// Refers to the index which represents the left window -- not in seq_num land
  left_index:              u32,
  /// Number of bytes currently referenced by elements in the window
  pub bytes_in_window:     usize,
  pub starts:              BTreeMap<u32, u32>,
  pub ends:                BTreeMap<u32, u32>,
}

#[derive(Debug)]
pub(super) struct RecvBuffer {
  buf:            Arc<RwLock<Vec<u8>>>,
  window_data:    RecvWindow,
  stream_send_tx: Sender<StreamSendThreadMsg>,
}

impl RecvBuffer {
  pub fn new(
    stream_send_tx: Sender<StreamSendThreadMsg>,
    initial_sequence_number: u32,
  ) -> RecvBuffer {
    RecvBuffer {
      buf: Arc::new(RwLock::new(vec![0u8; TCP_BUF_SIZE])),
      stream_send_tx,
      window_data: RecvWindow {
        initial_sequence_number: initial_sequence_number + 1,
        left_index:              0,
        bytes_in_window:         TCP_BUF_SIZE,
        starts:                  BTreeMap::new(),
        ends:                    BTreeMap::new(),
      },
    }
  }

  /// Read data into buffer, until buffer is full. Block until data is available to read
  pub fn recv_data(&self, data: &mut [u8]) -> Result<()> {
    todo!()
  }

  /// Launch recv thread
  pub fn start_recv_thread(&self) {
    todo!()
  }

  /// Handle incoming packet, with a seq_num and data, moving window appropriately.
  pub fn handle_seq(&mut self, seq_num: u32, mut data: Vec<u8>) {
    let size: u32 = data.len().try_into().unwrap();
    let win = &mut self.window_data;
    debug_assert!(seq_num >= win.initial_sequence_number);
    let s = seq_num - win.initial_sequence_number;
    let e = s + size;

    let starts: Vec<_> = win
      .starts
      .range(s..=e)
      .into_iter()
      .map(|(&k, &v)| (k, v))
      .collect();
    let ends: Vec<_> = win
      .ends
      .range(s..e)
      .into_iter()
      .map(|(&k, &v)| (k, v))
      .collect();

    let (l, r) = if starts.len() == 0 && ends.len() == 0 {
      win.starts.insert(s, e);
      win.ends.insert(e, s);

      let mut buf = self.buf.write().unwrap();
      debug_assert_eq!((e - s) as usize, data.len());
      buf[s as usize..e as usize].swap_with_slice(&mut data);

      (s, e)
    } else if starts.len() > 0 && ends.len() == 0 {
      // overlap left
      debug_assert!(starts.len() == 1);
      let (existing_s, existing_e) = starts[0];

      win.starts.remove(&existing_s);
      win.ends.remove(&existing_e);

      win.starts.insert(s, existing_e);
      win.ends.insert(existing_e, s);

      let mut buf = self.buf.write().unwrap();
      debug_assert!(((existing_s - s) as usize) <= data.len());
      buf[s as usize..existing_s as usize].swap_with_slice(&mut data[..(existing_s - s) as usize]);

      (s, existing_e)
    } else if starts.len() == 0 && ends.len() > 0 {
      // overlap right
      debug_assert!(ends.len() == 1);
      let (existing_e, existing_s) = ends[0];

      win.starts.remove(&existing_s);
      win.ends.remove(&existing_e);

      win.starts.insert(existing_s, e);
      win.ends.insert(e, existing_s);

      let mut buf = self.buf.write().unwrap();
      debug_assert!(((e - existing_e) as usize) <= data.len());
      buf[existing_e as usize..e as usize].swap_with_slice(&mut data[(existing_e - s) as usize..]);

      (existing_s, e)
    } else {
      let (right_s, right_e) = starts.last().unwrap();
      let (left_s, left_e) = ends.first().unwrap();
      win.starts.remove(&right_s);
      win.ends.remove(&right_e);
      win.starts.remove(&left_s);
      win.ends.remove(&left_e);

      let l = if *left_s < s { *left_s } else { s };
      let r = if *right_e > e { *right_e } else { e };
      win.starts.insert(l, r);
      win.ends.insert(r, l);

      let mut buf = self.buf.write().unwrap();
      buf[s as usize..e as usize].swap_with_slice(&mut data);

      (l, r)
    };

    if self.window_data.left_index == l || (l..r).contains(&self.window_data.left_index) {
      // TODO: tell reader to wake up
      self.window_data.left_index = r;
    }
  }
}

#[cfg(test)]
mod test {
  use std::sync::mpsc::{channel, Receiver};

  use super::*;

  fn setup(initial_seq: u32) -> (RecvBuffer, Receiver<StreamSendThreadMsg>) {
    let (recv_thread_tx, recv_thread_rx) = channel();
    (
      RecvBuffer::new(recv_thread_tx.clone(), initial_seq),
      recv_thread_rx,
    )
  }

  #[test]
  fn test_adding() {
    let (mut buf, _) = setup(0);
    buf.handle_seq(1, vec![0u8, 1u8]);
    debug_assert_eq!(buf.window_data.left_index, 2);
    buf.handle_seq(3, vec![2u8, 3u8]);
    debug_assert_eq!(buf.window_data.left_index, 4);

    assert_eq!(
      buf.buf.read().unwrap().clone()[0..4],
      vec![0u8, 1u8, 2u8, 3u8]
    );
  }

  #[test]
  fn test_disjoint() {
    let (mut buf, _) = setup(0);
    buf.handle_seq(1, vec![0u8, 1u8]);
    debug_assert_eq!(buf.window_data.left_index, 2);
    buf.handle_seq(8, vec![7u8, 8u8]);
    debug_assert_eq!(buf.window_data.left_index, 2);

    assert_eq!(
      buf.buf.read().unwrap().clone()[0..9],
      vec![0u8, 1u8, 0u8, 0u8, 0u8, 0u8, 0u8, 7u8, 8u8]
    );

    buf.handle_seq(4, vec![3u8, 4u8]);

    assert_eq!(
      buf.buf.read().unwrap().clone()[0..9],
      vec![0u8, 1u8, 0u8, 3u8, 4u8, 0u8, 0u8, 7u8, 8u8]
    );
    debug_assert_eq!(buf.window_data.left_index, 2);
  }

  #[test]
  fn test_exact_interval() {
    let (mut buf, _) = setup(0);
    buf.handle_seq(1, vec![0u8, 1u8]);
    debug_assert_eq!(buf.window_data.left_index, 2);
    buf.handle_seq(5, vec![4u8, 5u8]);
    debug_assert_eq!(buf.window_data.left_index, 2);

    assert_eq!(
      buf.buf.read().unwrap().clone()[0..6],
      vec![0u8, 1u8, 0u8, 0u8, 4u8, 5u8]
    );

    buf.handle_seq(3, vec![2u8, 3u8]);

    assert_eq!(
      buf.buf.read().unwrap().clone()[0..6],
      vec![0u8, 1u8, 2u8, 3u8, 4u8, 5u8]
    );
    debug_assert_eq!(buf.window_data.left_index, 6);
  }

  #[test]
  fn test_overlap_left() {
    let (mut buf, _) = setup(0);
    buf.handle_seq(1, vec![0u8, 1u8, 2u8]);
    buf.handle_seq(5, vec![4u8, 5u8]);

    assert_eq!(
      buf.buf.read().unwrap().clone()[0..6],
      vec![0u8, 1u8, 2u8, 0u8, 4u8, 5u8]
    );

    buf.handle_seq(3, vec![2u8, 3u8]);

    assert_eq!(
      buf.buf.read().unwrap().clone()[0..6],
      vec![0u8, 1u8, 2u8, 3u8, 4u8, 5u8]
    );
    debug_assert_eq!(buf.window_data.left_index, 6);
  }

  #[test]
  fn test_overlap_right() {
    let (mut buf, _) = setup(0);
    buf.handle_seq(1, vec![0u8, 1u8]);
    buf.handle_seq(4, vec![3u8, 4u8, 5u8]);

    assert_eq!(
      buf.buf.read().unwrap().clone()[0..6],
      vec![0u8, 1u8, 0u8, 3u8, 4u8, 5u8]
    );

    buf.handle_seq(3, vec![2u8, 3u8]);

    assert_eq!(
      buf.buf.read().unwrap().clone()[0..6],
      vec![0u8, 1u8, 2u8, 3u8, 4u8, 5u8]
    );
    debug_assert_eq!(buf.window_data.left_index, 6);
  }

  #[test]
  /// Consumes an entire existing interval, without intersecting others
  fn test_subsume_no_intersection() {
    let (mut buf, _) = setup(0);
    buf.handle_seq(3, vec![2u8, 3u8]);

    assert_eq!(
      buf.buf.read().unwrap().clone()[0..6],
      vec![0u8, 0u8, 2u8, 3u8, 0u8, 0u8]
    );

    buf.handle_seq(2, vec![1u8, 2u8, 3u8, 4u8]);

    assert_eq!(
      buf.buf.read().unwrap().clone()[0..6],
      vec![0u8, 1u8, 2u8, 3u8, 4u8, 0u8]
    );
    debug_assert_eq!(buf.window_data.left_index, 0);
  }

  #[test]
  /// Consumes an entire existing interval, with left intersection
  fn test_subsume_left_intersection() {
    let (mut buf, _) = setup(0);

    buf.handle_seq(1, vec![0u8, 1u8]);
    buf.handle_seq(4, vec![3u8, 4u8]);

    assert_eq!(
      buf.buf.read().unwrap().clone()[0..9],
      vec![0u8, 1u8, 0u8, 3u8, 4u8, 0u8, 0u8, 0u8, 0u8]
    );

    buf.handle_seq(2, vec![1u8, 2u8, 3u8, 4u8, 5u8]);

    assert_eq!(
      buf.buf.read().unwrap().clone()[0..9],
      vec![0u8, 1u8, 2u8, 3u8, 4u8, 5u8, 0u8, 0u8, 0u8]
    );
    debug_assert_eq!(buf.window_data.left_index, 6);
  }

  #[test]
  /// Consumes an entire existing interval, with right intersection
  fn test_subsume_right_intersection() {
    let (mut buf, _) = setup(0);

    buf.handle_seq(8, vec![7u8, 8u8]);
    buf.handle_seq(4, vec![3u8, 4u8]);

    assert_eq!(
      buf.buf.read().unwrap().clone()[0..9],
      vec![0u8, 0u8, 0u8, 3u8, 4u8, 0u8, 0u8, 7u8, 8u8]
    );

    buf.handle_seq(3, vec![2u8, 3u8, 4u8, 5u8, 6u8, 7u8]);

    assert_eq!(
      buf.buf.read().unwrap().clone()[0..9],
      vec![0u8, 0u8, 2u8, 3u8, 4u8, 5u8, 6u8, 7u8, 8u8]
    );
    debug_assert_eq!(buf.window_data.left_index, 0);
  }

  #[test]
  /// Consumes an entire existing interval, with both intersection
  fn test_subsume_both_intersection() {
    let (mut buf, _) = setup(0);

    buf.handle_seq(1, vec![0u8, 1u8]);
    buf.handle_seq(4, vec![3u8, 4u8]);
    buf.handle_seq(8, vec![7u8, 8u8]);

    assert_eq!(
      buf.buf.read().unwrap().clone()[0..9],
      vec![0u8, 1u8, 0u8, 3u8, 4u8, 0u8, 0u8, 7u8, 8u8]
    );

    buf.handle_seq(2, vec![1u8, 2u8, 3u8, 4u8, 5u8, 6u8, 7u8]);

    assert_eq!(
      buf.buf.read().unwrap().clone()[0..9],
      vec![0u8, 1u8, 2u8, 3u8, 4u8, 5u8, 6u8, 7u8, 8u8]
    );
    debug_assert_eq!(buf.window_data.left_index, 9);
  }
}
