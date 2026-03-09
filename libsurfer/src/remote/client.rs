use std::fmt::Write as _;
use std::str::Utf8Error;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::mpsc::Sender;

use crate::DisplayedItemTree;
use crate::displayed_item;
use crate::displayed_item::DisplayedItem;
use crate::displayed_item::DisplayedItemRef;
use crate::displayed_item_tree::ItemIndex;
use crate::displayed_item_tree::TargetPosition;
use crate::remote::protobuf::DisplayedItemChange;
use crate::remote::protobuf::DragVariablesRequest;
use crate::remote::protobuf::FullDisplayTreeRequest;
use bincode::Options;
use egui::response;
use eyre::{Context, Result, WrapErr as _, anyhow, bail, eyre};
use futures::executor::block_on;
use protobuf::surfer_remote_client::SurferRemoteClient;
use reqwest::StatusCode;
use std::collections::HashMap;

use thiserror::Error;
use tonic::Streaming;
use tonic::codegen::InterceptedService;
use tonic::metadata::MetadataValue;
use tonic::service::Interceptor;
use tonic::transport::{Channel, Endpoint};
use tracing::{info, warn};
use wellen::CompressedTimeTable;

use protobuf::{
    AddGroupRequest, AutheniticationRequest, AutheniticationResponse, DisplayedItemTreeResponse,
    FullDisplayTreeResponse, HierarchyRequest, HierarchyResponse as ProtoHierarchyResponse,
    InfoPageRequest, InfoPageResponse, LiveshareRequest, ReloadRequest, ReloadResponse,
    SignalRequests, SignalResponses, StatusRequest, StatusResponse, TimeTableRequest,
    TimeTableResponse,
};

pub mod protobuf {
    tonic::include_proto!("network");
}
use super::HierarchyResponse;
use crate::async_util::{perform_async_work, sleep_ms};
use crate::channels::checked_send;
use crate::message::Message;
use crate::remote::client::protobuf::ActionResponse;
use crate::remote::client::protobuf::reload_response::FileNotFound;
use crate::remote::client::protobuf::reload_response::MessageType;
use crate::remote::protobuf::DisplayedItemChangeResponse;
use crate::surver::{
    BINCODE_OPTIONS, HTTP_SERVER_KEY, HTTP_SERVER_VALUE_SURFER, SURFER_VERSION, SurverStatus,
    WELLEN_VERSION, X_SURFER_VERSION, X_WELLEN_VERSION,
};
use crate::wave_source::{LoadOptions, WaveSource};
use crate::wellen::{BodyResult, HeaderResult};
use std::sync::RwLock;
pub struct ServerProfile {
    server_url: String,
    pub session_id: Arc<RwLock<String>>,
    username: String,
    client_id: u64,
}

pub struct AppClient {
    client: Option<
        Arc<tokio::sync::Mutex<SurferRemoteClient<InterceptedService<Channel, ServerProfile>>>>,
    >,
    pub session_id: Arc<RwLock<String>>,
    server: String,
}
// For broadcast.
pub fn parse_signal_response_body_no_signals(
    data: Vec<u8>,
) -> Result<Vec<wellen::Signal>> {
    let mut reader = std::io::Cursor::new(data);

    let num_ids: u64 = leb128::read::unsigned(&mut reader)?;
    info!("Num of ids {num_ids}");
    if num_ids == 0 {
        return Ok(vec![]);
    }

    let opts = BINCODE_OPTIONS.allow_trailing_bytes();
    let mut out = Vec::with_capacity(num_ids as usize);
    for _ in 0..(num_ids - 1) {
        let compressed: wellen::CompressedSignal = opts.deserialize_from(&mut reader)?;
        let signal = compressed.uncompress();
        out.push(signal);
    }
    // for the final signal, we expect to consume all bytes
    let compressed: wellen::CompressedSignal = BINCODE_OPTIONS.deserialize_from(&mut reader)?;
    let signal = compressed.uncompress();
    out.push(signal);
    Ok(out)
}

pub fn parse_display_item(
    res: DisplayedItemChangeResponse,
) -> Result<(
    crate::displayed_item::DisplayedItemRef,
    crate::displayed_item::DisplayedItem,
)> {
    let displayed_item_ref =
        crate::displayed_item::DisplayedItemRef::from(res.displayed_item_ref as usize);

    let displayed_item_bytes = res.displayed_item_change;
    let msg =
        match ron::de::from_bytes::<crate::displayed_item::DisplayedItem>(&displayed_item_bytes)
            .context(format!("Failed loading display item"))
        {
            Ok(displayed_item) => {
                //
                return Ok((displayed_item_ref, displayed_item));
            }
            Err(e) => {
                return Err(e);
            }
        };
}

pub fn parse_signal_response_body(
    data: Vec<u8>,
    signals: &[wellen::SignalRef],
) -> Result<Vec<wellen::Signal>> {
    let mut reader = std::io::Cursor::new(data);
    let num_ids: u64 = leb128::read::unsigned(&mut reader)?;
    if num_ids > signals.len() as u64 {
        bail!(
            "Too many signals in response: {num_ids}, expected {}",
            signals.len()
        );
    }
    if num_ids == 0 {
        return Ok(vec![]);
    }

    let opts = BINCODE_OPTIONS.allow_trailing_bytes();
    let mut out = Vec::with_capacity(num_ids as usize);
    for _ in 0..(num_ids - 1) {
        let compressed: wellen::CompressedSignal = opts.deserialize_from(&mut reader)?;
        let signal = compressed.uncompress();
        out.push(signal);
    }
    // for the final signal, we expect to consume all bytes
    let compressed: wellen::CompressedSignal = BINCODE_OPTIONS.deserialize_from(&mut reader)?;
    let signal = compressed.uncompress();
    out.push(signal);
    Ok(out)
}

impl AppClient {
    // TODO: make this prettier
    async fn get_or_init_client(
        &mut self,
    ) -> Result<
        Arc<tokio::sync::Mutex<SurferRemoteClient<InterceptedService<Channel, ServerProfile>>>>,
    > {
        match self.client.as_ref() {
            None => {
                let channel = Endpoint::from_static("http://[::1]:50051")
                    .connect()
                    .await?;

                let profile = ServerProfile {
                    server_url: "".to_string(),
                    session_id: self.session_id.clone(),
                    username: "username_test".to_string(),
                    client_id: 23,
                };

                Ok(Arc::new(tokio::sync::Mutex::new(
                    SurferRemoteClient::with_interceptor(channel, profile),
                )))
            }
            Some(client) => Ok(client.clone()),
        }
    }

    pub fn new(server: String) -> Self {
        let session_id = Arc::new(RwLock::new(String::new()));

        Self {
            client: None,
            session_id: session_id,
            server: server.clone(),
        }
    }

    pub async fn add_to_display_tree(
        &mut self,
        item: displayed_item::DisplayedItem,
        target_pos: TargetPosition,
    ) -> Result<tonic::Response<DisplayedItemChangeResponse>> {
        let opt = ron::Options::default();

        let displayed_item_bytes = {
            ron::to_string::<displayed_item::DisplayedItem>(&item)
                .context("serializing a displayed item for liveshare")?
                .into_bytes()
        };
        let target_pos_bytes = {
            ron::to_string::<TargetPosition>(&target_pos)
                .context("serializing a displayed target position for liveshare")?
                .into_bytes()
        };

        //let bytes = opt.to_string_pretty(item, ron::ser::PrettyConfig::default()).unwrap().as_bytes().to_vec();

        //opt.to_string_pretty(item, ron::ser::PrettyConfig::default()).unwrap().as_bytes()
        let response = self
            .get_or_init_client()
            .await?
            .lock()
            .await
            .change_display_item(DisplayedItemChange {
                displayed_item_ref: None,
                displayed_item_change: displayed_item_bytes,
                target_position: Some(target_pos_bytes),
            })
            .await?;

        Ok(response)
    }

    pub async fn get_full_display_tree(
        &mut self,
        file_index: u64,
    ) -> Result<tonic::Response<FullDisplayTreeResponse>> {
        let response = self
            .get_or_init_client()
            .await?
            .lock()
            .await
            .get_full_display_tree(FullDisplayTreeRequest { file_index })
            .await?;

        Ok(response)
    }
    pub async fn change_variable_order(
        &mut self,
        itemIndexes: Vec<ItemIndex>,
        target_pos: TargetPosition,
    ) -> Result<tonic::Response<DisplayedItemTreeResponse>> {
        let variables_to_change_bytes = {
            ron::to_string::<Vec<ItemIndex>>(&itemIndexes)
                .context("serializing a displayed item for liveshare")?
                .into_bytes()
        };
        let target_pos_bytes = {
            ron::to_string::<TargetPosition>(&target_pos)
                .context("serializing a displayed target position for liveshare")?
                .into_bytes()
        };
        let response = self
            .get_or_init_client()
            .await?
            .lock()
            .await
            .change_variable_order(DragVariablesRequest {
                variables: variables_to_change_bytes,
                target_position: target_pos_bytes,
            })
            .await?;

        Ok(response)
    }
    pub async fn change_display_tree(
        &mut self,
        item_ref: displayed_item::DisplayedItemRef,
        item: displayed_item::DisplayedItem,
    ) -> Result<tonic::Response<DisplayedItemChangeResponse>> {
        let opt = ron::Options::default();

        let displayed_item_bytes = {
            ron::to_string::<displayed_item::DisplayedItem>(&item)
                .context("serializing a displayed item for liveshare")?
                .into_bytes()
        };

        //let bytes = opt.to_string_pretty(item, ron::ser::PrettyConfig::default()).unwrap().as_bytes().to_vec();

        //opt.to_string_pretty(item, ron::ser::PrettyConfig::default()).unwrap().as_bytes()
        let response = self
            .get_or_init_client()
            .await?
            .lock()
            .await
            .change_display_item(DisplayedItemChange {
                displayed_item_ref: Some(item_ref.0 as u64),
                displayed_item_change: displayed_item_bytes,
                target_position: None,
            })
            .await?;

        Ok(response)
    }

    pub async fn join_liveshare(
        &mut self,
        file_index: usize,
    ) -> Result<tonic::Response<Streaming<ActionResponse>>> {
        info!("Joined liveshare with file index: {file_index}");
        let response = self
            .get_or_init_client()
            .await?
            .lock()
            .await
            .join_liveshare(LiveshareRequest {
                file_index: file_index as u64,
            })
            .await?;

        Ok(response)
    }
    pub async fn get_time_table(&mut self, file_index: usize) -> Result<Vec<wellen::Time>> {
        let response = self
            .get_or_init_client()
            .await?
            .lock()
            .await
            .get_time_table(TimeTableRequest {
                file_index: file_index as u64,
            })
            .await?;
        //check_response(&server, &response)?;
        //let compressed_data = response.bytes().await?;
        let compressed_data = response.into_inner().body;
        let compressed: CompressedTimeTable = BINCODE_OPTIONS.deserialize(&compressed_data)?;
        let table = compressed.uncompress();
        Ok(table)
    }
    pub async fn reload(
        &mut self,
        file_index: usize,
    ) -> std::result::Result<SurverStatus, ReloadError> {
        let response = self
            .get_or_init_client()
            .await?
            .lock()
            .await
            .reload(ReloadRequest {
                file_index: file_index as u64,
            })
            .await
            .unwrap(); // TODO replace unwrap
        match response.into_inner().message_type {
            Some(message_type) => match message_type {
                MessageType::FileNotFound(_) => {
                    warn!("File not found on server");
                    Err(ReloadError::FileNotFound)
                }
                MessageType::FileUnchanged(_) => {
                    info!("File unchanged, no reload needed");
                    Err(ReloadError::FileUnchanged)
                }
                MessageType::FileChanged(file_changed) => {
                    info!("File reloaded at server");
                    let body = std::str::from_utf8(&file_changed.body)
                        .map_err(|err| ReloadError::WrongTextFormat(err))?;
                    let status = serde_json::from_str::<SurverStatus>(&body)?;
                    Ok(status)
                }
            },
            None => Err(ReloadError::MissingField),
        }
    }
    pub async fn get_hierarchy(&mut self, file_index: usize) -> Result<HierarchyResponse> {
        let response: tonic::Response<protobuf::HierarchyResponse> = self
            .get_or_init_client()
            .await?
            .lock()
            .await
            .get_hierarchy(HierarchyRequest {
                file_index: file_index as u64,
            })
            .await?;
        //check_response(&server, &response)?;
        //let compressed = response.bytes().await?;
        let compressed = response.into_inner().body;
        let raw = lz4_flex::decompress_size_prepended(&compressed)?;
        let mut reader = std::io::Cursor::new(raw);
        // first we read a value, expecting there to be more bytes
        let opts = BINCODE_OPTIONS.allow_trailing_bytes();
        let file_format: wellen::FileFormat = opts.deserialize_from(&mut reader)?;
        // the last value should consume all remaining bytes
        let hierarchy: wellen::Hierarchy = BINCODE_OPTIONS.deserialize_from(&mut reader)?;
        Ok(HierarchyResponse {
            hierarchy,
            file_format,
        })
    }
    pub async fn get_signals(
        &mut self,
        signals: &[wellen::SignalRef],
        max_url_length: u16,
        file_index: usize,
    ) -> Result<Vec<wellen::Signal>> {
        if signals.is_empty() {
            return Ok(vec![]);
        }

        let id_strings = format_signal_vec(signals);

        let response = self
            .get_or_init_client()
            .await?
            .lock()
            .await
            .get_signals(SignalRequests {
                file_index: file_index as u64,
                id_strings,
            })
            .await?;
        let data = response.into_inner().body;

        return parse_signal_response_body(data, signals);
    }

    pub async fn get_page(&mut self) -> Result<tonic::Response<InfoPageResponse>> {
        let req = InfoPageRequest {};

        let response: std::result::Result<tonic::Response<InfoPageResponse>, tonic::Status> = self
            .get_or_init_client()
            .await?
            .lock()
            .await
            .get_info_page(req)
            .await;

        match response {
            Ok(r) => return Ok(r),
            Err(e) => return Err(anyhow!(e.message().to_string())),
        }
    }
    pub async fn add_group(
        &mut self,
        item_refs: Vec<u64>,
        target: Vec<u8>,
        displayed_item: Vec<u8>,
    ) -> Result<tonic::Response<DisplayedItemChangeResponse>> {
        match self
            .get_or_init_client()
            .await?
            .lock()
            .await
            .add_group(AddGroupRequest {
                item_refs,
                target_position: target,
                displayed_item,
            })
            .await
        {
            Ok(ok) => Ok(ok),
            Err(err) => Err(anyhow!(err.message().to_string())),
        }
    }

    pub async fn authenticate(&mut self, token: String, username: String) -> Result<ServerProfile> {
        let req = AutheniticationRequest { username, token };

        let response: std::result::Result<tonic::Response<AutheniticationResponse>, tonic::Status> =
            self.get_or_init_client()
                .await?
                .lock()
                .await
                .authenticate_user(req)
                .await;

        match response {
            Ok(res) => {
                let res_message = res.into_inner();
                info!("Response username: {}", res_message.username);
                info!("Session ID: {}", res_message.session_id);
                let mut write = self.session_id.write().unwrap();
                *write = res_message.session_id;
                drop(write);
                info!("Session ID: {}", self.session_id.clone().read().unwrap());

                let server_profile = ServerProfile {
                    server_url: "".to_string(),
                    session_id: self.session_id.clone(),
                    username: res_message.username,
                    client_id: res_message.id,
                };
                Ok(server_profile)
            }
            Err(err) => Err(anyhow!(err.message().to_string())),
        }
    }
    pub async fn get_status(&mut self) -> Result<SurverStatus> {
        let response = self
            .get_or_init_client()
            .await?
            .lock()
            .await
            .get_status(StatusRequest {})
            .await?;

        let body = response.into_inner().body;
        let body = std::str::from_utf8(&body)?;

        let status = serde_json::from_str::<SurverStatus>(&body)?;
        Ok(status)
    }
}

pub fn send_display_item_changes(
    client: Option<Arc<tokio::sync::Mutex<AppClient>>>,
    changes: Vec<(DisplayedItemRef, DisplayedItem)>,
    sender: Sender<Message>,
) {
    if let Some(client) = client.clone() {
        perform_async_work(async move {
            let mut client = client.lock().await;
            for (item_ref, item) in changes {
                match client.change_display_tree(item_ref, item).await {
                    Ok(_) => {}
                    Err(e) => {
                        let _ = sender.send(Message::Error(e));
                    }
                };
            }
        });
    }
}

impl Interceptor for ServerProfile {
    fn call(
        &mut self,
        mut request: tonic::Request<()>,
    ) -> Result<tonic::Request<()>, tonic::Status> {
        let session_id = self.session_id.read().unwrap();
        let sess: MetadataValue<tonic::metadata::Ascii> = session_id
            .parse()
            .map_err(|_| tonic::Status::unauthenticated("bad sess"))?;
        request.metadata_mut().insert("authorization", sess);
        Ok(request)
    }
}

/// Returns a shared reqwest client to reuse HTTP connections and reduce TLS overhead.
async fn get_client() -> SurferRemoteClient<InterceptedService<Channel, ServerProfile>> {
    /*static CLIENT: OnceLock<SurferRemoteClient<Channel>> = OnceLock::new();
    CLIENT.get_or_init(|| {
        let client = block_on(async {
            SurferRemoteClient::connect("http://[::1]:50051")
                .await
                .unwrap()
        });

        client

    })*/
    let channel = Endpoint::from_static("http://[::1]:50051")
        .connect()
        .await
        .unwrap();

    let ser: SurferRemoteClient<InterceptedService<Channel, ServerProfile>> =
        SurferRemoteClient::with_interceptor(
            channel,
            ServerProfile {
                server_url: "".to_string(),
                session_id: Arc::new(RwLock::new(String::new())),
                username: "".to_string(),
                client_id: 0,
            },
        );
    ser
}

#[derive(Debug, Error)]
pub enum ReloadError {
    #[error("Received data is not utf8 text")]
    WrongTextFormat(Utf8Error),
    #[error("Missing field on the reload response")]
    MissingField,
    #[error("File not found on the server")]
    FileNotFound,
    #[error("File unchanged since last reload")]
    FileUnchanged,
    #[error("Unexpected response code: {0}")]
    UnexpectedStatus(StatusCode),
    #[error("Network error: {0}")]
    Network(#[from] reqwest::Error),
    #[error("Parse error: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("Response validation error: {0}")]
    Validation(#[from] eyre::Report),
}

fn check_response(server_url: &str, response: &reqwest::Response) -> Result<()> {
    let server = response
        .headers()
        .get(HTTP_SERVER_KEY)
        .ok_or(eyre!("no server header"))?
        .to_str()?;
    if server != HTTP_SERVER_VALUE_SURFER {
        bail!("Unexpected server {server} from {server_url}");
    }
    let surfer_version = response
        .headers()
        .get(X_SURFER_VERSION)
        .ok_or(eyre!("no surfer version header"))?
        .to_str()?;
    if surfer_version != SURFER_VERSION {
        // this mismatch may be OK as long as the wellen version matches
        info!(
            "Surfer version on the server: {surfer_version} does not match client version {SURFER_VERSION}"
        );
    }
    let wellen_version = response
        .headers()
        .get(X_WELLEN_VERSION)
        .ok_or(eyre!("no wellen version header"))?
        .to_str()?;
    if wellen_version != WELLEN_VERSION {
        bail!(
            "Version incompatibility! The server uses wellen {wellen_version}, our client uses wellen {WELLEN_VERSION}"
        );
    }
    Ok(())
}

async fn get_status(server: String) -> Result<SurverStatus> {
    let mut client = get_client().await;
    //let response = client.get(format!("{server}/get_status")).send().await?;
    let response = client.get_status(StatusRequest {}).await?;
    //check_response(&server, &response)?;
    let body = response.into_inner().body;
    let body = std::str::from_utf8(&body)?;
    //let body = response.text().await?;
    let status = serde_json::from_str::<SurverStatus>(&body)?;
    Ok(status)
}

async fn reload(
    server: String,
    file_index: usize,
) -> std::result::Result<SurverStatus, ReloadError> {
    let mut client = get_client().await;
    /*let response = client
        .get(format!("{server}/{file_index}/reload"))
        .send()
        .await?;
    check_response(&server, &response)?;*/
    let response = client
        .reload(ReloadRequest {
            file_index: file_index as u64,
        })
        .await
        .unwrap(); // TODO replace unwrap
    match response.into_inner().message_type {
        Some(message_type) => match message_type {
            MessageType::FileNotFound(_) => {
                warn!("File not found on server");
                Err(ReloadError::FileNotFound)
            }
            MessageType::FileUnchanged(_) => {
                info!("File unchanged, no reload needed");
                Err(ReloadError::FileUnchanged)
            }
            MessageType::FileChanged(file_changed) => {
                info!("File reloaded at server");
                let body = std::str::from_utf8(&file_changed.body)
                    .map_err(|err| ReloadError::WrongTextFormat(err))?;
                let status = serde_json::from_str::<SurverStatus>(&body)?;
                Ok(status)
            }
        },
        None => Err(ReloadError::MissingField),
    }

    /*let status_code = response.status();
    let body = response.text().await?;
    match status_code {
        StatusCode::NOT_MODIFIED => {
            info!("File unchanged, no reload needed");
            Err(ReloadError::FileUnchanged)
        }
        StatusCode::ACCEPTED => {
            info!("File reloaded at server");
            let status = serde_json::from_str::<SurverStatus>(&body)?;
            Ok(status)
        }
        code => {
            warn!("Unexpected response code: {code}");
            Err(ReloadError::UnexpectedStatus(code))
        }
    }*/
}

async fn get_hierarchy(server: String, file_index: usize) -> Result<HierarchyResponse> {
    let mut client = get_client().await;
    /*let response = client
    .get(format!("{server}/{file_index}/get_hierarchy"))
    .send()
    .await?;*/
    let response = client
        .get_hierarchy(HierarchyRequest {
            file_index: file_index as u64,
        })
        .await?;
    //check_response(&server, &response)?;
    //let compressed = response.bytes().await?;
    let compressed = response.into_inner().body;
    let raw = lz4_flex::decompress_size_prepended(&compressed)?;
    let mut reader = std::io::Cursor::new(raw);
    // first we read a value, expecting there to be more bytes
    let opts = BINCODE_OPTIONS.allow_trailing_bytes();
    let file_format: wellen::FileFormat = opts.deserialize_from(&mut reader)?;
    // the last value should consume all remaining bytes
    let hierarchy: wellen::Hierarchy = BINCODE_OPTIONS.deserialize_from(&mut reader)?;
    Ok(HierarchyResponse {
        hierarchy,
        file_format,
    })
}

async fn get_time_table(server: String, file_index: usize) -> Result<Vec<wellen::Time>> {
    let mut client = get_client().await;
    /*let response = client
    .get(format!("{server}/{file_index}/get_time_table"))
    .send()
    .await?;*/
    let response = client
        .get_time_table(TimeTableRequest {
            file_index: file_index as u64,
        })
        .await?;
    //check_response(&server, &response)?;
    //let compressed_data = response.bytes().await?;
    let compressed_data = response.into_inner().body;
    let compressed: CompressedTimeTable = BINCODE_OPTIONS.deserialize(&compressed_data)?;
    let table = compressed.uncompress();
    Ok(table)
}

// Helper to calculate URL length for a signal index
// Much more efficient than string conversion
// Extracted for testing
#[inline]
fn signal_url_len(index: usize) -> usize {
    index.checked_ilog10().unwrap_or(0) as usize + 2 // +1 for '/', +1 as ilog10 rounds down
}

pub async fn get_signals(
    server: String,
    signals: &[wellen::SignalRef],
    max_url_length: u16,
    file_index: usize,
) -> Result<Vec<wellen::Signal>> {
    if signals.is_empty() {
        return Ok(vec![]);
    }

    /*let max_url_length = max_url_length as usize;
    let base_url = format!("{server}/{file_index}/get_signals");
    let base_len = base_url.len();

    let mut all_results = Vec::with_capacity(signals.len());
    let mut current_batch = Vec::new();
    let mut current_url_len = base_len;

    for signal in signals {
        // Each signal adds: "/" + digits
        let signal_len = signal_url_len(signal.index());

        // Check if adding this signal would exceed the limit
        if current_url_len + signal_len > max_url_length && !current_batch.is_empty() {
            info!(
                "Fetching batch of {} signals due to URL length limit",
                current_batch.len()
            );
            // Fetch current batch
            let batch_results = get_signals_batch(&base_url, &current_batch).await?;
            all_results.extend(batch_results);

            // Start new batch
            current_batch.clear();
            current_url_len = base_len;
        }

        current_batch.push(*signal);
        current_url_len += signal_len;

    }
    */

    // Fetch remaining batch
    /*if !current_batch.is_empty() {
        let batch_results = get_signals_batch(&base_url, &current_batch).await?;
        all_results.extend(batch_results);
    }*/
    let id_strings = format_signal_vec(signals);
    let mut client = get_client().await;
    let response = client
        .get_signals(SignalRequests {
            file_index: file_index as u64,
            id_strings,
        })
        .await?;
    let data = response.into_inner().body;

    //let data = response.bytes().await?;
    let mut reader = std::io::Cursor::new(data);
    let num_ids: u64 = leb128::read::unsigned(&mut reader)?;
    if num_ids > signals.len() as u64 {
        bail!(
            "Too many signals in response: {num_ids}, expected {}",
            signals.len()
        );
    }
    if num_ids == 0 {
        return Ok(vec![]);
    }

    let opts = BINCODE_OPTIONS.allow_trailing_bytes();
    let mut out = Vec::with_capacity(num_ids as usize);
    for _ in 0..(num_ids - 1) {
        let compressed: wellen::CompressedSignal = opts.deserialize_from(&mut reader)?;
        let signal = compressed.uncompress();
        out.push(signal);
    }
    // for the final signal, we expect to consume all bytes
    let compressed: wellen::CompressedSignal = BINCODE_OPTIONS.deserialize_from(&mut reader)?;
    let signal = compressed.uncompress();
    out.push(signal);

    Ok(out)
}

// Helper to format signal URL
// Extracted for testing
#[inline]
fn format_signal_url(base_url: &str, signals: &[wellen::SignalRef]) -> String {
    let mut url = base_url.to_string();
    for signal in signals {
        write!(url, "/{}", signal.index()).unwrap();
    }
    url
}

fn format_signal_vec(signals: &[wellen::SignalRef]) -> Vec<String> {
    signals
        .iter()
        .map(|signal| signal.index().to_string())
        .collect()
}
/*
async fn get_signals_batch(
    base_url: &str,
    signals: &[wellen::SignalRef],
) -> Result<Vec<wellen::Signal>> {
    let client = get_client();
    let url = format_signal_url(base_url, signals);

    let response = client.get(url).send().await?;
    check_response(base_url, &response)?;
    let data = response.bytes().await?;
    let mut reader = std::io::Cursor::new(data);
    let num_ids: u64 = leb128::read::unsigned(&mut reader)?;
    if num_ids > signals.len() as u64 {
        bail!(
            "Too many signals in response: {num_ids}, expected {}",
            signals.len()
        );
    }
    if num_ids == 0 {
        return Ok(vec![]);
    }

    let opts = BINCODE_OPTIONS.allow_trailing_bytes();
    let mut out = Vec::with_capacity(num_ids as usize);
    for _ in 0..(num_ids - 1) {
        let compressed: wellen::CompressedSignal = opts.deserialize_from(&mut reader)?;
        let signal = compressed.uncompress();
        out.push(signal);
    }
    // for the final signal, we expect to consume all bytes
    let compressed: wellen::CompressedSignal = BINCODE_OPTIONS.deserialize_from(&mut reader)?;
    let signal = compressed.uncompress();
    out.push(signal);
    Ok(out)
}
*/
pub fn get_hierarchy_from_server(
    sender: Sender<Message>,
    server: Arc<tokio::sync::Mutex<AppClient>>,
    load_options: LoadOptions,
    file_index: usize,
) {
    let start = web_time::Instant::now();

    perform_async_work(async move {
        let server_url = server.lock().await.server.clone();
        let source = WaveSource::Url(server_url.clone());
        let mut cli = server.lock().await;
        let res = cli
            .get_hierarchy(file_index)
            .await
            .map_err(|e| anyhow!("{e:?}"))
            .with_context(|| format!("Failed to retrieve hierarchy from remote server "));

        let msg = match res {
            Ok(h) => {
                let header = HeaderResult::Remote(
                    Arc::new(h.hierarchy),
                    h.file_format,
                    server_url.clone(),
                    file_index,
                );
                Message::WaveHeaderLoaded(start, source, load_options, header)
            }
            Err(e) => Message::Error(e),
        };
        checked_send(&sender, msg);
        let msg = cli.get_full_display_tree(file_index as u64).await;
        match msg {
            Ok(r) => {
                let inner = r.into_inner();
                let items = match ron::de::from_bytes::<HashMap<DisplayedItemRef, DisplayedItem>>(
                    &inner.items,
                ) {
                    Ok(item) => item,
                    Err(e) => {
                        checked_send(
                            &sender,
                            Message::Error(eyre::Report::msg("Error parsing displayed items")),
                        );
                        return;
                    }
                };

                let item_tree = match ron::de::from_bytes::<DisplayedItemTree>(&inner.item_tree) {
                    Ok(item) => item,
                    Err(e) => {
                        checked_send(
                            &sender,
                            Message::Error(eyre::Report::msg("Error parsing item tree")),
                        );
                        return;
                    }
                };
                info!("Change full item tree");
                checked_send(&sender, Message::ChangeFullItemTree(item_tree, items));
            }
            Err(e) => {
                checked_send(&sender, Message::Error(e));
                return;
            }
        }
    });
}

pub fn get_time_table_from_server(
    sender: Sender<Message>,
    server: Arc<tokio::sync::Mutex<AppClient>>,
    file_index: usize,
) {
    let start = web_time::Instant::now();

    perform_async_work(async move {
        let mut server_guard = server.lock().await;
        let server_url = server_guard.server.clone();

        let source = WaveSource::Url(server_url.clone());
        let res = server_guard
            .get_time_table(file_index)
            .await
            .map_err(|e| anyhow!("{e:?}"))
            .with_context(|| {
                format!("Failed to retrieve time table from remote server {server_url}")
            });

        let msg = match res {
            Ok(table) => {
                Message::WaveBodyLoaded(start, source, BodyResult::Remote(table, server_url))
            }
            Err(e) => Message::Error(e),
        };
        checked_send(&sender, msg);
    });
}

pub fn get_server_status(
    sender: Sender<Message>,
    client: Arc<tokio::sync::Mutex<AppClient>>,
    delay_ms: u64,
) {
    let start = web_time::Instant::now();
    perform_async_work(async move {
        sleep_ms(delay_ms).await;
        let res = client
            .lock()
            .await
            .get_status()
            .await
            .map_err(|e| anyhow!("{e:?}"))
            .with_context(|| format!("Failed to retrieve status from remote server "));

        let msg = match res {
            Ok(status) => {
                Message::SetSurverStatus(start, client.lock().await.server.clone(), status)
            }
            Err(e) => Message::Error(e),
        };
        checked_send(&sender, msg);
    });
}

pub fn server_reload(
    sender: Sender<Message>,
    server: Arc<tokio::sync::Mutex<AppClient>>,
    load_options: LoadOptions,
    file_index: usize,
) {
    let start = web_time::Instant::now();
    perform_async_work(async move {
        let server_url = server.lock().await.server.clone();
        let res = server.lock().await.reload(file_index).await;
        let mut request_hierarchy = false;

        let msg = match res {
            Ok(status) => {
                request_hierarchy = true;
                Message::SetSurverStatus(start, server_url.clone(), status)
            }
            Err(crate::remote::ReloadError::FileUnchanged) => Message::StopProgressTracker,
            Err(e) => {
                let err = anyhow!("{e:?}");
                Message::Error(err)
            }
        };
        checked_send(&sender, msg);
        if request_hierarchy {
            get_hierarchy_from_server(sender, server, load_options, file_index);
        }
    });
}

mod tests {
    #[test]
    fn test_signal_url_length_calculation() {
        use crate::remote::client::signal_url_len;
        // Test edge cases for digit calculation
        assert_eq!(signal_url_len(0), 2); // "/0" -> 2 chars
        assert_eq!(signal_url_len(1), 2); // "/1" -> 2 chars
        assert_eq!(signal_url_len(9), 2); // "/9" -> 2 chars
        assert_eq!(signal_url_len(10), 3); // "/10" -> 3 chars
        assert_eq!(signal_url_len(99), 3); // "/99" -> 3 chars
        assert_eq!(signal_url_len(100), 4); // "/100" -> 4 chars
        assert_eq!(signal_url_len(999), 4); // "/999" -> 4 chars
        assert_eq!(signal_url_len(1000), 5); // "/1000" -> 5 chars
        assert_eq!(signal_url_len(65535), 6); // "/65535" -> 6 chars
    }

    #[test]
    fn test_empty_signals_returns_empty() {
        use crate::remote::get_signals;
        // Create a mock async runtime for testing
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let signals: Vec<wellen::SignalRef> = vec![];
            let result = get_signals("http://localhost:8080".to_string(), &signals, 1000, 0).await;

            // Should return Ok with empty vec without making any network calls
            assert!(result.is_ok());
            assert_eq!(result.unwrap().len(), 0);
        });
    }

    #[test]
    fn test_boundary_signal_indices() {
        use crate::remote::client::signal_url_len;
        // Test that we handle boundary cases correctly
        let boundary_indices = vec![0, 1, 9, 10, 99, 100, 999, 1000, 9999, 10000];

        for idx in boundary_indices {
            let sig_ref = wellen::SignalRef::from_index(idx);
            let len = signal_url_len(sig_ref.unwrap().index());

            // Verify the calculated length matches actual string length
            let actual = format!("/{idx}");
            assert_eq!(
                len,
                actual.len(),
                "URL length calculation mismatch for index {}: expected {}, got {}",
                idx,
                actual.len(),
                len
            );
        }
    }

    #[test]
    fn test_url_construction_format() {
        use crate::remote::client::format_signal_url;
        // Verify URL format matches expected pattern
        let base_url = "http://localhost:8080/get_signals";
        let signals: Vec<wellen::SignalRef> = vec![
            wellen::SignalRef::from_index(1),
            wellen::SignalRef::from_index(42),
            wellen::SignalRef::from_index(999),
        ]
        .into_iter()
        .flatten()
        .collect();

        let url = format_signal_url(base_url, &signals);

        assert_eq!(url, "http://localhost:8080/get_signals/1/42/999");
    }
}
