//! Raw C ABI bindings for the C++ wrapper around Souffle generated code.
//!
//! This crate intentionally does not bind Souffle C++ types directly.

#[cfg(test)]
mod tests {
    #[test]
    fn crate_loads() {
        assert_eq!(env!("CARGO_PKG_NAME"), "souffle-rs-sys");
    }
}
