use anyhow::{anyhow, Error, Result};

#[derive(Debug)]
pub enum Protocol {
  Test,
  RIP,
}

impl TryFrom<u8> for Protocol {
  type Error = Error;
  fn try_from(value: u8) -> Result<Protocol> {
    match value {
      0 => Ok(Protocol::Test),
      200 => Ok(Protocol::RIP),
      other => Err(anyhow!("Unrecognized protocol number {other}")),
    }
  }
}

impl Into<u8> for Protocol {
  fn into(self) -> u8 {
    match self {
      Protocol::Test => 0,
      Protocol::RIP => 200,
    }
  }
}
