use anyhow::Result;
use std::net::{Ipv4Addr, UdpSocket};
use std::sync::{Arc, Condvar, RwLock, RwLockReadGuard};

#[derive(Debug)]
pub enum State {
  UP,
  DOWN,
  CLOSED,
  ERRORED,
}

#[derive(Debug)]
pub struct Interface {
  pub id: usize,
  pub outgoing_link: UdpSocket,
  pub our_ip: Ipv4Addr,
  pub their_ip: Ipv4Addr,
  state: Arc<RwLock<State>>,
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
      state: Arc::new(RwLock::new(State::DOWN)),
    })
  }

  /// Sets interface to UP state
  /// TODO: should these getters and setter panic on a poisoned mutex of return err
  pub fn up(&self) {
    // Only panics if rwlock is poisoned
    let mut state = self.state.write().unwrap();
    *state = State::UP;
  }

  /// Sets interface to DOWN state
  pub fn down(&self) {
    // Only panics if rwlock is poisoned
    let mut state = self.state.write().unwrap();
    *state = State::DOWN;
  }

  pub fn send(&self) -> Result<()> {
    todo!()
  }

  /// Returns a ReadGuard of the state
  pub fn state(&self) -> RwLockReadGuard<State> {
    // Only panics if rwlock is poisoned
    return self.state.read().unwrap();
  }
}
