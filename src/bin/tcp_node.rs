use std::io::{stdin, stdout, Write};
use std::net::Ipv4Addr;

use anyhow::{anyhow, Result};
use clap::Parser;
use ip_tcp::ip::ip_layer::IpLayer;
use ip_tcp::ip::protocol::Protocol;
use ip_tcp::misc::lnx_config::LnxConfig;
use ip_tcp::InterfaceId;
use shellwords;

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
pub struct Args {
  /// Filename of the lnx file
  #[clap(required = true)]
  lnx_filename: String,
}

fn parse_send_args(tokens: Vec<String>) -> Result<(Ipv4Addr, Protocol, Vec<u8>)> {
  if tokens.len() != 4 {
    return Err(anyhow!(
      "'{}' expected 3 arguments received {}",
      tokens[0],
      tokens.len() - 1
    ));
  }
  let their_ip: Ipv4Addr = match tokens[1].parse::<Ipv4Addr>() {
    Ok(ip) => ip,
    Err(_) => {
      return Err(anyhow!("Failed to parse VIP"));
    }
  };
  let protocol: Protocol = match tokens[2].parse::<u8>() {
    Ok(protocol_num) => match Protocol::try_from(protocol_num) {
      Ok(protocol) => protocol,
      Err(e) => {
        return Err(anyhow!("Failed to parse protocol, {e}"));
      }
    },
    Err(_) => {
      return Err(anyhow!("Failed to parse protocol, must be u8"));
    }
  };
  Ok((their_ip, protocol, tokens[3].as_bytes().to_vec()))
}

fn parse_toggle_args(tokens: Vec<String>) -> Result<InterfaceId> {
  if tokens.len() != 2 {
    return Err(anyhow!(
      "'{}' expected 1 argument received {}",
      tokens[0],
      tokens.len() - 1
    ));
  }

  tokens[1]
    .parse()
    .map_err(|_| anyhow!("interface id must be positive int"))
}

fn run(mut ip_layer: IpLayer) -> Result<()> {
  let help_msg = |bad_cmd: Option<&str>| {
    if let Some(bad_cmd) = bad_cmd {
      eprintln!("Unrecognized command {bad_cmd}, expected one of ");
    }
    eprintln!(
      "Commands:
a <port>                       - Spawn a socket, bind it to the given port, 
                                 and start accepting connections on that port.
c <ip> <port>                  - Attempt to connect to the given ip address,
                                 in dot notation, on the given port.
s <socket> <data>              - Send a string on a socket.
r <socket> <numbytes> [y|n]    - Try to read data from a given socket. If
                                 the last argument is y, then you should
                                 block until numbytes is received, or the
                                 connection closes. If n, then don.t block;
                                 return whatever recv returns. Default is n.
sf <filename> <ip> <port>      - Connect to the given ip and port, send the
                                 entirety of the specified file, and close
                                 the connection.
rf <filename> <port>           - Listen for a connection on the given port.
                                 Once established, write everything you can
                                       read from the socket to the given file.
                                 Once the other side closes the connection,
                                 close the connection as well.
sd <socket> [read|write|both]  - v_shutdown on the given socket.
cl <socket>                    - v_close on the given socket.
up <id>                        - enable interface with id
down <id>                      - disable interface with id
li, interfaces                 - list interfaces
lr, routes                     - list routing table rows
ls, sockets                    - list sockets (fd, ip, port, state)
window <socket>                - lists window sizes for socket
q, quit                        - exit
h, help                        - show this help"
    )
  };

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

    let cmd = tokens[0].clone();
    match cmd.as_str() {
      "up" | "down" => match parse_toggle_args(tokens) {
        // we can unwrap, since the match ensures it's always a valid state
        Ok(id) => ip_layer
          .toggle_interface(id, cmd.parse().unwrap())
          .unwrap_or_else(|e| {
            eprintln!("Error: {e}");
          }),
        Err(e) => {
          eprintln!("Error: {e}");
        }
      },
      "interfaces" | "li" => ip_layer.print_interfaces(),
      "routes" | "lr" => ip_layer.print_routes(),
      "sockets" | "ls" => todo!(),
      "accept" | "a" => todo!(),
      "connect" | "c" => todo!(),
      "send" | "s" => todo!(),
      "recv" | "r" => todo!(),
      "shutdown" | "sd" => todo!(),
      "close" | "cl" => todo!(),
      "send_file" | "sf" => todo!(),
      "recv_file" | "rf" => todo!(),
      "quit" | "q" => break,
      "help" | "h" => help_msg(None),
      other => help_msg(Some(other)),
    }
  }
  Ok(())
}

fn main() -> Result<()> {
  let args = Args::parse();
  let config = LnxConfig::new(&args.lnx_filename)?;
  let mut ip_layer = IpLayer::new(config);
  ip_layer.register_handler(
    Protocol::Test,
    Box::new(|packet| {
      let data = String::from_utf8(packet.data().to_vec());
      if let Ok(s) = data {
        println!(
          concat!(
            "Node received packet!\n",
            "\tsource IP\t: {}\n",
            "\tdestination IP\t: {}\n",
            "\tprotocol\t: {}\n",
            "\tpayload length\t: {}\n",
            "\tpayload\t\t: {}\n",
          ),
          packet.source_address(),
          packet.destination_address(),
          packet.protocol(),
          s.len(),
          s
        );
      }
    }),
  );
  run(ip_layer).map_err(|e| {
    eprintln!("Fatal error: {e}");
    eprintln!("exiting...");
    e
  })
}
