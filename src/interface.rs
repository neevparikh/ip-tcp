use anyhow::Result;
use std::net::{Ipv4Addr, UdpSocket};
use std::sync::{Arc, Condvar, Mutex};

#[derive(Debug)]
enum State {
  UP,
  DOWN,
  CLOSED,
  ERRORED,
}

#[derive(Debug)]
pub struct Interface {
  id: usize,
  outgoing_link: UdpSocket,
  our_ip: Ipv4Addr,
  their_ip: Ipv4Addr,
  state: Arc<(Mutex<State>, Condvar)>,
}

impl Interface {
  pub fn new(
    id: usize,
    outgoing_link: UdpSocket,
    our_ip: Ipv4Addr,
    their_ip: Ipv4Addr,
  ) -> Result<Interface> {
    Ok(Interface {
      id,
      outgoing_link,
      our_ip,
      their_ip,
      state: Arc::new((Mutex::new(State::DOWN), Condvar::new())),
    })
  }

  pub fn up(&self) -> Result<()> {
    todo!()
  }

  pub fn down(&self) -> Result<()> {
    todo!()
  }

  pub fn send(&self) -> Result<()> {
    todo!()
  }
}
