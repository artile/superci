//! Superci-cache, built for AWS machines (Linux by architecture, and Windows), for the superci program to carry.

/// `superci-cache` by architecture (`x86_64`, `aarch64`, `x86_64-windows`).
pub const CACHE: [(&str, &[u8]); 3] = [("x86_64", include_bytes!(env!("CACHE_X86_64"))), ("aarch64", include_bytes!(env!("CACHE_AARCH64"))), ("x86_64-windows", include_bytes!(env!("CACHE_WINDOWS")))];
