use std::env;
use std::error::Error;
use std::process::Command;
use vergen_gitcl::{BuildBuilder, Emitter, GitclBuilder};

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo:rerun-if-env-changed=RUSTC");
    println!("cargo:rustc-check-cfg=cfg(nightly)");
    if rustc_is_nightly()? {
        println!("cargo:rustc-cfg=nightly");
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

fn rustc_is_nightly() -> Result<bool, Box<dyn Error>> {
    let rustc = env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let output = Command::new(rustc).arg("--version").output()?;
    if !output.status.success() {
        return Err(format!("rustc --version failed with {}", output.status).into());
    }

    let version = String::from_utf8(output.stdout)?;
    Ok(version
        .split_whitespace()
        .nth(1)
        .is_some_and(|version| version.ends_with("-nightly")))
}
