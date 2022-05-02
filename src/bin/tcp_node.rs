use std::any::type_name;
use std::fs::File;
use std::io::{stdin, stdout, Read, Write};
use std::net::Ipv4Addr;
use std::str::FromStr;
use std::thread;
use std::time::Instant;

use anyhow::{anyhow, Result};
use clap::Parser;
use ip_tcp::edebug;
use ip_tcp::ip::{IpLayer, Protocol};
use ip_tcp::misc::lnx_config::LnxConfig;
use ip_tcp::tcp::{Port, SocketId, SocketSide, TcpLayer, TcpLayerInfo, TcpListener, TcpStream};
use shellwords;

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
pub struct Args {
  /// Filename of the lnx file
  #[clap(required = true)]
  lnx_filename: String,
}

fn send_file(info: TcpLayerInfo, filename: String, dst_ip: Ipv4Addr, port: Port) -> Result<()> {
  let mut f = File::open(filename)?;
  let mut buf = [0u8; 2usize.pow(14)];

  let stream = TcpStream::connect(info, dst_ip, port)?;

  let now = Instant::now();
  loop {
    let n = f.read(&mut buf)?;
    if n == 0 {
      println!("Sending file finished");
      break;
    }
    stream.send(&buf[0..n]).unwrap();
  }
  stream.close()?;
  println!("Time: {}", Instant::now().duration_since(now).as_millis());
  Ok(())
}

fn recv_file(info: TcpLayerInfo, filename: String, port: Port) -> Result<()> {
  let mut f = File::create(filename)?;
  let mut listener = TcpListener::bind(port, info)?;
  let stream = listener.accept()?;
  listener.close()?;
  let mut buf = [0u8; 2usize.pow(14)];

  let now = Instant::now();
  loop {
    match stream.recv(&mut buf, true) {
      Ok(_) => f.write_all(&buf)?,
      Err(_) => {
        stream.close()?;
        break;
      }
    }
  }
  println!("Time: {}", Instant::now().duration_since(now).as_millis());
  Ok(())
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
    .map_err(|_| anyhow!("single arg must be {}", type_name::<T>()))
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
  // tokens = [cmd_name, args...] that's why we have 3 not 2
  let block = if tokens.len() != 3 {
    check_len(&tokens, 3)?;
    match tokens[3].as_ref() {
      "y" => true,
      "n" => false,
      s => return Err(anyhow!("unrecognized arg to block {s}, expected y|n")),
    }
  } else {
    false
  };

  Ok((tokens[1].parse()?, tokens[2].parse()?, block))
}

fn parse_shutdown_args(tokens: Vec<String>) -> Result<(SocketId, SocketSide)> {
  // tokens = [cmd_name, args...] that's why we have 2 not 1
  let side = if tokens.len() != 2 {
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

fn parse_and_run_cmd(
  tokens: Vec<String>,
  ip_layer: &mut IpLayer,
  tcp_layer: &mut TcpLayer,
) -> Result<bool> {
  let cmd = tokens[0].clone();
  match cmd.as_str() {
    // we can unwrap, since the match ensures it's always a valid state
    "up" | "down" => ip_layer.toggle_interface(parse_num(tokens)?, cmd.parse().unwrap())?,

    "interfaces" | "li" => ip_layer.print_interfaces(),
    "routes" | "lr" => ip_layer.print_routes(),
    "sockets" | "ls" => tcp_layer.print_sockets(),
    "window" | "lw" => tcp_layer.print_window(parse_num(tokens)?),

    "accept" | "a" => {
      let mut listener = TcpListener::bind(parse_num(tokens)?, tcp_layer.get_info())?;
      thread::spawn(move || loop {
        listener
          .accept()
          .err()
          .map(|e| edebug!("Failed to accept, {e}"));
      });
    }
    "connect" | "c" => {
      let (dst_ip, port) = parse_tcp_address(tokens)?;
      TcpStream::connect(tcp_layer.get_info(), dst_ip, port)?;
    }
    "send" | "s" => {
      let (socket_id, data) = parse_send_args(tokens)?;
      tcp_layer.send(socket_id, data)?;
    }
    "recv" | "r" => {
      let (socket_id, numbytes, should_block) = parse_recv_args(tokens)?;
      let mut data = vec![0u8; numbytes];
      tcp_layer.recv(socket_id, &mut data, should_block)?;
      println!(
        "{}",
        std::str::from_utf8(&data).unwrap_or("Not UTF8, can't print")
      );
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
      let info = tcp_layer.get_info();
      thread::spawn(move || {
        send_file(info, filename, ip, port).unwrap_or_else(|e| eprintln!("{e}"))
      });
    }
    "recv_file" | "rf" => {
      let (filename, port) = parse_recv_file_args(tokens)?;
      let info = tcp_layer.get_info();
      thread::spawn(move || recv_file(info, filename, port).unwrap_or_else(|e| eprintln!("{e}")));
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

    match parse_and_run_cmd(tokens, &mut ip_layer, &mut tcp_layer) {
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
  let tcp_layer = TcpLayer::new(ip_layer.get_ip_send_tx(), ip_layer.get_our_ip_addrs());
  ip_layer.register_handler(Protocol::TCP, tcp_layer.get_tcp_handler());
  run(ip_layer, tcp_layer).map_err(|e| {
    eprintln!("Fatal error: {e}");
    eprintln!("exiting...");
    e
  })
}
