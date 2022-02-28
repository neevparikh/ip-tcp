use crate::interface::Interface;

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::net::{SocketAddr, UdpSocket};
use std::str::FromStr;

use anyhow::{anyhow, Result};

#[derive(Debug)]
pub struct LnxConfig {
  /// UdpSocket to recv incoming messages on
  pub local_link: UdpSocket,

  pub interfaces: Vec<Interface>,
}

impl LnxConfig {
  pub fn new(lnx_filename: &str) -> Result<LnxConfig> {
    let f = File::open(lnx_filename)?;
    let mut reader = BufReader::new(f);

    let mut local_addr = String::new();
    reader.read_line(&mut local_addr)?;
    // remove newline
    local_addr.pop();
    let tokens: Vec<&str> = local_addr.split(" ").collect();

    if tokens.len() != 2 {
      return Err(anyhow!(
        "File {lnx_filename} improperly formatted at line 1"
      ));
    }

    let local_link = UdpSocket::bind(format!("{}:{}", tokens[0], tokens[1]))?;

    let mut interfaces = Vec::new();
    for (i, line) in reader.lines().enumerate() {
      let line = line?;

      let tokens: Vec<&str> = line.split(" ").collect();
      if tokens.len() != 4 {
        return Err(anyhow!(
          "File {lnx_filename} improperly formatted at line {i}"
        ));
      } else {
        interfaces.push(Interface::new(
          i,
          SocketAddr::from_str(&format!("{}:{}", tokens[0], tokens[1]))?,
          tokens[2].parse()?,
          tokens[3].parse()?,
        )?);
      }
    }

    Ok(LnxConfig {
      local_link,
      interfaces,
    })
  }
}
