use std::net::SocketAddr;
use std::sync::mpsc::Sender;
use std::sync::Arc;

use anyhow::Result;

use super::tcp_stream::TcpStream;
use super::Port;
use crate::IpPacket;

pub struct TcpListener {
  stream:     Arc<TcpStream>,
  port:       Port,
  ip_send_tx: Sender<IpPacket>,
}
