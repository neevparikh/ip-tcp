use anyhow::Result;

use super::protocol::Protocol;

/// TODO
pub struct IpPacket {
  header: Vec<u8>,
  data: Vec<u8>,
}

impl IpPacket {
  pub fn new() -> IpPacket {
    todo!();
  }

  pub fn version() -> &u8 {}
}
