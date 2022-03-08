use clap::Parser;

use anyhow::Result;
use ip::lnx_config::LnxConfig;
use ip::node::Node;
use ip::protocol::Protocol;

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
pub struct Args {
  /// Filename of the lnx file
  #[clap(required = true)]
  lnx_filename: String,
}

fn main() -> Result<()> {
  let args = Args::parse();
  let config = LnxConfig::new(&args.lnx_filename)?;
  let mut node = Node::new(config);
  node.register_handler(Protocol::Test, Box::new(|packet| {
    let data = String::from_utf8(packet.get_data().to_vec());
    if let Ok(s) = data {
      println!(concat!(
          "Node received packet!\n",
          "\tsource IP\t: {}\n",
          "\tdestination IP\t: {}\n",
          "\tprotocol\t: {}\n",
          "\tpayload length\t: {}\n",
          "\tpayload\t\t: {}\n",
          ), packet.source_address(), packet.destination_address(), packet.protocol(), s.len(), s);
    }
    vec![]
  }));
  node.run()?;
  Ok(())
}
