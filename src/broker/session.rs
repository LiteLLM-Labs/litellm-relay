use super::{caller::Peer, Broker, Context, Mode, Refusal, Reply, Target};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionIdentity {
    pub gateway_url: String,
    pub user_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionBearer {
    pub token: String,
    pub identity: SessionIdentity,
}

impl Broker {
    pub fn session_identity(&self) -> Option<SessionIdentity> {
        self.refresh();
        let target = self.lock_target().clone();
        let state = self.lock_state();
        let held = matches!(target.mode, Mode::StaticKey(_)) || state.credential.is_some();
        if !held {
            return None;
        }
        Some(SessionIdentity {
            gateway_url: target.gateway_url,
            user_id: state
                .credential
                .as_ref()
                .and_then(|credential| credential.user_id.clone()),
        })
    }

    pub fn session_bearer_for_peer(
        &self,
        peer: Peer,
        what: &str,
        context: Context,
    ) -> Result<SessionBearer, Refusal> {
        let target = self.admit(peer, what)?;
        self.session_bearer(&target, context)
    }

    pub fn session_bearer_for_daemon(&self) -> Result<SessionBearer, Refusal> {
        self.refresh();
        let target = self.lock_target().clone();
        self.session_bearer(&target, Context::NonInteractive)
    }

    fn session_bearer(&self, target: &Target, context: Context) -> Result<SessionBearer, Refusal> {
        match self.issue(target, context) {
            Reply::Credential(issued) => Ok(SessionBearer {
                token: issued.token,
                identity: SessionIdentity {
                    gateway_url: target.gateway_url.clone(),
                    user_id: self
                        .lock_state()
                        .credential
                        .as_ref()
                        .and_then(|credential| credential.user_id.clone()),
                },
            }),
            Reply::Refused(refusal) => Err(refusal),
            _ => Err(Refusal::GatewayError(
                "the broker answered without a credential".to_string(),
            )),
        }
    }
}
