use std::{
    hash::Hash,
    path::PathBuf,
    pin::Pin,
    sync::{Arc, Mutex, Weak},
};

use pin_project::{pin_project, pinned_drop};
use rqs_lib::{
    EndpointInfo, OutboundPayload, RQS, SendInfo, State, Visibility,
    channel::{ChannelAction, ChannelDirection, ChannelMessage, TransferType},
};
use thiserror::Error;
use tokio::sync::{broadcast, mpsc};
use tokio_stream::{Stream, StreamExt, wrappers::BroadcastStream};

#[derive(Clone, Debug)]
pub struct TransferRequest(ChannelMessage);

#[derive(Clone, Debug)]
pub struct Endpoint(EndpointInfo);

pub struct File {
    pub path: PathBuf,
}

#[derive(Default)]
pub struct Config {
    pub port: Option<u16>,
}

#[derive(Error, Debug)]
pub enum Error {
    #[error("Internal state corrupted")]
    CorruptedState,
    #[error("Unknown error: {0}")]
    Other(Box<dyn std::error::Error + Sync + Send>),
}

pub type Result<T> = std::result::Result<T, Error>;

pub struct Oxidrop {
    rqs: Arc<Mutex<RQS>>,
    sendinfo_send: mpsc::Sender<SendInfo>,
    endpoint_send: Mutex<broadcast::WeakSender<EndpointInfo>>,
}

impl Hash for TransferRequest {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.0.id.hash(state);
    }
}

impl PartialEq for TransferRequest {
    fn eq(&self, other: &Self) -> bool {
        self.0.id == other.0.id
    }
}

impl Eq for TransferRequest {}

impl Hash for Endpoint {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.0.id.hash(state);
    }
}

impl PartialEq for Endpoint {
    fn eq(&self, other: &Self) -> bool {
        self.0.id == other.0.id
    }
}

impl Eq for Endpoint {}

impl Endpoint {
    pub fn name(&self) -> &str {
        self.0.name.as_ref().unwrap_or(&self.0.fullname)
    }
}

impl TransferRequest {
    pub fn sender_name(&self) -> &str {
        self.0
            .meta
            .as_ref()
            .and_then(|m| m.source.as_ref())
            .map(|s| &s.name)
            .unwrap_or(&self.0.id)
    }
}

impl Oxidrop {
    pub async fn new(config: Config) -> Result<Self> {
        let mut rqs = RQS::new(Visibility::Visible, config.port.map(u32::from), None);
        let (sendinfo_send, _) = rqs
            .run()
            .await
            .map_err(|e| Error::Other(e.into_boxed_dyn_error()))?;

        Ok(Oxidrop {
            rqs: Arc::new(Mutex::new(rqs)),
            sendinfo_send,
            endpoint_send: Mutex::new(broadcast::channel(1).0.downgrade()),
        })
    }

    pub fn device_name(&self) -> String {
        hostname::get()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|_| "Unknown".to_string())
    }

    pub async fn accept_transfer(&self, request: &TransferRequest) -> Result<()> {
        self.rqs
            .lock()
            .map_err(|_| Error::CorruptedState)?
            .message_sender
            .send(ChannelMessage {
                id: request.0.meta.as_ref().unwrap().id.clone(),
                direction: ChannelDirection::FrontToLib,
                action: Some(ChannelAction::AcceptTransfer),
                ..Default::default()
            })
            .map_err(|e| Error::Other(Box::new(e)))?;

        Ok(())
    }

    pub async fn reject_transfer(&self, request: &TransferRequest) -> Result<()> {
        self.rqs
            .lock()
            .map_err(|_| Error::CorruptedState)?
            .message_sender
            .send(ChannelMessage {
                id: request.0.meta.as_ref().unwrap().id.clone(),
                direction: ChannelDirection::FrontToLib,
                action: Some(ChannelAction::RejectTransfer),
                ..Default::default()
            })
            .map_err(|e| Error::Other(Box::new(e)))?;

        Ok(())
    }

    pub async fn send_files(
        &self,
        endpoint: &Endpoint,
        files: impl Iterator<Item = File>,
    ) -> Result<()> {
        self.sendinfo_send
            .send(SendInfo {
                id: endpoint.0.id.clone(),
                name: endpoint
                    .0
                    .name
                    .as_ref()
                    .unwrap_or(&endpoint.0.fullname)
                    .clone(),
                addr: endpoint.0.ip.clone().unwrap() + ":" + endpoint.0.port.as_ref().unwrap(),
                ob: OutboundPayload::Files(
                    files
                        .map(|f| f.path.to_string_lossy().into_owned())
                        .collect(),
                ),
            })
            .await
            .map_err(|e| Error::Other(Box::new(e)))?;

        Ok(())
    }

    pub fn discover_endpoints(&self) -> Result<impl Stream<Item = Endpoint> + use<>> {
        #[pin_project(PinnedDrop)]
        struct StreamWrapper<S: Stream<Item = Endpoint>>(
            #[pin] S,
            Weak<Mutex<RQS>>,
            broadcast::WeakSender<EndpointInfo>,
        );

        impl<S: Stream<Item = Endpoint>> Stream for StreamWrapper<S> {
            type Item = Endpoint;

            fn poll_next(
                self: Pin<&mut Self>,
                cx: &mut std::task::Context<'_>,
            ) -> std::task::Poll<Option<Self::Item>> {
                self.project().0.poll_next(cx)
            }
        }

        #[pinned_drop]
        impl<'a, S: Stream<Item = Endpoint>> PinnedDrop for StreamWrapper<S> {
            fn drop(self: Pin<&mut Self>) {
                if let Some(rqs) = self.1.upgrade()
                    && let Some(sender) = self.2.upgrade()
                    && sender.receiver_count() == 1
                {
                    rqs.lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .stop_discovery();
                }
            }
        }

        let (endpoint_send, endpoint_recv) = {
            let mut endpoint_send_guard = self
                .endpoint_send
                .lock()
                .map_err(|_| Error::CorruptedState)?;

            match endpoint_send_guard.upgrade() {
                Some(endpoint_send) => (endpoint_send.downgrade(), endpoint_send.subscribe()),
                None => {
                    let (endpoint_send, endpoint_recv) = broadcast::channel(10);
                    *endpoint_send_guard = endpoint_send.downgrade();
                    self.rqs
                        .lock()
                        .map_err(|_| Error::CorruptedState)?
                        .discovery(endpoint_send.clone())
                        .map_err(|e| Error::Other(e.into_boxed_dyn_error()))?;
                    (endpoint_send.downgrade(), endpoint_recv)
                }
            }
        };

        Ok(StreamWrapper(
            BroadcastStream::new(endpoint_recv) //
                .filter_map(|r| {
                    r.ok()
                        .filter(|e| e.ip.is_some() && e.port.is_some())
                        .map(|e| Endpoint(e))
                }),
            Arc::downgrade(&self.rqs),
            endpoint_send,
        ))
    }

    pub fn get_transfer_requests(&self) -> Result<impl Stream<Item = TransferRequest>> {
        Ok(BroadcastStream::new(
            self.rqs
                .lock()
                .map_err(|_| Error::CorruptedState)?
                .message_sender
                .subscribe(),
        ) //
        .filter_map(|r| {
            r.ok()
                .and_then(|msg| match (&msg.direction, &msg.rtype, &msg.state) {
                    (
                        ChannelDirection::LibToFront,
                        Some(TransferType::Inbound),
                        Some(State::WaitingForUserConsent),
                    ) => Some(TransferRequest(msg)),
                    _ => None,
                })
        }))
    }
}
