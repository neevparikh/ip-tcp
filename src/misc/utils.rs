#[macro_export]
macro_rules! debug {
  ($($arg:tt)*) => {
    if cfg!(debug_assertions) {
      print!("[{}:{}] ", file!(), line!());
      println!($($arg)*)
    }
  };
}
#[macro_export]
macro_rules! edebug {
  ($($arg:tt)*) => {
    if cfg!(debug_assertions) {
      eprint!("[{}:{}] ", file!(), line!());
      eprintln!($($arg)*)
    }
  };
}
