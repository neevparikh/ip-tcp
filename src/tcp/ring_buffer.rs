#[derive(Debug)]
pub(super) struct RingBuffer {
  /// raw storage
  pub(super) buf:       Vec<u8>,
  /// indexes into first ready to read slot
  pub(super) read_idx:  usize,
  /// indexes into first free slot
  pub(super) write_idx: usize,
  /// bool to distinguish full vs empty
  pub(super) empty:     bool,
}

impl RingBuffer {
  pub fn new(size: usize) -> RingBuffer {
    RingBuffer {
      buf:       vec![0u8; size],
      read_idx:  0,
      write_idx: 0,
      empty:     true,
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

  fn _wrapping_sub(&self, a: usize, b: usize) -> usize {
    let l = self.len();
    if a < b {
      (b - a) % l
    } else {
      a - b
    }
  }

  pub fn push_with_offset(&mut self, data: &[u8], offset: usize) {
    let l = self.len();
    let size = data.len();
    let s = self.wrapping_add(self.write_idx, offset);
    let e = s + size;
    let e = if e == l { l } else { e % l };
    let r = self.read_idx;

    if !self.empty {
      debug_assert!(!(s <= r && e > r) && !(s >= e && e > r));
    } else {
      debug_assert!(!(s < r && e > r) && !(s >= e && e > r));
    }

    if s <= e {
      self.buf[s..e].copy_from_slice(&data);
    } else {
      self.buf[s..l].copy_from_slice(&data[..(l - s)]);
      self.buf[..e].copy_from_slice(&data[(l - s)..]);
    }
  }

  /// move write head
  pub fn move_write_idx(&mut self, num_bytes: usize) {
    let w = self.write_idx;
    let r = self.read_idx;
    let nw = self.wrapping_add(w, num_bytes);
    if !self.empty {
      debug_assert!(!(w <= r && nw > r) && !(w >= nw && nw > r));
    } else {
      debug_assert!(!(w < r && nw > r) && !(w >= nw && nw > r));
    }
    if nw == r {
      self.empty = false;
    }
    self.write_idx = nw;
  }

  pub fn _push(&mut self, data: &[u8]) {
    self.push_with_offset(data, 0);
  }

  /// Returns as much data as possible, up until reaching write_idx.
  pub fn pop(&mut self, size: usize) -> Vec<u8> {
    let l = self.len();
    let w = self.write_idx;
    let r = self.read_idx;
    let data = if self.empty && r == w {
      Vec::new()
    } else if r < w {
      let available = w - r;
      let s = size.min(available);
      self.read_idx = self.wrapping_add(self.read_idx, s);
      self.buf[r..r + s].to_vec() // can't overflow bc s <= w - r => r + s <= w <= l
    } else {
      let first = l - r;
      let second = w;
      let available = first + second;
      if size <= first {
        self.read_idx = self.wrapping_add(self.read_idx, size);
        self.buf[r..r + size].to_vec() // can't overflow bc size <= l - r => r + size <= l
      } else {
        let s = size.min(available);
        let rem = s - first; // can't overflow, bc s > first
        let mut data = self.buf[r..l].to_vec();
        let mut rest = self.buf[..rem].to_vec();
        self.read_idx = rem;
        data.append(&mut rest);
        data
      }
    };
    if self.read_idx == self.write_idx {
      self.empty = true;
    }
    data
  }

  pub fn len(&self) -> usize {
    self.buf.len()
  }

  pub(super) fn _get_raw_buf(&self) -> &[u8] {
    &self.buf[..self.len()]
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_push() {
    let mut b = RingBuffer::new(5);
    b._push(&[0, 1, 2, 3]);
    b.move_write_idx(4);
    assert_eq!(b._get_raw_buf().clone(), vec![0, 1, 2, 3, 0]);
    assert_eq!(b.write_idx, 4);
  }

  #[test]
  fn test_push_offset() {
    let mut b = RingBuffer::new(5);
    b.push_with_offset(&[1, 1, 1], 2);
    assert_eq!(b.write_idx, 0);
    assert_eq!(b._get_raw_buf().clone(), vec![0, 0, 1, 1, 1]);
  }

  #[test]
  fn test_pop() {
    let mut b = RingBuffer::new(5);
    b._push(&[0, 1, 2, 3]);
    b.move_write_idx(4);
    assert_eq!(b._get_raw_buf().clone(), vec![0, 1, 2, 3, 0]);

    let d = b.pop(2);
    assert_eq!(d, vec![0, 1]);
    assert_eq!(b.read_idx, 2);
  }

  #[test]
  fn test_push_full() {
    let mut b = RingBuffer::new(5);
    b._push(&[1, 1, 1, 1, 1]);
    b.move_write_idx(5);
    assert_eq!(b.write_idx, 0);
  }

  #[test]
  fn test_push_overwrite() {
    let mut b = RingBuffer::new(5);
    b._push(&[1, 1, 1]);
    b.move_write_idx(3);
    assert_eq!(b.write_idx, 3);
    assert_eq!(b._get_raw_buf().clone(), vec![1, 1, 1, 0, 0]);
    b.pop(2);
    assert_eq!(b.read_idx, 2);
    assert_eq!(b.write_idx, 3);
    b._push(&[2, 2, 2, 2]);
    b.move_write_idx(4);
    assert_eq!(b.write_idx, 2);
    assert_eq!(b._get_raw_buf().clone(), vec![2, 2, 1, 2, 2]);
  }

  #[test]
  fn test_pop_beyond_write_idx() {
    let mut b = RingBuffer::new(5);
    b._push(&[0, 1, 2, 3]);
    b.move_write_idx(4);
    assert_eq!(b._get_raw_buf().clone(), vec![0, 1, 2, 3, 0]);

    let d = b.pop(6);
    assert_eq!(d, vec![0, 1, 2, 3]);
    assert_eq!(b.read_idx, 4);
  }

  #[test]
  fn test_pop_when_full() {
    let mut b = RingBuffer::new(5);
    b._push(&[1, 1, 1, 1, 1]);
    b.move_write_idx(5);
    assert_eq!(b.write_idx, 0);
    let d = b.pop(1);
    assert_eq!(d, vec![1]);
    assert_eq!(b.read_idx, 1);
    let d = b.pop(1);
    assert_eq!(d, vec![1]);
    assert_eq!(b.read_idx, 2);
  }

  #[test]
  fn test_pop_when_full_two_sends() {
    let mut b = RingBuffer::new(5);
    assert!(b.empty);

    b._push(&[1, 1, 1, 1, 1]);
    b.move_write_idx(5);
    assert_eq!(b.write_idx, 0);
    assert!(!b.empty);

    let d = b.pop(1);
    assert_eq!(d, vec![1]);
    assert_eq!(b.read_idx, 1);

    b._push(&[2]);
    b.move_write_idx(1);
    assert_eq!(b.write_idx, 1);
    assert!(!b.empty);

    let d = b.pop(5);
    assert_eq!(d, vec![1, 1, 1, 1, 2]);
    assert_eq!(b.read_idx, 1);
    assert!(b.empty);
  }
}
