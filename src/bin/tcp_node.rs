use std::io::{stdin, stdout, Write};
use std::net::{Ipv4Addr, SocketAddr};
use std::str::FromStr;

use anyhow::{anyhow, Result};
use clap::Parser;
use ip_tcp::ip::ip_layer::IpLayer;
use ip_tcp::ip::protocol::Protocol;
use ip_tcp::misc::lnx_config::LnxConfig;
use ip_tcp::tcp::socket::{SocketId, SocketSide};
use ip_tcp::tcp::tcp_layer::TcpLayer;
use ip_tcp::tcp::Port;
use ip_tcp::InterfaceId;
use shellwords;

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
pub struct Args {
  /// Filename of the lnx file
  #[clap(required = true)]
  lnx_filename: String,
}

fn check_len(tokens: &[String], expected: usize) -> Result<()> {
  if tokens.len() != expected + 1 {
    Err(anyhow!(
      "'{}' expected {expected} argument received {}",
      tokens[0],
      tokens.len() - 1
    ))
  } else {
    Ok(())
  }
}

fn parse_num<T: FromStr>(tokens: Vec<String>) -> Result<T> {
  check_len(&tokens, 1)?;
  tokens[1]
    .parse()
    .map_err(|_| anyhow!("single arg must be positive int"))
}

fn parse_tcp_address(tokens: Vec<String>) -> Result<(Ipv4Addr, Port)> {
  check_len(&tokens, 2)?;
  Ok((tokens[1].parse()?, tokens[2].parse()?))
}

fn parse_send_args(tokens: Vec<String>) -> Result<(SocketId, Vec<u8>)> {
  check_len(&tokens, 2)?;
  Ok((tokens[1].parse()?, tokens[2..].join(" ").into_bytes()))
}

fn parse_recv_args(tokens: Vec<String>) -> Result<(SocketId, usize, bool)> {
  let block = if tokens.len() != 2 {
    check_len(&tokens, 3)?;
    match tokens[3].as_ref() {
      "y" => true,
      "n" => false,
      s => return Err(anyhow!("unrecognized arg {s}, expected y|n")),
    }
  } else {
    false
  };

  Ok((tokens[1].parse()?, tokens[2].parse()?, block))
}

fn parse_shutdown_args(tokens: Vec<String>) -> Result<(SocketId, SocketSide)> {
  let side = if tokens.len() != 1 {
    check_len(&tokens, 2)?;
    tokens[2].parse()?
  } else {
    SocketSide::Write
  };
  Ok((tokens[1].parse()?, side))
}

fn parse_send_file_args(tokens: Vec<String>) -> Result<(String, Ipv4Addr, Port)> {
  check_len(&tokens, 3)?;
  Ok((tokens[1].parse()?, tokens[2].parse()?, tokens[3].parse()?))
}

fn parse_recv_file_args(tokens: Vec<String>) -> Result<(String, Port)> {
  check_len(&tokens, 2)?;
  Ok((tokens[1].clone(), tokens[2].parse()?))
}

fn help_msg(bad_cmd: Option<&str>) {
  if let Some(bad_cmd) = bad_cmd {
    eprintln!("Unrecognized command {bad_cmd}, expected one of ");
  }
  eprintln!(
    "Commands:
a, accept <port>                          - Spawn a socket, bind it to the given port,
                                            and start accepting connections on that port.
c, connect <ip> <port>                    - Attempt to connect to the given ip address,
                                            in dot notation, on the given port.
s, send <socket> <data>                   - Send a string on a socket.
r, recv <socket> <numbytes> [y|n]         - Try to read data from a given socket. If
                                            the last argument is y, then you should
                                            block until numbytes is received, or the
                                            connection closes. If n, then don't block;
                                            return whatever recv returns. Default is n.
sd, shutdown <socket> [read|write|both]   - v_shutdown on the given socket.
cl, close <socket>                        - v_close on the given socket.


sf, send_file <filename> <ip> <port>      - Connect to the given ip and port, send the
                                            entirety of the specified file, and close
                                            the connection.
rf, recv_file <filename> <port>           - Listen for a connection on the given port.
                                            Once established, write everything you can
                                            read from the socket to the given file.
                                            Once the other side closes the connection,
                                            close the connection as well.

li, interfaces                            - list interfaces
lr, routes                                - list routing table rows
ls, sockets                               - list sockets (fd, ip, port, state)
lw, window <socket>                       - lists window sizes for socket

up <id>                                   - enable interface with id
down <id>                                 - disable interface with id

q, quit                                   - exit
h, help                                   - show this help"
  )
}

fn parse(tokens: Vec<String>, ip_layer: &mut IpLayer, tcp_layer: &mut TcpLayer) -> Result<bool> {
  let cmd = tokens[0].clone();
  match cmd.as_str() {
    // we can unwrap, since the match ensures it's always a valid state
    "up" | "down" => ip_layer.toggle_interface(parse_num(tokens)?, cmd.parse().unwrap())?,

    "interfaces" | "li" => ip_layer.print_interfaces(),
    "routes" | "lr" => ip_layer.print_routes(),
    "sockets" | "ls" => tcp_layer.print_sockets(),
    "window" | "lw" => (),

    "accept" | "a" => tcp_layer.accept(parse_num(tokens)?),
    "connect" | "c" => {
      let (dst_ip, port) = parse_tcp_address(tokens)?;
      match ip_layer.get_src_from_dst(dst_ip) {
        Some(src_ip) => tcp_layer.connect(src_ip, dst_ip, port),
        None => println!("Destination ip was not reachable"),
      }
    }
    "send" | "s" => {
      let (socket_id, data) = parse_send_args(tokens)?;
      tcp_layer.send(socket_id, data)?;
    }
    "recv" | "r" => {
      let (socket_id, numbytes, should_block) = parse_recv_args(tokens)?;
      tcp_layer.recv(socket_id, numbytes, should_block)?;
    }
    "shutdown" | "sd" => {
      let (socket_id, shutdown_method) = parse_shutdown_args(tokens)?;
      tcp_layer.shutdown(socket_id, shutdown_method);
    }
    "close" | "cl" => {
      tcp_layer.close(parse_num(tokens)?);
    }

    "send_file" | "sf" => {
      let (filename, ip, port) = parse_send_file_args(tokens)?;
      tcp_layer.send_file(filename, ip, port);
    }
    "recv_file" | "rf" => {
      let (filename, port) = parse_recv_file_args(tokens)?;
      tcp_layer.recv_file(filename, port);
    }

    "quit" | "q" => return Ok(true),
    "help" | "h" => help_msg(None),
    other => help_msg(Some(other)),
  }
  Ok(false)
}

fn run(mut ip_layer: IpLayer, mut tcp_layer: TcpLayer) -> Result<()> {
  loop {
    print!("> ");
    stdout().flush()?;

    // read user input
    let mut buf = String::new();
    let bytes = stdin().read_line(&mut buf)?;
    // this means EOF was sent
    if bytes == 0 {
      println!("");
      break;
    }

    let s = buf.as_str().trim();
    let tokens: Vec<String> = match shellwords::split(s) {
      Ok(tokens) if tokens.len() == 0 => continue,
      Ok(tokens) => tokens,
      Err(e) => {
        eprintln!("Error: {e}");
        continue;
      }
    };

    match parse(tokens, &mut ip_layer, &mut tcp_layer) {
      Ok(false) => (),
      Ok(true) => break,
      Err(e) => eprintln!("Error: {e}"),
    }
  }
  Ok(())
}

fn main() -> Result<()> {
  let args = Args::parse();
  let config = LnxConfig::new(&args.lnx_filename)?;
  let mut ip_layer = IpLayer::new(config);
  let tcp_layer = TcpLayer::new(ip_layer.get_ip_send_tx());
  ip_layer.register_handler(Protocol::TCP, tcp_layer.get_tcp_handler());
  run(ip_layer, tcp_layer).map_err(|e| {
    eprintln!("Fatal error: {e}");
    eprintln!("exiting...");
    e
  })
}
