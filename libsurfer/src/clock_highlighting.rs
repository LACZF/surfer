//! Drawing and handling of clock highlighting.
use derive_more::{Display, FromStr};
use egui::{Shape, Ui};
use emath::{Pos2, Rect};
use enum_iterator::Sequence;
use epaint::{CornerRadius, Stroke};
use serde::{Deserialize, Serialize};

use crate::{config::SurferConfig, message::Message, view::DrawingContext};

const CLOCK_LINE_BASE_UNIT: f32 = 30.0;

pub(crate) enum ClockHighlightData {
    Line(Vec<(f32, Vec<usize>)>),
    Cycle(Vec<Vec<f32>>),
    None,
}

impl ClockHighlightData {
    pub(crate) fn has_edges(&self) -> bool {
        match self {
            Self::Line(clock_edges) => !clock_edges.is_empty(),
            Self::Cycle(clock_edges_by_clock) => {
                clock_edges_by_clock.iter().any(|edges| !edges.is_empty())
            }
            Self::None => false,
        }
    }
}

fn active_clock_count(clock_edges: &[(f32, Vec<usize>)]) -> usize {
    let max_clock_idx = clock_edges
        .iter()
        .flat_map(|(_, clock_indices)| clock_indices.iter().copied())
        .max()
        .map_or(0, |max_idx| max_idx + 1);
    let mut active = vec![false; max_clock_idx];
    for (_, clock_indices) in clock_edges {
        for &clock_idx in clock_indices {
            active[clock_idx] = true;
        }
    }
    active.into_iter().filter(|is_active| *is_active).count()
}

fn clock_highlight_color(
    clock_idx: usize,
    single_active_clock: bool,
    fallback_color: egui::Color32,
    color_list: &[egui::Color32],
) -> egui::Color32 {
    if single_active_clock {
        fallback_color
    } else {
        match clock_idx % (color_list.len() + 1) {
            0 => fallback_color,
            color_idx => color_list[color_idx - 1],
        }
    }
}

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
    clock_edges: &ClockHighlightData,
    ctx: &mut DrawingContext,
    config: &SurferConfig,
) {
    match clock_edges {
        ClockHighlightData::Line(clock_edges) => {
            let y_start = (ctx.to_screen)(0., 0.).y;
            let y_end = y_start + ctx.cfg.canvas_size.y;
            let stroke_width = config.theme.clock_highlight_line.width;
            let fallback_color = config.theme.clock_highlight_line.color;
            let color_list = &config.theme.clock_highlight_line_colors;
            let single_active_clock = active_clock_count(clock_edges) <= 1;

            for (x, clock_indices) in clock_edges.iter() {
                let x_pos = (ctx.to_screen)(*x, 0.).x;
                let line = [Pos2::new(x_pos, y_start), Pos2::new(x_pos, y_end)];

                if clock_indices.len() == 1 {
                    let clock_idx = clock_indices[0];
                    let stroke_color = clock_highlight_color(
                        clock_idx,
                        single_active_clock,
                        fallback_color,
                        color_list,
                    );
                    let stroke = Stroke::new(stroke_width, stroke_color);
                    ctx.painter.vline(x_pos, y_start..=y_end, stroke);
                    continue;
                }

                let dash_length = CLOCK_LINE_BASE_UNIT / clock_indices.len() as f32;
                let gap_length = CLOCK_LINE_BASE_UNIT - dash_length;

                for (phase_idx, clock_idx) in clock_indices.iter().copied().enumerate() {
                    let stroke_color = clock_highlight_color(
                        clock_idx,
                        single_active_clock,
                        fallback_color,
                        color_list,
                    );
                    let stroke = Stroke::new(stroke_width, stroke_color);
                    let offset = phase_idx as f32 * dash_length;

                    ctx.painter.add(Shape::dashed_line_with_offset(
                        &line,
                        stroke,
                        &[dash_length],
                        &[gap_length],
                        offset,
                    ));
                }
            }
        }
        ClockHighlightData::Cycle(clock_edges_by_clock) => {
            // Process clock edges in pairs: every other cycle gets highlighted
            let fallback_fill_color = config.theme.clock_highlight_cycle;
            let color_list = &config.theme.clock_highlight_cycle_colors;
            let active_clocks = clock_edges_by_clock
                .iter()
                .filter(|edges| !edges.is_empty())
                .count();
            let single_active_clock = active_clocks <= 1;
            let use_transparent_fill = active_clocks > 1;

            for (clock_idx, clock_edges) in clock_edges_by_clock.iter().enumerate() {
                let fill_color = clock_highlight_color(
                    clock_idx,
                    single_active_clock,
                    fallback_fill_color,
                    color_list,
                );
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
        ClockHighlightData::None => (),
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
