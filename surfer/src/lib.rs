#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
fn android_main(app: winit::platform::android::activity::AndroidApp) {
    use eframe::NativeOptions;
    use libsurfer::run_egui;
    let state = libsurfer::SystemState::new().unwrap();
    let options = NativeOptions {
        event_loop_builder: Some(Box::new(move |builder| {
            use winit::platform::android::EventLoopBuilderExtAndroid;
            builder.with_android_app(app);
        })),
        ..Default::default()
    };
    eframe::run_native("Surfer", options, Box::new(|cc| Ok(run_egui(cc, state)?))).unwrap();
}
