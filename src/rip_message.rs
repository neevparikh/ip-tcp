use anyhow::{anyhow, Result};

use crate::edebug;

#[derive(Debug)]
pub enum RipCommand {
  Request,
  Response,
}

#[derive(Debug)]
pub struct RipMsg {
  /// Entries for the packet; triple of (cost, address, mask)
  pub command: RipCommand,
  pub entries: Vec<RipEntry>,
}

#[derive(Debug)]
pub struct RipEntry {
  pub cost: u8,
  pub address: u32,
  pub mask: u32,
}

pub const INFINITY_COST: u8 = 16;

impl RipMsg {
  pub fn unpack(bytes: &[u8]) -> Result<RipMsg> {
    if bytes.len() < 4 {
      return Err(anyhow!("Error: RipMsg was less than 4 bytes"));
    }

    if (bytes.len() - 4) % 12 != 0 {
      return Err(anyhow!(
        concat!(
          "Error: RipMsg does not have enough bytes for complete entry structs, ",
          "only has {}"
        ),
        bytes.len()
      ));
    }

    let command = u16::from_be_bytes(bytes[0..2].try_into().unwrap());
    let command = match command {
      1 => RipCommand::Request,
      2 => RipCommand::Response,
      other => return Err(anyhow!("Invalid cmd type {:?}", other)),
    };
    let num_entries = u16::from_be_bytes(bytes[2..4].try_into().unwrap());

    if (bytes.len() - 4) / 12 != num_entries as usize {
      edebug!("RipMsg num_entries doesn't match with actual entries");
      edebug!(
        "num_entries: {num_entries}, actual: {}",
        (bytes.len() - 4) / 12
      );
    }

    let mut entries = Vec::new();

    for i in (4..bytes.len()).step_by(12) {
      entries.push(RipEntry::unpack(&bytes[i..i + 12])?);
    }

    debug_assert!(entries.len() == num_entries as usize);

    Ok(RipMsg { command, entries })
  }

  pub fn pack(&self) -> Vec<u8> {
    let mut buffer = Vec::new();
    let cmd = match self.command {
      RipCommand::Request => 1u16,
      RipCommand::Response => 2u16,
    };
    buffer.extend_from_slice(&u16::to_be_bytes(cmd));
    buffer.extend_from_slice(&u16::to_be_bytes(
      self.entries.len().try_into().unwrap_or_else(|_| {
        let max = u16::max_value();
        edebug!(
          "warning: {:#?} has more than {:?} entries, keeping first {:?} entries...",
          self,
          max,
          max
        );
        max
      }),
    ));

    for entry in self.entries.iter().take(u16::max_value() as usize) {
      buffer.extend_from_slice(&entry.pack());
    }

    buffer
  }
}

impl RipEntry {
  pub fn unpack(bytes: &[u8]) -> Result<RipEntry> {
    if bytes.len() != 12 {
      return Err(anyhow!("Error: RipEntry was incorrect size"));
    }
    let cost = u32::from_be_bytes(bytes[0..4].try_into().unwrap());
    if cost > 16 {
      return Err(anyhow!("Cost: {cost} exceeded 16"));
    }
    let cost = cost as u8;
    Ok(RipEntry {
      cost,
      address: u32::from_be_bytes(bytes[4..8].try_into().unwrap()),
      mask: u32::from_be_bytes(bytes[8..12].try_into().unwrap()),
    })
  }
  pub fn pack(&self) -> Vec<u8> {
    let mut buffer = Vec::new();
    buffer.extend_from_slice(&u32::to_be_bytes(self.cost as u32));
    buffer.extend_from_slice(&u32::to_be_bytes(self.address));
    buffer.extend_from_slice(&u32::to_be_bytes(self.mask));
    buffer
  }
}
