//! Handling of external communication in Surver.
use crate::Annotation;
use crate::Annotation::{Arrow, Rect};
use crate::AnnotationGroup;
use crate::annotation::Annotatable;
use crate::CommentMessage;
use bincode::Options;
//use extism::ToBytes;
use eyre::{Result, WrapErr as _, anyhow, bail};
use http_body_util::Full;
use hyper::body::Bytes;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use itertools::Itertools;
use std::collections::HashMap;
use std::fs;
use std::iter::repeat_with;

use crate::annotation::AnnotationData;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, RwLock};
use std::time::{Instant, SystemTime};
use tokio::net::TcpListener;
use tokio::spawn;
use tokio::sync::Notify;
use tokio::sync::{Mutex, broadcast};
use tokio_stream::Stream;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request as TonicRequest, Response as TonicResponse, Status};
use tracing::{error, info, warn};
use uuid::Uuid;
use wellen::{
    CompressedSignal, CompressedTimeTable, FileFormat, Hierarchy, Signal, SignalRef, Time, viewers,
};

use crate::displayed_item::{DisplayedItem, DisplayedItemRef};
use crate::displayed_item_tree::{DisplayedItemTree, ItemIndex, TargetPosition};
use crate::surver::server::protobuf::action_response::ResponseType;
use crate::surver::server::protobuf::reload_response::{
    FileChanged, FileNotFound, FileUnchanged, MessageType,
};
use crate::surver::server::protobuf::surfer_remote_server::{SurferRemote, SurferRemoteServer};

use crate::surver::server::protobuf::*;
use crate::surver::{
    BINCODE_OPTIONS, HTTP_SERVER_KEY, HTTP_SERVER_VALUE_SURFER, SURFER_VERSION, SurverFileInfo,
    SurverStatus, WELLEN_SURFER_DEFAULT_OPTIONS, WELLEN_VERSION, X_SURFER_VERSION,
    X_WELLEN_VERSION, modification_time_string,
};

pub mod protobuf {
    tonic::include_proto!("network");
}

#[derive(Clone, Debug)]
pub enum BroadcastClientMessage {
    NewUser {
        username: String,
        id: u64,
    },
    DisplayedItemChange {
        displayed_item_ref: u64,
        displayed_item_change: Vec<u8>,
        id: u64,
        target_position: Option<Vec<u8>>,
        tree: Option<Vec<u8>>,
    },
    DisplayedItemTreeChange {
        item_tree: Vec<u8>,
    },
    LeavingUser {
        id: u64,
    },
    RemoveAnnotationComment {
        id: u64,
        annotation_id: u64,
        comment_id: u64,
    },
    AddAnnotationComment {
        id: u64,
        annotation_id: u64,
        message: String,
        username: String,
    },
    AddAnnotationGroup {
        id: u64,
        name: String,
    },
    Annotation {
        data: Vec<u8>,
        annotation_id: u64,
        id: u64,
    },
    RemoveAnnotation {
        id: u64,
        annotation_id: Vec<u8>,
    },
    UpdateAnnotationGroup {
        id: u64,
        annotation_id: u64,
        group_name: String,
    },
}

type ClientId = u64;
struct Client {
    pub id: ClientId,
    pub username: String,
    pub session_id: String,
    pub file_index: Mutex<Option<FileIndex>>,
    pub liveshare_session: Mutex<Option<Arc<Mutex<Liveshare>>>>,
    //pub receiver: Option<broadcast::Receiver<Arc<BroadcastClientMessage>>>,
}

struct Liveshare {
    pub broadcast_sender: broadcast::Sender<BroadcastClientMessage>,
    // the broadcast closes if there is no receiver
    pub _broadcast_receiver: broadcast::Receiver<BroadcastClientMessage>,
    pub clients: HashMap<ClientId, Arc<Client>>,
    pub file_index: FileIndex,
    pub next_display_item_ref: Mutex<u64>,
    pub item_tree: DisplayedItemTree,
    pub displayed_items: HashMap<DisplayedItemRef, DisplayedItem>,
    pub next_annotation_id: Mutex<u64>,
    pub annotations: Vec<Annotation>,
    pub annotation_groups: Vec<AnnotationGroup>, // List of unique group names
}

impl Liveshare {
    fn new(file_index: FileIndex) -> Self {
        let (sender, receiver) = broadcast::channel::<BroadcastClientMessage>(128);

        Self {
            broadcast_sender: sender,
            _broadcast_receiver: receiver,
            clients: HashMap::default(),
            file_index,
            next_display_item_ref: Mutex::new(0),
            item_tree: DisplayedItemTree::new(),
            displayed_items: HashMap::new(),
            next_annotation_id: Mutex::new(0),
            annotations: Vec::new(),
            annotation_groups: vec![AnnotationGroup {
                name: String::from("Ungrouped"),
                cycle_counter: 0,
                annotations: Vec::new(),
            }],
        }
    }
    pub fn add_annotation_to_group(&mut self, group_name: String, id_to_add: egui::Id) {
        if let Some(idx) = self
            .annotation_groups
            .iter()
            .position(|group| group.name == group_name)
        {
            self.annotation_groups[idx].annotations.push(id_to_add);
        }
    }
}

type FileIndex = usize;
struct MySurver {
    pub state: Arc<RwLock<SurverState>>,
    pub shared: Arc<ReadOnly>,
    pub txs: Vec<Sender<LoaderMessage>>,
    pub liveshare_sessions: HashMap<FileIndex, Arc<Mutex<Liveshare>>>,
    pub users: Mutex<HashMap<ClientId, Arc<Client>>>, // TODO: probably switch to RwLock?
    pub session_id_map: Mutex<HashMap<String, ClientId>>,
    pub next_free_id: Mutex<u64>,
}

impl MySurver {
    fn new(
        state: Arc<RwLock<SurverState>>,
        shared: Arc<ReadOnly>,
        txs: Vec<Sender<LoaderMessage>>,
        file_indexes: Vec<FileIndex>,
    ) -> Self {
        let mut liveshare_sessions = HashMap::new(); //<FileIndex, Mutex<Liveshare>>;
        for file_index in file_indexes {
            liveshare_sessions.insert(file_index, Arc::new(Mutex::new(Liveshare::new(file_index))));
        }

        MySurver {
            state,
            shared,
            txs,
            liveshare_sessions,
            users: Mutex::new(HashMap::new()),
            session_id_map: Mutex::new(HashMap::new()),
            next_free_id: Mutex::new(1),
        }
    }
}

type ResponseStreamItem = Result<ActionResponse, Status>;
type ResponseStream = std::pin::Pin<Box<dyn Stream<Item = ResponseStreamItem> + Send>>;

impl MySurver {
    // check server token

    // check authorization token / session id
    async fn check_auth<T>(&self, request: &TonicRequest<T>) -> Result<Arc<Client>, Status> {
        // get key match
        let session_id = request
            .metadata()
            .get("authorization")
            .ok_or_else(|| Status::unauthenticated("authorization token / session id missing"))?
            .to_str()
            .map_err(|err| Status::invalid_argument(format!("session id not ascii: {err}")))?;

        // turn to string, should be able
        let session_id_map_guard = self.session_id_map.lock().await;

        let client_id = session_id_map_guard
            .get(session_id)
            .ok_or_else(|| Status::unauthenticated("bad authorization token / session id"))?;
        //drop(session_id_map_guard);
        let users_lock = self.users.lock().await;

        //get corresponding user/client
        let client = users_lock
            .get(client_id)
            .ok_or_else(|| Status::unauthenticated("idk what went wrong"))?;

        info!("Client {}: {}, made a request", client.id, client.username);
        Ok(client.clone())
    }
}

#[tonic::async_trait]
impl SurferRemote for MySurver {
    type JoinLiveshareStream = ResponseStream;

    async fn update_annotation_comment(
        &self,
        _request: TonicRequest<UpdateAnnotationCommentRequest>,
    ) -> Result<TonicResponse<UpdateAnnotationCommentResponse>, Status> {
        Err(Status::internal("TODO fix"))
    }

    async fn update_annotation_group(
        &self,
        request: TonicRequest<UpdateAnnotationGroupRequest>,
    ) -> Result<TonicResponse<UpdateAnnotationGroupResponse>, Status> {
        let client = self.check_auth(&request).await?;
        let inner = request.into_inner();
        let annotation_id = unsafe {
            egui::Id::from_high_entropy_bits(inner.annotation_id) // TODO: This may panic, so validate annotation_id prior.
        };
        let group_name = inner.group_name;

        match *client.file_index.lock().await {
            Some(_file_index) => match client.liveshare_session.lock().await.clone() {
                Some(liveshare_session) => {
                    let mut liveshare_session = liveshare_session.lock().await;

                    let mut target = None;
                    for group in &mut liveshare_session.annotation_groups {
                        if let Some(idx) =
                            group.annotations.iter().position(|&id| id == annotation_id)
                        {
                            group.cycle_counter = 0;
                            target = Some(group.annotations.remove(idx));
                            break;
                        }
                    }

                    match target {
                        Some(id) => {
                            liveshare_session.add_annotation_to_group(group_name.clone(), id);
                        }
                        None => {
                            warn!("Error: Just removed non existent id!");
                        }
                    }

                    liveshare_session
                        .broadcast_sender
                        .send(BroadcastClientMessage::UpdateAnnotationGroup {
                            id: client.id,
                            annotation_id: inner.annotation_id,
                            group_name: group_name.clone(),
                        })
                        .map_err(|err| Status::internal(err.to_string()))?;
                    Ok(TonicResponse::new(UpdateAnnotationGroupResponse {
                        annotation_id: inner.annotation_id,
                        group_name,
                    }))
                }
                None => Err(Status::internal(
                    "liveshare session that you joined dont exist",
                )),
            },
            None => Err(Status::failed_precondition(
                "client is not in a liveshare session, should be if calling add_group",
            )),
        }
    }

    async fn add_annotation_comment(
        &self,
        request: TonicRequest<AddAnnotationCommentRequest>,
    ) -> Result<TonicResponse<AddAnnotationCommentResponse>, Status> {
        let client = self.check_auth(&request).await?;

        let inner = request.into_inner();
        let file_index = client.file_index.lock().await;
        let annotation_id = inner.annotation_id;
        let message = inner.message;
        let id;
        unsafe {
            id = egui::Id::from_high_entropy_bits(annotation_id); // TODO: This may panic, so validate annotation_id prior.
        }
        let user = client.username.clone();

        if let Some(index) = *file_index {
            match self.liveshare_sessions.get(&index) {
                Some(liveshare_session) => {
                    let mut sess = liveshare_session.lock().await;
                    if let Some(target) = sess.annotations.iter_mut().find(|a| a.get_id() == id) {
                        let comment = target.get_comment_box_mut();
                        comment.message_chain.push(CommentMessage {
                            id: egui::Id::new(("comment", comment.message_id_source)),
                            user: user.clone(),
                            text: message.clone(),
                        });
                        comment.message_id_source += 1;

                        sess.broadcast_sender
                            .send(BroadcastClientMessage::AddAnnotationComment {
                                id: client.id,
                                annotation_id,
                                message: message.clone(),
                                username: client.username.clone(),
                            })
                            .map_err(|err| Status::internal(err.to_string()))?;

                        Ok(TonicResponse::new(AddAnnotationCommentResponse {
                            annotation_id,
                            username: user,
                            message,
                        }))
                    } else {
                        Err(Status::internal("Couldn't find the annotation"))
                    }
                }
                None => Err(Status::internal(
                    "Couldn't find associated liveshare session",
                )),
            }
        } else {
            Err(Status::internal("Couldn't find file index"))
        }
    }
    async fn remove_annotation_comment(
        &self,
        request: TonicRequest<RemoveAnnotationCommentRequest>,
    ) -> Result<TonicResponse<RemoveAnnotationCommentResponse>, Status> {
        let client = self.check_auth(&request).await?;

        let inner = request.into_inner();
        let file_index = client.file_index.lock().await;
        let annotation_id = inner.annotation_id;

        let msg_id = inner.comment_id;
        let id;
        let message_id;
        unsafe {
            message_id = egui::Id::from_high_entropy_bits(msg_id);
            id = egui::Id::from_high_entropy_bits(annotation_id); // TODO: This may panic, so validate annotation_id prior.
        }
        let _user = client.username.clone();

        if let Some(index) = *file_index {
            match self.liveshare_sessions.get(&index) {
                Some(liveshare_session) => {
                    let mut sess = liveshare_session.lock().await;
                    if let Some(target) = sess.annotations.iter_mut().find(|a| a.get_id() == id) {
                        target
                            .get_comment_box_mut()
                            .message_chain
                            .retain(|comment_message| comment_message.id != message_id);

                        sess.broadcast_sender
                            .send(BroadcastClientMessage::RemoveAnnotationComment {
                                id: client.id,
                                annotation_id,
                                comment_id: msg_id,
                            })
                            .map_err(|err| Status::internal(err.to_string()))?;

                        Ok(TonicResponse::new(RemoveAnnotationCommentResponse {
                            annotation_id,
                            comment_id: msg_id,
                        }))
                    } else {
                        Err(Status::internal("Couldn't find the annotation"))
                    }
                }
                None => Err(Status::internal(
                    "Couldn't find associated liveshare session",
                )),
            }
        } else {
            Err(Status::internal("Couldn't find file index"))
        }
    }
    async fn add_annotation_group(
        &self,
        request: TonicRequest<AddAnnotationGroupRequest>,
    ) -> Result<TonicResponse<AddAnnotationGroupResponse>, Status> {
        let client = self.check_auth(&request).await?;

        let inner = request.into_inner();
        let file_index = client.file_index.lock().await;
        let group_name = inner.name;

        if let Some(index) = *file_index {
            match self.liveshare_sessions.get(&index) {
                Some(liveshare_session) => {
                    let new_group = AnnotationGroup {
                        name: group_name.clone(),
                        cycle_counter: 0,
                        annotations: Vec::new(),
                    };
                    let mut sess = liveshare_session.lock().await;
                    sess.annotation_groups.push(new_group);

                    sess.broadcast_sender
                        .send(BroadcastClientMessage::AddAnnotationGroup {
                            id: client.id,
                            name: group_name.clone(),
                        })
                        .map_err(|err| Status::internal(err.to_string()))?;

                    Ok(TonicResponse::new(AddAnnotationGroupResponse {
                        name: group_name,
                    }))
                }
                None => Err(Status::internal(
                    "Couldn't find associated liveshare session",
                )),
            }
        } else {
            Err(Status::internal("Couldn't find file index"))
        }
    }
    async fn remove_annotation_group(
        &self,
        request: TonicRequest<RemoveAnnotationGroupRequest>,
    ) -> Result<TonicResponse<RemoveAnnotationGroupResponse>, Status> {
        let client = self.check_auth(&request).await?;

        let inner = request.into_inner();
        let file_index = client.file_index.lock().await;
        let group_name = inner.name;

        if let Some(index) = *file_index {
            match self.liveshare_sessions.get(&index) {
                Some(liveshare_session) => {
                    let mut sess = liveshare_session.lock().await;
                    if let Some(idx) = sess
                        .annotation_groups
                        .iter()
                        .position(|group| group.name == group_name)
                    {
                        sess.annotation_groups.remove(idx);

                        Ok(TonicResponse::new(RemoveAnnotationGroupResponse {
                            name: group_name,
                        }))
                    } else {
                        Err(Status::internal(
                            "Couldn't find the group and therefore can't delete it.",
                        ))
                    }
                }
                None => Err(Status::internal(
                    "Couldn't find associated liveshare session",
                )),
            }
        } else {
            Err(Status::internal("Couldn't find file index"))
        }
    }
    async fn remove_annotation(
        &self,
        request: TonicRequest<AnnotationRemovalRequest>,
    ) -> Result<TonicResponse<AnnotationRemovalResponse>, Status> {
        let client = self.check_auth(&request).await?;
        let inner = request.into_inner();
        let id_to_remove = ron::de::from_bytes::<egui::Id>(&inner.id)
            .map_err(|err| Status::from_error(Box::new(err)))?;

        match *client.file_index.lock().await {
            Some(_file_index) => match client.liveshare_session.lock().await.clone() {
                Some(liveshare_session) => {
                    let mut liveshare_session = liveshare_session.lock().await;
                    // Remove annotation
                    liveshare_session
                        .annotations
                        .retain(|annotation| annotation.get_id() != id_to_remove);
                    // Remove annotation from groups
                    for group in &mut liveshare_session.annotation_groups {
                        if let Some(idx) =
                            group.annotations.iter().position(|&id| id == id_to_remove)
                        {
                            group.cycle_counter = 0;
                            group.annotations.remove(idx);
                        }
                    }
                    liveshare_session
                        .broadcast_sender
                        .send(BroadcastClientMessage::RemoveAnnotation {
                            id: client.id,
                            annotation_id: inner.id,
                        })
                        .map_err(|err| Status::internal(err.to_string()))?;
                    Ok(TonicResponse::new(AnnotationRemovalResponse {}))
                }
                None => Err(Status::internal(
                    "liveshare session that you joined dont exist",
                )),
            },
            None => Err(Status::failed_precondition(
                "client is not in a liveshare session, should be if calling add_group",
            )),
        }
    }

    async fn get_annotation_list(
        &self,
        request: TonicRequest<AnnotationListRequest>,
    ) -> Result<TonicResponse<AnnotationListResponse>, Status> {
        let _client = self.check_auth(&request).await?;

        let inner = request.into_inner();

        match self.liveshare_sessions.get(&(inner.file_index as usize)) {
            Some(liveshare_session) => {
                let sess = liveshare_session.lock().await;
                let annotation_bytes = ron::ser::to_string::<Vec<Annotation>>(&sess.annotations)
                    .unwrap()
                    .into_bytes(); // TODO: Remove unwrap

                let annotation_group_bytes =
                    ron::ser::to_string::<Vec<AnnotationGroup>>(&sess.annotation_groups)
                        .unwrap()
                        .into_bytes();

                Ok(TonicResponse::new(AnnotationListResponse {
                    annotations: annotation_bytes,
                    annotion_groups: annotation_group_bytes,
                }))
            }
            None => Err(Status::internal(
                "liveshare session that you joined dont exist",
            )),
        }
    }

    async fn create_or_change_annotation(
        &self,
        request: TonicRequest<NewAnnotationRequest>,
    ) -> Result<TonicResponse<NewAnnotationResponse>, Status> {
        let client = self.check_auth(&request).await?;
        let inner = request.into_inner();

        match *client.file_index.lock().await {
            Some(_file_index) => match client.liveshare_session.lock().await.clone() {
                Some(liveshare_session) => {
                    let mut sess = liveshare_session.lock().await;
                    let mut annotation_ref = sess.next_annotation_id.lock().await;
                    *annotation_ref += 1;
                    let id = *annotation_ref;
                    drop(annotation_ref);

                    let anno = ron::de::from_bytes::<Annotation>(&inner.data).unwrap(); // TODO: No unwrap
                    let new_anno = match anno {
                        Arrow(mut arrow) => {
                            arrow.annotation_data = AnnotationData::new(
                                egui::Id::new(("annotation", id)),
                                arrow.annotation_data.name,
                                id as i32,
                            );
                            Annotation::Arrow(arrow)
                        }
                        Rect(mut rect) => {
                            rect.annotation_data = AnnotationData::new(
                                egui::Id::new(("annotation", id)),
                                rect.annotation_data.name,
                                id as i32,
                            );
                            Annotation::Rect(rect)
                        }
                    };
                    let new_id = match &new_anno {
                        Arrow(a) => a.get_id(),
                        Rect(r) => r.get_id(),
                    };
                    let _cpy = new_id;

                    sess.annotations.push(new_anno.clone());
                    sess.add_annotation_to_group(String::from("Ungrouped"), new_id);

                    let data = ron::ser::to_string::<Annotation>(&new_anno)
                        .unwrap()
                        .into_bytes(); // TODO: No unwrap
                    let _ = sess
                        .broadcast_sender
                        .send(BroadcastClientMessage::Annotation {
                            data: data.clone(),
                            annotation_id: id,
                            id: client.id,
                        });
                    Ok(TonicResponse::new(NewAnnotationResponse {
                        data,
                        creator_user_id: 0,
                        creators_local_annotation_id: id,
                    }))
                }
                None => Err(Status::internal(
                    "liveshare session that you joined dont exist",
                )),
            },
            None => Err(Status::failed_precondition(
                "client is not in a liveshare session, should be if calling add_group",
            )),
        }
    }
    async fn remove_items(
        &self,
        request: TonicRequest<RemoveItemsRequest>,
    ) -> Result<TonicResponse<RemoveItemsResponse>, Status> {
        let client = self.check_auth(&request).await?;
        let inner = request.into_inner();

        match *client.file_index.lock().await {
            Some(_file_index) => match client.liveshare_session.lock().await.clone() {
                Some(liveshare_session) => {
                    let mut liveshare_session = liveshare_session.lock().await;

                    // Remove items
                    info!("removing items: {:?}", inner.item_refs);
                    let item_refs = inner
                        .item_refs
                        .iter()
                        .map(|item_u64| DisplayedItemRef(*item_u64 as usize))
                        .sorted_unstable_by(|a, b| Ord::cmp(b, a));
                    for id in item_refs {
                        //waves.remove_displayed_item(*id);

                        let Some(idx) = liveshare_session
                            .item_tree
                            .iter()
                            .enumerate()
                            .find(|(_, node)| node.item_ref == id)
                            .map(|(idx, _)| ItemIndex(idx))
                        else {
                            continue;
                        };

                        for _removed_ref in liveshare_session.item_tree.remove_recursive(idx) {
                            /*if let Some(DisplayedItem::Marker(m)) =
                                liveshare_session.items.remove(&removed_ref)
                            {
                                self.markers.remove(&m.idx);
                            }*/

                            //self.annotations.retain(|annotation| !annotation.is_attached(&removed_ref)); TODO: add annotation
                        }
                    }

                    let tree_bytes = match ron::ser::to_string::<DisplayedItemTree>(
                        &liveshare_session.item_tree,
                    ) {
                        Ok(tree_str) => Ok(tree_str.into_bytes()),
                        Err(err) => Err(Status::from_error(Box::new(err))),
                    }?;

                    liveshare_session
                        .broadcast_sender
                        .send(BroadcastClientMessage::DisplayedItemTreeChange {
                            item_tree: tree_bytes.clone(),
                        })
                        .map_err(|err| Status::internal(err.to_string()))?;

                    Ok(TonicResponse::new(RemoveItemsResponse {}))
                }
                None => Err(Status::internal(
                    "liveshare session that you joined dont exist",
                )),
            },
            None => Err(Status::failed_precondition(
                "client is not in a liveshare session, should be if calling add_group",
            )),
        }
    }

    async fn add_group(
        &self,
        request: TonicRequest<AddGroupRequest>,
    ) -> Result<TonicResponse<DisplayedItemChangeResponse>, Status> {
        let client = self.check_auth(&request).await?;
        let inner = request.into_inner();

        match *client.file_index.lock().await {
            Some(_file_index) => match client.liveshare_session.lock().await.clone() {
                Some(liveshare_session) => {
                    let mut liveshare_session = liveshare_session.lock().await;
                    let mut item_ref_guard = liveshare_session.next_display_item_ref.lock().await;

                    *item_ref_guard += 1;
                    let item_ref = DisplayedItemRef(*item_ref_guard as usize);
                    drop(item_ref_guard);

                    let item = ron::de::from_bytes::<DisplayedItem>(&inner.displayed_item)
                        .map_err(|e| Status::from_error(Box::new(e)))?;
                    let target_pos = ron::de::from_bytes::<TargetPosition>(&inner.target_position)
                        .map_err(|e| Status::from_error(Box::new(e)))?;

                    liveshare_session
                        .item_tree
                        .insert_item(item_ref, target_pos)
                        .map_err(|err| {
                            Status::cancelled(format!("couldn't insert item: {err:?}")) // Todo: reset/wait with item ref drop?
                        })?;
                    liveshare_session.displayed_items.insert(item_ref, item);

                    let item_refs = inner
                        .item_refs
                        .iter()
                        .map(|item_ref| DisplayedItemRef(*item_ref as usize))
                        .collect::<Vec<DisplayedItemRef>>();

                    let item_idxs = liveshare_session
                        .item_tree
                        .iter()
                        .enumerate()
                        .filter_map(|(idx, node)| {
                            item_refs
                                .contains(&node.item_ref)
                                .then_some(crate::displayed_item_tree::ItemIndex(idx))
                        })
                        .collect::<Vec<_>>();

                    if let Err(e) = liveshare_session.item_tree.move_items(
                        item_idxs,
                        crate::displayed_item_tree::TargetPosition {
                            before: ItemIndex(target_pos.before.0 + 1), // TODO: why is +2 needed when +1 on client?
                            level: target_pos.level.saturating_add(1),
                        },
                    ) {
                        error!("failed to move items into group: {e:?}");
                    }

                    let tree_bytes = match ron::ser::to_string::<DisplayedItemTree>(
                        &liveshare_session.item_tree,
                    ) {
                        Ok(tree_str) => Ok(tree_str.into_bytes()),
                        Err(err) => Err(Status::from_error(Box::new(err))),
                    }?;

                    liveshare_session
                        .broadcast_sender
                        .send(BroadcastClientMessage::DisplayedItemChange {
                            displayed_item_ref: item_ref.0 as u64,
                            displayed_item_change: inner.displayed_item.clone(),
                            id: client.id,
                            target_position: Some(inner.target_position.clone()),
                            tree: Some(tree_bytes.clone()),
                        })
                        .map_err(|err| Status::internal(err.to_string()))?;

                    Ok(TonicResponse::new(DisplayedItemChangeResponse {
                        displayed_item_ref: item_ref.0 as u64,
                        displayed_item_change: inner.displayed_item,
                        displayed_item_tree: Some(tree_bytes),
                        target_position: Some(inner.target_position),
                    }))
                }
                None => Err(Status::internal(
                    "liveshare session that you joined dont exist",
                )),
            },
            None => Err(Status::failed_precondition(
                "client is not in a liveshare session, should be if calling add_group",
            )),
        }
    }
    async fn get_full_display_tree(
        &self,
        request: TonicRequest<FullDisplayTreeRequest>,
    ) -> Result<TonicResponse<FullDisplayTreeResponse>, Status> {
        let client = self.check_auth(&request).await?;
        let inner = request.into_inner();

        *client.file_index.lock().await = Some(inner.file_index as usize);
        let liveshare_session = self
            .liveshare_sessions
            .get(&(inner.file_index as usize))
            .ok_or(Status::not_found(format!(
                "file/liveshare session: {} not found",
                inner.file_index
            )))?
            .lock()
            .await;

        let item_tree_bytes = {
            match ron::to_string::<DisplayedItemTree>(&liveshare_session.item_tree)
                .context("serializing item tree for liveshare")
            {
                Ok(res) => Ok(res.into_bytes()),
                Err(_e) => Err(Status::invalid_argument(
                    "Couldn't serialize liveshare sess",
                )),
            }
        }?;
        let display_items = {
            match ron::to_string::<HashMap<DisplayedItemRef, DisplayedItem>>(
                &liveshare_session.displayed_items,
            )
            .context("serializing item tree for liveshare")
            {
                Ok(res) => Ok(res.into_bytes()),
                Err(_e) => Err(Status::invalid_argument(
                    "Couldn't serialize liveshare sess",
                )),
            }
        }?;

        Ok(TonicResponse::new(FullDisplayTreeResponse {
            item_tree: item_tree_bytes,
            items: display_items,
        }))
    }
    async fn change_variable_order(
        &self,
        request: TonicRequest<DragVariablesRequest>,
    ) -> Result<TonicResponse<DisplayedItemTreeResponse>, Status> {
        let client = self.check_auth(&request).await?;
        let message = request.into_inner();
        let target_pos = ron::de::from_bytes::<TargetPosition>(&message.target_position)
            .map_err(|e| Status::from_error(Box::new(e)))?;

        let to_move = ron::de::from_bytes::<Vec<ItemIndex>>(&message.variables)
            .map_err(|e| Status::from_error(Box::new(e)))?;

        info!("Changing variable order on server");

        match *client.file_index.lock().await {
            Some(_file_index) => {
                //
                match client.liveshare_session.lock().await.clone() {
                    Some(liveshare_session) => {
                        //
                        let mut liveshare_session = liveshare_session.lock().await;

                        let _ = liveshare_session.item_tree.move_items(to_move, target_pos);
                        let item_tree_bytes = {
                            match ron::to_string::<DisplayedItemTree>(&liveshare_session.item_tree)
                                .context("serializing item tree for liveshare")
                            {
                                Ok(res) => Ok(res.into_bytes()),
                                Err(_e) => Err(Status::invalid_argument(
                                    "Couldn't serialize liveshare sess",
                                )),
                            }
                        }?;
                        liveshare_session
                            .broadcast_sender
                            .send(BroadcastClientMessage::DisplayedItemTreeChange {
                                item_tree: item_tree_bytes.clone(),
                            })
                            .map_err(|err| Status::internal(format!("server problem {}", err)))?;
                        Ok(TonicResponse::new(DisplayedItemTreeResponse {
                            items_tree: item_tree_bytes,
                        }))
                    }
                    None => Err(Status::internal(
                        "liveshare session that you joined dont exist",
                    )),
                }
            }
            None => Err(Status::failed_precondition(
                "client hasn't joined a session yet",
            )),
        }
    }

    async fn authenticate_user(
        &self,
        request: TonicRequest<AutheniticationRequest>,
    ) -> Result<TonicResponse<AutheniticationResponse>, Status> {
        // TODO: Validate the token by using the authenticationrequest field
        // self.validate_server_token(&request).await?;
        let _connected_ip = request.local_addr(); //local or remote addr??
        let message = request.into_inner();

        let mut next_free_id_guard = self.next_free_id.lock().await;
        let id = *next_free_id_guard;
        *next_free_id_guard += 1;
        drop(next_free_id_guard);

        let username = message.username;

        let session_id = Uuid::new_v4().to_string();

        let client = Arc::new(Client {
            id,
            username: username.clone(),
            session_id: session_id.clone(),
            file_index: Mutex::new(None),
            liveshare_session: Mutex::new(None),
        });

        let mut session_id_map_guard = self.session_id_map.lock().await;
        session_id_map_guard.insert(session_id.clone(), client.id);
        drop(session_id_map_guard);

        let mut users = self.users.lock().await;
        users.insert(id, client);
        info!("New user joined, current user map: ");
        for user in users.values() {
            let username = user.username.as_str();
            let client_id = user.id;
            let session_id = user.session_id.as_str();
            let file = user.file_index.lock().await;
            info!(
                "username: {username}, client_id: {client_id}, session_id: {session_id}, file: {file:?}"
            );
        }

        drop(users);

        Ok(TonicResponse::new(AutheniticationResponse {
            session_id,
            username,
            id,
        }))
    }
    async fn get_info_page(
        &self,
        request: TonicRequest<InfoPageRequest>,
    ) -> Result<TonicResponse<InfoPageResponse>, Status> {
        self.check_auth(&request).await?;
        // valid token, but no command => return info
        let _ = request;
        info!("Loading info page");
        let body = get_info_page(&self.shared, &self.state);
        Ok(TonicResponse::new(InfoPageResponse {
            server_key: HTTP_SERVER_VALUE_SURFER.to_string(),
            body,
        }))
    }
    async fn get_status(
        &self,
        request: TonicRequest<StatusRequest>,
    ) -> Result<TonicResponse<StatusResponse>, Status> {
        //self.check_auth(&request).await?;
        let _ = request;

        match get_status(&self.state) {
            Ok(res) => Ok(TonicResponse::new(StatusResponse { body: res })),
            Err(e) => Err(Status::invalid_argument(e.to_string())), // TODO: fix better error
        }
    }

    async fn get_hierarchy(
        &self,
        request: TonicRequest<HierarchyRequest>,
    ) -> Result<TonicResponse<HierarchyResponse>, Status> {
        self.check_auth(&request).await?;
        match get_hierarchy(&self.state, request.into_inner().file_index as usize) {
            Ok(res) => Ok(TonicResponse::new(HierarchyResponse { body: res })),
            Err(e) => Err(Status::invalid_argument(e.to_string())), // TODO: fix better error
        }
    }

    async fn get_time_table(
        &self,
        request: TonicRequest<TimeTableRequest>,
    ) -> Result<TonicResponse<TimeTableResponse>, Status> {
        self.check_auth(&request).await?;
        match get_timetable(&self.state, request.into_inner().file_index as usize).await {
            Ok(res) => Ok(TonicResponse::new(TimeTableResponse { body: res })),
            Err(e) => Err(Status::invalid_argument(e.to_string())), // TODO: fix better error
        }
    }

    async fn get_signals(
        &self,
        request: TonicRequest<SignalRequests>,
    ) -> Result<TonicResponse<SignalResponses>, Status> {
        let client = self.check_auth(&request).await?;
        let inner = request.into_inner();
        info!(
            "Client: {} requesting signals: {:?} from file {}",
            client.id, inner.id_strings, inner.file_index
        );
        let str_slice: Vec<&str> = inner
            .id_strings
            .iter()
            .map(|string| string as &str)
            .collect();
        match get_signals(
            &self.state,
            inner.file_index as usize,
            &self.txs,
            &str_slice,
        )
        .await
        {
            Ok(body) => {
                //let shared_body = Arc::new(body);
                // if client.is_in_liveshare:  do broadcast, else just send signal
                if let Some(file_index) = *client.file_index.lock().await {
                    let _liveshare_session =
                        self.liveshare_sessions
                            .get(&file_index)
                            .ok_or(Status::not_found(format!(
                                "file: {} not found",
                                inner.file_index
                            )))?;

                    /*
                    liveshare_guard
                        .broadcast_sender
                        .send(BroadcastClientMessage::LoadSignal {
                            body: body.clone(),
                            id: client.id,
                        })
                        .map_err(|err| Status::internal(format!("server problem {}", err)))?;*/
                }
                Ok(TonicResponse::new(SignalResponses { body })) // TODO: maybe remove clone and send back nothing here? Also no sure if "body" will be consumed or where
            }
            Err(e) => Err(Status::invalid_argument(e.to_string())), // TODO: fix better error
        }
    }

    async fn reload(
        &self,
        request: TonicRequest<ReloadRequest>,
    ) -> Result<TonicResponse<ReloadResponse>, Status> {
        self.check_auth(&request).await?;
        let mut state_guard = self.state.write().expect("State lock poisoned in reload");
        let file_index = request.into_inner().file_index as usize;
        let file_info = &mut state_guard.file_infos[file_index];
        // Check file existence, size, and mtime
        let Ok(meta) = fs::metadata(file_info.filename.clone()) else {
            drop(state_guard);
            return Ok(TonicResponse::new(ReloadResponse {
                message_type: Some(MessageType::FileNotFound(FileNotFound {})),
            }));
            //return Err(Status::invalid_argument(String::from("wrong")));
        };
        let mtime = meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        // Should probably look at file lengths as well for extra safety, but they are not updated correctly at the moment
        let unchanged = file_info.last_modification_time == Some(mtime) && file_info.last_reload_ok;
        if unchanged {
            drop(state_guard);
            return Ok(TonicResponse::new(ReloadResponse {
                message_type: Some(MessageType::FileUnchanged(FileUnchanged {})),
            }));
        }
        file_info.last_modification_time = Some(mtime);
        info!(
            "File modification time updated to {}",
            file_info.modification_time_string()
        );
        file_info.reloading = true;
        file_info.last_reload_ok = false;
        drop(state_guard);
        info!("Reload requested");
        if self.txs[file_index].send(LoaderMessage::Reload).is_err() {
            error!("Server error: loader/receiver closed"); //TODO: better error message
        }

        match get_status(&self.state) {
            Ok(res) => Ok(TonicResponse::new(ReloadResponse {
                message_type: Some(MessageType::FileChanged(FileChanged { body: res })),
            })),
            Err(e) => {
                error!("Server error: loader/receiver closed"); //TODO: better error message
                return Err(Status::invalid_argument(e.to_string())); // TODO: fix better error
            }
        }
    }

    // Idea:
    // if connected, send the displayedItems to server.
    // Server sends back full displayitemtree
    // Client requests signals not loaded.

    async fn change_display_item(
        &self,
        request: TonicRequest<DisplayedItemChange>,
    ) -> Result<TonicResponse<DisplayedItemChangeResponse>, Status> {
        let client = self.check_auth(&request).await?;
        let message = request.into_inner();

        match *client.file_index.lock().await {
            Some(_file_index) => {
                //
                //let mut target_posi = None;
                match client.liveshare_session.lock().await.clone() {
                    Some(liveshare_session) => {
                        //
                        let displayed_item = match ron::de::from_bytes::<DisplayedItem>(
                            &message.displayed_item_change,
                        ) {
                            Ok(item) => Ok(item),
                            Err(_e) => Err(tonic::Status::new(
                                200.into(),
                                "Error parsing displayed item",
                            )),
                        }?;

                        let mut liveshare_session = liveshare_session.lock().await;

                        let (displayed_item_ref, existing_item) = match message.displayed_item_ref {
                            Some(displayed_item_ref) => (displayed_item_ref, true),
                            None => {
                                let mut id = liveshare_session.next_display_item_ref.lock().await;
                                *id += 1;
                                (*id, false)
                            }
                        };
                        liveshare_session.displayed_items.insert(
                            DisplayedItemRef(displayed_item_ref as usize),
                            displayed_item,
                        );
                        // this acts as a "if change_item_tree {"
                        let displayed_item_tree = match message.target_position.clone() {
                            Some(target_pos_bytes) => {
                                let target_pos =
                                    ron::de::from_bytes::<TargetPosition>(&target_pos_bytes)
                                        .map_err(|e| Status::from_error(Box::new(e)))?;

                                if !existing_item {
                                    info!("Inserting item");
                                    let _ = liveshare_session.item_tree.insert_item(
                                        DisplayedItemRef(displayed_item_ref as usize),
                                        target_pos,
                                    );
                                }
                                let item_tree_bytes = ron::ser::to_string::<DisplayedItemTree>(
                                    &liveshare_session.item_tree,
                                )
                                .map_err(|e| Status::from_error(Box::new(e)))?
                                .into();
                                Some(item_tree_bytes)
                            }
                            None => None,
                        };

                        let displayed_item_change = message.displayed_item_change;
                        let copy = displayed_item_change.clone();
                        liveshare_session
                            .broadcast_sender
                            .send(BroadcastClientMessage::DisplayedItemChange {
                                displayed_item_ref,
                                displayed_item_change,
                                id: client.id,
                                target_position: message.target_position.clone(),
                                tree: displayed_item_tree.clone(),
                            })
                            .map_err(|err| Status::internal(format!("server problem {}", err)))?;

                        Ok(TonicResponse::new(DisplayedItemChangeResponse {
                            displayed_item_ref,
                            displayed_item_change: copy,
                            displayed_item_tree,
                            target_position: message.target_position,
                        }))
                    }
                    None => Err(Status::internal("Not in a liveshare session")),
                }
            }
            None => Err(Status::failed_precondition(
                "client hasn't joined a session yet",
            )),
        }
    }

    async fn next_item_ref(
        &self,
        request: TonicRequest<ItemRefRequest>,
    ) -> Result<TonicResponse<ItemRefResponse>, Status> {
        let client = self.check_auth(&request).await?;
        match *client.file_index.lock().await {
            Some(_file_index) => {
                //
                match client.liveshare_session.lock().await.clone() {
                    Some(liveshare_session) => {
                        let liveshare_session = liveshare_session.lock().await;
                        let mut item_ref_guard =
                            liveshare_session.next_display_item_ref.lock().await;
                        let item_ref = *item_ref_guard;
                        *item_ref_guard += 1;
                        drop(item_ref_guard);

                        Ok(TonicResponse::new(ItemRefResponse { item_ref }))
                    }
                    None => Err(Status::internal(
                        "liveshare session that you joined dont exist",
                    )),
                }
            }
            None => Err(Status::failed_precondition(
                "client is not in a liveshare session, should be if asking for item ref",
            )),
        }
    }

    async fn join_liveshare(
        &self,
        request: TonicRequest<LiveshareRequest>,
    ) -> Result<TonicResponse<Self::JoinLiveshareStream>, Status> {
        let client = self.check_auth(&request).await?;
        let inner = request.into_inner();

        *client.file_index.lock().await = Some(inner.file_index as usize);
        let this_session = self
            .liveshare_sessions
            .get(&(inner.file_index as usize))
            .ok_or(Status::not_found(format!(
                "file/liveshare session: {} not found",
                inner.file_index
            )))?
            .clone();
        let mut session_guard = this_session.lock().await;

        let this_id = client.id;

        session_guard.clients.insert(client.id, client.clone());
        *client.liveshare_session.lock().await = Some(this_session.clone());

        //client.receiver; // = Some(liveshare_session.broadcast_sender.subscribe());

        let (output_sender, output_receiver) =
            tokio::sync::mpsc::channel::<ResponseStreamItem>(128);

        for (id, client) in session_guard.clients.iter() {
            if let Err(err) = output_sender
                .send(Ok(ActionResponse {
                    response_type: Some(ResponseType::NewClientJoinedResponse(
                        NewClientJoinedResponse {
                            id: *id,
                            username: client.username.clone(),
                        },
                    )),
                }))
                .await
            {
                info!("Error when sending userlist: {err}");
            }
        }
        // check if client has disconnected while sending
        /*async fn checked_send(
            sender: Sender<ResponseStreamItem>,
            item: ResponseStreamItem,
        ) -> Result<(), Status> {
            if sender.send::<ResponseStreamItem>(item).await.is_err() {
                tracing::info!("Client {} disconnected", this_id);
            };
            Ok(())
        }*/

        session_guard
            .broadcast_sender
            .send(BroadcastClientMessage::NewUser {
                username: client.username.clone(),
                id: client.id,
            })
            .map_err(|err| {
                Status::internal(format!("failed to announce that you joined: {:?}", err))
            })?;

        let mut broadcast_receiver = session_guard.broadcast_sender.subscribe();
        let broadcast_sender = session_guard.broadcast_sender.clone();
        drop(session_guard);

        spawn(async move {
            loop {
                match broadcast_receiver.recv().await {
                    Ok(message) => {
                        //

                        if let Err(_send_err) = match message {
                            BroadcastClientMessage::DisplayedItemTreeChange { item_tree } => {
                                output_sender
                                    .send(Ok(ActionResponse {
                                        response_type: Some(ResponseType::ItemTreeChange(
                                            DisplayedItemTreeResponse {
                                                items_tree: item_tree,
                                            },
                                        )),
                                    }))
                                    .await
                            }
                            BroadcastClientMessage::NewUser { username, id } => {
                                output_sender
                                    .send(Ok(ActionResponse {
                                        response_type: Some(ResponseType::NewClientJoinedResponse(
                                            NewClientJoinedResponse { id, username },
                                        )),
                                    }))
                                    .await
                            }
                            BroadcastClientMessage::LeavingUser { id } => {
                                output_sender
                                    .send(Ok(ActionResponse {
                                        response_type: Some(ResponseType::ClientLeftResponse(
                                            ClientLeftResponse { id },
                                        )),
                                    }))
                                    .await
                            }
                            BroadcastClientMessage::DisplayedItemChange {
                                displayed_item_ref,
                                displayed_item_change,
                                id,
                                target_position,
                                tree,
                            } => {
                                if id != this_id {
                                    output_sender
                                        .send(Ok(ActionResponse {
                                            response_type: Some(
                                                ResponseType::DisplayedItemChangeOrAdd(
                                                    DisplayedItemChangeResponse {
                                                        displayed_item_ref,
                                                        displayed_item_change,
                                                        target_position,
                                                        displayed_item_tree: tree,
                                                    },
                                                ),
                                            ),
                                        }))
                                        .await
                                } else {
                                    Ok(())
                                }
                            }
                            BroadcastClientMessage::RemoveAnnotationComment {
                                id,
                                annotation_id,
                                comment_id,
                            } => {
                                if id != this_id {
                                    output_sender
                                        .send(Ok(ActionResponse {
                                            response_type: Some(
                                                ResponseType::RemoveAnnotationComment(
                                                    RemoveAnnotationCommentResponse {
                                                        annotation_id,
                                                        comment_id,
                                                    },
                                                ),
                                            ),
                                        }))
                                        .await
                                } else {
                                    Ok(())
                                }
                            }
                            BroadcastClientMessage::AddAnnotationComment {
                                id,
                                annotation_id,
                                message,
                                username,
                            } => {
                                if id != this_id {
                                    output_sender
                                        .send(Ok(ActionResponse {
                                            response_type: Some(
                                                ResponseType::AddAnnotationComment(
                                                    AddAnnotationCommentResponse {
                                                        annotation_id,
                                                        message,
                                                        username,
                                                    },
                                                ),
                                            ),
                                        }))
                                        .await
                                } else {
                                    Ok(())
                                }
                            }
                            BroadcastClientMessage::AddAnnotationGroup { id, name } => {
                                if id != this_id {
                                    output_sender
                                        .send(Ok(ActionResponse {
                                            response_type: Some(ResponseType::AddAnnotationGroup(
                                                AddAnnotationGroupResponse { name },
                                            )),
                                        }))
                                        .await
                                } else {
                                    Ok(())
                                }
                            }
                            BroadcastClientMessage::Annotation {
                                data,
                                annotation_id,
                                id,
                            } => {
                                if id != this_id {
                                    output_sender
                                        .send(Ok(ActionResponse {
                                            response_type: Some(
                                                ResponseType::NewAnnotationResponse(
                                                    NewAnnotationResponse {
                                                        data,
                                                        creator_user_id: 0,
                                                        creators_local_annotation_id: annotation_id,
                                                    },
                                                ),
                                            ),
                                        }))
                                        .await
                                } else {
                                    Ok(())
                                }
                            }
                            BroadcastClientMessage::RemoveAnnotation { id, annotation_id } => {
                                if id != this_id {
                                    output_sender
                                        .send(Ok(ActionResponse {
                                            response_type: Some(ResponseType::RemoveAnnotation(
                                                RemoveAnnotation { id: annotation_id },
                                            )),
                                        }))
                                        .await
                                } else {
                                    Ok(())
                                }
                            }
                            BroadcastClientMessage::UpdateAnnotationGroup {
                                id,
                                annotation_id,
                                group_name,
                            } => {
                                if id != this_id {
                                    output_sender
                                        .send(Ok(ActionResponse {
                                            response_type: Some(
                                                ResponseType::UpdateAnnotationGroup(
                                                    UpdateAnnotationGroupResponse {
                                                        annotation_id,
                                                        group_name,
                                                    },
                                                ),
                                            ),
                                        }))
                                        .await
                                } else {
                                    Ok(())
                                }
                            }
                        } {
                            info!("Client {} disconnected", this_id);

                            let mut session_guard = this_session.lock().await;
                            session_guard.clients.remove(&this_id);
                            drop(session_guard);

                            if let Err(err) = broadcast_sender
                                .send(BroadcastClientMessage::LeavingUser { id: this_id })
                            {
                                info!("error broadcasting client disconnection: {}", err);
                            }
                            break;
                        }
                    }
                    Err(err) => {
                        if let Err(_send_err) = output_sender
                            .send(Err(Status::internal(format!("broadcast broke: {}", err))))
                            .await
                        {
                            info!("Client {} disconnected", this_id);
                        }
                        break;
                    }
                }
            }
        });

        let out_stream = ReceiverStream::new(output_receiver);

        Ok(TonicResponse::new(
            Box::pin(out_stream) as Self::JoinLiveshareStream
        ))
    }
}

struct ReadOnly {
    url: String,
    token: String,
}

struct FileInfo {
    filename: String,
    hierarchy: Arc<Hierarchy>,
    file_format: FileFormat,
    header_len: u64,
    body_len: u64,
    body_progress: Arc<AtomicU64>,
    notify: Arc<Notify>,
    timetable: Vec<Time>,
    signals: HashMap<SignalRef, Signal>,
    reloading: bool,
    requested_in_session: bool,
    last_reload_ok: bool,
    last_reload_time: Option<Instant>,
    last_modification_time: Option<SystemTime>,
}

#[derive(Default)]
struct SurverState {
    file_infos: Vec<FileInfo>,
}

impl FileInfo {
    fn modification_time_string(&self) -> String {
        modification_time_string(self.last_modification_time)
    }

    fn reload_time_string(&self) -> String {
        if let Some(time) = self.last_reload_time {
            return format!("{:?} ago", time.elapsed());
        }
        "never".to_string()
    }

    pub fn html_table_line(&self) -> String {
        let bytes_loaded = self.body_progress.load(Ordering::SeqCst);

        let progress = if bytes_loaded == self.body_len {
            format!(
                "{} loaded",
                bytesize::ByteSize::b(self.body_len + self.header_len)
            )
        } else {
            format!(
                "{} / {}",
                bytesize::ByteSize::b(bytes_loaded + self.header_len),
                bytesize::ByteSize::b(self.body_len + self.header_len)
            )
        };

        format!(
            "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
            self.filename,
            progress,
            self.modification_time_string(),
            self.reload_time_string()
        )
    }
}

impl From<&FileInfo> for SurverFileInfo {
    fn from(file_info: &FileInfo) -> Self {
        Self {
            bytes: file_info.body_len + file_info.header_len,
            bytes_loaded: file_info.body_progress.load(Ordering::SeqCst) + file_info.header_len,
            filename: file_info.filename.clone(),
            format: file_info.file_format,
            reloading: file_info.reloading,
            last_load_ok: file_info.last_reload_ok,
            last_modification_time: file_info.last_modification_time,
        }
    }
}
enum LoaderMessage {
    SignalRequest(SignalRequest),
    Reload,
}

type SignalRequest = Vec<SignalRef>;

fn get_info_page(shared: &Arc<ReadOnly>, state: &Arc<RwLock<SurverState>>) -> String {
    let state_guard = state.read().expect("State lock poisoned in get_info_page");
    let html_table_content = state_guard
        .file_infos
        .iter()
        .map(FileInfo::html_table_line)
        .collect::<Vec<_>>()
        .join("\n");
    drop(state_guard);

    format!(
        r#"
    <!DOCTYPE html><html lang="en">
    <head>
    <link rel="icon" href="favicon.ico" sizes="any">
    <title>Surver - Surfer Remote Server</title>
    </head>
    <body>
    <h1>Surver - Surfer Remote Server</h1>
    <b>To connect, run:</b> <code>surfer {}</code><br>
    <b>Wellen version:</b> {WELLEN_VERSION}<br>
    <b>Surfer version:</b> {SURFER_VERSION}<br>
    <table border="1" cellpadding="5" cellspacing="0">
    <tr><th>Filename</th><th>Load progress</th><th>File modification time</th><th>(Re)load time</th></tr>
    {}
    </table>
    </body></html>
    "#,
        shared.url, html_table_content
    )
}

fn get_hierarchy(state: &Arc<RwLock<SurverState>>, file_index: usize) -> Result<Vec<u8>> {
    let state_guard = state.read().expect("State lock poisoned in get_hierarchy");
    let file_info = &state_guard.file_infos[file_index];
    let mut raw = BINCODE_OPTIONS.serialize(&file_info.file_format)?;
    let mut raw2 = BINCODE_OPTIONS.serialize(file_info.hierarchy.as_ref())?;
    drop(state_guard);
    raw.append(&mut raw2);
    let compressed = lz4_flex::compress_prepend_size(&raw);
    info!(
        "Sending hierarchy. {} raw, {} compressed.",
        bytesize::ByteSize::b(raw.len() as u64),
        bytesize::ByteSize::b(compressed.len() as u64)
    );
    Ok(compressed)
}

async fn get_timetable(state: &Arc<RwLock<SurverState>>, file_index: usize) -> Result<Vec<u8>> {
    let notify = {
        let state_guard = state.read().expect("State lock poisoned in get_timetable");
        state_guard.file_infos[file_index].notify.clone()
    };

    // Wait until the time table is available
    let table = loop {
        {
            let state_guard = state.read().expect("State lock poisoned in get_timetable");
            let timetable = &state_guard.file_infos[file_index].timetable;
            if !timetable.is_empty() {
                break timetable.clone();
            }
        }

        notify.notified().await;
    };

    let raw_size = table.len() * std::mem::size_of::<Time>();
    let compressed = BINCODE_OPTIONS.serialize(&CompressedTimeTable::compress(&table))?;
    info!(
        "Sending timetable. {} raw, {} compressed.",
        bytesize::ByteSize::b(raw_size as u64),
        bytesize::ByteSize::b(compressed.len() as u64)
    );
    Ok(compressed)
}

fn get_status(state: &Arc<RwLock<SurverState>>) -> Result<Vec<u8>> {
    let state_guard = state.read().expect("State lock poisoned in get_status");
    let file_infos = state_guard
        .file_infos
        .iter()
        .map(SurverFileInfo::from)
        .collect::<Vec<_>>();
    drop(state_guard);
    let status = SurverStatus {
        wellen_version: WELLEN_VERSION.to_string(),
        surfer_version: SURFER_VERSION.to_string(),
        file_infos,
    };
    Ok(serde_json::to_vec(&status)?)
}

async fn get_signals(
    state: &Arc<RwLock<SurverState>>,
    file_index: usize,
    txs: &[Sender<LoaderMessage>],
    id_strings: &[&str],
) -> Result<Vec<u8>> {
    let ids = id_strings
        .iter()
        .map(|id_str| {
            id_str
                .parse::<u64>()
                .map_err(|e| anyhow!("Failed to parse signal id `{id_str}`: {e:#}"))
                .and_then(|index| {
                    SignalRef::from_index(index as usize)
                        .ok_or_else(|| anyhow!("Invalid signal index: {}", index))
                })
        })
        .collect::<Result<Vec<SignalRef>>>()?;

    if ids.is_empty() {
        return Ok(vec![]);
    }
    let num_ids = ids.len();

    // send request to background thread
    txs[file_index].send(LoaderMessage::SignalRequest(ids.clone()))?;

    let notify = {
        let state_guard = state.read().expect("State lock poisoned in get_signals");
        state_guard.file_infos[file_index].notify.clone()
    };

    // Wait for all signals to be loaded
    let mut data = vec![];
    leb128::write::unsigned(&mut data, num_ids as u64)?;
    let mut raw_size = 0;
    loop {
        {
            let state_guard = state.read().expect("State lock poisoned in get_signals");
            if ids
                .iter()
                .all(|id| state_guard.file_infos[file_index].signals.contains_key(id))
            {
                for id in ids {
                    let signal = &state_guard.file_infos[file_index].signals[&id];
                    raw_size += BINCODE_OPTIONS.serialize(signal)?.len();
                    let comp = CompressedSignal::compress(signal);
                    data.append(&mut BINCODE_OPTIONS.serialize(&comp)?);
                }
                break;
            }
        };
        // Wait for notification that signals have been loaded
        notify.notified().await;
    }
    info!(
        "Sending {} signals. {} raw, {} compressed.",
        num_ids,
        bytesize::ByteSize::b(raw_size as u64),
        bytesize::ByteSize::b(data.len() as u64)
    );
    Ok(data)
}

const CONTENT_TYPE: &str = "Content-Type";
const JSON_MIME: &str = "application/json";
const OCTET_MIME: &str = "application/octet-stream";
const HTML_MIME: &str = "text/html; charset=utf-8";

trait DefaultHeader {
    fn default_header(self) -> Self;
}

impl DefaultHeader for hyper::http::response::Builder {
    fn default_header(self) -> Self {
        self.header(HTTP_SERVER_KEY, HTTP_SERVER_VALUE_SURFER)
            .header(X_WELLEN_VERSION, WELLEN_VERSION)
            .header(X_SURFER_VERSION, SURFER_VERSION)
            .header("Cache-Control", "no-cache")
    }
}

fn build_response(
    status: StatusCode,
    content_type: &str,
    body: Vec<u8>,
) -> Result<Response<Full<Bytes>>> {
    Ok(Response::builder()
        .status(status)
        .header(CONTENT_TYPE, content_type)
        .default_header()
        .body(Full::from(body))?)
}

fn not_found_response(message: &[u8]) -> Result<Response<Full<Bytes>>> {
    build_response(StatusCode::NOT_FOUND, OCTET_MIME, message.to_vec())
}

fn mark_file_requested(state: &Arc<RwLock<SurverState>>, file_index: usize) {
    let mut state_guard = state
        .write()
        .expect("State lock poisoned in request tracking");
    state_guard.file_infos[file_index].requested_in_session = true;
}

fn handle_reload_cmd(
    state: &Arc<RwLock<SurverState>>,
    txs: &[Sender<LoaderMessage>],
    file_index: usize,
) -> Result<Response<Full<Bytes>>> {
    let mtime = {
        let state_guard = state
            .read()
            .expect("State lock poisoned in reload before metadata");
        // Read metadata before taking the write lock to minimize lock contention.
        let Ok(meta) = fs::metadata(&state_guard.file_infos[file_index].filename) else {
            return not_found_response(b"error: file not found");
        };
        meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH)
    };

    let mut state_guard = state.write().expect("State lock poisoned in reload");
    let file_info = &mut state_guard.file_infos[file_index];

    // Should probably look at file lengths as well for extra safety, but they are not updated correctly at the moment
    let unchanged = file_info.last_modification_time == Some(mtime) && file_info.last_reload_ok;
    if unchanged {
        if file_info.requested_in_session {
            drop(state_guard);
            return build_response(
                StatusCode::NOT_MODIFIED,
                JSON_MIME,
                b"info: file unchanged".to_vec(),
            );
        }
        // If file is unchanged but not yet requested in this session.
        // Probably a new Surver session is started, so return file.
        file_info.requested_in_session = true;
        drop(state_guard);
        let body = get_status(state)?;
        return build_response(StatusCode::ACCEPTED, JSON_MIME, body);
    }
    file_info.requested_in_session = true;
    file_info.last_modification_time = Some(mtime);
    info!(
        "File modification time updated to {}",
        file_info.modification_time_string()
    );
    file_info.reloading = true;
    file_info.last_reload_ok = false;
    drop(state_guard);
    info!("Reload requested");
    txs[file_index].send(LoaderMessage::Reload)?;
    let body = get_status(state)?;
    build_response(StatusCode::ACCEPTED, JSON_MIME, body)
}

async fn handle_cmd(
    state: &Arc<RwLock<SurverState>>,
    txs: &[Sender<LoaderMessage>],
    cmd: &str,
    file_index: Option<usize>,
    args: &[&str],
) -> Result<Response<Full<Bytes>>> {
    // Check file index is valid if provided
    if let Some(file_index) = file_index {
        let state_guard = state.read().expect("State lock poisoned in handle_cmd");
        if file_index >= state_guard.file_infos.len() {
            drop(state_guard);
            return not_found_response(b"Invalid file index");
        }
    }
    match (file_index, cmd, args) {
        (_, "get_status", []) => {
            let body = get_status(state)?;
            build_response(StatusCode::OK, JSON_MIME, body)
        }
        (Some(file_index), "get_hierarchy", []) => {
            mark_file_requested(state, file_index);
            let body = get_hierarchy(state, file_index)?;
            build_response(StatusCode::OK, OCTET_MIME, body)
        }
        (Some(file_index), "get_time_table", []) => {
            mark_file_requested(state, file_index);
            let body = get_timetable(state, file_index).await?;
            build_response(StatusCode::OK, OCTET_MIME, body)
        }
        (Some(file_index), "get_signals", id_strings) => {
            mark_file_requested(state, file_index);
            let body = get_signals(state, file_index, txs, id_strings).await?;
            build_response(StatusCode::OK, OCTET_MIME, body)
        }
        (Some(file_index), "reload", []) => handle_reload_cmd(state, txs, file_index),
        _ => {
            // unknown command or unexpected number of arguments
            not_found_response(&[])
        }
    }
}

async fn handle(
    state: Arc<RwLock<SurverState>>,
    shared: Arc<ReadOnly>,
    txs: Vec<Sender<LoaderMessage>>,
    req: Request<hyper::body::Incoming>,
) -> Result<Response<Full<Bytes>>> {
    // Check if favicon is requested
    if req.uri().path() == "/favicon.ico" {
        let favicon_data = include_bytes!("assets/favicon.ico");
        return Ok(Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "image/x-icon")
            .header("Cache-Control", "public, max-age=604800")
            .body(Full::from(&favicon_data[..]))?);
    }
    // check to see if the correct token was received
    let path_parts = req.uri().path().split('/').skip(1).collect::<Vec<_>>();

    // check token
    if let Some(provided_token) = path_parts.first() {
        if *provided_token != shared.token {
            warn!(
                "Received request with invalid token: {provided_token} != {}\n{:?}",
                shared.token,
                req.uri()
            );
            return not_found_response(&[]);
        }
    } else {
        // no token
        warn!("Received request with no token: {:?}", req.uri());
        return not_found_response(&[]);
    }

    // Try to parse file index from path_parts[1]
    let (file_index, cmd_idx) = path_parts
        .get(1)
        .and_then(|s| s.parse::<usize>().ok())
        .map_or((None, 1), |idx| (Some(idx), 2));
    // check command
    let response = if let Some(cmd) = path_parts.get(cmd_idx) {
        handle_cmd(&state, &txs, cmd, file_index, &path_parts[cmd_idx + 1..]).await?
    } else {
        // valid token, but no command => return info
        let body = Full::from(get_info_page(&shared, &state));
        Response::builder()
            .status(StatusCode::OK)
            .header(CONTENT_TYPE, HTML_MIME)
            .default_header()
            .body(body)?
    };

    Ok(response)
}

const MIN_TOKEN_LEN: usize = 8;
const RAND_TOKEN_LEN: usize = 24;

pub type ServerStartedFlag = Arc<std::sync::atomic::AtomicBool>;

pub async fn surver_main(
    port: u16,
    bind_address: String,
    token: Option<String>,
    filenames: &[String],
    started: Option<ServerStartedFlag>,
) -> Result<()> {
    // if no token was provided, we generate one
    let token = token.unwrap_or_else(|| {
        // generate a random ASCII token
        repeat_with(fastrand::alphanumeric)
            .take(RAND_TOKEN_LEN)
            .collect()
    });

    if token.len() < MIN_TOKEN_LEN {
        bail!("Token `{token}` is too short. At least {MIN_TOKEN_LEN} characters are required!");
    }

    let state = Arc::new(RwLock::new(SurverState { file_infos: vec![] }));

    let mut file_indexes: Vec<FileIndex> = Vec::new();
    let mut txs: Vec<Sender<LoaderMessage>> = Vec::new();
    // load files
    for (file_index, filename) in filenames.iter().enumerate() {
        let start_read_header = web_time::Instant::now();
        let header_result = wellen::viewers::read_header_from_file(
            filename.clone(),
            &WELLEN_SURFER_DEFAULT_OPTIONS,
        )
        .map_err(|e| anyhow!("{e:?}"))
        .with_context(|| format!("Failed to parse wave file: {filename}"))?;
        info!(
            "Loaded header of {filename} in {:?}",
            start_read_header.elapsed()
        );

        let file_info = FileInfo {
            filename: filename.clone(),
            hierarchy: Arc::new(header_result.hierarchy),
            file_format: header_result.file_format,
            header_len: 0, // FIXME: get value from wellen
            body_len: header_result.body_len,
            body_progress: Arc::new(AtomicU64::new(0)),
            notify: Arc::new(Notify::new()),
            timetable: vec![],
            signals: HashMap::new(),
            reloading: false,
            requested_in_session: false,
            last_reload_ok: true,
            last_reload_time: None,
            last_modification_time: None,
        };
        {
            let mut state_guard = state.write().expect("State lock poisoned when adding file");
            state_guard.file_infos.push(file_info);
        }
        // channel to communicate with loader
        let (tx, rx) = std::sync::mpsc::channel::<LoaderMessage>();
        txs.push(tx.clone());
        // start work thread
        let state_2 = state.clone();
        std::thread::spawn(move || loader(&state_2, header_result.body, file_index, &rx));
        file_indexes.push(file_index as FileIndex);
    }
    let ip_addr: std::net::IpAddr = bind_address
        .parse()
        .with_context(|| format!("Invalid bind address: {bind_address}"))?;
    let use_localhost = ip_addr.is_loopback();
    if !use_localhost {
        warn!(
            "Server is binding to {bind_address} instead of 127.0.0.1/0:0:0:0:0:0:0:1 (localhost)"
        );
        warn!("This may make the server accessible from external networks");
        warn!("Surver traffic is unencrypted and unauthenticated - use with caution!");
    }

    // immutable read-only data
    let addr = SocketAddr::new(ip_addr, port);
    let url = format!("http://{addr}/{token}");
    let url_copy = url.clone();
    let token_copy = token.clone();
    let shared = Arc::new(ReadOnly { url, token });

    // print out status
    info!("Starting server on {addr}. To use:");
    info!("1. Setup an ssh tunnel: -L {port}:localhost:{port}");
    let hostname = whoami::hostname();
    if let Ok(hostname) = hostname.as_ref()
        && hostname != "localhost"
        && let Ok(username) = whoami::username()
    {
        info!(
            "   The correct command may be: ssh -L {port}:localhost:{port} {username}@{hostname} "
        );
    }

    info!("2. Start Surfer: surfer {url_copy} ");
    if !use_localhost && let Ok(hostname) = hostname {
        let hosturl = format!("http://{hostname}:{port}/{token_copy}");
        info!("or, if the host is directly accessible:");
        info!("1. Start Surfer: surfer {hosturl} ");
    }
    // create listener and serve it
    let listener = TcpListener::bind(&addr).await?;

    // we have started the server
    if let Some(started) = started {
        started.store(true, Ordering::SeqCst);
    }

    let g_addr = "[::1]:50051".parse()?;
    let _surver = MySurver::new(state.clone(), shared.clone(), txs.clone(), file_indexes);
    tokio::task::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(SurferRemoteServer::new(_surver))
            .serve(g_addr)
            .await
            .unwrap();
    });

    // main server loop
    loop {
        //new gRPC / Tonic
        //  let _surver = MySurver::default();
        //Server::builder().add_service(surver).serve(addr).await?;

        //original http1
        let (stream, _) = listener.accept().await?;
        let io = TokioIo::new(stream);

        let state = state.clone();
        let shared = shared.clone();
        let txs = txs.clone();
        tokio::task::spawn(async move {
            let service =
                service_fn(move |req| handle(state.clone(), shared.clone(), txs.clone(), req));
            if let Err(e) = http1::Builder::new().serve_connection(io, service).await {
                error!("server error: {e}");
            }
        });
    }
}

/// Thread that loads the body and signals.
fn loader(
    state: &Arc<RwLock<SurverState>>,
    mut body_cont: viewers::ReadBodyContinuation<std::io::BufReader<std::fs::File>>,
    file_index: usize,
    rx: &std::sync::mpsc::Receiver<LoaderMessage>,
) -> Result<()> {
    loop {
        // load the body of the file
        let start_load_body = web_time::Instant::now();
        let (filename, hierarchy, body_progress) = {
            let state_guard = state
                .read()
                .expect("State lock poisoned in loader before body load");
            let file_info = &state_guard.file_infos[file_index];
            (
                file_info.filename.clone(),
                file_info.hierarchy.clone(),
                file_info.body_progress.clone(),
            )
        };

        // Parse body without holding the state lock to reduce contention with request handling.
        let body_result = viewers::read_body(body_cont, &hierarchy, Some(body_progress))
            .map_err(|e| anyhow!("{e:?}"))
            .with_context(|| format!("Failed to parse body of wave file: {filename}"))?;

        info!(
            "Loaded body of {} in {:?}",
            filename,
            start_load_body.elapsed()
        );

        // update state with body results
        {
            let mut state_guard = state
                .write()
                .expect("State lock poisoned in loader after body load");
            let file_info = &mut state_guard.file_infos[file_index];
            file_info.timetable = body_result.time_table;
            file_info.signals.clear(); // Clear old signals on reload
            if let Ok(meta) = fs::metadata(&file_info.filename) {
                file_info.last_modification_time = Some(meta.modified()?);
                info!(
                    "File modification time of {} set to {}",
                    filename,
                    file_info.modification_time_string()
                );
            }
            file_info.last_reload_time = Some(Instant::now());
            file_info.reloading = false;
            file_info.last_reload_ok = true;
            file_info.notify.notify_waiters();
        }
        // source is private, only owned by us
        let mut source = body_result.source;

        // process requests for signals to be loaded
        loop {
            let msg = rx.recv()?;

            match msg {
                LoaderMessage::SignalRequest(ids) => {
                    // make sure that we do not load signals that have already been loaded
                    let mut filtered_ids = {
                        let state_guard = state
                            .read()
                            .expect("State lock poisoned in loader signal request");
                        ids.iter()
                            .filter(|id| {
                                !state_guard.file_infos[file_index].signals.contains_key(id)
                            })
                            .copied()
                            .collect::<Vec<_>>()
                    };

                    // check if there is anything left to do
                    if filtered_ids.is_empty() {
                        continue;
                    }

                    // load signals without holding the lock
                    filtered_ids.sort();
                    filtered_ids.dedup();
                    let result = {
                        let state_guard = state
                            .read()
                            .expect("State lock poisoned in loader signal request");
                        source.load_signals(
                            &filtered_ids,
                            &state_guard.file_infos[file_index].hierarchy,
                            true,
                        )
                    };

                    // store signals
                    {
                        let mut state_guard = state
                            .write()
                            .expect("State lock poisoned in loader when storing signals");
                        for signal in result {
                            state_guard.file_infos[file_index]
                                .signals
                                .insert(signal.signal_ref(), signal);
                        }
                        state_guard.file_infos[file_index].notify.notify_waiters();
                    }
                }
                LoaderMessage::Reload => {
                    let state_guard = state
                        .read()
                        .expect("State lock poisoned in loader before reload");
                    info!(
                        "Reloading waveform file: {}",
                        state_guard.file_infos[file_index].filename
                    );
                    // Reset progress counter
                    state_guard.file_infos[file_index]
                        .body_progress
                        .store(0, Ordering::SeqCst);

                    // Re-read header to get new body continuation
                    let header_result = wellen::viewers::read_header_from_file(
                        state_guard.file_infos[file_index].filename.clone(),
                        &WELLEN_SURFER_DEFAULT_OPTIONS,
                    )
                    .map_err(|e| anyhow!("{e:?}"))
                    .with_context(|| {
                        format!(
                            "Failed to reload wave file: {}",
                            state_guard.file_infos[file_index].filename
                        )
                    })?;

                    body_cont = header_result.body;
                    break; // Break inner loop to reload the body
                }
            }
        }
    }
}
