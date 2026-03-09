use futures::executor::block_on;
use pb::liveshare_client::LiveshareClient;
use std::sync::mpsc as std_mpsc;
use tokio::sync::mpsc as tokio_mpsc;
use tokio_stream::StreamExt;

use crate::SystemState;

use crate::async_util::perform_async_work;
use crate::remote::pb::action_request::RequestType;
use crate::remote::pb::{JoinRequest, SignalFileId, SignalRequests};
use crate::{
    message::Message,
    remote::pb::{ActionRequest, action_response::ResponseType},
};

pub mod pb {
    tonic::include_proto!("liveshare");
}

struct ClientHandle {
    pub id: u64,
    pub session_id: String,
}

pub struct LiveshareSession {
    to_server_sender: tokio_mpsc::Sender<ActionRequest>,
    client_id: u64,
}

impl LiveshareSession {
    pub async fn new_client_handle(
        dst: String,
        token: String,
    ) -> std::result::Result<ClientHandle, Box<dyn std::error::Error>> {
        let mut client = LiveshareClient::connect(dst).await?;

        Ok(ClientHandle {
            id: 1,
            session_id: "12345".to_string(),
        })
    }

    pub async fn new(
        dst: String,
        token: String,
        main_message_sender: std_mpsc::Sender<Message>,
    ) -> std::result::Result<Self, Box<dyn std::error::Error>> {
        let mut client = LiveshareClient::connect(dst).await?;

        let (to_server_sender, receiver) = tokio_mpsc::channel::<ActionRequest>(128);

        let to_server_stream = tokio_stream::wrappers::ReceiverStream::new(receiver);

        let mut from_server_stream = client
            .join_session(to_server_stream)
            .await
            .unwrap()
            .into_inner();

        perform_async_work(async move {
            while let Some(message) = from_server_stream.next().await {
                match message {
                    Ok(message) => match message.response_type {
                        Some(response) => {
                            match response {
                                ResponseType::JoinResponse(join_response) => {
                                    //
                                    join_response.my_id;
                                }
                                ResponseType::NewClientJoinedResponse(
                                    new_client_joined_response,
                                ) => {
                                    //
                                }
                                ResponseType::SignalResponses(signal_responses) => {
                                    //
                                }
                                ResponseType::NewAnnotationResponse(new_annotation_response) => {
                                    //
                                }
                                ResponseType::DeleteAnnotationResponse(
                                    delete_annotation_response,
                                ) => {
                                    //
                                }
                                ResponseType::CloseSignalResponse(close_signal_response) => {
                                    //
                                }
                                ResponseType::ChangeViewResponse(change_view_response) => {
                                    //
                                }
                            }
                        }
                        None => {
                            tracing::warn!("empty message received from liveshare");
                        }
                    },
                    Err(err) => {
                        tracing::error!("{:?}", err);
                    }
                }
            }
        });

        to_server_sender
            .send(ActionRequest {
                request_type: Some(pb::action_request::RequestType::JoinRequest(JoinRequest {
                    token,
                    name: "Axel".to_string(),
                })),
            })
            .await?;

        Ok(LiveshareSession {
            to_server_sender,
            client_id: 0,
        })
    }

    async fn open_signal(&self) -> Result<(), Box<dyn std::error::Error>> {
        let id_strings_vec = vec!["1".to_string()];

        let id_strings = id_strings_vec
            .into_iter()
            .map(|id_string| SignalFileId { id: id_string })
            .collect();

        self.to_server_sender
            .send(ActionRequest {
                request_type: Some(RequestType::SignalRequests(SignalRequests { id_strings })),
            })
            .await?;

        // something like this spread between here and response handler:, comes from wave_source::load_variables
        //            LoadSignalPayload::Remote(server) => {
        //                perform_async_work(async move {
        //                    let res =
        //                        crate::remote::get_signals(server.clone(), &signals, max_url_length, 0)
        //                            .await
        //                            .map_err(|e| anyhow!("{e:?}"))
        //                            .with_context(|| {
        //                                format!("Failed to retrieve signals from remote server {server}")
        //                            });
        //
        //                    let msg = match res {
        //                        Ok(loaded) => {
        //                            let res = LoadSignalsResult::remote(server, loaded, from_unique_id);
        //                            Message::SignalsLoaded(start, res)
        //                        }
        //                        Err(e) => Message::Error(e),
        //                    };
        //                    checked_send(&sender, msg);
        //                });
        //            }
        Ok(())
    }
}
