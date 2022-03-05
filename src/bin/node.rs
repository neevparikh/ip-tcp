use clap::Parser;

use anyhow::Result;
use ip::lnx_config::LnxConfig;
use ip::node::Node;

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
  node.run()?;
  Ok(())
}
