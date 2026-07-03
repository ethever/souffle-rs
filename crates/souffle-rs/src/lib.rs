//! Safe Rust API for embedded Souffle Datalog programs.
//!
//! This crate will expose typed relation insertion, execution, and output
//! retrieval over the C ABI provided by `souffle-rs-sys`.

#[cfg(test)]
mod tests {
    #[test]
    fn crate_loads() {
        assert_eq!(env!("CARGO_PKG_NAME"), "souffle-rs");
    }
}
