use std::{
    collections::{HashSet, VecDeque},
    sync::{
        atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering},
        Arc, Barrier, Mutex,
    },
    thread,
    time::Duration,
};

use anyhow::{anyhow, Result};

use super::{
    caller::{AllowedCaller, Peer, Verdict},
    key::{ExtendOutcome, KeyServer, MintOutcome, MintRequest, Minted},
    Broker, CallerCheck, Clock, Dependencies, SettingsSource, SettingsVersion,
};
use crate::{
    ai_tools::{
        gateway_credential::{
            AuthorizationServer, AuthorizationServerDocument, Discovery, Grant, IssuedTokens,
            OAuthError, TokenReply,
        },
        idp::{test_support::jwt_with_exp, Session},
        token::IdentityProvider,
    },
    config::{IdpSection, RelaySettings},
};

pub(crate) const GATEWAY: &str = "https://gateway.example.com";
pub(crate) const NOW: i64 = 1_800_000_000;
pub(crate) const HOSTNAME: &str = "laptop";
const IDENTITY_TOKEN_LIFETIME: i64 = 8 * 3600;

pub(crate) fn idp_settings(team: Option<&str>) -> RelaySettings {
    let mut settings = RelaySettings::default();
    settings.gateway.url = GATEWAY.to_string();
    settings.idp = IdpSection {
        issuer: "https://idp.example.com".to_string(),
        client_id: "relay-client".to_string(),
        ..IdpSection::default()
    };
    settings.claude.team = team.map(str::to_string);
    settings
}

pub(crate) fn static_key_settings(key: &str) -> RelaySettings {
    let mut settings = RelaySettings::default();
    settings.gateway.url = GATEWAY.to_string();
    settings.gateway.api_key = Some(key.to_string());
    settings
}

#[derive(Clone, Default)]
pub(crate) struct FakeClock(Arc<AtomicI64>);

impl FakeClock {
    pub(crate) fn advance(&self, seconds: i64) {
        self.0.fetch_add(seconds, Ordering::SeqCst);
    }
}

impl Clock for FakeClock {
    fn now(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

struct AuthState {
    supported: bool,
    session_lifetime: i64,
    valid_clients: HashSet<String>,
    registrations: u64,
    exchanges: u64,
    refreshes: u64,
    issued: u64,
}

#[derive(Clone)]
pub(crate) struct FakeAuth(Arc<Mutex<AuthState>>);

impl FakeAuth {
    fn new(supported: bool) -> Self {
        Self(Arc::new(Mutex::new(AuthState {
            supported,
            session_lifetime: 3600,
            valid_clients: HashSet::new(),
            registrations: 0,
            exchanges: 0,
            refreshes: 0,
            issued: 0,
        })))
    }

    pub(crate) fn set_session_lifetime(&self, seconds: i64) {
        self.0.lock().unwrap().session_lifetime = seconds;
    }

    /// The Gateway's sealing key rotated: every registered client and every
    /// refresh token it issued stop being recognized.
    pub(crate) fn rotate(&self) {
        self.0.lock().unwrap().valid_clients.clear();
    }

    pub(crate) fn registrations(&self) -> u64 {
        self.0.lock().unwrap().registrations
    }

    pub(crate) fn exchanges(&self) -> u64 {
        self.0.lock().unwrap().exchanges
    }

    pub(crate) fn refreshes(&self) -> u64 {
        self.0.lock().unwrap().refreshes
    }

    fn issue(state: &mut AuthState, team: Option<String>) -> TokenReply {
        state.issued += 1;
        TokenReply::Issued(IssuedTokens {
            access_token: format!("llm_session_{}", state.issued),
            refresh_token: Some(format!("refresh-{}", state.issued)),
            expires_in: Some(state.session_lifetime),
            user_id: Some("user-1".to_string()),
            team_id: team,
        })
    }

    fn refused(status: u16, error: &str) -> TokenReply {
        TokenReply::Refused {
            status,
            error: OAuthError {
                error: error.to_string(),
                error_description: format!("{error} description"),
            },
        }
    }
}

impl AuthorizationServer for FakeAuth {
    fn discover(&self, _gateway_url: &str) -> Result<Discovery> {
        let supported = self.0.lock().unwrap().supported;
        if !supported {
            return Ok(Discovery::Unsupported);
        }
        Ok(Discovery::Supported(AuthorizationServerDocument {
            token_endpoint: format!("{GATEWAY}/token"),
            registration_endpoint: format!("{GATEWAY}/register"),
            resource: Some(GATEWAY.to_string()),
        }))
    }

    fn register(&self, _registration_endpoint: &str) -> Result<String> {
        let mut state = self.0.lock().unwrap();
        state.registrations += 1;
        let client_id = format!("client-{}", state.registrations);
        state.valid_clients.insert(client_id.clone());
        Ok(client_id)
    }

    fn token(&self, _document: &AuthorizationServerDocument, grant: Grant) -> Result<TokenReply> {
        let mut state = self.0.lock().unwrap();
        match grant {
            Grant::Exchange {
                client_id, team, ..
            } => {
                state.exchanges += 1;
                if !state.valid_clients.contains(&client_id) {
                    return Ok(Self::refused(401, "invalid_client"));
                }
                Ok(Self::issue(&mut state, team))
            }
            Grant::Refresh { client_id, .. } => {
                state.refreshes += 1;
                if !state.valid_clients.contains(&client_id) {
                    return Ok(Self::refused(400, "invalid_grant"));
                }
                Ok(Self::issue(&mut state, None))
            }
        }
    }
}

struct IdentityState {
    sign_ins: u64,
    refreshes: u64,
    sign_in_delay: Duration,
}

#[derive(Clone)]
pub(crate) struct FakeIdentity {
    clock: FakeClock,
    state: Arc<Mutex<IdentityState>>,
}

impl FakeIdentity {
    fn new(clock: FakeClock) -> Self {
        Self {
            clock,
            state: Arc::new(Mutex::new(IdentityState {
                sign_ins: 0,
                refreshes: 0,
                sign_in_delay: Duration::ZERO,
            })),
        }
    }

    pub(crate) fn set_sign_in_delay(&self, delay: Duration) {
        self.state.lock().unwrap().sign_in_delay = delay;
    }

    pub(crate) fn sign_ins(&self) -> u64 {
        self.state.lock().unwrap().sign_ins
    }

    fn session(&self, serial: u64) -> Session {
        Session {
            id_token: jwt_with_exp(self.clock.now() + IDENTITY_TOKEN_LIFETIME),
            refresh_token: Some(format!("idp-refresh-{serial}")),
        }
    }
}

impl IdentityProvider for FakeIdentity {
    fn sign_in(&self, _idp: &IdpSection, browser: &dyn Fn(&str)) -> Result<Session> {
        let (serial, delay) = {
            let mut state = self.state.lock().unwrap();
            state.sign_ins += 1;
            (state.sign_ins, state.sign_in_delay)
        };
        browser("https://idp.example.com/authorize");
        thread::sleep(delay);
        Ok(self.session(serial))
    }

    fn refresh(&self, _idp: &IdpSection, _refresh_token: &str) -> Result<Session> {
        let serial = {
            let mut state = self.state.lock().unwrap();
            state.refreshes += 1;
            state.refreshes + 100
        };
        Ok(self.session(serial))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MintCall {
    pub gateway_url: String,
    pub bearer: String,
    pub team: String,
    pub alias: String,
}

#[derive(Default)]
struct KeysState {
    minted: u64,
    mint_script: VecDeque<Result<MintOutcome>>,
    extend_script: VecDeque<Result<ExtendOutcome>>,
    mints: Vec<MintCall>,
    extends: Vec<(String, String)>,
    deletes: Vec<(String, String)>,
    delete_urls: Vec<String>,
}

#[derive(Clone, Default)]
pub(crate) struct FakeKeys(Arc<Mutex<KeysState>>);

impl FakeKeys {
    pub(crate) fn script_mint(&self, outcome: Result<MintOutcome>) {
        self.0.lock().unwrap().mint_script.push_back(outcome);
    }

    pub(crate) fn script_extend(&self, outcome: Result<ExtendOutcome>) {
        self.0.lock().unwrap().extend_script.push_back(outcome);
    }

    pub(crate) fn mints(&self) -> Vec<MintCall> {
        self.0.lock().unwrap().mints.clone()
    }

    pub(crate) fn extends(&self) -> Vec<(String, String)> {
        self.0.lock().unwrap().extends.clone()
    }

    pub(crate) fn deletes(&self) -> Vec<(String, String)> {
        self.0.lock().unwrap().deletes.clone()
    }

    pub(crate) fn delete_urls(&self) -> Vec<String> {
        self.0.lock().unwrap().delete_urls.clone()
    }
}

impl KeyServer for FakeKeys {
    fn mint(&self, gateway_url: &str, bearer: &str, request: &MintRequest) -> Result<MintOutcome> {
        let mut state = self.0.lock().unwrap();
        state.mints.push(MintCall {
            gateway_url: gateway_url.to_string(),
            bearer: bearer.to_string(),
            team: request.team_id.clone(),
            alias: request.alias.clone(),
        });
        if let Some(scripted) = state.mint_script.pop_front() {
            return scripted;
        }
        state.minted += 1;
        Ok(MintOutcome::Minted(Minted {
            token: format!("sk-{}", state.minted),
            expires_at: None,
        }))
    }

    fn extend(&self, _gateway_url: &str, bearer: &str, key: &str) -> Result<ExtendOutcome> {
        let mut state = self.0.lock().unwrap();
        state.extends.push((bearer.to_string(), key.to_string()));
        state
            .extend_script
            .pop_front()
            .unwrap_or(Ok(ExtendOutcome::Extended { expires_at: None }))
    }

    fn delete(&self, gateway_url: &str, bearer: &str, key: &str) -> Result<()> {
        let mut state = self.0.lock().unwrap();
        state.deletes.push((bearer.to_string(), key.to_string()));
        state.delete_urls.push(gateway_url.to_string());
        Ok(())
    }
}

#[derive(Clone)]
pub(crate) struct FakeCallers {
    checks: Arc<AtomicU64>,
    verdict: Arc<Mutex<Verdict>>,
    gate: Arc<Mutex<Option<Arc<Barrier>>>>,
    allowlist: Arc<Mutex<Vec<AllowedCaller>>>,
}

impl FakeCallers {
    fn allowing() -> Self {
        Self {
            checks: Arc::new(AtomicU64::new(0)),
            verdict: Arc::new(Mutex::new(Verdict::Allowed {
                caller: AllowedCaller::new("com.anthropic.claude-code", Some("Q6L2SF6YDW")),
                level: 1,
            })),
            gate: Arc::new(Mutex::new(None)),
            allowlist: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub(crate) fn gate_next_check(&self) -> Arc<Barrier> {
        let gate = Arc::new(Barrier::new(2));
        *self.gate.lock().unwrap() = Some(Arc::clone(&gate));
        gate
    }

    pub(crate) fn refuse(&self, chain: &[&str]) {
        *self.verdict.lock().unwrap() = Verdict::Refused {
            chain: chain.iter().map(|entry| entry.to_string()).collect(),
        };
    }

    pub(crate) fn checks(&self) -> u64 {
        self.checks.load(Ordering::SeqCst)
    }

    pub(crate) fn last_allowlist(&self) -> Vec<AllowedCaller> {
        self.allowlist.lock().unwrap().clone()
    }
}

impl CallerCheck for FakeCallers {
    fn check(&self, callers: &[AllowedCaller], _peer: Peer) -> Verdict {
        self.checks.fetch_add(1, Ordering::SeqCst);
        *self.allowlist.lock().unwrap() = callers.to_vec();
        if let Some(gate) = self.gate.lock().unwrap().take() {
            gate.wait();
            gate.wait();
        }
        self.verdict.lock().unwrap().clone()
    }
}

#[derive(Clone)]
pub(crate) struct FakeSettings {
    version: Arc<AtomicU64>,
    settings: Arc<Mutex<RelaySettings>>,
    fail_next_load: Arc<AtomicBool>,
}

impl FakeSettings {
    fn new(settings: RelaySettings) -> Self {
        Self {
            version: Arc::new(AtomicU64::new(0)),
            settings: Arc::new(Mutex::new(settings)),
            fail_next_load: Arc::new(AtomicBool::new(false)),
        }
    }

    pub(crate) fn set(&self, settings: RelaySettings) {
        *self.settings.lock().unwrap() = settings;
        self.version.fetch_add(1, Ordering::SeqCst);
    }

    pub(crate) fn fail_next_load(&self) {
        self.fail_next_load.store(true, Ordering::SeqCst);
    }
}

impl SettingsSource for FakeSettings {
    fn version(&self) -> Option<SettingsVersion> {
        Some(SettingsVersion(self.version.load(Ordering::SeqCst)))
    }

    fn load(&self) -> Option<RelaySettings> {
        if self.fail_next_load.swap(false, Ordering::SeqCst) {
            return None;
        }
        Some(self.settings.lock().unwrap().clone())
    }
}

pub(crate) struct Rig {
    pub broker: Arc<Broker>,
    pub clock: FakeClock,
    pub auth: FakeAuth,
    pub keys: FakeKeys,
    pub identity: FakeIdentity,
    pub callers: FakeCallers,
    pub settings: FakeSettings,
    browser_opens: Arc<AtomicU64>,
}

impl Rig {
    pub(crate) fn new(settings: RelaySettings) -> Self {
        Self::build(settings, true)
    }

    pub(crate) fn without_exchange(settings: RelaySettings) -> Self {
        Self::build(settings, false)
    }

    fn build(settings: RelaySettings, exchange_supported: bool) -> Self {
        let clock = FakeClock::default();
        clock.advance(NOW);
        let auth = FakeAuth::new(exchange_supported);
        let keys = FakeKeys::default();
        let identity = FakeIdentity::new(clock.clone());
        let callers = FakeCallers::allowing();
        let source = FakeSettings::new(settings.clone());
        let browser_opens = Arc::new(AtomicU64::new(0));
        let opens = Arc::clone(&browser_opens);
        let deps = Dependencies {
            auth: Box::new(auth.clone()),
            keys: Box::new(keys.clone()),
            identity: Box::new(identity.clone()),
            clock: Box::new(clock.clone()),
            callers: Box::new(callers.clone()),
            browser: Box::new(move |_url| {
                opens.fetch_add(1, Ordering::SeqCst);
            }),
            hostname: HOSTNAME.to_string(),
            settings: Box::new(source.clone()),
        };
        Self {
            broker: Arc::new(Broker::new(&settings, deps)),
            clock,
            auth,
            keys,
            identity,
            callers,
            settings: source,
            browser_opens,
        }
    }

    pub(crate) fn peer(&self) -> Peer {
        Peer {
            pid: 4242,
            audit_token: None,
        }
    }

    pub(crate) fn browser_opens(&self) -> u64 {
        self.browser_opens.load(Ordering::SeqCst)
    }

    pub(crate) fn failing_mint(message: &str) -> Result<MintOutcome> {
        Err(anyhow!("{message}"))
    }
}
