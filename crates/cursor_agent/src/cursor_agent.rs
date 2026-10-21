use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result, anyhow, bail};
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use credentials_provider::CredentialsProvider;
use futures::{AsyncReadExt as _, FutureExt as _, future::Shared};
use gpui::{App, AsyncApp, Context, Entity, Global, SharedString, Task};
use gpui_util::ResultExt as _;
use http_client::{
    AsyncBody, HttpClient, Method, Request as HttpRequest, RequestTimeout, http::request::Builder,
};
use language_model::{
    LanguageModel, LanguageModelEffortLevel, LanguageModelId, LanguageModelName,
    LanguageModelProviderId, LanguageModelProviderName, LanguageModelToolChoiceSupport,
};
use rand::RngCore as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use uuid::Uuid;

mod protocol;
mod run;
mod usage;

pub use run::{RunRegistry, stream_completion};
pub use usage::{CursorUsage, PlanUsage, SpendLimitUsage, fetch_usage};

pub const PROVIDER_ID: LanguageModelProviderId = LanguageModelProviderId::new("cursor");
pub const PROVIDER_NAME: LanguageModelProviderName = LanguageModelProviderName::new("Cursor");

pub const CURSOR_API_URL: &str = "https://api2.cursor.sh";
const CURSOR_WEBSITE_URL: &str = "https://cursor.com";

/// Keychain slot for the browser sign-in tokens.
const CREDENTIALS_KEY: &str = "https://api2.cursor.sh/zed-cursor-agent";

/// Cursor's API rejects client versions it does not know; this is the desktop release the
/// agent protocol here was verified against.
pub(crate) const CLIENT_VERSION: &str = "3.5.33";

/// Treat tokens this close to expiry as expired so a Run never starts with a token that lapses
/// mid-stream. Cursor's own CLI uses the same margin.
const EXPIRY_BUFFER: Duration = Duration::from_secs(5 * 60);

const MODEL_CATALOG_TIMEOUT: Duration = Duration::from_secs(15);

/// How long a usage reading counts as current, so composers opening the usage card don't each
/// ask Cursor again.
const USAGE_TTL: Duration = Duration::from_secs(60);

// Browser sign-in polling mirrors Cursor's CLI: 404 means the user has not approved yet.
const POLL_BASE_DELAY: Duration = Duration::from_secs(1);
const POLL_MAX_DELAY: Duration = Duration::from_secs(10);
const POLL_BACKOFF: f64 = 1.2;
const POLL_MAX_ATTEMPTS: u32 = 150;
const POLL_MAX_CONSECUTIVE_ERRORS: u32 = 3;

/// One identifier per app process, like a long-lived IDE session.
pub(crate) static SESSION_ID: LazyLock<String> = LazyLock::new(|| Uuid::new_v4().to_string());

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Credentials {
    access_token: String,
    refresh_token: String,
    /// Cursor ties requests to a machine through `x-cursor-checksum`. Without the desktop app
    /// there is no machine id to borrow, so one is generated at sign-in and kept with the tokens.
    machine_id: String,
    mac_machine_id: String,
}

impl Credentials {
    pub fn expires_at(&self) -> Option<SystemTime> {
        jwt_expiry(&self.access_token)
    }

    pub fn is_expired(&self) -> bool {
        self.expires_at()
            .is_some_and(|expires_at| expires_at <= SystemTime::now() + EXPIRY_BUFFER)
    }
}

fn jwt_expiry(token: &str) -> Option<SystemTime> {
    let payload = token.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?;
    let claims: Value = serde_json::from_slice(&bytes).ok()?;
    let expires_at = claims.get("exp")?.as_u64()?;
    Some(UNIX_EPOCH + Duration::from_secs(expires_at))
}

fn random_bytes() -> [u8; 32] {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    bytes
}

fn new_machine_id() -> String {
    format!("{:x}", Sha256::digest(random_bytes()))
}

/// Mirrors the Cursor client's checksum, including JavaScript's 32-bit shift semantics: the
/// timestamp is shifted by 40 and 32 bits in the original, which JavaScript reduces to 8 and 0.
fn checksum(machine_id: &str, mac_machine_id: &str, now: SystemTime) -> String {
    let millis = now
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    let time = (millis / 1_000_000) as u32;
    let mut bytes = [
        (time >> 8) as u8,
        time as u8,
        (time >> 24) as u8,
        (time >> 16) as u8,
        (time >> 8) as u8,
        time as u8,
    ];
    let mut key: u8 = 165;
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = (*byte ^ key).wrapping_add(index as u8);
        key = *byte;
    }
    let mut result = STANDARD.encode(bytes);
    result.push_str(machine_id);
    if !mac_machine_id.is_empty() {
        result.push('/');
        result.push_str(mac_machine_id);
    }
    result
}

fn timezone() -> String {
    std::fs::read_link("/etc/localtime")
        .ok()
        .and_then(|path| {
            path.to_string_lossy()
                .split_once("zoneinfo/")
                .map(|(_, zone)| zone.to_string())
        })
        .unwrap_or_else(|| "UTC".to_string())
}

pub(crate) fn client_os() -> &'static str {
    if cfg!(target_os = "macos") {
        "mac"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "linux"
    }
}

fn client_arch() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        arch => arch,
    }
}

/// Headers every authenticated Cursor API request carries.
pub(crate) fn authenticated_request(builder: Builder, credentials: &Credentials) -> Builder {
    let request_id = Uuid::new_v4().to_string();
    builder
        .header(
            "authorization",
            format!("Bearer {}", credentials.access_token),
        )
        .header(
            "x-cursor-checksum",
            checksum(
                &credentials.machine_id,
                &credentials.mac_machine_id,
                SystemTime::now(),
            ),
        )
        .header("x-cursor-client-version", CLIENT_VERSION)
        .header("x-cursor-timezone", timezone())
        .header("x-request-id", request_id.as_str())
        .header("x-original-request-id", request_id.as_str())
        .header("x-session-id", SESSION_ID.as_str())
        .header("x-cursor-client-os", client_os())
        .header("x-cursor-client-arch", client_arch())
        .header("x-cursor-client-device-type", "desktop")
        .header("x-ghost-mode", "true")
}

struct LoginRequest {
    uuid: String,
    verifier: String,
    url: String,
}

impl LoginRequest {
    fn new() -> Self {
        let verifier = URL_SAFE_NO_PAD.encode(random_bytes());
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let uuid = Uuid::new_v4().to_string();
        let url = format!(
            "{CURSOR_WEBSITE_URL}/loginDeepControl?challenge={challenge}&uuid={uuid}&mode=login&redirectTarget=cli"
        );
        Self {
            uuid,
            verifier,
            url,
        }
    }
}

enum PollResult {
    Pending,
    Approved {
        access_token: String,
        refresh_token: String,
    },
}

async fn poll_once(http_client: &dyn HttpClient, login: &LoginRequest) -> Result<PollResult> {
    let url = format!(
        "{CURSOR_API_URL}/auth/poll?uuid={}&verifier={}",
        login.uuid, login.verifier
    );
    let request = HttpRequest::builder()
        .method(Method::GET)
        .uri(url)
        .header("content-type", "application/json")
        .body(AsyncBody::default())?;
    let mut response = http_client.send(request).await?;
    if response.status().as_u16() == 404 {
        return Ok(PollResult::Pending);
    }
    let mut body = String::new();
    response.body_mut().read_to_string(&mut body).await?;
    if !response.status().is_success() {
        bail!("Cursor sign-in poll returned HTTP {}", response.status());
    }
    let value: Value = serde_json::from_str(&body).context("Cursor sign-in poll was not JSON")?;
    let token = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|token| !token.is_empty())
            .ok_or_else(|| anyhow!("Cursor sign-in response is missing `{key}`"))
    };
    Ok(PollResult::Approved {
        access_token: token("accessToken")?,
        refresh_token: token("refreshToken")?,
    })
}

fn poll_delay(attempt: u32) -> Duration {
    POLL_BASE_DELAY
        .mul_f64(POLL_BACKOFF.powi(attempt as i32))
        .min(POLL_MAX_DELAY)
}

/// Opens Cursor's sign-in page and waits for the user to approve this client in the browser.
async fn browser_sign_in(
    http_client: Arc<dyn HttpClient>,
    machine_ids: Option<(String, String)>,
    cx: &mut AsyncApp,
) -> Result<Credentials> {
    let login = LoginRequest::new();
    cx.update(|cx| cx.open_url(&login.url));

    let mut consecutive_errors = 0;
    for attempt in 0..POLL_MAX_ATTEMPTS {
        match poll_once(http_client.as_ref(), &login).await {
            Ok(PollResult::Pending) => consecutive_errors = 0,
            Ok(PollResult::Approved {
                access_token,
                refresh_token,
            }) => {
                let (machine_id, mac_machine_id) =
                    machine_ids.unwrap_or_else(|| (new_machine_id(), new_machine_id()));
                return Ok(Credentials {
                    access_token,
                    refresh_token,
                    machine_id,
                    mac_machine_id,
                });
            }
            Err(error) => {
                consecutive_errors += 1;
                if consecutive_errors >= POLL_MAX_CONSECUTIVE_ERRORS {
                    return Err(error);
                }
            }
        }
        cx.background_executor().timer(poll_delay(attempt)).await;
    }
    bail!("Timed out waiting for Cursor sign-in to be approved in the browser")
}

#[derive(Clone, Debug, PartialEq)]
pub struct ModelVariant {
    pub id: String,
    /// The reasoning effort encoded in the wire id, normalized to Zed's effort values.
    pub effort: Option<&'static str>,
}

/// Cursor lists every effort and speed as its own model id; variants that differ only in effort
/// are grouped into one model whose effort levels select the wire id.
#[derive(Clone, Debug, PartialEq)]
pub struct CursorModel {
    pub id: String,
    pub display_name: String,
    pub variants: Vec<ModelVariant>,
    pub context_window: u64,
    pub max_output_tokens: u64,
}

// Suffix order matters only in that each entry must not be a dash-suffix of a later one.
const EFFORT_SUFFIXES: [&str; 8] = [
    "xhigh", "medium", "minimal", "extra", "none", "low", "high", "max",
];
const ZED_EFFORT_ORDER: [&str; 6] = ["minimal", "low", "medium", "high", "xhigh", "max"];
const DEFAULT_CONTEXT_WINDOW: u64 = 200_000;
const DEFAULT_MAX_OUTPUT_TOKENS: u64 = 65_536;

struct ParsedModelId<'a> {
    base: &'a str,
    effort: Option<&'static str>,
    fast: bool,
}

fn parse_model_id(id: &str) -> ParsedModelId<'_> {
    let (rest, fast) = match id.strip_suffix("-fast") {
        Some(rest) => (rest, true),
        None => (id, false),
    };
    for suffix in EFFORT_SUFFIXES {
        if let Some(base) = rest
            .strip_suffix(suffix)
            .and_then(|base| base.strip_suffix('-'))
        {
            let effort = match suffix {
                "extra" => "xhigh",
                suffix => suffix,
            };
            return ParsedModelId {
                base,
                effort: Some(effort),
                fast,
            };
        }
    }
    ParsedModelId {
        base: rest,
        effort: None,
        fast,
    }
}

fn clean_display_name(name: &str, keep_thinking: bool) -> String {
    let mut name = name.trim().to_string();
    for suffix in [" Fast", " Extra High"] {
        if let Some(stripped) = name.strip_suffix(suffix) {
            name = stripped.to_string();
        }
    }
    if let Some(stripped) = name.strip_suffix(" (Thinking)") {
        name = if keep_thinking {
            format!("{stripped} Thinking")
        } else {
            stripped.to_string()
        };
    }
    if !keep_thinking && let Some(stripped) = name.strip_suffix(" Thinking") {
        name = stripped.to_string();
    }
    for suffix in [" None", " Low", " Medium", " High", " Max", " Minimal"] {
        if let Some(stripped) = name.strip_suffix(suffix) {
            name = stripped.to_string();
            break;
        }
    }
    name.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn positive_number(model: &Value, keys: &[&str]) -> Option<u64> {
    keys.iter().find_map(|key| match model.get(*key)? {
        Value::Number(number) => number.as_u64().filter(|value| *value > 0),
        Value::String(text) => text.trim().parse().ok().filter(|value| *value > 0),
        _ => None,
    })
}

pub fn models_from_catalog(payload: &Value) -> Vec<CursorModel> {
    let Some(entries) = payload.get("models").and_then(Value::as_array) else {
        return Vec::new();
    };

    struct Group<'a> {
        key: String,
        fast: bool,
        entries: Vec<&'a Value>,
        variants: Vec<ModelVariant>,
    }
    let mut groups: Vec<Group> = Vec::new();
    for entry in entries {
        let Some(id) = ["modelId", "model_id", "id"]
            .iter()
            .find_map(|key| entry.get(*key).and_then(Value::as_str))
            .map(str::trim)
            .filter(|id| !id.is_empty())
        else {
            continue;
        };
        let parsed = parse_model_id(id);
        let key = if parsed.fast {
            format!("{}-fast", parsed.base)
        } else {
            parsed.base.to_string()
        };
        let variant = ModelVariant {
            id: id.to_string(),
            effort: parsed.effort,
        };
        match groups.iter_mut().find(|group| group.key == key) {
            Some(group) => {
                group.entries.push(entry);
                group.variants.push(variant);
            }
            None => groups.push(Group {
                key,
                fast: parsed.fast,
                entries: vec![entry],
                variants: vec![variant],
            }),
        }
    }

    groups
        .into_iter()
        .map(|group| {
            let id = match group.variants.as_slice() {
                [only] => only.id.clone(),
                _ => group.key.clone(),
            };
            let keep_thinking = parse_model_id(&id).base.contains("-thinking");
            let mut display_name = group
                .entries
                .iter()
                .filter_map(|entry| {
                    ["displayName", "display_name", "name"]
                        .iter()
                        .find_map(|key| entry.get(*key).and_then(Value::as_str))
                })
                .map(|name| clean_display_name(name, keep_thinking))
                .filter(|name| !name.is_empty())
                .min_by_key(|name| name.len())
                .unwrap_or_else(|| id.clone());
            if group.fast && !display_name.ends_with(" Fast") {
                display_name.push_str(" Fast");
            }
            let context_window = group
                .entries
                .iter()
                .map(|entry| {
                    positive_number(
                        entry,
                        &[
                            "contextWindow",
                            "context_window",
                            "contextLength",
                            "context_length",
                            "contextTokenLimit",
                            "context_token_limit",
                        ],
                    )
                    .unwrap_or(DEFAULT_CONTEXT_WINDOW)
                })
                .min()
                .unwrap_or(DEFAULT_CONTEXT_WINDOW);
            let max_output_tokens = group
                .entries
                .iter()
                .filter_map(|entry| {
                    positive_number(
                        entry,
                        &[
                            "maxTokens",
                            "max_tokens",
                            "maxOutputTokens",
                            "max_output_tokens",
                        ],
                    )
                })
                .max()
                .unwrap_or(DEFAULT_MAX_OUTPUT_TOKENS);
            CursorModel {
                id,
                display_name,
                variants: group.variants,
                context_window,
                max_output_tokens,
            }
        })
        .collect()
}

impl CursorModel {
    /// Selectable efforts in Zed's order; a single-variant model has none to choose from.
    pub fn effort_levels(&self) -> Vec<&'static str> {
        if self.variants.len() < 2 {
            return Vec::new();
        }
        ZED_EFFORT_ORDER
            .into_iter()
            .filter(|effort| {
                self.variants
                    .iter()
                    .any(|variant| variant.effort == Some(*effort))
            })
            .collect()
    }

    pub fn default_variant(&self) -> Option<&ModelVariant> {
        let with_effort = |effort: Option<&str>| {
            self.variants
                .iter()
                .find(|variant| variant.effort == effort)
        };
        with_effort(Some("medium"))
            .or_else(|| with_effort(None))
            .or_else(|| with_effort(Some("high")))
            .or_else(|| self.variants.first())
    }

    /// The wire model id for a requested effort, falling back to the default variant.
    pub fn wire_model_id(&self, effort: Option<&str>) -> &str {
        effort
            .and_then(|effort| {
                self.variants
                    .iter()
                    .find(|variant| variant.effort == Some(effort))
            })
            .or_else(|| self.default_variant())
            .map_or(self.id.as_str(), |variant| variant.id.as_str())
    }
}

fn effort_label(effort: &str) -> &'static str {
    match effort {
        "minimal" => "Minimal",
        "low" => "Low",
        "medium" => "Medium",
        "high" => "High",
        "xhigh" => "Extra High",
        _ => "Max",
    }
}

pub fn language_model(model: &CursorModel) -> LanguageModel {
    let default_effort = model.default_variant().and_then(|variant| variant.effort);
    let effort_levels: Arc<[LanguageModelEffortLevel]> = model
        .effort_levels()
        .into_iter()
        .map(|effort| LanguageModelEffortLevel {
            name: effort_label(effort).into(),
            value: effort.into(),
            is_default: Some(effort) == default_effort,
            supports_disabling_thinking: None,
        })
        .collect();
    LanguageModel {
        supports_tools: true,
        supports_images: true,
        // Cursor's agent decides on its own whether to call a tool; it cannot be forced or barred.
        tool_choice_support: LanguageModelToolChoiceSupport {
            auto: true,
            any: false,
            none: false,
        },
        supports_streaming_tools: false,
        supports_thinking: !effort_levels.is_empty(),
        supported_effort_levels: effort_levels,
        max_output_tokens: Some(model.max_output_tokens),
        ..LanguageModel::new(
            LanguageModelId::from(model.id.clone()),
            LanguageModelName::from(model.display_name.clone()),
            PROVIDER_ID,
            PROVIDER_NAME,
            format!("cursor/{}", model.id),
            model.context_window,
        )
    }
}

/// Connect unary responses may arrive as plain JSON or as a single enveloped frame.
fn parse_unary_payload(body: &[u8]) -> Result<Value> {
    let text = String::from_utf8_lossy(body);
    let trimmed = text.trim();
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        return serde_json::from_str(trimmed).context("Cursor response was not valid JSON");
    }
    let header: [u8; 4] = body
        .get(1..5)
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| anyhow!("Cursor response was empty"))?;
    let length = u32::from_be_bytes(header) as usize;
    let payload = body
        .get(5..5 + length)
        .ok_or_else(|| anyhow!("Cursor response frame was truncated"))?;
    serde_json::from_slice(payload).context("Cursor response frame was not valid JSON")
}

pub async fn fetch_models(
    http_client: &dyn HttpClient,
    credentials: &Credentials,
) -> Result<Vec<CursorModel>> {
    let builder = HttpRequest::builder()
        .method(Method::POST)
        .uri(format!(
            "{CURSOR_API_URL}/agent.v1.AgentService/GetUsableModels"
        ))
        .header("content-type", "application/json")
        .header("connect-protocol-version", "1")
        .extension(RequestTimeout(MODEL_CATALOG_TIMEOUT));
    let request = authenticated_request(builder, credentials).body(AsyncBody::from("{}"))?;
    let mut response = http_client.send(request).await?;
    let mut body = Vec::new();
    response.body_mut().read_to_end(&mut body).await?;
    if !response.status().is_success() {
        let detail: String = String::from_utf8_lossy(&body).chars().take(300).collect();
        bail!("HTTP {} {}", response.status(), detail.trim());
    }
    let models = models_from_catalog(&parse_unary_payload(&body)?);
    if models.is_empty() {
        bail!("Cursor returned no models for this account");
    }
    Ok(models)
}

pub struct State {
    credentials: Option<Credentials>,
    sign_in_task: Option<Task<()>>,
    load_task: Option<Shared<Task<()>>>,
    /// Owned here rather than by the task that started it: dropping a `Task` cancels it, and
    /// both the load and sign-in tasks release their own handles when they finish.
    models_task: Option<Shared<Task<()>>>,
    credentials_provider: Arc<dyn CredentialsProvider>,
    http_client: Arc<dyn HttpClient>,
    models: Vec<CursorModel>,
    models_error: Option<SharedString>,
    last_auth_error: Option<SharedString>,
    auth_generation: u64,
    usage: Option<CursorUsage>,
    usage_error: Option<SharedString>,
    usage_fetched_at: Option<Instant>,
    usage_task: Option<Task<()>>,
}

struct GlobalState(Entity<State>);

impl Global for GlobalState {}

impl State {
    pub fn new(
        http_client: Arc<dyn HttpClient>,
        credentials_provider: Arc<dyn CredentialsProvider>,
        cx: &mut Context<Self>,
    ) -> Self {
        let load_task = cx
            .spawn({
                let credentials_provider = credentials_provider.clone();
                async move |this, cx| {
                    let credentials = match credentials_provider
                        .read_credentials(CREDENTIALS_KEY, cx)
                        .await
                    {
                        Ok(Some((_, bytes))) => serde_json::from_slice::<Credentials>(&bytes)
                            .context("Failed to parse saved Cursor credentials")
                            .log_err(),
                        Ok(None) => None,
                        Err(error) => {
                            log::error!("Failed to load Cursor credentials: {error:#}");
                            None
                        }
                    };
                    let models_task = this
                        .update(cx, |state, cx| {
                            state.credentials = credentials;
                            cx.notify();
                            state.refresh_models(cx);
                            state.refresh_usage(false, cx);
                            state.models_task.clone()
                        })
                        .log_err()
                        .flatten();
                    // Authentication waits on this task, so it finishes only once models are
                    // known; clearing the handle must stay the last step because it cancels this task.
                    if let Some(models_task) = models_task {
                        models_task.await;
                    }
                    this.update(cx, |state, cx| {
                        state.load_task = None;
                        cx.notify();
                    })
                    .log_err();
                }
            })
            .shared();

        Self {
            credentials: None,
            sign_in_task: None,
            load_task: Some(load_task),
            models_task: None,
            credentials_provider,
            http_client,
            models: Vec::new(),
            models_error: None,
            last_auth_error: None,
            auth_generation: 0,
            usage: None,
            usage_error: None,
            usage_fetched_at: None,
            usage_task: None,
        }
    }

    /// The app's Cursor sign-in, once the Cursor provider has been registered.
    pub fn global(cx: &App) -> Option<Entity<Self>> {
        cx.try_global::<GlobalState>()
            .map(|global| global.0.clone())
    }

    pub fn set_global(state: Entity<Self>, cx: &mut App) {
        cx.set_global(GlobalState(state));
    }

    pub fn is_authenticated(&self) -> bool {
        self.credentials
            .as_ref()
            .is_some_and(|credentials| !credentials.is_expired())
    }

    /// Signed in once, but the browser sign-in has since run out.
    pub fn is_session_expired(&self) -> bool {
        self.credentials
            .as_ref()
            .is_some_and(Credentials::is_expired)
    }

    pub fn expires_at(&self) -> Option<SystemTime> {
        self.credentials.as_ref().and_then(Credentials::expires_at)
    }

    pub fn credentials(&self) -> Option<Credentials> {
        self.credentials
            .clone()
            .filter(|credentials| !credentials.is_expired())
    }

    pub fn http_client(&self) -> Arc<dyn HttpClient> {
        self.http_client.clone()
    }

    pub fn is_signing_in(&self) -> bool {
        self.sign_in_task.is_some()
    }

    pub fn last_auth_error(&self) -> Option<SharedString> {
        self.last_auth_error.clone()
    }

    pub fn models_error(&self) -> Option<SharedString> {
        self.models_error.clone()
    }

    pub fn models(&self) -> &[CursorModel] {
        &self.models
    }

    /// The last usage reading, kept while a newer one loads.
    pub fn usage(&self) -> Option<&CursorUsage> {
        self.usage.as_ref()
    }

    pub fn usage_error(&self) -> Option<SharedString> {
        self.usage_error.clone()
    }

    pub fn is_loading_usage(&self) -> bool {
        self.usage_task.is_some()
    }

    /// Loads the account's usage again, unless a reading from the last minute is still current
    /// and `force` is false.
    pub fn refresh_usage(&mut self, force: bool, cx: &mut Context<Self>) {
        if self.usage_task.is_some() {
            return;
        }
        let is_current = self
            .usage_fetched_at
            .is_some_and(|fetched_at| fetched_at.elapsed() < USAGE_TTL);
        if is_current && !force {
            return;
        }
        let Some(credentials) = self.credentials() else {
            return;
        };
        let auth_generation = self.auth_generation;
        let http_client = self.http_client.clone();
        self.usage_task = Some(cx.spawn(async move |this, cx| {
            let result = fetch_usage(http_client.as_ref(), &credentials).await;
            this.update(cx, |state, cx| {
                if state.auth_generation == auth_generation {
                    state.usage_fetched_at = Some(Instant::now());
                    match result {
                        Ok(usage) => {
                            state.usage = Some(usage);
                            state.usage_error = None;
                        }
                        Err(error) => {
                            log::error!("Failed to load Cursor usage: {error:#}");
                            state.usage_error = Some(format!("{error:#}").into());
                        }
                    }
                }
                // Releasing the handle cancels this task, so nothing may follow it.
                state.usage_task = None;
                cx.notify();
            })
            .log_err();
        }));
        cx.notify();
    }

    /// The in-flight load of saved credentials and models, or `None` once it finished.
    pub fn load_task(&self) -> Option<Shared<Task<()>>> {
        self.load_task.clone()
    }

    fn refresh_models(&mut self, cx: &mut Context<Self>) {
        let Some(credentials) = self.credentials() else {
            return;
        };
        let auth_generation = self.auth_generation;
        let http_client = self.http_client.clone();
        let task = cx
            .spawn(async move |this, cx| {
                let result = fetch_models(http_client.as_ref(), &credentials).await;
                this.update(cx, |state, cx| {
                    if state.auth_generation != auth_generation {
                        return;
                    }
                    match result {
                        Ok(models) => {
                            state.models = models;
                            state.models_error = None;
                        }
                        Err(error) => {
                            log::error!("Failed to load Cursor models: {error:#}");
                            state.models_error =
                                Some(format!("Failed to load models: {error:#}").into());
                        }
                    }
                    cx.notify();
                })
                .log_err();
            })
            .shared();
        self.models_task = Some(task);
    }

    /// Opens Cursor's sign-in page in the browser and waits for approval. No-op while a sign-in
    /// is already running.
    pub fn sign_in(&mut self, cx: &mut Context<Self>) {
        if self.is_signing_in() {
            return;
        }
        // Signing in again after expiry keeps the machine identity Cursor already associates
        // with this account.
        let machine_ids = self.credentials.as_ref().map(|credentials| {
            (
                credentials.machine_id.clone(),
                credentials.mac_machine_id.clone(),
            )
        });
        let http_client = self.http_client.clone();
        let credentials_provider = self.credentials_provider.clone();
        self.last_auth_error = None;
        self.sign_in_task = Some(cx.spawn(async move |this, cx| {
            let result = async {
                let credentials = browser_sign_in(http_client, machine_ids, cx).await?;
                let json = serde_json::to_vec(&credentials)?;
                credentials_provider
                    .write_credentials(CREDENTIALS_KEY, "Bearer", &json, cx)
                    .await
                    .context("Failed to save Cursor credentials")?;
                anyhow::Ok(credentials)
            }
            .await;
            this.update(cx, |state, cx| {
                match result {
                    Ok(credentials) => {
                        state.auth_generation = state.auth_generation.wrapping_add(1);
                        state.credentials = Some(credentials);
                        state.models_error = None;
                        state.refresh_models(cx);
                        state.usage = None;
                        state.usage_error = None;
                        state.usage_fetched_at = None;
                        state.usage_task = None;
                        state.refresh_usage(true, cx);
                    }
                    Err(error) => {
                        log::error!("Cursor sign-in failed: {error:#}");
                        state.last_auth_error = Some(format!("Sign-in failed: {error:#}").into());
                    }
                }
                // Releasing the handle cancels this task, so nothing may follow it.
                state.sign_in_task = None;
                cx.notify();
            })
            .log_err();
        }));
        cx.notify();
    }

    pub fn cancel_sign_in(&mut self, cx: &mut Context<Self>) {
        if self.sign_in_task.take().is_some() {
            cx.notify();
        }
    }

    /// Clears credentials immediately; the returned task deletes them from the keychain.
    pub fn sign_out(&mut self, cx: &mut Context<Self>) -> Task<Result<()>> {
        self.auth_generation = self.auth_generation.wrapping_add(1);
        self.credentials = None;
        self.sign_in_task = None;
        self.models_task = None;
        self.models.clear();
        self.models_error = None;
        self.last_auth_error = None;
        self.usage = None;
        self.usage_error = None;
        self.usage_fetched_at = None;
        self.usage_task = None;
        cx.notify();

        let credentials_provider = self.credentials_provider.clone();
        cx.spawn(async move |_this, cx| {
            credentials_provider
                .delete_credentials(CREDENTIALS_KEY, cx)
                .await
                .context("Failed to delete Cursor credentials from the keychain")
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::AppContext as _;
    use serde_json::json;

    fn token_expiring_at(seconds: u64) -> String {
        let payload = URL_SAFE_NO_PAD.encode(json!({ "sub": "user", "exp": seconds }).to_string());
        format!("header.{payload}.signature")
    }

    pub(crate) fn test_credentials() -> Credentials {
        credentials(token_expiring_at(u64::from(u32::MAX)))
    }

    fn credentials(access_token: String) -> Credentials {
        Credentials {
            access_token,
            refresh_token: "refresh".into(),
            machine_id: "machine".into(),
            mac_machine_id: "mac".into(),
        }
    }

    #[test]
    fn reads_jwt_expiry() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or(0);
        assert!(!credentials(token_expiring_at(now + 3_600)).is_expired());
        assert!(credentials(token_expiring_at(now + 60)).is_expired());
        assert_eq!(
            credentials(token_expiring_at(1_000)).expires_at(),
            Some(UNIX_EPOCH + Duration::from_secs(1_000))
        );
        assert_eq!(credentials("not-a-jwt".into()).expires_at(), None);
    }

    #[test]
    fn login_url_carries_challenge_of_verifier() {
        let login = LoginRequest::new();
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(login.verifier.as_bytes()));
        assert!(
            login
                .url
                .starts_with("https://cursor.com/loginDeepControl?")
        );
        assert!(login.url.contains(&format!("challenge={challenge}")));
        assert!(login.url.contains(&format!("uuid={}", login.uuid)));
        assert!(login.url.ends_with("&mode=login&redirectTarget=cli"));
    }

    #[test]
    fn poll_backoff_is_capped() {
        assert_eq!(poll_delay(0), Duration::from_secs(1));
        assert!(poll_delay(3) > poll_delay(2));
        assert_eq!(poll_delay(100), POLL_MAX_DELAY);
    }

    #[test]
    fn checksum_matches_reference() {
        // Reference values computed with Cursor's JavaScript implementation.
        let now = UNIX_EPOCH + Duration::from_millis(1_784_473_211_864);
        assert_eq!(checksum("machine", "mac", now), "nwcJFTOvmachine/mac");
        assert_eq!(checksum("machine", "", now), "nwcJFTOvmachine");
        assert_eq!(new_machine_id().len(), 64);
    }

    #[test]
    fn groups_catalog_variants() {
        let payload = json!({
            "models": [
                { "modelId": "default", "displayName": "Auto" },
                { "modelId": "claude-4.6-sonnet-medium", "displayName": "Claude 4.6 Sonnet Medium", "contextWindow": 200000 },
                { "modelId": "claude-4.6-sonnet-high", "displayName": "Claude 4.6 Sonnet High", "maxTokens": "32000" },
                { "modelId": "cursor-grok-4.6-xhigh-fast", "displayName": "Grok 4.6 Extra High Fast" },
                { "modelId": "cursor-grok-4.6-low-fast", "displayName": "Grok 4.6 Low Fast" },
                { "modelId": "gpt-5-extra", "displayName": "GPT-5" },
                { "displayName": "missing id" }
            ]
        });
        let models = models_from_catalog(&payload);
        let ids: Vec<&str> = models.iter().map(|model| model.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "default",
                "claude-4.6-sonnet",
                "cursor-grok-4.6-fast",
                "gpt-5-extra"
            ]
        );

        let auto = &models[0];
        assert_eq!(auto.display_name, "Auto");
        assert!(auto.effort_levels().is_empty());
        assert_eq!(auto.wire_model_id(Some("high")), "default");

        let sonnet = &models[1];
        assert_eq!(sonnet.display_name, "Claude 4.6 Sonnet");
        assert_eq!(sonnet.effort_levels(), ["medium", "high"]);
        assert_eq!(sonnet.wire_model_id(None), "claude-4.6-sonnet-medium");
        assert_eq!(sonnet.wire_model_id(Some("high")), "claude-4.6-sonnet-high");
        assert_eq!(
            sonnet.wire_model_id(Some("max")),
            "claude-4.6-sonnet-medium"
        );
        assert_eq!(sonnet.context_window, 200_000);
        assert_eq!(sonnet.max_output_tokens, 32_000);

        let grok = &models[2];
        assert_eq!(grok.display_name, "Grok 4.6 Fast");
        assert_eq!(grok.effort_levels(), ["low", "xhigh"]);
        assert_eq!(
            grok.wire_model_id(Some("xhigh")),
            "cursor-grok-4.6-xhigh-fast"
        );

        let language_model = language_model(sonnet);
        assert!(language_model.supports_thinking);
        let defaults: Vec<_> = language_model
            .supported_effort_levels
            .iter()
            .map(|level| (level.value.as_ref(), level.is_default))
            .collect();
        assert_eq!(defaults, [("medium", true), ("high", false)]);
    }

    struct FakeCredentialsProvider {
        storage: parking_lot::Mutex<Option<(String, Vec<u8>)>>,
    }

    impl CredentialsProvider for FakeCredentialsProvider {
        fn read_credentials<'a>(
            &'a self,
            _url: &'a str,
            _cx: &'a AsyncApp,
        ) -> std::pin::Pin<Box<dyn Future<Output = Result<Option<(String, Vec<u8>)>>> + 'a>>
        {
            Box::pin(async { Ok(self.storage.lock().clone()) })
        }

        fn write_credentials<'a>(
            &'a self,
            _url: &'a str,
            username: &'a str,
            password: &'a [u8],
            _cx: &'a AsyncApp,
        ) -> std::pin::Pin<Box<dyn Future<Output = Result<()>> + 'a>> {
            self.storage
                .lock()
                .replace((username.to_string(), password.to_vec()));
            Box::pin(async { Ok(()) })
        }

        fn delete_credentials<'a>(
            &'a self,
            _url: &'a str,
            _cx: &'a AsyncApp,
        ) -> std::pin::Pin<Box<dyn Future<Output = Result<()>> + 'a>> {
            *self.storage.lock() = None;
            Box::pin(async { Ok(()) })
        }
    }

    /// Answers the sign-in poll with tokens and the catalog with one model.
    fn fake_cursor_api() -> Arc<dyn HttpClient> {
        http_client::FakeHttpClient::create(|request| async move {
            let path = request.uri().path().to_string();
            let body = if path == "/auth/poll" {
                json!({
                    "accessToken": test_credentials().access_token,
                    "refreshToken": "refresh",
                })
            } else if path.ends_with("/GetUsableModels") {
                json!({ "models": [{ "modelId": "default", "displayName": "Auto" }] })
            } else {
                return Ok(http_client::Response::builder()
                    .status(404)
                    .body(AsyncBody::default())?);
            };
            Ok(http_client::Response::builder()
                .status(200)
                .body(AsyncBody::from(body.to_string()))?)
        })
    }

    #[gpui::test]
    async fn sign_in_loads_models(cx: &mut gpui::TestAppContext) {
        let credentials_provider = Arc::new(FakeCredentialsProvider {
            storage: parking_lot::Mutex::new(None),
        });
        let state = cx.new(|cx| State::new(fake_cursor_api(), credentials_provider.clone(), cx));
        cx.run_until_parked();
        state.read_with(cx, |state, _| assert!(!state.is_authenticated()));

        state.update(cx, |state, cx| state.sign_in(cx));
        cx.run_until_parked();
        state.read_with(cx, |state, _| {
            assert!(state.is_authenticated());
            assert!(!state.is_signing_in());
            assert_eq!(state.models().len(), 1);
        });
        assert!(credentials_provider.storage.lock().is_some());

        // A restart loads the saved credentials and the models again.
        let state = cx.new(|cx| State::new(fake_cursor_api(), credentials_provider.clone(), cx));
        cx.run_until_parked();
        state.read_with(cx, |state, _| {
            assert!(state.is_authenticated());
            assert!(state.load_task().is_none());
            assert_eq!(state.models().len(), 1);
        });
    }

    #[test]
    fn parses_unary_payloads() -> Result<()> {
        assert_eq!(
            parse_unary_payload(br#"{"models":[]}"#)?,
            json!({ "models": [] })
        );
        let body = br#"{"models":[1]}"#;
        let mut framed = vec![0];
        framed.extend_from_slice(&(body.len() as u32).to_be_bytes());
        framed.extend_from_slice(body);
        assert_eq!(parse_unary_payload(&framed)?, json!({ "models": [1] }));
        assert!(parse_unary_payload(&[0, 0, 0, 0, 9, b'{']).is_err());
        Ok(())
    }
}
