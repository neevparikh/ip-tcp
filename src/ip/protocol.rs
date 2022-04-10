use std::fmt;

use anyhow::{anyhow, Error, Result};

#[derive(Debug, PartialEq, Eq, Hash)]
pub enum Protocol {
  Test,
  Tcp,
  RIP,
}

impl fmt::Display for Protocol {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "{:?}", self)
  }
}

impl TryFrom<u8> for Protocol {
  type Error = Error;
  fn try_from(value: u8) -> Result<Protocol> {
    match value {
      0 => Ok(Protocol::Test),
      6 => Ok(Protocol::Tcp),
      200 => Ok(Protocol::RIP),
      other => Err(anyhow!("Unrecognized protocol number {other}")),
    }
  }
}

impl Into<u8> for Protocol {
  fn into(self) -> u8 {
    match self {
      Protocol::Test => 0,
      Protocol::Tcp => 6,
      Protocol::RIP => 200,
    }
  }
}
