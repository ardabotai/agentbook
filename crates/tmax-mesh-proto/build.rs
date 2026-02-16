use anyhow::Result;
use std::path::PathBuf;

fn main() -> Result<()> {
    let protoc = protoc_bin_vendored::protoc_bin_path()?;
    let include = protoc_bin_vendored::include_path()?;
    // Safety: build scripts run in a controlled single-process environment.
    unsafe {
        std::env::set_var("PROTOC", protoc);
    }

    let protos = vec![
        PathBuf::from("proto/tmax_mesh.proto"),
        PathBuf::from("proto/tmax_host.proto"),
    ];
    let includes = vec![PathBuf::from("proto"), include];
    tonic_prost_build::configure()
        .build_server(true)
        .build_client(true)
        .compile_protos(&protos, &includes)?;
    Ok(())
}
