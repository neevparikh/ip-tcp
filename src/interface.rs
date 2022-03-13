use anyhow::{anyhow, Result};
use std::fmt;
use std::net::{Ipv4Addr, SocketAddr, ToSocketAddrs};

use crate::InterfaceId;

#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub enum State {
  UP,
  DOWN,
  CLOSED,
  ERRORED,
}

#[derive(Debug)]
pub struct Interface {
  pub id: InterfaceId,
  pub outgoing_link: String,
  pub outgoing_link_addr: SocketAddr,
  pub our_ip: Ipv4Addr,
  pub their_ip: Ipv4Addr,
  state: State,
}

impl Interface {
  pub fn new(
    id: InterfaceId,
    outgoing_link: String,
    our_ip: Ipv4Addr,
    their_ip: Ipv4Addr,
  ) -> Result<Interface> {
    let outgoing_link_addr = match outgoing_link.to_socket_addrs()?.next() {
      Some(addr) => addr,
      None => return Err(anyhow!("Invalid socket_addr: {outgoing_link}")),
    };
    Ok(Interface {
      id,
      outgoing_link,
      outgoing_link_addr,
      our_ip,
      their_ip,
      state: State::UP,
    })
  }

  /// Sets interface to UP state
  pub fn up(&mut self) {
    self.state = State::UP;
  }

  /// Sets interface to DOWN state
  pub fn down(&mut self) {
    self.state = State::DOWN;
  }

  pub fn send(&self) -> Result<()> {
    todo!()
  }

  pub fn state(&self) -> State {
    return self.state;
  }
}

impl fmt::Display for Interface {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(
      f,
      "{}: status {:?}, outgoing_link {}, their_ip {}, our_ip {}",
      self.id,
      self.state(),
      self.outgoing_link,
      self.their_ip,
      self.our_ip
    )
  }
}
