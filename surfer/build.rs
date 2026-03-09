use std::error::Error;
use vergen_gitcl::{BuildBuilder, Emitter, GitclBuilder};

fn main() -> Result<(), Box<dyn Error>> {
    // compile protobuf
    tonic_prost_build::compile_protos("../proto/network.proto")?;
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
