fn main() {
    let protoc = protoc_bin_vendored::protoc_bin_path().expect("failed to find bundled protoc");
    unsafe {
        std::env::set_var("PROTOC", protoc);
    }

    println!("cargo:rerun-if-changed=proto/simulator_protocol.proto");
    println!("cargo:rerun-if-changed=build.rs");

    prost_build::Config::new()
        .compile_protos(&["proto/simulator_protocol.proto"], &["proto"])
        .expect("failed to compile protobuf schema");
}
