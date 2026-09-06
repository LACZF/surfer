#![deny(unused_crate_dependencies)]

include!(concat!(env!("OUT_DIR"), "/surfer_plugin_fingerprint.rs"));

use vhpi::startup_routines;

mod vhpi_plugin;

#[unsafe(no_mangle)]
pub static SURFER_PLUGIN_FINGERPRINT: [u8; 32] = SURFER_PLUGIN_FINGERPRINT_VALUE;

startup_routines! {
    vhpi_plugin::vhpi_plugin_startup,
}
