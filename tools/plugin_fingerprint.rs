use sha2::Digest;
use std::{env, fs, path::Path};

pub fn generate() -> Result<(), Box<dyn std::error::Error>> {
    let manifest_dir_value = env::var("CARGO_MANIFEST_DIR")?;
    let manifest_dir = Path::new(&manifest_dir_value);
    let workspace_dir = manifest_dir.parent().unwrap_or(manifest_dir);
    let protocol = workspace_dir.join("surfer-simulator-protocol/proto/simulator_protocol.proto");
    let lockfile = workspace_dir.join("Cargo.lock");

    println!("cargo:rerun-if-changed={}", protocol.display());
    println!("cargo:rerun-if-changed={}", lockfile.display());
    println!(
        "cargo:rerun-if-changed={}",
        workspace_dir.join("tools/plugin_fingerprint.rs").display()
    );

    let mut hasher = sha2::Sha256::new();
    hasher.update(fs::read(protocol)?);
    hasher.update(fs::read(lockfile)?);
    let fingerprint = hasher.finalize();
    let values = fingerprint
        .iter()
        .map(|byte| format!("0x{byte:02x}"))
        .collect::<Vec<_>>()
        .join(", ");
    fs::write(
        Path::new(&env::var("OUT_DIR")?).join("surfer_plugin_fingerprint.rs"),
        format!("pub const SURFER_PLUGIN_FINGERPRINT_VALUE: [u8; 32] = [{values}];\n"),
    )?;
    Ok(())
}
