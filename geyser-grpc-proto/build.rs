use std::path::Path;

fn main() -> anyhow::Result<()> {
    let proto_path = Path::new("proto/geyser.proto");

    // directory the main .proto file resides in
    let proto_dir = proto_path
        .parent()
        .expect("proto file should reside in a directory");

    tonic_build::configure()
        .protoc_arg("--experimental_allow_proto3_optional")
        .compile(&[proto_path], &[proto_dir])?;

    Ok(())
}
