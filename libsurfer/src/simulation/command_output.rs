use std::sync::{LazyLock, Mutex};

use derive_more::Display;
use ecolor::Color32;
use egui::{Align, Layout, RichText, TextWrapMode};
use egui_extras::{Column, TableBuilder};
use regex::Regex;

use crate::simulation::simulators::logging::LogLevel;
use crate::{SystemState, message::Message};
use surfer_time_types::TimeScale;

/// A regex to match time spans in the command output messages.
static TIME_SPAN_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\d+(?:\.\d+)?\s*(?:zs|as|fs|ps|ns|μs|us|ms|s)(?:\s*[+-]\s*\d+)?")
        .expect("time regex must be valid")
});

/// Parses a clickable time string to ticks using the given timescale.
///
/// Returns `None` if the time string is invalid or cannot be parsed.
fn parse_clickable_time_to_ticks(
    time_text: &str,
    timescale: &crate::time::TimeScale,
) -> Option<num::BigInt> {
    let trimmed = time_text.trim();
    let base_time = trimmed
        .char_indices()
        .skip(1)
        .find_map(|(idx, ch)| ((ch == '+') || (ch == '-')).then_some(idx))
        .map_or(trimmed, |idx| trimmed[..idx].trim_end());

    crate::time::parse_time_string_to_ticks(base_time, timescale)
}

/// Finds all time spans in the given message and returns their start and end indices.
fn find_time_spans(message: &str) -> Vec<(usize, usize)> {
    TIME_SPAN_RE
        .find_iter(message)
        .filter_map(|m| {
            let start = m.start();
            let end = m.end();
            let left_ok = message[..start]
                .chars()
                .next_back()
                .is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_' && c != '.');
            let right_ok = message[end..]
                .chars()
                .next()
                .is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_' && c != '.');

            (left_ok && right_ok).then_some((start, end))
        })
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Display)]
/// Represents the source of a command output line.
pub(crate) enum CommandOutputStream {
    #[display("log")]
    Log,
    #[display("command")]
    Command,
    #[display("stdout")]
    Stdout,
    #[display("stderr")]
    Stderr,
}

#[derive(Clone, Debug)]
/// Represents a single line of command output.
struct CommandOutputLine {
    source: String,
    stream: CommandOutputStream,
    message: String,
    level: LogLevel,
}

/// A global buffer to store command output lines.
///
/// This is used to display the command output in the GUI.
static COMMAND_OUTPUT: Mutex<Vec<CommandOutputLine>> = Mutex::new(vec![]);

/// Records a command output line to the global buffer.
#[inline]
pub(crate) fn record_command_output(
    source: &str,
    stream: CommandOutputStream,
    level: LogLevel,
    message: &str,
) {
    COMMAND_OUTPUT
        .lock()
        .expect("Failed to lock command output buffer. Thread poisoned?")
        .push(CommandOutputLine {
            source: source.to_string(),
            stream,
            message: message.to_string(),
            level,
        });
}

/// Clears the command output buffer.
#[inline]
pub(crate) fn clear_command_output() {
    COMMAND_OUTPUT
        .lock()
        .expect("Failed to lock command output buffer. Thread poisoned?")
        .clear();
}

impl SystemState {
    /// Draws the command output window.
    pub(crate) fn draw_command_output_window(&self, ctx: &egui::Context, msgs: &mut Vec<Message>) {
        let mut open = true;
        let mut should_close = false;
        let timescale = self
            .user
            .waves
            .as_ref()
            .map(|waves| waves.inner.metadata().timescale.clone());
        egui::Window::new("Command Output")
            .open(&mut open)
            .collapsible(true)
            .resizable(true)
            .show(ctx, |ui| {
                ui.with_layout(Layout::bottom_up(Align::LEFT), |ui| {
                    ui.style_mut().wrap_mode = Some(TextWrapMode::Extend);
                    ui.horizontal(|ui| {
                        if ui.button("Clear").clicked() {
                            clear_command_output();
                        }
                        if ui.button("Close").clicked() {
                            should_close = true;
                        }
                    });
                    ui.separator();
                    ui.with_layout(Layout::top_down(Align::LEFT), |ui| {
                        egui::ScrollArea::horizontal().show(ui, |ui| {
                            let records = COMMAND_OUTPUT.lock().unwrap();

                            TableBuilder::new(ui)
                                .column(Column::auto().resizable(true))
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
                                        ui.heading("Level");
                                    });
                                    header.col(|ui| {
                                        ui.heading("Message");
                                    });
                                })
                                .body(|body| {
                                    let num_rows = records.len();
                                    body.rows(20.0, num_rows, |row| {
                                        let record = &records[row.index()];
                                        self.message_row(msgs, timescale.as_ref(), row, record);
                                    });
                                });
                        });
                    });
                });
            });

        if !open || should_close {
            msgs.push(Message::SetCommandOutputVisible(false));
        }
    }

    fn message_row(
        &self,
        msgs: &mut Vec<Message>,
        timescale: Option<&TimeScale>,
        mut row: egui_extras::TableRow<'_, '_>,
        record: &CommandOutputLine,
    ) {
        let stream_color = match record.stream {
            CommandOutputStream::Log => Color32::LIGHT_GRAY,
            CommandOutputStream::Command => Color32::LIGHT_BLUE,
            CommandOutputStream::Stdout => Color32::LIGHT_GREEN,
            CommandOutputStream::Stderr => Color32::LIGHT_RED,
        };
        let level_color = match record.level {
            LogLevel::Info => self.user.config.theme.log_colors.info,
            LogLevel::Warning => self.user.config.theme.log_colors.warn,
            LogLevel::Error => self.user.config.theme.log_colors.error,
            LogLevel::Failure => self.user.config.theme.log_colors.failure,
        };
        row.col(|ui| {
            ui.label(
                RichText::new(record.source.clone())
                    .color(Color32::GRAY)
                    .monospace(),
            );
        });
        row.col(|ui| {
            ui.label(
                RichText::new(record.stream.to_string())
                    .color(stream_color)
                    .monospace(),
            );
        });
        row.col(|ui| {
            ui.label(
                RichText::new(record.level.to_string())
                    .color(level_color)
                    .monospace(),
            );
        });
        row.col(|ui| format_message(msgs, timescale, &record.message, ui));
    }
}

/// Helper function to format a message with clickable timestamps.
///
/// It finds all time spans in the message and makes them clickable.
/// When clicked, it sends a message to set the cursor to that time.
fn format_message(
    msgs: &mut Vec<Message>,
    timescale: Option<&TimeScale>,
    message: &str,
    ui: &mut egui::Ui,
) {
    let spans = find_time_spans(message);
    if spans.is_empty() {
        ui.label(RichText::new(message).monospace());
        return;
    }
    ui.horizontal(|ui| {
        let mut last = 0usize;
        for (start, end) in spans {
            if start > last {
                ui.label(RichText::new(&message[last..start]).monospace());
            }

            let time_text = &message[start..end];
            let response = ui.link(RichText::new(time_text).monospace());
            if response.clicked()
                && let Some(timescale) = timescale
                && let Some(time_tick) = parse_clickable_time_to_ticks(time_text, timescale)
            {
                msgs.push(Message::Batch(vec![
                    Message::CursorSet(time_tick),
                    Message::GoToCursorIfNotInView,
                ]));
            }

            last = end;
        }

        if last < message.len() {
            ui.label(RichText::new(&message[last..]).monospace());
        }
    });
}

#[cfg(test)]
mod tests {
    use super::find_time_spans;

    #[test]
    fn finds_time_with_and_without_space() {
        let message = "at 980ns and 1.5 ms";
        let spans = find_time_spans(message);

        assert_eq!(spans.len(), 2);
        assert_eq!(&message[spans[0].0..spans[0].1], "980ns");
        assert_eq!(&message[spans[1].0..spans[1].1], "1.5 ms");
    }

    #[test]
    fn finds_time_inside_delta_cycle_notation() {
        let message = "failure at 980ns+0";
        let spans = find_time_spans(message);

        assert_eq!(spans.len(), 1);
        assert_eq!(&message[spans[0].0..spans[0].1], "980ns+0");
    }
}
