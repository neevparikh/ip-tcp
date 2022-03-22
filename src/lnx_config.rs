use std::fs::File;
use std::io::{BufRead, BufReader};
use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};

use anyhow::{anyhow, Result};

use crate::interface::Interface;

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

    // this is required because UdpSocket::bind by default converts "localhost:PORT" to
    // ipv6 - [::1]:PORT
    // ipv4 - [127.0.0.1]:PORT
    // We only want ipv4, so we filter and pass in the rest into UdpSocket
    let addr = (format!("{}:{}", tokens[0], tokens[1]).to_socket_addrs()?).filter(|a| a.is_ipv4());
    let local_link = UdpSocket::bind(addr.collect::<Vec<SocketAddr>>().as_slice())?;

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
          format!("{}:{}", tokens[0], tokens[1]),
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
