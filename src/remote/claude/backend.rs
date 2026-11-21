use super::{
    super::protocol::{Decision, Detail, Failure, Result, Session},
    supervisor::{Client, Request},
    Claude,
};
use serde::de::DeserializeOwned;
use serde_json::Value;
use uuid::Uuid;

pub enum Backend {
    Local(Box<Claude>),
    Remote(Client),
}

impl From<Claude> for Backend {
    fn from(claude: Claude) -> Self {
        Self::Local(Box::new(claude))
    }
}

impl Backend {
    async fn call<T: DeserializeOwned>(&self, request: Request) -> Result<T> {
        let value = match self {
            Self::Local(claude) => request.execute(claude).await?,
            Self::Remote(client) => client.call(request).await?,
        };
        serde_json::from_value(value)
            .map_err(|_| Failure::new(502, "Invalid Claude supervisor response."))
    }
    pub async fn list(&self) -> Result<Vec<Session>> {
        self.call(Request::List).await
    }
    pub async fn create(&self, cwd: String) -> Result<Session> {
        self.call(Request::Create { cwd }).await
    }
    pub async fn read(&self, id: Uuid) -> Result<Detail> {
        self.call(Request::Read { id }).await
    }
    pub async fn send(&self, id: Uuid, text: String, model: Option<String>) -> Result<Value> {
        self.call(Request::Send { id, text, model }).await
    }
    pub async fn approve(&self, id: Uuid, approval: Uuid, decision: Decision) -> Result<Value> {
        self.call(Request::Approve {
            id,
            approval,
            decision,
        })
        .await
    }
    pub async fn interrupt(&self, id: Uuid) -> Result<Value> {
        self.call(Request::Interrupt { id }).await
    }
    pub async fn handoff_choices(&self) -> Result<Vec<Session>> {
        self.call(Request::HandoffChoices).await
    }
    pub async fn handoff(&self, id: Uuid) -> Result<()> {
        self.call(Request::Handoff { id }).await
    }
}
