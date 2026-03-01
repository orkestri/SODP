fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Use the vendored protoc binary so the build works without a system install.
    let protoc = protoc_bin_vendored::protoc_bin_path()?;
    unsafe { std::env::set_var("PROTOC", protoc); }

    tonic_build::compile_protos("proto/bench.proto")?;
    Ok(())
}
