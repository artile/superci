//! Superci-layers, built for AWS machines by architecture, for the superci program to carry.

/// `superci-layers` by architecture (`x86_64`, `aarch64`).
pub const LAYERS: [(&str, &[u8]); 2] = [("x86_64", include_bytes!(env!("LAYERS_X86_64"))), ("aarch64", include_bytes!(env!("LAYERS_AARCH64")))];
