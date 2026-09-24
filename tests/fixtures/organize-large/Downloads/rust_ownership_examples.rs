// Rust ownership examples
fn take(s: String) {}
fn borrow(s: &str) {}
fn main() { let s = String::from("hi"); borrow(&s); take(s); }
