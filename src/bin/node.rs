use clap::Parser;
use debug_print::{debug_eprintln as edebug, debug_println as debug};

use ip::lnx_config::LnxConfig;

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
pub struct Args {
  /// Filename of the lnx file
  #[clap(required = true)]
  lnx_filename: String,
}

fn main() {
  let args = Args::parse();
  dbg!(LnxConfig::new(&args.lnx_filename));
}
