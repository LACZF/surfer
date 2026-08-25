#![deny(unused_crate_dependencies)]

include!(concat!(env!("OUT_DIR"), "/surfer_plugin_fingerprint.rs"));

mod vpi_plugin;

#[unsafe(no_mangle)]
pub extern "C" fn SURFER_PLUGIN_FINGERPRINT() -> *const u8 {
    SURFER_PLUGIN_FINGERPRINT_VALUE.as_ptr()
}

vpi::startup_routines! {
    vpi_plugin::vpi_plugin_startup,
}
