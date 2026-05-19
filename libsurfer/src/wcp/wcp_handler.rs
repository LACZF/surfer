use crate::{
    SystemState, WcpClientCapabilities,
    displayed_item::{DisplayedItem, DisplayedItemRef},
    message::Message,
    wave_container::{ScopeRefExt, VariableRef, VariableRefExt},
    wave_data::WaveData,
    wave_source::{LoadOptions, WaveSource, string_to_wavesource},
};

use futures::executor::block_on;
use itertools::Itertools;
use std::sync::atomic::Ordering;
use surfer_translation_types::ScopeRef;
use tracing::{trace, warn};

use surfer_wcp::{
    ItemInfo, ItemStyle, MarkerInfo, WcpCSMessage, WcpCommand, WcpResponse, WcpSCMessage,
};

impl SystemState {
    pub fn handle_wcp_commands(&mut self) {
        let Some(receiver) = &mut self.channels.wcp_c2s_receiver else {
            return;
        };

        let mut messages = vec![];
        loop {
            match receiver.try_recv() {
                Ok(command) => {
                    messages.push(command);
                }
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                    trace!("WCP Command sender disconnected");
                    break;
                }
            }
        }
        for message in messages {
            self.handle_wcp_cs_message(&message);
        }
    }

    fn handle_wcp_cs_message(&mut self, message: &WcpCSMessage) {
        if !self.wcp_greeted_signal.load(Ordering::Relaxed) {
            if let WcpCSMessage::greeting { .. } = message {
            } else {
                self.send_error("WCP server has not received greeting messages", vec![], "");
                return;
            }
        }
        match message {
            WcpCSMessage::command(command) => {
                match command {
                    WcpCommand::get_item_list => {
                        if let Some(waves) = &self.user.waves {
                            let ids: Vec<surfer_wcp::DisplayedItemRef> = self
                                .get_displayed_items(waves)
                                .iter()
                                .map(std::convert::Into::into)
                                .collect_vec();
                            self.send_response(WcpResponse::get_item_list { ids });
                        } else {
                            self.send_error("No waveform loaded", vec![], "No waveform loaded");
                        }
                    }
                    WcpCommand::get_item_info { ids } => {
                        let Some(waves) = &self.user.waves else {
                            self.send_error("get_item_info", vec![], "No waveform loaded");
                            return;
                        };
                        let query_ids: Vec<surfer_wcp::DisplayedItemRef> = if ids.is_empty() {
                            waves
                                .items_tree
                                .iter_visible()
                                .map(|node| node.item_ref.into())
                                .collect_vec()
                        } else {
                            ids.clone()
                        };
                        let mut results: Vec<ItemInfo> = Vec::new();
                        for id in &query_ids {
                            let item_ref: DisplayedItemRef = id.into();
                            if let Some(item) = waves.displayed_items.get(&item_ref) {
                                results.push(item_info_from(item, *id));
                            } else {
                                self.send_error(
                                    "get_item_info",
                                    vec![],
                                    &format!("No item with id {id:?}"),
                                );
                                return;
                            }
                        }
                        self.send_response(WcpResponse::get_item_info { results });
                    }
                    WcpCommand::add_variables { variables } => {
                        if self.user.waves.is_some() {
                            self.save_current_canvas(format!("Add {} variables", variables.len()));
                        }
                        if let Some(waves) = self.user.waves.as_mut() {
                            let variable_refs = variables
                                .iter()
                                .map(|n| VariableRef::from_hierarchy_string(n))
                                .collect_vec();
                            let (cmd, ids) = waves.add_variables(
                                &self.translators,
                                variable_refs,
                                None,
                                true,
                                false,
                                None,
                            );
                            if let Some(cmd) = cmd {
                                self.load_variables(cmd);
                            }
                            self.send_response(WcpResponse::add_variables {
                                ids: ids.into_iter().map(std::convert::Into::into).collect_vec(),
                            });
                            self.invalidate_draw_commands();
                        } else {
                            self.send_error(
                                "add_variables",
                                vec![],
                                "Can't add signals. No waveform loaded",
                            );
                        }
                    }
                    WcpCommand::add_scope { scope, recursive } => {
                        if self.user.waves.is_some() {
                            self.save_current_canvas(format!("Add scope {scope}"));
                        }
                        let scope = ScopeRef::from_hierarchy_string(scope);
                        let variables = self.get_scope(scope, *recursive);
                        if let Some(waves) = self.user.waves.as_mut() {
                            let (cmd, ids) = waves.add_variables(
                                &self.translators,
                                variables,
                                None,
                                true,
                                false,
                                None,
                            );
                            if let Some(cmd) = cmd {
                                self.load_variables(cmd);
                            }
                            self.send_response(WcpResponse::add_scope {
                                ids: ids.into_iter().map(std::convert::Into::into).collect_vec(),
                            });
                            self.invalidate_draw_commands();
                        } else {
                            self.send_error("scope_add", vec![], "No waveform loaded");
                        }
                    }
                    WcpCommand::add_items { items, recursive } => {
                        if self.user.waves.is_some() {
                            self.save_current_canvas(format!("Add {} items", items.len()));
                        }

                        let mut variables: Vec<VariableRef> = Vec::new();
                        let mut style_by_path: Vec<(String, Option<ItemStyle>)> = Vec::new();
                        for add_item in items {
                            let path = add_item.path();
                            let style = add_item.style().cloned();
                            let variable_ref = VariableRef::from_hierarchy_string(path);
                            let scope = ScopeRef::from_hierarchy_string(path);
                            let scope_variables = self.get_scope(scope, *recursive);
                            style_by_path.push((path.to_owned(), style));
                            variables.push(variable_ref);
                            variables.extend(scope_variables);
                        }

                        if let Some(waves) = self.user.waves.as_mut() {
                            let (cmd, ids) = waves.add_variables(
                                &self.translators,
                                variables,
                                None,
                                true,
                                true,
                                None,
                            );
                            if let Some(cmd) = cmd {
                                self.load_variables(cmd);
                            }
                            let waves = self.user.waves.as_mut().unwrap();
                            for (item_ref, (_path, style)) in
                                ids.iter().zip(style_by_path.iter().cycle())
                            {
                                if let Some(style) = style {
                                    apply_style(waves, *item_ref, style);
                                }
                            }
                            self.send_response(WcpResponse::add_items {
                                ids: ids.into_iter().map(std::convert::Into::into).collect_vec(),
                            });
                            self.invalidate_draw_commands();
                        } else {
                            self.send_error(
                                "add_items",
                                vec![],
                                "Can't add items. No waveform loaded",
                            );
                        }
                    }
                    WcpCommand::add_markers { markers } => {
                        if self.user.waves.is_some() {
                            self.save_current_canvas(format!("Add {} markers", markers.len()));
                        }
                        if let Some(waves) = self.user.waves.as_mut() {
                            let mut ids = vec![];
                            for marker in markers {
                                let MarkerInfo {
                                    time,
                                    name,
                                    move_focus,
                                } = marker;
                                if let Some(id) = waves.add_marker(time, name.clone(), *move_focus)
                                {
                                    ids.push(id.into());
                                } else {
                                    self.send_error("add_markers", vec![], "Cannot add marker");
                                    return;
                                }
                            }
                            self.send_response(WcpResponse::add_markers { ids });
                        } else {
                            self.send_error("add_markers", vec![], "No waveform loaded");
                        }
                    }
                    WcpCommand::reload => {
                        self.update(Message::ReloadWaveform(false));
                        self.send_response(WcpResponse::ack);
                    }
                    WcpCommand::set_viewport_to { timestamp } => {
                        self.update(Message::GoToTime(Some(timestamp.clone()), 0));
                        self.send_response(WcpResponse::ack);
                    }
                    WcpCommand::set_viewport_range { start, end } => {
                        self.update(Message::ZoomToRange {
                            start: start.clone(),
                            end: end.clone(),
                            viewport_idx: 0,
                        });
                        self.send_response(WcpResponse::ack);
                    }
                    WcpCommand::set_item_color { id, color } => {
                        let Some(waves) = self.user.waves.as_mut() else {
                            self.send_error("set_item_color", vec![], "No waveform loaded");
                            return;
                        };
                        let item_ref: DisplayedItemRef = id.into();
                        if let Some(item) = waves.displayed_items.get_mut(&item_ref) {
                            item.set_color(&Some(color.clone()));
                            self.invalidate_draw_commands();
                            self.send_response(WcpResponse::ack);
                        } else {
                            self.send_error(
                                "set_item_color",
                                vec![],
                                &format!("Item {id:?} not found"),
                            );
                        }
                    }
                    WcpCommand::remove_items { ids } => {
                        let Some(_) = self.user.waves.as_mut() else {
                            self.send_error("remove_items", vec![], "No waveform loaded");
                            return;
                        };
                        let msgs = vec![Message::RemoveItems(
                            ids.iter().map(std::convert::Into::into).collect(),
                        )];
                        self.update(Message::Batch(msgs));

                        self.send_response(WcpResponse::ack);
                    }
                    WcpCommand::focus_item { id } => {
                        let Some(waves) = &self.user.waves else {
                            self.send_error("remove_items", vec![], "No waveform loaded");
                            return;
                        };
                        // TODO: Create a `.into` function here instead of unwrapping and wrapping
                        // it to prevent future type errors
                        if let Some(vidx) = waves.get_displayed_item_index(&id.into()) {
                            self.update(Message::FocusItem(vidx));

                            self.send_response(WcpResponse::ack);
                        } else {
                            self.send_error(
                                "focus_item",
                                vec![],
                                format!("No item with ID {id:?}").as_str(),
                            );
                        }
                    }
                    WcpCommand::clear => {
                        if let Some(wave) = &self.user.waves {
                            self.update(Message::RemoveItems(self.get_displayed_items(wave)));
                        }

                        self.send_response(WcpResponse::ack);
                    }
                    WcpCommand::load { source } => {
                        match string_to_wavesource(source) {
                            WaveSource::Url(url) => {
                                self.update(Message::LoadWaveformFileFromUrl(
                                    url,
                                    LoadOptions::Clear,
                                ));
                                self.send_response(WcpResponse::ack);
                            }
                            WaveSource::File(file) => {
                                // FIXME add support for loading transaction files via Message::LoadTransactionFile
                                let msg = Message::LoadFile(file, LoadOptions::Clear);
                                self.update(msg);
                                self.send_response(WcpResponse::ack);
                            }
                            _ => {
                                self.send_error(
                                    "load",
                                    vec![],
                                    format!("{source} is not legal wave source").as_str(),
                                );
                            }
                        }
                    }
                    WcpCommand::zoom_to_fit { viewport_idx } => {
                        self.update(Message::ZoomToFit {
                            viewport_idx: *viewport_idx,
                        });
                        self.send_response(WcpResponse::ack);
                    }
                    WcpCommand::set_cursor { timestamp } => {
                        self.update(Message::CursorSet(timestamp.to_owned()));
                        self.send_response(WcpResponse::ack);
                    }
                    WcpCommand::shutdown => {
                        warn!("WCP Shutdown message should not reach this place");
                    }

                    WcpCommand::set_item_background_color { id, color } => {
                        let Some(waves) = self.user.waves.as_mut() else {
                            self.send_error(
                                "set_item_background_color",
                                vec![],
                                "No waveform loaded",
                            );
                            return;
                        };
                        let item_ref: DisplayedItemRef = id.into();
                        if let Some(item) = waves.displayed_items.get_mut(&item_ref) {
                            item.set_background_color(&Some(color.clone()));
                            self.invalidate_draw_commands();
                            self.send_response(WcpResponse::ack);
                        } else {
                            self.send_error(
                                "set_item_background_color",
                                vec![],
                                &format!("Item {id:?} not found"),
                            );
                        }
                    }
                    WcpCommand::set_item_format { id, format } => {
                        let Some(waves) = self.user.waves.as_mut() else {
                            self.send_error("set_item_format", vec![], "No waveform loaded");
                            return;
                        };
                        let item_ref: DisplayedItemRef = id.into();
                        if let Some(DisplayedItem::Variable(var)) =
                            waves.displayed_items.get_mut(&item_ref)
                        {
                            var.format = Some(format.clone());
                            self.invalidate_draw_commands();
                            self.send_response(WcpResponse::ack);
                        } else if waves.displayed_items.contains_key(&item_ref) {
                            self.send_error(
                                "set_item_format",
                                vec![],
                                &format!("Item {id:?} is not a variable"),
                            );
                        } else {
                            self.send_error(
                                "set_item_format",
                                vec![],
                                &format!("Item {id:?} not found"),
                            );
                        }
                    }
                    WcpCommand::set_item_name { id, name } => {
                        let Some(waves) = self.user.waves.as_mut() else {
                            self.send_error("set_item_name", vec![], "No waveform loaded");
                            return;
                        };
                        let item_ref: DisplayedItemRef = id.into();
                        if let Some(item) = waves.displayed_items.get_mut(&item_ref) {
                            item.set_name(Some(name.clone()));
                            self.invalidate_draw_commands();
                            self.send_response(WcpResponse::ack);
                        } else {
                            self.send_error(
                                "set_item_name",
                                vec![],
                                &format!("Item {id:?} not found"),
                            );
                        }
                    }
                    WcpCommand::add_scope_as_group { scope, recursive } => {
                        if self.user.waves.is_none() {
                            self.send_error("add_scope_as_group", vec![], "No waveform loaded");
                            return;
                        }
                        self.save_current_canvas(format!("Add scope as group {scope}"));
                        let scope_ref = ScopeRef::from_hierarchy_string(scope);
                        let waves = self.user.waves.as_ref().unwrap();
                        let pos = waves.end_insert_position();
                        self.add_scope_as_group(&scope_ref, pos, *recursive, None);

                        let waves = self.user.waves.as_ref().unwrap();
                        let ids: Vec<surfer_wcp::DisplayedItemRef> = waves
                            .items_tree
                            .iter()
                            .skip(pos.before.0)
                            .map(|node| node.item_ref.into())
                            .collect_vec();
                        self.invalidate_draw_commands();
                        self.send_response(WcpResponse::add_items { ids });
                    }
                    WcpCommand::add_divider { name } => {
                        if self.user.waves.is_none() {
                            self.send_error("add_divider", vec![], "No waveform loaded");
                            return;
                        }
                        self.save_current_canvas("Add divider".to_owned());
                        let waves = self.user.waves.as_mut().unwrap();
                        waves.add_divider(name.clone(), None);
                        let id: surfer_wcp::DisplayedItemRef = waves
                            .items_tree
                            .iter()
                            .last()
                            .map(|node| node.item_ref.into())
                            .unwrap();
                        self.invalidate_draw_commands();
                        self.send_response(WcpResponse::add_items { ids: vec![id] });
                    }
                    WcpCommand::group_items { ids, name } => {
                        let Some(waves) = self.user.waves.as_mut() else {
                            self.send_error("group_items", vec![], "No waveform loaded");
                            return;
                        };
                        let item_refs: Vec<DisplayedItemRef> =
                            ids.iter().map(std::convert::Into::into).collect();
                        for item_ref in &item_refs {
                            if !waves.displayed_items.contains_key(item_ref) {
                                self.send_error(
                                    "group_items",
                                    vec![],
                                    &format!("Item {item_ref:?} not found"),
                                );
                                return;
                            }
                        }
                        let group_name = name.clone().unwrap_or_else(|| "Group".to_string());
                        self.update(Message::GroupNew {
                            name: Some(group_name),
                            before: None,
                            items: Some(item_refs),
                        });
                        let waves = self.user.waves.as_ref().unwrap();
                        let group_id: surfer_wcp::DisplayedItemRef = waves
                            .items_tree
                            .iter()
                            .last()
                            .map(|node| node.item_ref.into())
                            .unwrap();
                        self.send_response(WcpResponse::add_items {
                            ids: vec![group_id],
                        });
                    }
                    WcpCommand::dissolve_group { id } => {
                        let Some(waves) = &self.user.waves else {
                            self.send_error("dissolve_group", vec![], "No waveform loaded");
                            return;
                        };
                        let item_ref: DisplayedItemRef = id.into();
                        if !waves.displayed_items.contains_key(&item_ref) {
                            self.send_error(
                                "dissolve_group",
                                vec![],
                                &format!("Item {id:?} not found"),
                            );
                            return;
                        }
                        self.update(Message::GroupDissolve(Some(item_ref)));
                        self.send_response(WcpResponse::ack);
                    }
                    WcpCommand::get_variables { scope } => {
                        let Some(waves) = &self.user.waves else {
                            self.send_error("get_variables", vec![], "No waveform loaded");
                            return;
                        };
                        let paths = if let Some(scope_str) = scope {
                            let Some(container) = waves.inner.as_waves() else {
                                self.send_error(
                                    "get_variables",
                                    vec![],
                                    "Not a waveform container",
                                );
                                return;
                            };
                            let scope_ref = ScopeRef::from_hierarchy_string(scope_str);
                            container
                                .variables_in_scope(&scope_ref)
                                .iter()
                                .map(|v| v.full_path_string())
                                .collect_vec()
                        } else {
                            waves.inner.variable_names()
                        };
                        self.send_response(WcpResponse::get_variables { paths });
                    }
                    WcpCommand::get_scopes { scope } => {
                        let Some(waves) = &self.user.waves else {
                            self.send_error("get_scopes", vec![], "No waveform loaded");
                            return;
                        };
                        let paths = if let Some(scope_str) = scope {
                            let scope_ref = ScopeRef::from_hierarchy_string(scope_str);
                            waves
                                .inner
                                .as_waves()
                                .and_then(|w| w.child_scopes(&scope_ref).ok())
                                .unwrap_or_default()
                                .iter()
                                .map(|s| s.full_name())
                                .collect_vec()
                        } else {
                            waves.inner.scope_names()
                        };
                        self.send_response(WcpResponse::get_scopes { paths });
                    }
                    WcpCommand::get_translators => {
                        let names = self
                            .translators
                            .all_translator_names()
                            .into_iter()
                            .map(str::to_owned)
                            .collect_vec();
                        self.send_response(WcpResponse::get_translators { names });
                    }
                    WcpCommand::scroll_to_start { viewport_idx } => {
                        self.update(Message::GoToStart {
                            viewport_idx: *viewport_idx,
                        });
                        self.send_response(WcpResponse::ack);
                    }
                    WcpCommand::scroll_to_end { viewport_idx } => {
                        self.update(Message::GoToEnd {
                            viewport_idx: *viewport_idx,
                        });
                        self.send_response(WcpResponse::ack);
                    }
                    WcpCommand::undo => {
                        self.update(Message::Undo(1));
                        self.send_response(WcpResponse::ack);
                    }
                    WcpCommand::redo => {
                        self.update(Message::Redo(1));
                        self.send_response(WcpResponse::ack);
                    }
                    WcpCommand::batch { commands } => {
                        self.save_current_canvas("Batch operation".to_owned());
                        for cmd in commands {
                            self.handle_wcp_cs_message(&WcpCSMessage::command(cmd.clone()));
                        }
                    }
                }
            }
            WcpCSMessage::greeting { version, commands } => {
                if version == "0" {
                    self.wcp_client_capabilities = WcpClientCapabilities::new();
                    if commands.iter().any(|s| s == "waveforms_loaded") {
                        self.wcp_client_capabilities.waveforms_loaded = true;
                    }
                    if commands.iter().any(|s| s == "goto_declaration") {
                        self.wcp_client_capabilities.goto_declaration = true;
                    }
                    if commands.iter().any(|s| s == "add_drivers") {
                        self.wcp_client_capabilities.add_drivers = true;
                    }
                    if commands.iter().any(|s| s == "add_loads") {
                        self.wcp_client_capabilities.add_loads = true;
                    }
                    self.wcp_greeted_signal.store(true, Ordering::Relaxed);
                    self.wcp_greeted_signal.store(true, Ordering::Relaxed);
                    self.send_greeting();
                } else {
                    self.send_error(
                        "greeting",
                        vec![],
                        &format!("Surfer only supports WCP version 0, client requested {version}"),
                    );
                }
            }
        }
    }

    fn send_greeting(&self) {
        let commands = vec![
            "add_variables",
            "set_viewport_to",
            "cursor_set",
            "reload",
            "add_scope",
            "add_items",
            "get_item_list",
            "set_item_color",
            "get_item_info",
            "clear_item",
            "focus_item",
            "clear",
            "load",
            "zoom_to_fit",
            "add_markers",
            "set_viewport_range_to",
            "set_item_background_color",
            "set_item_format",
            "set_item_name",
            "add_scope_as_group",
            "add_divider",
            "group_items",
            "dissolve_group",
            "get_variables",
            "get_scopes",
            "get_translators",
            "scroll_to_start",
            "scroll_to_end",
            "undo",
            "redo",
            "batch",
        ]
        .into_iter()
        .map(str::to_string)
        .collect_vec();

        let greeting = WcpSCMessage::create_greeting(0, commands);

        self.channels
            .wcp_s2c_sender
            .as_ref()
            .map(|ch| block_on(ch.send(greeting)));
    }

    fn send_response(&self, result: WcpResponse) {
        self.channels
            .wcp_s2c_sender
            .as_ref()
            .map(|ch| block_on(ch.send(WcpSCMessage::response(result))));
    }

    fn send_error(&self, error: &str, arguments: Vec<String>, message: &str) {
        self.channels.wcp_s2c_sender.as_ref().map(|ch| {
            block_on(ch.send(WcpSCMessage::create_error(
                error.to_string(),
                arguments,
                message.to_string(),
            )))
        });
    }

    fn get_displayed_items(&self, waves: &WaveData) -> Vec<DisplayedItemRef> {
        // TODO check call sites since visible items may now differ from loaded items
        waves
            .items_tree
            .iter_visible()
            .map(|node| node.item_ref)
            .collect_vec()
    }
}

fn item_info_from(item: &DisplayedItem, id: surfer_wcp::DisplayedItemRef) -> ItemInfo {
    let (name, item_type) = match item {
        DisplayedItem::Variable(var) => (
            var.manual_name
                .clone()
                .unwrap_or_else(|| var.display_name.clone()),
            "Variable",
        ),
        DisplayedItem::Divider(d) => (
            d.name
                .clone()
                .unwrap_or_else(|| "Name not found!".to_string()),
            "Divider",
        ),
        DisplayedItem::Marker(m) => (
            m.name
                .clone()
                .unwrap_or_else(|| "Name not found!".to_string()),
            "Marker",
        ),
        DisplayedItem::TimeLine(t) => (
            t.name
                .clone()
                .unwrap_or_else(|| "Name not found!".to_string()),
            "TimeLine",
        ),
        DisplayedItem::Placeholder(p) => (
            p.manual_name
                .clone()
                .unwrap_or_else(|| "Name not found!".to_string()),
            "Placeholder",
        ),
        DisplayedItem::Stream(s) => (
            s.manual_name
                .clone()
                .unwrap_or_else(|| s.display_name.clone()),
            "Stream",
        ),
        DisplayedItem::Group(g) => (g.name.clone(), "Group"),
    };
    ItemInfo {
        name,
        t: item_type.to_string(),
        id,
    }
}

fn apply_style(waves: &mut WaveData, item_ref: DisplayedItemRef, style: &ItemStyle) {
    if let Some(item) = waves.displayed_items.get_mut(&item_ref) {
        if style.color.is_some() {
            item.set_color(&style.color);
        }
        if style.background_color.is_some() {
            item.set_background_color(&style.background_color);
        }
        if let Some(name) = &style.name {
            item.set_name(Some(name.clone()));
        }
        if let Some(format) = &style.format
            && let DisplayedItem::Variable(var) = item
        {
            var.format = Some(format.clone());
        }
    }
}
