#![deny(unused_crate_dependencies)]

include!(concat!(env!("OUT_DIR"), "/surfer_plugin_fingerprint.rs"));

mod vpi_plugin;

#[unsafe(no_mangle)]
pub static SURFER_PLUGIN_FINGERPRINT: [u8; 32] = SURFER_PLUGIN_FINGERPRINT_VALUE;

vpi::startup_routines! {
    vpi_plugin::vpi_plugin_startup,
}
