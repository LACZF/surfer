use vhpi::startup_routines;

mod nvc_plugin;

startup_routines! {
    nvc_plugin::nvc_plugin_startup,
}
