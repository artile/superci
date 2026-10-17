//! superci-layers as a library: what the program (`main.rs`) is made of, also for its tests.
/// containerd's own API, as its .proto files say it (see build.rs).
pub mod containerd {
    pub mod types { tonic::include_proto!("containerd.types"); }
    pub mod services {
        pub mod snapshots { pub mod v1 { tonic::include_proto!("containerd.services.snapshots.v1"); } }
        pub mod content { pub mod v1 { tonic::include_proto!("containerd.services.content.v1"); } }
        pub mod leases { pub mod v1 { tonic::include_proto!("containerd.services.leases.v1"); } }
    }
}
pub mod client;
pub mod content;
pub mod error;
pub mod fetch;
pub mod filters;
pub mod index;
pub mod keep;
pub mod meta;
pub mod mount;
pub mod overlay;
pub mod server;
pub mod setup;
pub mod store;

