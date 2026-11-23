use crate::message::Message;
use crate::state::{ComputedSignalDialogState, SignalSliceDialogState, SignalSplitDialogState};
use ecolor::Color32;
use egui::{Layout, RichText};
use emath::Align;

#[derive(Debug, Default, Copy, Clone)]
pub struct ReloadWaveformDialog {
    /// `true` to persist the setting returned by the dialog.
    do_not_show_again: bool,
}

#[derive(Debug, Default, Copy, Clone)]
pub struct OpenSiblingStateFileDialog {
    do_not_show_again: bool,
}

/// Draw a dialog that asks the user if it wants to load a state file situated in the same directory as the waveform file.
pub(crate) fn draw_open_sibling_state_file_dialog(
    ctx: &egui::Context,
    dialog: OpenSiblingStateFileDialog,
    msgs: &mut Vec<Message>,
) {
    let mut do_not_show_again = dialog.do_not_show_again;
    egui::Window::new("State file detected")
            .auto_sized()
            .collapsible(false)
            .fixed_pos(ctx.content_rect().center())
            .show(ctx, |ui| {
                let label = ui.label(RichText::new("A state file was detected in the same directory as the loaded file.\nLoad state?").heading());
                ui.set_width(label.rect.width());
                ui.add_space(5.0);
                ui.checkbox(
                    &mut do_not_show_again,
                    "Remember my decision for this session",
                );
                ui.add_space(14.0);
                ui.with_layout(Layout::right_to_left(Align::TOP), |ui| {
                    // Sets the style when focused
                    ui.style_mut().visuals.widgets.active.weak_bg_fill = Color32::BLUE;
                    let load_button = ui.button("Load");
                    let dont_load_button = ui.button("Don't load");
                    ctx.memory_mut(|mem| {
                        if !matches!(mem.focused(), Some(id) if id == load_button.id || id == dont_load_button.id)
                        {
                            mem.request_focus(load_button.id);
                        }
                    });

                    if load_button.clicked() {
                        msgs.push(Message::CloseOpenSiblingStateFileDialog {
                            load_state: true,
                            do_not_show_again,
                        });
                    } else if dont_load_button.clicked() {
                        msgs.push(Message::CloseOpenSiblingStateFileDialog {
                            load_state: false,
                            do_not_show_again,
                        });
                    } else if do_not_show_again != dialog.do_not_show_again {
                        msgs.push(Message::UpdateOpenSiblingStateFileDialog(OpenSiblingStateFileDialog {
                            do_not_show_again,
                        }));
                    }
                });
            });
}

/// Draw a dialog that asks for user confirmation before re-loading a file.
/// This is triggered by a file loading event from disk.
pub(crate) fn draw_reload_waveform_dialog(
    ctx: &egui::Context,
    dialog: ReloadWaveformDialog,
    msgs: &mut Vec<Message>,
) {
    let mut do_not_show_again = dialog.do_not_show_again;
    egui::Window::new("File Change")
        .auto_sized()
        .collapsible(false)
        .fixed_pos(ctx.content_rect().center())
        .show(ctx, |ui| {
            let label = ui.label(RichText::new("File on disk has changed. Reload?").heading());
            ui.set_width(label.rect.width());
            ui.add_space(5.0);
            ui.checkbox(
                &mut do_not_show_again,
                "Remember my decision for this session",
            );
            ui.add_space(14.0);
            ui.with_layout(Layout::right_to_left(Align::TOP), |ui| {
                // Sets the style when focused
                ui.style_mut().visuals.widgets.active.weak_bg_fill = Color32::BLUE;
                let reload_button = ui.button("Reload");
                let leave_button = ui.button("Leave");
                ctx.memory_mut(|mem| {
                    if !matches!(mem.focused(), Some(id) if id == reload_button.id || id == leave_button.id)
                    {
                        mem.request_focus(reload_button.id);
                    }
                });

                if reload_button.clicked() {
                    msgs.push(Message::CloseReloadWaveformDialog {
                        reload_file: true,
                        do_not_show_again,
                    });
                } else if leave_button.clicked() {
                    msgs.push(Message::CloseReloadWaveformDialog {
                        reload_file: false,
                        do_not_show_again,
                    });
                } else if do_not_show_again != dialog.do_not_show_again {
                    msgs.push(Message::UpdateReloadWaveformDialog(ReloadWaveformDialog {
                        do_not_show_again,
                    }));
                }
            });
        });
}

pub(crate) fn draw_computed_signal_dialog(
    ctx: &egui::Context,
    dialog: &ComputedSignalDialogState,
    msgs: &mut Vec<Message>,
) {
    if !dialog.open {
        return;
    }
    let mut name = dialog.name.clone();
    let mut expression = dialog.expression.clone();
    let mut open = dialog.open;
    let sources = dialog.sources.clone();

    egui::Window::new("Add computed signal")
        .auto_sized()
        .collapsible(false)
        .resizable(true)
        .show(ctx, |ui| {
            ui.label("Create a virtual signal from an expression over existing signals.");
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label("Name:");
                ui.text_edit_singleline(&mut name);
            });
            ui.horizontal(|ui| {
                ui.label("Expression:");
                ui.add(egui::TextEdit::singleline(&mut expression).desired_width(360.0));
            });
            ui.add_space(4.0);
            ui.label("Sources (use these names in expression):");
            if sources.is_empty() {
                ui.label(RichText::new("No sources. Select variables and open this dialog.").weak());
            } else {
                for (n, r) in &sources {
                    ui.monospace(format!("{n} = {:?}", r));
                }
            }
            ui.add_space(4.0);
            ui.label("Operators: + - * / % & | ^ ~ ! << >> == != < <= > >= && || ?: numbers: 10 0xff 0b1010");
            if let Some(err) = &dialog.error {
                ui.colored_label(Color32::RED, err);
            }
            ui.add_space(8.0);
            ui.with_layout(Layout::right_to_left(Align::TOP), |ui| {
                if ui.button("Add").clicked() {
                    let expr = expression.trim().to_string();
                    let nm = name.trim().to_string();
                    if nm.is_empty() {
                        msgs.push(Message::UpdateComputedSignalDialog(ComputedSignalDialogState {
                            open: true, name: name.clone(), expression: expression.clone(), sources: sources.clone(), error: Some("Name is required".into()),
                        }));
                        return;
                    }
                    if expr.is_empty() {
                        msgs.push(Message::UpdateComputedSignalDialog(ComputedSignalDialogState {
                            open: true, name: name.clone(), expression: expression.clone(), sources: sources.clone(), error: Some("Expression is required".into()),
                        }));
                        return;
                    }
                    match crate::computed_signal::parse_expression(&expr) {
                        Ok(_) => msgs.push(Message::AddComputedSignal {
                            name: nm,
                            expression: expr,
                            sources,
                            insert_before: None,
                        }),
                        Err(e) => msgs.push(Message::UpdateComputedSignalDialog(ComputedSignalDialogState {
                            open: true, name: name.clone(), expression: expression.clone(), sources: sources.clone(), error: Some(e),
                        })),
                    }
                }
                if ui.button("Cancel").clicked() {
                    open = false;
                    msgs.push(Message::CloseComputedSignalDialog);
                }
            });
            if !open {
                msgs.push(Message::CloseComputedSignalDialog);
            }
            // update live edits
            if name != dialog.name || expression != dialog.expression {
                msgs.push(Message::UpdateComputedSignalDialog(ComputedSignalDialogState {
                    open: true,
                    name,
                    expression,
                    sources: dialog.sources.clone(),
                    error: None,
                }));
            }
        });
}

pub(crate) fn draw_signal_slice_dialog(
    ctx: &egui::Context,
    dialog: &SignalSliceDialogState,
    msgs: &mut Vec<Message>,
) {
    if !dialog.open { return; }
    let mut msb = dialog.msb.clone();
    let mut lsb = dialog.lsb.clone();
    egui::Window::new("Slice signal").auto_sized().collapsible(false).show(ctx, |ui| {
        ui.label("Create a new signal from selected bit range [msb:lsb].");
        ui.horizontal(|ui| { ui.label("MSB:"); ui.text_edit_singleline(&mut msb); });
        ui.horizontal(|ui| { ui.label("LSB:"); ui.text_edit_singleline(&mut lsb); });
        if let Some(err) = &dialog.error { ui.colored_label(Color32::RED, err); }
        ui.with_layout(Layout::right_to_left(Align::TOP), |ui| {
            if ui.button("Add").clicked() {
                let Some(target) = dialog.target else { return; };
                let Ok(msb_v) = msb.trim().parse::<i64>() else {
                    msgs.push(Message::UpdateSignalSliceDialog(SignalSliceDialogState { open:true, target:Some(target), msb: msb.clone(), lsb: lsb.clone(), error:Some("MSB must be integer".into()) })); return;
                };
                let Ok(lsb_v) = lsb.trim().parse::<i64>() else {
                    msgs.push(Message::UpdateSignalSliceDialog(SignalSliceDialogState { open:true, target:Some(target), msb: msb.clone(), lsb: lsb.clone(), error:Some("LSB must be integer".into()) })); return;
                };
                msgs.push(Message::AddSignalSlice { target, msb: msb_v, lsb: lsb_v });
                msgs.push(Message::CloseSignalSliceDialog);
            }
            if ui.button("Cancel").clicked() { msgs.push(Message::CloseSignalSliceDialog); }
        });
        if msb != dialog.msb || lsb != dialog.lsb {
            msgs.push(Message::UpdateSignalSliceDialog(SignalSliceDialogState { open:true, target:dialog.target, msb, lsb, error:None }));
        }
    });
}

pub(crate) fn draw_signal_split_dialog(
    ctx: &egui::Context,
    dialog: &SignalSplitDialogState,
    msgs: &mut Vec<Message>,
) {
    if !dialog.open { return; }
    let mut width = dialog.width.clone();
    egui::Window::new("Split signal by width").auto_sized().collapsible(false).show(ctx, |ui| {
        ui.label("Split selected signal into multiple signals by chunk width.");
        ui.horizontal(|ui| { ui.label("Width:"); ui.text_edit_singleline(&mut width); });
        if let Some(err) = &dialog.error { ui.colored_label(Color32::RED, err); }
        ui.with_layout(Layout::right_to_left(Align::TOP), |ui| {
            if ui.button("Split").clicked() {
                let Some(target) = dialog.target else { return; };
                let Ok(w) = width.trim().parse::<u32>() else {
                    msgs.push(Message::UpdateSignalSplitDialog(SignalSplitDialogState { open:true, target:Some(target), width: width.clone(), error:Some("Width must be positive integer".into()) })); return;
                };
                if w == 0 {
                    msgs.push(Message::UpdateSignalSplitDialog(SignalSplitDialogState { open:true, target:Some(target), width: width.clone(), error:Some("Width must be > 0".into()) })); return;
                }
                msgs.push(Message::SplitSignalByWidth { target, width: w });
                msgs.push(Message::CloseSignalSplitDialog);
            }
            if ui.button("Cancel").clicked() { msgs.push(Message::CloseSignalSplitDialog); }
        });
        if width != dialog.width {
            msgs.push(Message::UpdateSignalSplitDialog(SignalSplitDialogState { open:true, target:dialog.target, width, error:None }));
        }
    });
}
