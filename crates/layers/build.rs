// containerd's snapshot and content services, from their own .proto files (proto/, as containerd's API module has
// them), compiled without protoc.
fn main() {
    let files = ["proto/services/snapshots/v1/snapshots.proto", "proto/services/content/v1/content.proto", "proto/services/leases/v1/leases.proto"];
    for f in files.iter().chain(&["proto/types/mount.proto"]) { println!("cargo:rerun-if-changed={f}") }
    let fds = protox::compile(files, ["proto"]).expect("containerd's protos");
    tonic_prost_build::configure().build_client(true).build_server(true).compile_fds(fds).expect("containerd's services");
}
