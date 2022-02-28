#[macro_export]
macro_rules! debug {
  ($($arg:tt)*) => {
    #[cfg(debug_assertions)]
    print!("[{}:{}] ", file!(), line!());
    println!($($arg)*)
  };
}
#[macro_export]
macro_rules! edebug {
  ($($arg:tt)*) => {
    #[cfg(debug_assertions)]
    eprint!("[{}:{}] ", file!(), line!());
    eprintln!($($arg)*)
  };
}
