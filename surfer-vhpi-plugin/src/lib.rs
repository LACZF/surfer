#![deny(unused_crate_dependencies)]

include!(concat!(env!("OUT_DIR"), "/surfer_plugin_fingerprint.rs"));

use vhpi::startup_routines;

mod vhpi_plugin;

#[unsafe(no_mangle)]
pub extern "C" fn SURFER_PLUGIN_FINGERPRINT() -> *const u8 {
    SURFER_PLUGIN_FINGERPRINT_VALUE.as_ptr()
}

startup_routines! {
    vhpi_plugin::vhpi_plugin_startup,
}
