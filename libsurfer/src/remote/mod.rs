mod client;

use serde::{Deserialize, Serialize};

pub use client::{
    AppClient, ReloadError, ServerProfile, create_or_change_annotation, get_hierarchy_from_server,
    get_server_status, get_signals, get_time_table_from_server, parse_display_item,
    parse_signal_response_body, parse_signal_response_body_no_signals, protobuf, remove_annotation,
    remove_items, send_display_item_changes, server_reload,
};
#[derive(Serialize, Deserialize)]
pub struct HierarchyResponse {
    pub hierarchy: wellen::Hierarchy,
    pub file_format: wellen::FileFormat,
}
