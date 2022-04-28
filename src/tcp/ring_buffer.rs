use crate::edebug;

#[derive(Debug)]
pub(super) struct RingBuffer {
  /// raw storage
  buf:              Vec<u8>,
  /// indexes into last read element
  read_idx:         usize,
  /// indexes into first free slot
  write_idx:        usize,
  /// place to move write idx when writing with offsets
  future_write_idx: usize,
}

impl RingBuffer {
  pub fn new(size: usize) -> RingBuffer {
    RingBuffer {
      buf:              vec![0u8; size],
      read_idx:         size - 1,
      write_idx:        0,
      future_write_idx: 0,
    }
  }

  fn wrapping_add(&self, a: usize, b: usize) -> usize {
    let l = self.len();
    let c = a + b;
    if c >= l {
      c % l
    } else {
      c
    }
  }

  pub fn push_with_offset(&mut self, data: &[u8], offset: usize) {
    let l = self.len();
    let size = data.len();
    let s = self.wrapping_add(self.write_idx, offset);
    let e = s + size;
    let e = if e == l { l } else { e % l };

    debug_assert!(!(s < self.read_idx && e > self.read_idx + 1));

    if s <= e {
      self.buf[s..e].copy_from_slice(&data);
    } else {
      self.buf[s..].copy_from_slice(&data[..(l - s)]);
      self.buf[..e].copy_from_slice(&data[(l - s)..]);
    }
  }

  /// move write head
  pub fn move_write_idx(&mut self, num_bytes: usize) {
    let w = self.wrapping_add(self.write_idx, num_bytes);
    debug_assert!(!(self.write_idx < self.read_idx && self.read_idx < w));
    self.write_idx = w;
  }

  pub fn push(&mut self, data: &[u8]) {
    self.push_with_offset(data, 0);
  }

  /// Returns as much data as possible, up until reaching write_idx.
  pub fn pop(&mut self, size: usize) -> Vec<u8> {
    let l = self.len();
    let w = self.write_idx;
    let r = self.wrapping_add(self.read_idx, 1);
    if r <= w {
      let available = w - r;
      let s = size.min(available);
      let data = self.buf[r..r + s].to_vec();
      self.read_idx = r + s - 1;
      data
    } else {
      let first = l - r;
      let second = w;
      let available = first + second;
      if size <= first {
        let data = self.buf[r..r + size].to_vec();
        self.read_idx = r + size - 1;
        data
      } else {
        let s = size.min(available);
        let rem = s - first;
        let mut data = self.buf[r..].to_vec();
        let mut rest = self.buf[..rem].to_vec();
        self.read_idx = rem.saturating_sub(1);
        data.append(&mut rest);
        data
      }
    }
  }

  pub fn len(&self) -> usize {
    self.buf.len()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_push() {
    let mut b = RingBuffer::new(5);
    b.push(&[0, 1, 2, 3]);
    b.move_write_idx(4);
    assert_eq!(b.buf.clone(), vec![0, 1, 2, 3, 0]);
    assert_eq!(b.write_idx, 4);
  }

  #[test]
  fn test_push_offset() {
    let mut b = RingBuffer::new(5);
    b.push_with_offset(&[1, 1, 1], 2);
    assert_eq!(b.write_idx, 0);
    assert_eq!(b.buf.clone(), vec![0, 0, 1, 1, 1]);
  }

  #[test]
  fn test_pop() {
    let mut b = RingBuffer::new(5);
    b.push(&[0, 1, 2, 3]);
    b.move_write_idx(4);
    assert_eq!(b.buf.clone(), vec![0, 1, 2, 3, 0]);

    let d = b.pop(2);
    assert_eq!(d, vec![0, 1]);
    assert_eq!(b.read_idx, 1);
  }

  #[test]
  fn test_push_full() {
    let mut b = RingBuffer::new(5);
    b.push(&[1, 1, 1, 1, 1]);
    b.move_write_idx(5);
    assert_eq!(b.write_idx, 0);
  }

  #[test]
  fn test_push_overwrite() {
    let mut b = RingBuffer::new(5);
    b.push(&[1, 1, 1]);
    b.move_write_idx(3);
    assert_eq!(b.write_idx, 3);
    assert_eq!(b.buf.clone(), vec![1, 1, 1, 0, 0]);
    b.pop(2);
    assert_eq!(b.read_idx, 1);
    assert_eq!(b.write_idx, 3);
    b.push(&[2, 2, 2, 2]);
    assert_eq!(b.buf.clone(), vec![2, 2, 1, 2, 2]);
  }

  #[test]
  fn test_pop_beyond_write_idx() {
    let mut b = RingBuffer::new(5);
    b.push(&[0, 1, 2, 3]);
    b.move_write_idx(4);
    assert_eq!(b.buf.clone(), vec![0, 1, 2, 3, 0]);

    let d = b.pop(6);
    assert_eq!(d, vec![0, 1, 2, 3]);
    assert_eq!(b.read_idx, 3);
  }
}
