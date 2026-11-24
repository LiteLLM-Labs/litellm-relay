use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use axum::extract::ws::{Message, WebSocket};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

use super::{
    protocol::{now_ms, Action, Command, Failure, Outcome, Reply, Result},
    store::{Device, Store},
};

enum Outbound {
    Command(Command),
    Close,
}
struct Connection {
    generation: Uuid,
    sender: mpsc::Sender<Outbound>,
    pending: HashMap<Uuid, oneshot::Sender<Outcome>>,
    seen: Instant,
}

pub(super) struct Pending {
    id: Uuid,
    receiver: oneshot::Receiver<Outcome>,
}

pub struct Peers {
    connections: Mutex<HashMap<Uuid, Connection>>,
    store: Arc<Store>,
}

impl Peers {
    pub fn new(store: Arc<Store>) -> Self {
        Self {
            connections: Mutex::new(HashMap::new()),
            store,
        }
    }

    pub fn online(&self, id: Uuid) -> bool {
        self.connections.lock().is_ok_and(|connections| {
            connections
                .get(&id)
                .is_some_and(|connection| connection.seen.elapsed() < Duration::from_secs(90))
        })
    }

    pub fn disconnect(&self, id: Uuid) {
        if let Ok(mut connections) = self.connections.lock() {
            if let Some(connection) = connections.remove(&id) {
                let _ = connection.sender.try_send(Outbound::Close);
            }
        }
    }

    pub async fn request(&self, device: &Device, action: Action) -> Result<Value> {
        let pending = self.dispatch(device, action)?;
        self.receive(device, pending).await
    }

    pub(super) fn dispatch(&self, device: &Device, action: Action) -> Result<Pending> {
        let id = Uuid::new_v4();
        let (sender, receiver) = oneshot::channel();
        {
            let mut connections = self
                .connections
                .lock()
                .map_err(|_| Failure::new(503, "Device connections are unavailable."))?;
            let connection = connections
                .get_mut(&device.id)
                .filter(|connection| connection.seen.elapsed() < Duration::from_secs(90))
                .ok_or_else(|| {
                    Failure::new(
                        503,
                        "This device is offline. Keep Relay Remote connected on the device.",
                    )
                })?;
            if connection.pending.len() >= 16 {
                return Err(Failure::new(429, "This device is busy. Try again shortly."));
            }
            connection
                .sender
                .try_send(Outbound::Command(Command { id, action }))
                .map_err(|_| Failure::new(503, "The device is busy or disconnected."))?;
            connection.pending.insert(id, sender);
        }
        Ok(Pending { id, receiver })
    }

    pub(super) async fn receive(&self, device: &Device, pending: Pending) -> Result<Value> {
        let reply = tokio::time::timeout(Duration::from_secs(25), pending.receiver).await;
        if let Ok(mut connections) = self.connections.lock() {
            if let Some(connection) = connections.get_mut(&device.id) {
                connection.pending.remove(&pending.id);
            }
        }
        match reply {
            Ok(Ok(outcome)) => outcome.into_result(),
            Ok(Err(_)) => Err(Failure::new(
                503,
                "The device disconnected. Refresh before sending again.",
            )),
            Err(_) => Err(Failure::new(
                504,
                "The device did not confirm the request. Refresh before sending again.",
            )),
        }
    }

    pub async fn connected(
        self: Arc<Self>,
        mut socket: WebSocket,
        device: Device,
        expires_at: i64,
    ) {
        if self.store.device(device.id, &device.subject).is_err() {
            return;
        }
        let expiry = tokio::time::sleep(Duration::from_millis(
            expires_at.saturating_sub(now_ms()).max(0) as u64,
        ));
        tokio::pin!(expiry);
        let generation = Uuid::new_v4();
        let (sender, mut receiver) = mpsc::channel(16);
        {
            let Ok(mut connections) = self.connections.lock() else {
                return;
            };
            if let Some(previous) = connections.insert(
                device.id,
                Connection {
                    generation,
                    sender,
                    pending: HashMap::new(),
                    seen: Instant::now(),
                },
            ) {
                let _ = previous.sender.try_send(Outbound::Close);
            }
        }
        let _ = self.store.seen(device.id, now_ms());
        let mut heartbeat = tokio::time::interval(Duration::from_secs(30));
        loop {
            tokio::select! {
                incoming = socket.recv() => {
                    match incoming {
                        Some(Ok(Message::Text(text))) => {
                            let Ok(reply) = serde_json::from_str::<Reply>(&text) else { break; };
                            if let Ok(mut connections) = self.connections.lock() {
                                if let Some(connection) = connections.get_mut(&device.id).filter(|connection| connection.generation == generation) {
                                    connection.seen = Instant::now();
                                    if let Some(sender) = connection.pending.remove(&reply.id) { let _ = sender.send(reply.result); }
                                }
                            }
                        }
                        Some(Ok(Message::Pong(_))) => {
                            if let Ok(mut connections) = self.connections.lock() {
                                if let Some(connection) = connections.get_mut(&device.id).filter(|connection| connection.generation == generation) { connection.seen = Instant::now(); }
                            }
                            let _ = self.store.seen(device.id, now_ms());
                        }
                        Some(Ok(Message::Ping(bytes))) => { if socket.send(Message::Pong(bytes)).await.is_err() { break; } }
                        _ => break,
                    }
                }
                outbound = receiver.recv() => {
                    match outbound {
                        Some(Outbound::Command(command)) => {
                            let Ok(text) = serde_json::to_string(&command) else { break; };
                            if socket.send(Message::Text(text.into())).await.is_err() { break; }
                        }
                        _ => break,
                    }
                }
                _ = heartbeat.tick() => {
                    if !self.online(device.id) || socket.send(Message::Ping(Vec::new().into())).await.is_err() { break; }
                }
                _ = &mut expiry => break,
            }
        }
        if let Ok(mut connections) = self.connections.lock() {
            if connections
                .get(&device.id)
                .is_some_and(|connection| connection.generation == generation)
            {
                connections.remove(&device.id);
            }
        }
        let _ = socket.send(Message::Close(None)).await;
    }
}
