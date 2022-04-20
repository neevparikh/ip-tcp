use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, RwLock};

use anyhow::Result;

use super::tcp_stream::StreamSendThreadMsg;
use super::TCP_BUF_SIZE;
use crate::{debug, edebug};

#[derive(Debug)]
struct RecvWindow {
  /// Refers to the first valid initial_sequence_number
  initial_sequence_number: Option<u32>,

  /// Refers to the index which represents the left window -- not in seq_num land
  left_index:   AtomicU32,
  /// Refers to the index where the reader is
  reader_index: u32,
  data_ready:   Condvar,
  pub starts:   BTreeMap<u32, u32>,
  pub ends:     BTreeMap<u32, u32>,
}

#[derive(Debug)]
pub(super) struct RecvBuffer {
  buf:            Arc<RwLock<Vec<u8>>>,
  window_data:    RecvWindow,
  stream_send_tx: Sender<StreamSendThreadMsg>,
}

impl RecvBuffer {
  pub fn new(stream_send_tx: Sender<StreamSendThreadMsg>) -> RecvBuffer {
    RecvBuffer {
      buf: Arc::new(RwLock::new(vec![0u8; TCP_BUF_SIZE])),
      stream_send_tx,
      window_data: RecvWindow {
        initial_sequence_number: None,
        left_index:              AtomicU32::new(0),
        reader_index:            0,
        data_ready:              Condvar::new(),
        starts:                  BTreeMap::new(),
        ends:                    BTreeMap::new(),
      },
    }
  }

  /// Read data into buffer, until buffer is full. TODO: Block until data is available to read
  pub fn read_data(&mut self, data: &mut [u8]) -> Result<()> {
    if dbg!(self.window_data.left_index.load(Ordering::SeqCst) - self.window_data.reader_index)
      as usize
      >= data.len()
    {
      let mut ready_slice = self.buf.read().unwrap()[self.window_data.reader_index as usize
        ..(self.window_data.reader_index as usize) + data.len()]
        .to_vec();
      data.swap_with_slice(&mut ready_slice);
      self.window_data.reader_index = self.window_data.reader_index.wrapping_add(1);
    } else {
      // Block here
      todo!();
    }
    Ok(())
  }

  pub fn set_initial_seq_num(&mut self, initial_seq: u32) {
    self.window_data.initial_sequence_number = Some(initial_seq);
  }

  /// Handle incoming packet, with a seq_num and data, moving window appropriately.
  pub fn handle_seq(&mut self, seq_num: u32, mut data: Vec<u8>) {
    let size: u32 = data.len().try_into().unwrap();
    let win = &mut self.window_data;
    if seq_num >= win.initial_sequence_number.unwrap() {
      let s = seq_num - win.initial_sequence_number.unwrap(); // TODO make this all wrapping
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
        // disjoint
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
        buf[s as usize..existing_s as usize]
          .swap_with_slice(&mut data[..(existing_s - s) as usize]);

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
        buf[existing_e as usize..e as usize]
          .swap_with_slice(&mut data[(existing_e - s) as usize..]);

        (existing_s, e)
      } else {
        // general case of arbitrary intersections in the middle
        let (_, right_e) = starts.last().unwrap();
        let (_, left_s) = ends.first().unwrap();

        for (s, e) in &starts {
          win.starts.remove(&s);
          win.ends.remove(&e);
        }
        for (e, s) in &ends {
          win.starts.remove(&s);
          win.ends.remove(&e);
        }

        let l = if *left_s < s { *left_s } else { s };
        let r = if *right_e > e { *right_e } else { e };

        win.starts.insert(l, r);
        win.ends.insert(r, l);

        let mut buf = self.buf.write().unwrap();
        buf[s as usize..e as usize].swap_with_slice(&mut data);

        (l, r)
      };

      if let Err(_) = self
        .stream_send_tx
        .send(StreamSendThreadMsg::Ack(seq_num.wrapping_add(1)))
      {
        edebug!("Could not send message to tcp_stream via stream_send_tx...");
        return;
      }

      self
        .window_data
        .left_index
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |left_index| {
          if left_index == l || (l..r).contains(&left_index) {
            Some(r)
          } else {
            None
          }
        })
        .ok();
    } else {
      debug!("Out of order seq_num - TODO: accept wrapping");
    }
  }
}

#[cfg(test)]
mod test {
  use std::sync::mpsc::{channel, Receiver};

  use super::*;

  fn setup(initial_seq: u32) -> (RecvBuffer, Receiver<StreamSendThreadMsg>) {
    let (recv_thread_tx, recv_thread_rx) = channel();
    let mut rb = RecvBuffer::new(recv_thread_tx.clone());
    rb.set_initial_seq_num(initial_seq);
    (rb, recv_thread_rx)
  }

  #[test]
  fn test_adding() {
    let (mut buf, _) = setup(0);
    buf.handle_seq(1, vec![0u8, 1u8]);
    debug_assert_eq!(buf.window_data.left_index.load(Ordering::SeqCst), 2);
    buf.handle_seq(3, vec![2u8, 3u8]);
    debug_assert_eq!(buf.window_data.left_index.load(Ordering::SeqCst), 4);

    assert_eq!(
      buf.buf.read().unwrap().clone()[0..4],
      vec![0u8, 1u8, 2u8, 3u8]
    );
  }

  #[test]
  fn test_disjoint() {
    let (mut buf, _) = setup(0);
    buf.handle_seq(1, vec![0u8, 1u8]);
    debug_assert_eq!(buf.window_data.left_index.load(Ordering::SeqCst), 2);
    buf.handle_seq(8, vec![7u8, 8u8]);
    debug_assert_eq!(buf.window_data.left_index.load(Ordering::SeqCst), 2);

    assert_eq!(
      buf.buf.read().unwrap().clone()[0..9],
      vec![0u8, 1u8, 0u8, 0u8, 0u8, 0u8, 0u8, 7u8, 8u8]
    );

    buf.handle_seq(4, vec![3u8, 4u8]);

    assert_eq!(
      buf.buf.read().unwrap().clone()[0..9],
      vec![0u8, 1u8, 0u8, 3u8, 4u8, 0u8, 0u8, 7u8, 8u8]
    );
    debug_assert_eq!(buf.window_data.left_index.load(Ordering::SeqCst), 2);
  }

  #[test]
  fn test_exact_interval() {
    let (mut buf, _) = setup(0);
    buf.handle_seq(1, vec![0u8, 1u8]);
    debug_assert_eq!(buf.window_data.left_index.load(Ordering::SeqCst), 2);
    buf.handle_seq(5, vec![4u8, 5u8]);
    debug_assert_eq!(buf.window_data.left_index.load(Ordering::SeqCst), 2);

    assert_eq!(
      buf.buf.read().unwrap().clone()[0..6],
      vec![0u8, 1u8, 0u8, 0u8, 4u8, 5u8]
    );

    buf.handle_seq(3, vec![2u8, 3u8]);

    assert_eq!(
      buf.buf.read().unwrap().clone()[0..6],
      vec![0u8, 1u8, 2u8, 3u8, 4u8, 5u8]
    );
    debug_assert_eq!(buf.window_data.left_index.load(Ordering::SeqCst), 6);
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
    debug_assert_eq!(buf.window_data.left_index.load(Ordering::SeqCst), 6);
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
    debug_assert_eq!(buf.window_data.left_index.load(Ordering::SeqCst), 6);
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
    debug_assert_eq!(buf.window_data.left_index.load(Ordering::SeqCst), 0);
  }

  #[test]
  /// Consumes an entire existing interval, with left intersection
  fn test_subsume_left_intersection() {
    let (mut buf, _) = setup(0);

    buf.handle_seq(1, vec![0u8, 1u8]);

    dbg!(&buf.window_data.starts);
    dbg!(&buf.window_data.ends);

    debug_assert!(buf.window_data.starts.contains_key(&0u32));
    debug_assert!(buf.window_data.ends.contains_key(&2u32));
    debug_assert_eq!(buf.window_data.starts[&0u32], 2u32);
    debug_assert_eq!(buf.window_data.ends[&2u32], 0u32);

    buf.handle_seq(4, vec![3u8, 4u8]);

    dbg!(&buf.window_data.starts);
    dbg!(&buf.window_data.ends);

    debug_assert!(buf.window_data.starts.contains_key(&3u32));
    debug_assert!(buf.window_data.ends.contains_key(&5u32));
    debug_assert_eq!(buf.window_data.starts[&3u32], 5u32);
    debug_assert_eq!(buf.window_data.ends[&5u32], 3u32);

    assert_eq!(
      buf.buf.read().unwrap().clone()[0..9],
      vec![0u8, 1u8, 0u8, 3u8, 4u8, 0u8, 0u8, 0u8, 0u8]
    );

    buf.handle_seq(2, vec![1u8, 2u8, 3u8, 4u8, 5u8]);

    assert_eq!(
      buf.buf.read().unwrap().clone()[0..9],
      vec![0u8, 1u8, 2u8, 3u8, 4u8, 5u8, 0u8, 0u8, 0u8]
    );
    debug_assert_eq!(buf.window_data.left_index.load(Ordering::SeqCst), 6);

    dbg!(&buf.window_data.starts);
    dbg!(&buf.window_data.ends);
    debug_assert!(buf.window_data.starts.contains_key(&0u32));
    debug_assert!(buf.window_data.ends.contains_key(&6u32));
    debug_assert_eq!(buf.window_data.starts[&0u32], 6u32);
    debug_assert_eq!(buf.window_data.ends[&6u32], 0u32);
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
    debug_assert_eq!(buf.window_data.left_index.load(Ordering::SeqCst), 0);

    dbg!(&buf.window_data.starts);
    dbg!(&buf.window_data.ends);
    debug_assert!(buf.window_data.starts.contains_key(&2u32));
    debug_assert!(buf.window_data.ends.contains_key(&9u32));
    debug_assert_eq!(buf.window_data.starts[&2u32], 9u32);
    debug_assert_eq!(buf.window_data.ends[&9u32], 2u32);
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
    debug_assert_eq!(buf.window_data.left_index.load(Ordering::SeqCst), 9);

    dbg!(&buf.window_data.starts);
    dbg!(&buf.window_data.ends);
    debug_assert!(buf.window_data.starts.contains_key(&0u32));
    debug_assert!(buf.window_data.ends.contains_key(&9u32));
    debug_assert_eq!(buf.window_data.starts[&0u32], 9u32);
    debug_assert_eq!(buf.window_data.ends[&9u32], 0u32);
  }
}
