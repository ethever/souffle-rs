//! Build helper for `souffle-rs`.
//!
//! This crate will own build-time integration with `souffle -G`, C++ wrapper
//! compilation, and link configuration.

#[cfg(test)]
mod tests {
    #[test]
    fn crate_loads() {
        assert_eq!(env!("CARGO_PKG_NAME"), "souffle-rs-build");
    }
}
