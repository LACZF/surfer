use crate::remote::protobuf::AnnotationRemovalRequest;
use crate::remote::protobuf::UpdateAnnotationGroupRequest;
use std::fmt::Write as _;
use std::str::Utf8Error;
use std::sync::Arc;

use crate::remote::protobuf::AddAnnotationCommentRequest;
use crate::remote::protobuf::AddAnnotationCommentResponse;
use crate::remote::protobuf::AddAnnotationGroupRequest;
use crate::remote::protobuf::RemoveAnnotationGroupRequest;
use crate::remote::protobuf::RemoveAnnotationGroupResponse;
use std::sync::mpsc::Sender;

use crate::Annotation;
use crate::DisplayedItemTree;
use crate::displayed_item;
use crate::displayed_item::DisplayedItem;
use crate::displayed_item::DisplayedItemRef;
use crate::displayed_item_tree::ItemIndex;
use crate::displayed_item_tree::TargetPosition;
use crate::remote::protobuf::AnnotationListRequest;
use crate::remote::protobuf::AnnotationListResponse;
use crate::remote::protobuf::DisplayedItemChange;
use crate::remote::protobuf::DragVariablesRequest;
use crate::remote::protobuf::FullDisplayTreeRequest;
use crate::remote::protobuf::NewAnnotationResponse;
use crate::remote::protobuf::NewAnnotationRequest;
use crate::remote::protobuf::RemoveItemsRequest;
use bincode::Options;
use eyre::{Context, Result, anyhow, bail, eyre};
use protobuf::surfer_remote_client::SurferRemoteClient;
use reqwest::StatusCode;

use thiserror::Error;
use tonic::Streaming;
use tonic::codegen::InterceptedService;
use tonic::metadata::MetadataValue;
use tonic::service::Interceptor;
use tonic::transport::{Channel, Endpoint};
use tracing::{info, warn};
use wellen::CompressedTimeTable;

use protobuf::*;

pub mod protobuf {
    tonic::include_proto!("network");
}
use super::HierarchyResponse;
use crate::async_util::{perform_async_work, sleep_ms};
use crate::channels::checked_send;
use crate::message::Message;
use crate::remote::client::protobuf::ActionResponse;
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

type GrpcClient = SurferRemoteClient<InterceptedService<Channel, ServerProfile>>;
pub struct AppClient {
    client: Option<Arc<tokio::sync::Mutex<GrpcClient>>>,
    pub session_id: Arc<RwLock<String>>,
    server: String,
    pub cancel_token: tokio_util::sync::CancellationToken,
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
    Option<crate::displayed_item_tree::DisplayedItemTree>,
)> {
    let displayed_item_ref =
        crate::displayed_item::DisplayedItemRef::from(res.displayed_item_ref as usize);

    let displayed_item_tree = res
        .displayed_item_tree
        .and_then(|bytes| ron::de::from_bytes::<DisplayedItemTree>(&bytes).ok());

    let displayed_item_bytes = res.displayed_item_change;
    match ron::de::from_bytes::<crate::displayed_item::DisplayedItem>(&displayed_item_bytes)
        .context("Failed loading display item".to_string())
    {
        Ok(displayed_item) => Ok((displayed_item_ref, displayed_item, displayed_item_tree)),
        Err(e) => Err(e),
    }
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

    pub async fn create_or_change_annotation(
        &mut self,
        anno: Annotation,
        id: Option<u64>,
    ) -> Result<tonic::Response<NewAnnotationResponse>> {
        let annotation_bytes = {
            ron::to_string::<Annotation>(&anno)
                .context("serializing an annotation item for liveshare")?
                .into_bytes()
        };

        let response = self
            .get_or_init_client()
            .await?
            .lock()
            .await
            .create_or_change_annotation(NewAnnotationRequest {
                data: annotation_bytes,
                id,
            })
            .await?;

        Ok(response)
    }

    pub fn new(server: String) -> Self {
        let session_id = Arc::new(RwLock::new(String::new()));

        Self {
            client: None,
            session_id,
            server: server.clone(),
            cancel_token: tokio_util::sync::CancellationToken::new(), // Used to cancel streaming.
        }
    }
    pub fn cancel_stream(&mut self) {
        self.cancel_token.cancel();
    }
    pub async fn add_to_display_tree(
        &mut self,
        item: displayed_item::DisplayedItem,
        target_pos: TargetPosition,
    ) -> Result<tonic::Response<DisplayedItemChangeResponse>> {
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
    pub async fn add_annotation_comment(
        &mut self,
        annotation_id: egui::Id,
        message: String,
    ) -> Result<tonic::Response<AddAnnotationCommentResponse>> {
        let response = self
            .get_or_init_client()
            .await?
            .lock()
            .await
            .add_annotation_comment(AddAnnotationCommentRequest {
                annotation_id: annotation_id.value(),
                message,
            })
            .await?;

        Ok(response)
    }

    pub async fn add_annotation_group(
        &mut self,
        name: String,
    ) -> Result<tonic::Response<AddAnnotationGroupResponse>> {
        let response = self
            .get_or_init_client()
            .await?
            .lock()
            .await
            .add_annotation_group(AddAnnotationGroupRequest { name })
            .await?;
        Ok(response)
    }

    pub async fn remove_annotation_group(
        &mut self,
        name: String,
    ) -> Result<tonic::Response<RemoveAnnotationGroupResponse>> {
        let response = self
            .get_or_init_client()
            .await?
            .lock()
            .await
            .remove_annotation_group(RemoveAnnotationGroupRequest { name })
            .await?;
        Ok(response)
    }
    pub async fn remove_annotation_comment(
        &mut self,
        annotation_id: egui::Id,
        commend_id: egui::Id,
    ) -> Result<tonic::Response<RemoveAnnotationCommentResponse>> {
        let response = self
            .get_or_init_client()
            .await?
            .lock()
            .await
            .remove_annotation_comment(RemoveAnnotationCommentRequest {
                annotation_id: annotation_id.value(),
                comment_id: commend_id.value(),
            })
            .await?;
        Ok(response)
    }

    pub async fn update_annotation_group(&mut self, anno_id: egui::Id, name: String) -> Result<()> {
        info!("sending anno move to server");
        self.get_or_init_client()
            .await?
            .lock()
            .await
            .update_annotation_group(UpdateAnnotationGroupRequest {
                annotation_id: anno_id.value(),
                group_name: name,
            })
            .await?;
        Ok(())
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
        item_indexes: Vec<ItemIndex>,
        target_pos: TargetPosition,
    ) -> Result<tonic::Response<DisplayedItemTreeResponse>> {
        let variables_to_change_bytes = {
            ron::to_string::<Vec<ItemIndex>>(&item_indexes)
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
        let displayed_item_bytes = {
            ron::to_string::<displayed_item::DisplayedItem>(&item)
                .context("serializing a displayed item for liveshare")?
                .into_bytes()
        };

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
                        .map_err(ReloadError::WrongTextFormat)?;
                    let status = serde_json::from_str::<SurverStatus>(body)?;
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

        parse_signal_response_body(data, signals)
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
            Ok(r) => Ok(r),
            Err(e) => Err(anyhow!(e.message().to_string())),
        }
    }
    pub async fn get_annotation_list(
        &mut self,
        file_index: u64,
    ) -> Result<tonic::Response<AnnotationListResponse>> {
        let response = self
            .get_or_init_client()
            .await?
            .lock()
            .await
            .get_annotation_list(AnnotationListRequest { file_index })
            .await?;

        Ok(response)
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

        let status = serde_json::from_str::<SurverStatus>(body)?;
        Ok(status)
    }

    pub async fn remove_items(&mut self, ids: Vec<DisplayedItemRef>) -> Result<()> {
        let _response = self
            .get_or_init_client()
            .await?
            .lock()
            .await
            .remove_items(RemoveItemsRequest {
                item_refs: ids
                    .iter()
                    .map(|item_ref| item_ref.0 as u64)
                    .collect::<Vec<u64>>(),
            })
            .await?;
        Ok(())
    }

    pub async fn remove_annotation(&mut self, anno_id: egui::Id) -> Result<()> {
        let id = ron::to_string(&anno_id)?.into_bytes();
        let _response = self
            .get_or_init_client()
            .await?
            .lock()
            .await
            .remove_annotation(AnnotationRemovalRequest { id })
            .await?;
        Ok(())
    }
}

pub fn remove_annotation(
    client: Arc<tokio::sync::Mutex<AppClient>>,
    annotation: egui::Id,
    sender: Sender<Message>,
) {
    perform_async_work(async move {
        match client.lock().await.remove_annotation(annotation).await {
            Ok(_res) => {}
            Err(e) => {
                checked_send(&sender, Message::Error(e));
            }
        };
    });
}

pub fn create_or_change_annotation(
    client: Arc<tokio::sync::Mutex<AppClient>>,
    annotation: Annotation,
    sender: Sender<Message>,
) {
    perform_async_work(async move {
        let msg = match client
            .lock()
            .await
            .create_or_change_annotation(annotation, None)
            .await
        {
            Ok(res) => {
                let res = res.into_inner();
                let annotation = ron::de::from_bytes::<Annotation>(&res.data).unwrap(); // TODO: no unwrap!

                Message::AnnotationAdded(annotation)
            }
            Err(e) => Message::Error(e),
        };
        checked_send(&sender, msg);
    });
}

pub fn send_display_item_changes(
    client: Option<Arc<tokio::sync::Mutex<AppClient>>>,
    changes: Vec<(DisplayedItemRef, DisplayedItem)>,
    sender: Sender<Message>,
    use_liveshare: bool,
) {
    if use_liveshare && let Some(client) = client.clone() {
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

pub fn remove_items(
    client: Option<Arc<tokio::sync::Mutex<AppClient>>>,
    ids: Vec<DisplayedItemRef>,
    sender: Sender<Message>,
) {
    if let Some(client) = client.clone() {
        perform_async_work(async move {
            let mut client = client.lock().await;
            if let Err(err) = client.remove_items(ids).await {
                checked_send(&sender, Message::Error(err));
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

async fn get_status(_server: String) -> Result<SurverStatus> {
    let mut client = get_client().await;
    let response = client.get_status(StatusRequest {}).await?;
    let body = response.into_inner().body;
    let body = std::str::from_utf8(&body)?;
    let status = serde_json::from_str::<SurverStatus>(body)?;
    Ok(status)
}

async fn reload(
    _server: String,
    file_index: usize,
) -> std::result::Result<SurverStatus, ReloadError> {
    let mut client = get_client().await;
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
                    .map_err(ReloadError::WrongTextFormat)?;
                let status = serde_json::from_str::<SurverStatus>(body)?;
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

async fn get_hierarchy(_server: String, file_index: usize) -> Result<HierarchyResponse> {
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

async fn get_time_table(_server: String, file_index: usize) -> Result<Vec<wellen::Time>> {
    let mut client = get_client().await;
    let response = client
        .get_time_table(TimeTableRequest {
            file_index: file_index as u64,
        })
        .await?;
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
    _server: String,
    signals: &[wellen::SignalRef],
    _max_url_length: u16,
    file_index: usize,
) -> Result<Vec<wellen::Signal>> {
    if signals.is_empty() {
        return Ok(vec![]);
    }
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

pub async fn get_hierarchy_from_server(
    sender: Sender<Message>,
    server: Arc<tokio::sync::Mutex<AppClient>>,
    load_options: LoadOptions,
    file_index: usize,
) {
    let start = web_time::Instant::now();

    let server_url = server.lock().await.server.clone();
    let source = WaveSource::Url(server_url.clone());
    let mut cli = server.lock().await;
    let res = cli
        .get_hierarchy(file_index)
        .await
        .map_err(|e| anyhow!("{e:?}"))
        .with_context(|| "Failed to retrieve hierarchy from remote server ".to_string());

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
            .with_context(|| "Failed to retrieve status from remote server ".to_string());

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
            get_hierarchy_from_server(sender, server, load_options, file_index).await;
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
