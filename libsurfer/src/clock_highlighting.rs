//! Drawing and handling of clock highlighting.
use derive_more::{Display, FromStr};
use egui::{Shape, Ui};
use emath::{Pos2, Rect};
use enum_iterator::Sequence;
use epaint::{CornerRadius, Stroke};
use serde::{Deserialize, Serialize};

use crate::{config::SurferConfig, message::Message, view::DrawingContext};

const CLOCK_LINE_BASE_UNIT: f32 = 30.0;

#[derive(PartialEq, Copy, Clone, Debug, Deserialize, Display, FromStr, Sequence, Serialize)]
pub enum ClockHighlightType {
    /// Draw a line at every posedge of the clocks
    Line,

    /// Highlight every other cycle
    Cycle,

    /// No highlighting
    None,
}

pub(crate) fn draw_clock_edge_marks(
    clock_edges_by_clock: &[Vec<f32>],
    ctx: &mut DrawingContext,
    config: &SurferConfig,
    clock_highlight_type: ClockHighlightType,
) {
    match clock_highlight_type {
        ClockHighlightType::Line => {
            let y_start = (ctx.to_screen)(0., 0.).y;
            let y_end = y_start + ctx.cfg.canvas_size.y;
            let stroke_width = config.theme.clock_highlight_line.width;
            let fallback_color = config.theme.clock_highlight_line.color;
            let color_list = &config.theme.clock_highlight_line_colors;
            let num_clocks = clock_edges_by_clock.len();
            let draw_dashed = num_clocks > 1;
            let dash_length = CLOCK_LINE_BASE_UNIT / num_clocks.max(1) as f32;
            let gap_length = CLOCK_LINE_BASE_UNIT - dash_length;

            for (clock_idx, clock_edges) in clock_edges_by_clock.iter().enumerate() {
                let stroke_color = color_list
                    .get(clock_idx % color_list.len().max(1))
                    .copied()
                    .unwrap_or(fallback_color);
                let stroke = Stroke::new(stroke_width, stroke_color);
                let offset = clock_idx as f32 * dash_length;

                for x in clock_edges {
                    let x_pos = (ctx.to_screen)(*x, 0.).x;
                    if draw_dashed {
                        ctx.painter.add(Shape::dashed_line_with_offset(
                            &[Pos2::new(x_pos, y_start), Pos2::new(x_pos, y_end)],
                            stroke,
                            &[dash_length],
                            &[gap_length],
                            offset,
                        ));
                    } else {
                        ctx.painter.vline(x_pos, y_start..=y_end, stroke);
                    }
                }
            }
        }
        ClockHighlightType::Cycle => {
            // Process clock edges in pairs: every other cycle gets highlighted
            let fallback_fill_color = config.theme.clock_highlight_cycle;
            let color_list = &config.theme.clock_highlight_cycle_colors;
            let use_transparent_fill = clock_edges_by_clock.len() > 1;

            for (clock_idx, clock_edges) in clock_edges_by_clock.iter().enumerate() {
                let fill_color = color_list
                    .get(clock_idx % color_list.len().max(1))
                    .copied()
                    .unwrap_or(fallback_fill_color);
                let fill_color = if use_transparent_fill {
                    egui::Color32::from_rgba_unmultiplied(
                        fill_color.r(),
                        fill_color.g(),
                        fill_color.b(),
                        128,
                    )
                } else {
                    fill_color
                };

                for chunk in clock_edges.chunks(2) {
                    if let [x_start, x_end] = chunk {
                        let Pos2 {
                            x: x_end_screen,
                            y: y_start,
                        } = (ctx.to_screen)(*x_end, 0.);
                        ctx.painter.rect_filled(
                            Rect {
                                min: (ctx.to_screen)(*x_start, 0.),
                                max: Pos2 {
                                    x: x_end_screen,
                                    y: ctx.cfg.canvas_size.y + y_start,
                                },
                            },
                            CornerRadius::ZERO,
                            fill_color,
                        );
                    }
                }
            }
        }
        ClockHighlightType::None => (),
    }
}

pub(crate) fn clock_highlight_type_menu(
    ui: &mut Ui,
    msgs: &mut Vec<Message>,
    clock_highlight_type: ClockHighlightType,
) {
    for highlight_type in enum_iterator::all::<ClockHighlightType>() {
        if ui
            .radio(
                highlight_type == clock_highlight_type,
                highlight_type.to_string(),
            )
            .clicked()
        {
            msgs.push(Message::SetClockHighlightType(highlight_type));
        }
    }
}
