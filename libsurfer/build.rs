use std::error::Error;
use vergen_gitcl::{BuildBuilder, Emitter, GitclBuilder};

fn main() -> Result<(), Box<dyn Error>> {
    // compile protobuf
    println!("cargo:rerun-if-changed=./src/grpc/surfer_remote.proto");
    {
        let proto_path = std::path::Path::new("./src/grpc/surfer_remote.proto");
        let proto_dir = proto_path
            .parent()
            .expect("proto files should reside in a directory");
        if let Err(err) = tonic_prost_build::configure()
            .out_dir("./src/grpc/")
            .compile_protos(&[proto_path], &[proto_dir])
        {
            println!("{:?}", err);
        }
    }
    // describe with tags=true so that v0.4.0 gets picked up (not annotated)
    let git = GitclBuilder::default()
        .all()
        .describe(true, true, None)
        .build()?;
    let build = BuildBuilder::all_build()?;
    Emitter::default()
        .add_instructions(&build)?
        .add_instructions(&git)?
        .emit()?;
    Ok(())
}
