use std::sync::Mutex;

use ecolor::Color32;
use egui::{RichText, TextWrapMode};
use egui_extras::{Column, TableBuilder};

use crate::{SystemState, message::Message};

#[derive(Clone, Copy)]
pub enum CommandOutputStream {
    Command,
    Stdout,
    Stderr,
}

#[derive(Clone)]
struct CommandOutputLine {
    source: String,
    stream: CommandOutputStream,
    message: String,
}

static COMMAND_OUTPUT: Mutex<Vec<CommandOutputLine>> = Mutex::new(vec![]);

pub fn record_command_output(source: &str, stream: CommandOutputStream, message: &str) {
    COMMAND_OUTPUT
        .lock()
        .expect("Failed to lock command output buffer. Thread poisoned?")
        .push(CommandOutputLine {
            source: source.to_string(),
            stream,
            message: message.to_string(),
        });
}

pub fn clear_command_output() {
    COMMAND_OUTPUT
        .lock()
        .expect("Failed to lock command output buffer. Thread poisoned?")
        .clear();
}

impl SystemState {
    pub fn draw_command_output_window(&self, ctx: &egui::Context, msgs: &mut Vec<Message>) {
        let mut open = true;
        let mut should_close = false;
        egui::Window::new("Command Output")
            .open(&mut open)
            .collapsible(true)
            .resizable(true)
            .show(ctx, |ui| {
                ui.style_mut().wrap_mode = Some(TextWrapMode::Extend);

                egui::ScrollArea::horizontal().show(ui, |ui| {
                    let records = COMMAND_OUTPUT.lock().unwrap();

                    TableBuilder::new(ui)
                        .column(Column::auto().resizable(true))
                        .column(Column::auto().resizable(true))
                        .column(Column::remainder().resizable(true))
                        .vscroll(true)
                        .stick_to_bottom(true)
                        .header(20.0, |mut header| {
                            header.col(|ui| {
                                ui.heading("Tool");
                            });
                            header.col(|ui| {
                                ui.heading("Type");
                            });
                            header.col(|ui| {
                                ui.heading("Message");
                            });
                        })
                        .body(|mut body| {
                            for record in records.iter() {
                                let (stream_name, stream_color) = match record.stream {
                                    CommandOutputStream::Command => {
                                        ("command", Color32::LIGHT_BLUE)
                                    }
                                    CommandOutputStream::Stdout => ("stdout", Color32::LIGHT_GREEN),
                                    CommandOutputStream::Stderr => ("stderr", Color32::LIGHT_RED),
                                };

                                body.row(20.0, |mut row| {
                                    row.col(|ui| {
                                        ui.label(
                                            RichText::new(record.source.clone())
                                                .color(Color32::GRAY)
                                                .monospace(),
                                        );
                                    });
                                    row.col(|ui| {
                                        ui.label(
                                            RichText::new(stream_name)
                                                .color(stream_color)
                                                .monospace(),
                                        );
                                    });
                                    row.col(|ui| {
                                        ui.label(RichText::new(record.message.clone()).monospace());
                                    });
                                });
                            }
                        });
                });

                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("Clear").clicked() {
                        clear_command_output();
                    }
                    if ui.button("Close").clicked() {
                        should_close = true;
                    }
                });
            });

        if !open || should_close {
            msgs.push(Message::SetCommandOutputVisible(false));
        }
    }
}
