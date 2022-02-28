use anyhow::Result;
use std::net::{Ipv4Addr, SocketAddr};

#[derive(Debug, PartialEq, Eq)]
pub enum State {
  UP,
  DOWN,
  CLOSED,
  ERRORED,
}

#[derive(Debug)]
pub struct Interface {
  pub id: usize,
  pub outgoing_link: SocketAddr,
  pub our_ip: Ipv4Addr,
  pub their_ip: Ipv4Addr,
  state: State,
}

impl Interface {
  pub fn new(
    id: usize,
    outgoing_link: SocketAddr,
    our_ip: Ipv4Addr,
    their_ip: Ipv4Addr,
  ) -> Result<Interface> {
    Ok(Interface {
      id,
      outgoing_link,
      our_ip,
      their_ip,
      state: State::DOWN,
    })
  }

  /// Sets interface to UP state
  pub fn up(&self) {
    let mut state = self.state;
    state = State::UP;
  }

  /// Sets interface to DOWN state
  pub fn down(&self) {
    let mut state = self.state;
    state = State::DOWN;
  }

  pub fn send(&self) -> Result<()> {
    todo!()
  }

  pub fn state(&self) -> &State {
    return &self.state;
  }
}
