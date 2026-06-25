use std::error::Error;
use vergen_gitcl::{Build, Emitter, Gitcl};

fn main() -> Result<(), Box<dyn Error>> {
    // describe with tags=true so that v0.4.0 gets picked up (not annotated)
    let git = Gitcl::all().describe(true, true, None).build();
    let build = Build::all_build();
    Emitter::default()
        .add_instructions(&build)?
        .add_instructions(&git)?
        .emit()?;
    Ok(())
}
