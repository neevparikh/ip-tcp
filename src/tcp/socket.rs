use std::str::FromStr;

use anyhow::{anyhow, Error, Result};

pub type SocketId = usize;

pub enum SocketSide {
  Read,
  Write,
  Both,
}

impl FromStr for SocketSide {
  type Err = Error;
  fn from_str(input: &str) -> Result<Self> {
    match input {
      "r" | "read" => Ok(SocketSide::Read),
      "w" | "write" => Ok(SocketSide::Write),
      "both" => Ok(SocketSide::Both),
      other => Err(anyhow!(
        "Unknown type {other}, valid: read (r), write (w), both"
      )),
    }
  }
}
