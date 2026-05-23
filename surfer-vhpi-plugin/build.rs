include!("../tools/plugin_fingerprint.rs");

fn main() {
    generate().expect("failed to generate plugin fingerprint");

    // Increase stack size to 32 MB to handle large VHDL hierarchies
    #[cfg(target_os = "windows")]
    println!("cargo:rustc-link-arg=/STACK:33554432");
}
