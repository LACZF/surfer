use derive_more::Display;
use ecolor::Color32;
use egui::{Context, Frame, Id, Ui};
use egui_remixicon::icons;
use emath::{Pos2, Rect, RectTransform};
use epaint::Stroke;
use num::BigInt;
use serde::Deserialize;

use crate::{
    SystemState,
    arrow::{ArrowAnnotation, ArrowHeadMode, WavePoint},
    comment::{Comment, CommentMessage},
    config::SurferTheme,
    displayed_item::DisplayedItemRef,
    graphics::{Anchor, GraphicsY},
    message::Message,
    mousegestures::DragCommitMeta,
    rectangle::RectAnnotation,
    time::TimeFormatter,
    view::DrawingContext,
    viewport::Viewport,
    wave_data::WaveData,
};

const DEFAULT_HIDE_RADIUS: f32 = 5.0;

/// Annotation primitives that can be created via mouse dragging.
#[derive(Clone, PartialEq, Copy, Display, Debug, Deserialize)]
pub enum AnnotationKind {
    /// Create a rectangular annotation spanning a time range and wave range.
    Rectangle,
    /// Create an arrow annotation with a head at the end point.
    ArrowSingleHead,
    /// Create an arrow annotation with heads at both start and end.
    ArrowDoubleHead,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct AnnotationData {
    pub id: Id,
    pub visible: bool,
    pub name: String,
    pub stroke: Stroke,
    pub show_comments: bool,
    pub comment_box: Comment,
}

impl AnnotationData {
    pub(crate) fn new(id_source: impl std::hash::Hash, name: String, num: i32) -> Self {
        let id = Id::new(id_source);
        let c_id = Id::new(("comment_box", num));
        AnnotationData {
            id,
            visible: true,
            name,
            stroke: Stroke::new(2.0, Color32::from_rgb(255, 255, 255)),
            show_comments: false,
            comment_box: Comment::new(c_id, id),
        }
    }
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub enum Annotation {
    Arrow(ArrowAnnotation),
    Rect(RectAnnotation),
}
impl Annotatable for Annotation {
    fn get_id(&self) -> Id {
        match self {
            Annotation::Arrow(a) => a.get_id(),
            Annotation::Rect(r) => r.get_id(),
        }
    }

    fn get_type(&self) -> &str {
        match self {
            Annotation::Arrow(a) => a.get_type(),
            Annotation::Rect(r) => r.get_type(),
        }
    }

    fn set_name(&mut self, name: String) {
        match self {
            Annotation::Arrow(a) => a.set_name(name),
            Annotation::Rect(r) => r.set_name(name),
        }
    }

    fn get_name(&self) -> String {
        match self {
            Annotation::Arrow(a) => a.get_name(),
            Annotation::Rect(r) => r.get_name(),
        }
    }

    fn is_selected(&mut self) {
        match self {
            Annotation::Arrow(a) => a.is_selected(),
            Annotation::Rect(r) => r.is_selected(),
        }
    }

    fn set_visibility(&mut self, visible: bool) {
        match self {
            Annotation::Arrow(a) => a.set_visibility(visible),
            Annotation::Rect(r) => r.set_visibility(visible),
        }
    }

    fn show_comments(&self) -> bool {
        match self {
            Annotation::Arrow(a) => a.show_comments(),
            Annotation::Rect(r) => r.show_comments(),
        }
    }
    fn show_comment_box(&self) -> bool {
        match self {
            Annotation::Arrow(a) => a.show_comment_box(),
            Annotation::Rect(r) => r.show_comment_box(),
        }
    }

    fn set_show_comments(&mut self, show: bool) {
        match self {
            Annotation::Arrow(a) => a.set_show_comments(show),
            Annotation::Rect(r) => r.set_show_comments(show),
        }
    }

    fn get_comment_box(&self) -> Comment {
        match self {
            Annotation::Arrow(a) => a.get_comment_box(),
            Annotation::Rect(r) => r.get_comment_box(),
        }
    }

    fn get_comment_box_mut(&mut self) -> &mut Comment {
        match self {
            Annotation::Arrow(a) => a.get_comment_box_mut(),
            Annotation::Rect(r) => r.get_comment_box_mut(),
        }
    }

    fn is_visible(&self) -> bool {
        match self {
            Annotation::Arrow(a) => a.is_visible(),
            Annotation::Rect(r) => r.is_visible(),
        }
    }

    fn get_center_time(&self) -> BigInt {
        match self {
            Annotation::Arrow(a) => a.get_center_time(),
            Annotation::Rect(r) => r.get_center_time(),
        }
    }

    fn get_start_time(&self) -> BigInt {
        match self {
            Annotation::Arrow(a) => a.get_start_time(),
            Annotation::Rect(r) => r.get_start_time(),
        }
    }

    fn get_end_time(&self) -> BigInt {
        match self {
            Annotation::Arrow(a) => a.get_end_time(),
            Annotation::Rect(r) => r.get_end_time(),
        }
    }

    fn is_attached(&self, removed_ref: &DisplayedItemRef) -> bool {
        match self {
            Annotation::Arrow(a) => a.is_attached(removed_ref),
            Annotation::Rect(r) => r.is_attached(removed_ref),
        }
    }

    fn get_from_wave(&self) -> Option<GraphicsY> {
        match self {
            Annotation::Arrow(a) => a.get_from_wave(),
            Annotation::Rect(r) => r.get_from_wave(),
        }
    }

    fn get_to_wave(&self) -> Option<GraphicsY> {
        match self {
            Annotation::Arrow(a) => a.get_to_wave(),
            Annotation::Rect(r) => r.get_to_wave(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw(
        &self,
        ui: &mut Ui,
        waves: &WaveData,
        viewport_idx: usize,
        ctx: &mut DrawingContext,
        theme: &SurferTheme,
        msgs: &mut Vec<Message>,
        y_offset: f32,
        to_screen: RectTransform,
        time_formatter: &TimeFormatter,
    ) {
        match self {
            Annotation::Arrow(a) => a.draw(
                ui,
                waves,
                viewport_idx,
                ctx,
                theme,
                msgs,
                y_offset,
                to_screen,
                time_formatter,
            ),
            Annotation::Rect(r) => r.draw(
                ui,
                waves,
                viewport_idx,
                ctx,
                theme,
                msgs,
                y_offset,
                to_screen,
                time_formatter,
            ),
        }
    }

    fn get_comment_position(
        &self,
        viewport: &Viewport,
        ctx: &DrawingContext,
        waves: &WaveData,
        offset: f32,
    ) -> Pos2 {
        match self {
            Annotation::Arrow(a) => a.get_comment_position(viewport, ctx, waves, offset),
            Annotation::Rect(r) => r.get_comment_position(viewport, ctx, waves, offset),
        }
    }

    fn get_time_info(&self, time_formatter: &TimeFormatter) -> String {
        match self {
            Annotation::Arrow(a) => a.get_time_info(time_formatter),
            Annotation::Rect(r) => r.get_time_info(time_formatter),
        }
    }

    fn get_messages(&self) -> Vec<CommentMessage> {
        match self {
            Annotation::Arrow(a) => a.get_messages(),
            Annotation::Rect(r) => r.get_messages(),
        }
    }
}

pub trait Annotatable {
    fn get_id(&self) -> Id;
    fn get_type(&self) -> &str;
    fn set_name(&mut self, name: String);
    fn get_name(&self) -> String;
    fn is_selected(&mut self);
    fn set_visibility(&mut self, visible: bool);
    fn show_comments(&self) -> bool;
    fn show_comment_box(&self) -> bool;
    fn set_show_comments(&mut self, show: bool);
    fn get_comment_box(&self) -> Comment;
    fn get_comment_box_mut(&mut self) -> &mut Comment;
    fn get_messages(&self) -> Vec<CommentMessage>;
    fn is_visible(&self) -> bool;
    fn get_center_time(&self) -> BigInt;
    fn get_start_time(&self) -> BigInt;
    fn get_end_time(&self) -> BigInt;
    /// Checks whether the annotation is attached to the given Item.
    fn is_attached(&self, removed_ref: &DisplayedItemRef) -> bool;
    fn get_time_info(&self, time_formatter: &TimeFormatter) -> String;
    fn get_from_wave(&self) -> Option<GraphicsY>;
    fn get_to_wave(&self) -> Option<GraphicsY>;
    #[allow(clippy::too_many_arguments)]
    fn draw(
        &self,
        ui: &mut Ui,
        waves: &WaveData,
        viewport_idx: usize,
        ctx: &mut DrawingContext,
        theme: &SurferTheme,
        msgs: &mut Vec<Message>,
        y_offset: f32,
        to_screen: RectTransform,
        time_formatter: &TimeFormatter,
    );
    fn draw_quick_menu(
        &self,
        ui: &mut egui::Ui,
        msgs: &mut Vec<Message>,
        waves: &WaveData,
        viewport_rect: egui::Rect,
        position: Pos2,
    ) {
        let id: Id = self.get_id();

        let menu_rect = egui::Rect::from_min_size(position, egui::vec2(0.0, 0.0));

        if !viewport_rect.intersects(menu_rect) {
            return;
        }

        egui::Area::new(egui::Id::new(("annotation_quick_menu", id)))
            .order(egui::Order::Foreground)
            .fixed_pos(position)
            .show(ui.ctx(), |ui| {
                Frame::popup(ui.style())
                    .fill(ui.visuals().extreme_bg_color)
                    .stroke(Stroke::new(
                        1.0,
                        ui.visuals().widgets.noninteractive.bg_stroke.color,
                    ))
                    .corner_radius(8.0)
                    .inner_margin(egui::Margin::same(4))
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        ui.spacing_mut().button_padding = egui::vec2(4.0, 2.0);

                        ui.horizontal(|ui| {
                            if ui
                                .button(icons::SEARCH_LINE)
                                .on_hover_text("Go to annotation")
                                .clicked()
                            {
                                msgs.push(Message::GoToAnnotationPosition(
                                    id,
                                    waves.last_active_viewport_idx,
                                ));
                            }

                            let vis_icon = if self.is_visible() {
                                icons::EYE_LINE
                            } else {
                                icons::EYE_OFF_LINE
                            };

                            if ui
                                .button(vis_icon)
                                .on_hover_text("Toggle visibility")
                                .clicked()
                            {
                                msgs.push(Message::ToggleAnnotationVisiblility(id));
                            }

                            if ui
                                .button(icons::DELETE_BIN_LINE)
                                .on_hover_text("Delete annotation")
                                .clicked()
                            {
                                msgs.push(Message::RemoveAnnotation(id));
                            }

                            if self.is_visible() {
                                let comment = self.get_comment_box();

                                let chat_icon = if comment.visible {
                                    icons::CHAT_4_LINE
                                } else {
                                    icons::CHAT_OFF_LINE
                                };

                                if ui
                                    .button(chat_icon)
                                    .on_hover_text("Toggle comment visibility")
                                    .clicked()
                                {
                                    msgs.push(Message::ToggleCommentVisibility(id));
                                }
                            }
                        });
                    });
            });
    }

    fn draw_hover_info(
        &self,
        group_name: String,
        ui: &mut egui::Ui,
        (time_start_str, time_end_str): (&str, &str),
    ) {
        ui.label(format!("Start time: {time_start_str} "));
        ui.label(format!("End time:   {time_end_str} "));
        ui.painter().add(egui::Shape::line_segment(
            [ui.cursor().left_top(), ui.cursor().right_top()],
            egui::Stroke::new(0.2, egui::Color32::LIGHT_GRAY),
        ));
        ui.label(format!("Name: {}", self.get_name()));
        ui.label(format!("Group: {}", group_name));
        ui.label(format!("Type: {}", self.get_type()));
        ui.label(format!("ID: {:?}", self.get_id()));
    }
    fn hide_annotation(&self, ui: &mut egui::Ui, stroke: Stroke, center: Pos2) -> Rect {
        ui.painter()
            .circle_filled(center, DEFAULT_HIDE_RADIUS, stroke.color);

        egui::Rect::from_center_size(
            center,
            egui::vec2(DEFAULT_HIDE_RADIUS * 2.0, DEFAULT_HIDE_RADIUS * 2.0),
        )
    }
    fn get_comment_position(
        &self,
        viewport: &Viewport,
        ctx: &DrawingContext,
        waves: &WaveData,
        offset: f32,
    ) -> Pos2;

    fn draw_comment_box(
        &self,
        ui: &mut egui::Ui,
        viewport_idx: usize,
        msgs: &mut Vec<Message>,
        comment_position: Pos2,
    ) -> (Id, Comment) {
        let mut comment = self.get_comment_box();
        comment.id = Id::new((comment.id, viewport_idx));

        comment.name = self.get_name();

        // X-coordinate
        comment.rect.min.x = comment_position.x + comment.offset.x;
        comment.rect.max.x = comment_position.x + comment.offset.x + comment.size.x;

        // Y-coordinate
        comment.rect.min.y = comment_position.y + comment.offset.y;
        comment.rect.max.y = comment_position.y + comment.offset.y + comment.size.y;

        comment.anchor = comment_position;
        ui.add(&mut comment);
        // Handle "Enter" key to submit new comment
        if let Some(save_text) = &comment.save_text {
            msgs.push(Message::AddCommentMessage(
                comment.annotation_id,
                save_text.clone(),
                "user".to_string(),
            ));
        }

        (comment.annotation_id, comment)
    }

    fn update_comment_box(&mut self, comment: Comment) {
        let c = self.get_comment_box_mut();
        c.name = comment.name;
        c.new_text = comment.new_text;
        c.offset = comment.offset;
        c.size = comment.size;
        c.rect = comment.rect;
        c.visible = comment.visible;
    }
}

impl WaveData {
    pub fn delete_annotation(&mut self, id: egui::Id) {
        self.annotations
            .retain(|annotation| annotation.get_id() != id);
    }

    #[must_use]
    pub fn get_annotation_by_id(&self, id: &egui::Id) -> Option<&Annotation> {
        self.annotations.iter().find(|anno| anno.get_id() == *id)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn draw_annotations(
        &self,
        ui: &mut egui::Ui,
        viewport: &Viewport,
        viewport_idx: usize,
        ctx: &mut DrawingContext,
        theme: &SurferTheme,
        msgs: &mut Vec<Message>,
        y_offset: f32,
        viewport_rect: egui::Rect,
        to_screen: RectTransform,
        time_formatter: &TimeFormatter,
    ) {
        let mut comment_changes = Vec::new();

        for annotation in &self.annotations {
            annotation.draw(
                ui,
                self,
                viewport_idx,
                ctx,
                theme,
                msgs,
                y_offset,
                to_screen,
                time_formatter,
            );

            if self.selected_annotation == Some(annotation.get_id())
                && viewport_idx == self.last_active_viewport_idx
            {
                let mut menu_position = self.annotation_menu_pos.unwrap();
                let menu_time = self.annotation_menu_time.clone().unwrap();

                menu_position.x = viewport.pixel_from_time(
                    &menu_time,
                    ctx.cfg.canvas_size.x,
                    &self.safe_num_timestamps(),
                );
                let temp_y = menu_position.y;
                menu_position = (ctx.to_screen)(menu_position.x, menu_position.y);
                menu_position.y = temp_y;

                annotation.draw_quick_menu(ui, msgs, self, viewport_rect, menu_position);
            }
        }
        for annotation in &self.annotations {
            if annotation.show_comment_box() && annotation.is_visible() {
                let comment_position =
                    annotation.get_comment_position(viewport, ctx, self, y_offset);
                let (id, comment) =
                    annotation.draw_comment_box(ui, viewport_idx, msgs, comment_position);
                // Only update comment if change has been made or something is being written
                if comment.change || annotation.get_comment_box().new_text != comment.new_text {
                    comment_changes.push((id, comment));
                }
            }
        }
        if !comment_changes.is_empty() {
            msgs.push(Message::UpdateCommentBox(comment_changes));
        }
    }
}

impl SystemState {
    /// Clamp a canvas point to valid waveform Y bounds and optionally snap to visible item anchors.
    ///
    /// Snapping can be disabled by callers (for example while shift is held during
    /// annotation interactions).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn clamp_y(
        &self,
        pos: Pos2,
        max_y: f32,
        snap_y: bool,
        waves: &WaveData,
        ctx: &mut DrawingContext<'_>,
        anchor: Anchor,
        y_offset: f32,
    ) -> Pos2 {
        let mut y = pos.y.clamp(waves.get_content_start(ctx), max_y);
        if snap_y {
            let local_y = y - y_offset;

            if let Some(snapped_y) = waves
                .get_item_at_y(local_y)
                .and_then(|vidx| waves.items_tree.get_visible(vidx))
                .and_then(|node| {
                    let gy = GraphicsY {
                        item: node.item_ref,
                        anchor,
                    };

                    waves.get_item_y(&gy)
                })
            {
                y = snapped_y + y_offset;
            }
        }

        Pos2 {
            x: pos.x,
            y: y.min(max_y),
        }
    }

    pub(crate) fn draw_gesture_rectangle(
        &self,
        start_location: Pos2,
        waves: &WaveData,
        ui: &Context,
        current_location: Pos2,
        ctx: &mut DrawingContext,
        y_offset: f32,
    ) {
        let modifiers = ui.input(|i| i.modifiers);
        let max_y = waves.get_content_height(ctx);
        let current_anchor = {
            if current_location.y > start_location.y {
                Anchor::Bottom
            } else {
                Anchor::Top
            }
        };
        let start_anchor = {
            if start_location.y < current_location.y {
                Anchor::Top
            } else {
                Anchor::Bottom
            }
        };
        let end = self.clamp_y(
            current_location,
            max_y,
            !modifiers.shift,
            waves,
            ctx,
            current_anchor,
            y_offset,
        );
        let start = self.clamp_y(
            start_location,
            max_y,
            !modifiers.shift,
            waves,
            ctx,
            start_anchor,
            y_offset,
        );
        let color = self.user.config.theme.annotation_rectangle.color;
        let stroke = Stroke {
            color,
            width: self.user.config.theme.annotation_rectangle.width,
        };

        let start_pos = (ctx.to_screen)(start.x, start.y);
        let end_pos = (ctx.to_screen)(end.x, end.y);

        let temp_rect = emath::Rect::from_two_pos(start_pos, end_pos);

        ctx.painter
            .rect_stroke(temp_rect, 0.0, stroke, egui::StrokeKind::Middle);
    }

    /// Create and emit a rectangle annotation from drag start/end points.
    ///
    /// With shift pressed, vertical bounds are encoded as percentual anchors instead of
    /// strict top/bottom item anchors.
    pub(crate) fn create_rectangle(
        &self,
        end_location: Pos2,
        start_location: Pos2,
        msgs: &mut Vec<Message>,
        drag: &DragCommitMeta<'_>,
        ctx: &mut DrawingContext<'_>,
        ui: &Context,
    ) {
        let modifiers = ui.input(|i| i.modifiers);
        let max_y = drag.waves.get_content_height(ctx);

        let end_anchor = if end_location.y > start_location.y {
            Anchor::Bottom
        } else {
            Anchor::Top
        };

        let start_anchor = if start_location.y < end_location.y {
            Anchor::Top
        } else {
            Anchor::Bottom
        };

        let end = self.clamp_y(
            end_location,
            max_y,
            !modifiers.shift,
            drag.waves,
            ctx,
            end_anchor,
            drag.y_offset,
        );

        let start = self.clamp_y(
            start_location,
            max_y,
            !modifiers.shift,
            drag.waves,
            ctx,
            start_anchor,
            drag.y_offset,
        );

        let rect = emath::Rect::from_two_pos(start, end);

        let viewport = &drag.waves.viewports[drag.viewport_idx];

        let t1 = viewport.as_time_bigint(start_location.x, drag.frame_width, drag.num_timestamps);
        let t2 = viewport.as_time_bigint(end_location.x, drag.frame_width, drag.num_timestamps);

        let (time_start, time_end) = (t1.clone().min(t2.clone()), t1.max(t2));

        let get_anchored_y = |y: f32, anchor: Anchor| {
            drag.waves
                .get_item_at_y(y)
                .and_then(|vidx| drag.waves.items_tree.get_visible(vidx))
                .map(|node| GraphicsY {
                    item: node.item_ref,
                    anchor,
                })
        };

        let get_percentual_y = |lookup_y: f32, scale_y: f32| {
            drag.waves
                .get_item_at_y(lookup_y)
                .and_then(|vidx| drag.waves.items_tree.get_visible(vidx))
                .map(|node| {
                    let item = node.item_ref;
                    let p = drag.waves.get_item_y_scale(item, scale_y);

                    GraphicsY {
                        item,
                        anchor: Anchor::Percentual(p.unwrap_or(0.)),
                    }
                })
        };

        let (wave_from, wave_to) = if modifiers.shift {
            let from = get_percentual_y(
                start.y.min(end.y) - drag.y_offset,
                start.y.min(end.y) - drag.y_offset,
            );

            let to = get_percentual_y(
                end.y.max(start.y) - drag.y_offset - self.user.config.layout.waveforms_gap * 2.,
                end.y.max(start.y) - drag.y_offset,
            );

            (from, to)
        } else {
            let y_from = start.y.min(end.y);
            let y_to = start.y.max(end.y);

            let from = get_anchored_y(y_from - drag.y_offset, Anchor::Top);

            let mut adjusted_y = y_to - drag.y_offset;
            if y_to > drag.waves.get_content_start(ctx) {
                adjusted_y -= self.user.config.layout.waveforms_gap * 2.0;
            }

            let to = get_anchored_y(adjusted_y, Anchor::Bottom);

            (from, to)
        };

        msgs.push(Message::RectangleAdded {
            time_at_start: time_start,
            time_at_end: time_end,
            wave_from,
            wave_to,
            rect,
        });
    }

    /// Create and emit an arrow annotation from drag start/end points.
    ///
    /// The end timestamp may snap to signal edges when available.
    pub(crate) fn create_arrow(
        &self,
        end_location: Pos2,
        start_location: Pos2,
        msgs: &mut Vec<Message>,
        drag: &DragCommitMeta<'_>,
        ctx: &mut DrawingContext<'_>,
    ) {
        let start_pos = (ctx.to_screen)(start_location.x, start_location.y);
        let end_pos = (ctx.to_screen)(end_location.x, end_location.y);

        let time_from: BigInt = drag.waves.viewports[drag.viewport_idx].as_time_bigint(
            start_location.x,
            drag.frame_width,
            drag.num_timestamps,
        );

        let snap_pos = Some(Pos2::new(end_location.x, end_location.y - drag.y_offset));

        let time_to: BigInt = self
            .snap_to_edge(snap_pos, drag.waves, drag.frame_width, drag.viewport_idx)
            .unwrap_or_else(|| {
                drag.waves.viewports[drag.viewport_idx].as_time_bigint(
                    end_location.x,
                    drag.frame_width,
                    drag.num_timestamps,
                )
            });

        let attached_item_to = drag
            .waves
            .item_ref_at_canvas_y(end_location.y - drag.y_offset);
        let attached_item_from = drag
            .waves
            .item_ref_at_canvas_y(start_location.y - drag.y_offset);

        let mut head_mode = ArrowHeadMode::End;

        if self.annotation_kind == Some(AnnotationKind::ArrowDoubleHead) {
            head_mode = ArrowHeadMode::Double;
        }

        let wave_point_from = WavePoint {
            time: time_from.clone(),
            attached_item: attached_item_from,
            screen_pos: start_pos,
        };

        let wave_point_to = WavePoint {
            time: time_to.clone(),
            attached_item: attached_item_to,
            screen_pos: end_pos,
        };

        if attached_item_to.is_some() {
            msgs.push(Message::ArrowAdded {
                wave_point_from,
                wave_point_to,
                head_mode,
            });
        }
    }
}
