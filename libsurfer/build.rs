use std::error::Error;
use vergen_gitcl::{BuildBuilder, Emitter, GitclBuilder};

include!("../tools/plugin_fingerprint.rs");

fn main() -> Result<(), Box<dyn Error>> {
    generate()?;

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
