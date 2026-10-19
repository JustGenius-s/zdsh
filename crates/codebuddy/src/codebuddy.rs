use std::collections::HashSet;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use futures::AsyncReadExt as _;
use http_client::{
    AsyncBody, CustomHeaders, HttpClient, Method, Request as HttpRequest, RequestTimeout,
    http::{HeaderName, HeaderValue},
};
use open_ai::{ReasoningEffort, ResponseStreamEvent};
use serde_json::Value;
use uuid::Uuid;

pub const CODEBUDDY_API_URL: &str = "https://copilot.tencent.com/v2";

// `/v3/config` rejects requests whose user agent does not name a CLI version it recognizes
// (HTTP 400, code 12403), so both constants must move together.
const CLI_VERSION: &str = "2.113.0";
const USER_AGENT: &str = "CLI/2.113.0 CodeBuddy/2.113.0";

// Catalog discovery sits in front of the first chat request, so a stuck upstream must not stall it.
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(10);

const FALLBACK_REASONING_EFFORTS: [ReasoningEffort; 5] = [
    ReasoningEffort::Low,
    ReasoningEffort::Medium,
    ReasoningEffort::High,
    ReasoningEffort::XHigh,
    ReasoningEffort::Max,
];

#[derive(Clone, Debug, PartialEq)]
pub struct Model {
    pub id: String,
    pub display_name: String,
    pub max_input_tokens: u64,
    pub max_output_tokens: Option<u64>,
    pub supports_tools: bool,
    pub supports_images: bool,
    pub temperature: Option<f32>,
    pub reasoning_efforts: Vec<ReasoningEffort>,
    pub default_reasoning_effort: Option<ReasoningEffort>,
}

impl Model {
    pub fn supports_reasoning(&self) -> bool {
        !self.reasoning_efforts.is_empty()
    }

    pub fn is_lite(&self) -> bool {
        let id = self.id.to_ascii_lowercase();
        ["flash", "echo", "lite"]
            .iter()
            .any(|marker| id.contains(marker))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Identity {
    pub uid: String,
    pub enterprise_id: String,
}

/// The configured URL may point at the chat endpoint or a versioned prefix; the account and
/// catalog endpoints live at the product root.
pub fn product_origin(api_url: &str) -> String {
    let mut base = chat_api_url(api_url);
    for version in ["/v2", "/v1"] {
        if let Some(stripped) = base.strip_suffix(version) {
            base = stripped.trim_end_matches('/').to_string();
        }
    }
    base
}

/// The base that `/chat/completions` is appended to.
pub fn chat_api_url(api_url: &str) -> String {
    let base = api_url.trim().trim_end_matches('/');
    base.strip_suffix("/chat/completions")
        .unwrap_or(base)
        .trim_end_matches('/')
        .to_string()
}

/// Resolves the tenant identity that `/v3/config` requires; without it the catalog silently
/// comes back empty.
pub async fn discover_identity(
    client: &dyn HttpClient,
    api_url: &str,
    api_key: &str,
) -> Result<Identity> {
    let url = format!("{}/v2/accounts", product_origin(api_url));
    let body = get_json(client, &url, api_key, None)
        .await
        .context("CodeBuddy account lookup failed")?;
    identity_from_accounts(&body)
        .ok_or_else(|| anyhow!("CodeBuddy returned no account with both a uid and an enterpriseId"))
}

pub async fn fetch_models(
    client: &dyn HttpClient,
    api_url: &str,
    api_key: &str,
) -> Result<Vec<Model>> {
    let identity = discover_identity(client, api_url, api_key).await?;
    let url = format!("{}/v3/config", product_origin(api_url));
    let body = get_json(client, &url, api_key, Some(&identity))
        .await
        .context("CodeBuddy model catalog request failed")?;
    let payload = body
        .get("data")
        .filter(|data| data.is_object())
        .unwrap_or(&body);
    let models = models_from_config(payload);
    if models.is_empty() {
        let has_raw_models = payload
            .get("models")
            .and_then(Value::as_array)
            .is_some_and(|models| !models.is_empty());
        if has_raw_models {
            bail!("CodeBuddy returned models, but none of them can be used for chat");
        }
        bail!("CodeBuddy returned no models for this account");
    }
    Ok(models)
}

async fn get_json(
    client: &dyn HttpClient,
    url: &str,
    api_key: &str,
    identity: Option<&Identity>,
) -> Result<Value> {
    let mut builder = HttpRequest::builder()
        .method(Method::GET)
        .uri(url)
        .header("Authorization", format!("Bearer {}", api_key.trim()))
        .header("Accept", "application/json")
        .header("User-Agent", USER_AGENT)
        .extension(RequestTimeout(DISCOVERY_TIMEOUT));
    if let Some(identity) = identity {
        builder = builder
            .header("X-User-Id", identity.uid.as_str())
            .header("X-Enterprise-Id", identity.enterprise_id.as_str());
    }
    let request = builder.body(AsyncBody::default())?;
    let mut response = client.send(request).await?;
    let mut body = String::new();
    response.body_mut().read_to_string(&mut body).await?;
    if !response.status().is_success() {
        let detail: String = body.trim().chars().take(200).collect();
        bail!("HTTP {} {}", response.status(), detail);
    }
    serde_json::from_str(&body).context("CodeBuddy response was not JSON")
}

fn identity_from_accounts(body: &Value) -> Option<Identity> {
    let accounts = body.get("data")?.get("accounts")?.as_array()?;
    let mut first_listed = None;
    for account in accounts {
        let uid = account.get("uid").and_then(Value::as_str).map(str::trim);
        let enterprise_id = account
            .get("enterpriseId")
            .and_then(Value::as_str)
            .map(str::trim);
        let (Some(uid), Some(enterprise_id)) = (uid, enterprise_id) else {
            continue;
        };
        if uid.is_empty() || enterprise_id.is_empty() {
            continue;
        }
        let identity = Identity {
            uid: uid.to_string(),
            enterprise_id: enterprise_id.to_string(),
        };
        if account.get("lastLogin").and_then(Value::as_bool) == Some(true) {
            return Some(identity);
        }
        first_listed.get_or_insert(identity);
    }
    first_listed
}

fn models_from_config(payload: &Value) -> Vec<Model> {
    let Some(source) = payload
        .get("models")
        .and_then(Value::as_array)
        .or_else(|| payload.get("data").and_then(Value::as_array))
    else {
        return Vec::new();
    };

    let mut seen = HashSet::new();
    let mut models: Vec<Model> = source
        .iter()
        .filter_map(model_from_config)
        .filter(|model| seen.insert(model.id.to_ascii_lowercase()))
        .collect();

    let preferred = cli_model_order(payload);
    if !preferred.is_empty() {
        models.sort_by_key(|model| {
            preferred
                .iter()
                .position(|id| id.eq_ignore_ascii_case(&model.id))
                .unwrap_or(usize::MAX)
        });
    }
    models
}

/// The CLI agent entry lists the models CodeBuddy recommends first for coding.
fn cli_model_order(payload: &Value) -> Vec<String> {
    let Some(agents) = payload.get("agents").and_then(Value::as_array) else {
        return Vec::new();
    };
    let cli_agent = agents.iter().find(|agent| {
        agent.get("name").and_then(Value::as_str) == Some("cli")
            || agent
                .get("tags")
                .and_then(Value::as_array)
                .is_some_and(|tags| tags.iter().any(|tag| tag.as_str() == Some("cli")))
    });
    cli_agent
        .and_then(|agent| agent.get("models"))
        .and_then(Value::as_array)
        .map(|ids| {
            ids.iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn model_from_config(item: &Value) -> Option<Model> {
    let id = item.get("id")?.as_str()?.trim();
    if id.is_empty() || !is_chat_model(id) {
        return None;
    }
    let display_name = item
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or(id);

    let reasoning = item.get("reasoning");
    let default_effort_name = reasoning.and_then(|reasoning| {
        reasoning
            .get("effort")
            .and_then(Value::as_str)
            .or_else(|| reasoning.get("defaultEffort").and_then(Value::as_str))
    });
    let supports_reasoning = bool_field(item, "supportsReasoning")
        || bool_field(item, "onlyReasoning")
        || default_effort_name.is_some();
    let mut reasoning_efforts: Vec<ReasoningEffort> = if supports_reasoning {
        reasoning
            .and_then(|reasoning| reasoning.get("supportedEfforts"))
            .and_then(Value::as_array)
            .map(|efforts| {
                efforts
                    .iter()
                    .filter_map(Value::as_str)
                    .filter_map(parse_reasoning_effort)
                    .collect()
            })
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    if supports_reasoning && reasoning_efforts.is_empty() {
        reasoning_efforts = FALLBACK_REASONING_EFFORTS.to_vec();
    }
    let default_reasoning_effort = default_effort_name
        .and_then(parse_reasoning_effort)
        .filter(|effort| reasoning_efforts.contains(effort));

    Some(Model {
        id: id.to_string(),
        display_name: display_name.to_string(),
        max_input_tokens: number_field(item, "maxInputTokens").unwrap_or(0),
        max_output_tokens: number_field(item, "maxOutputTokens").filter(|tokens| *tokens > 0),
        supports_tools: bool_field(item, "supportsToolCall") || bool_field(item, "supportsTools"),
        supports_images: bool_field(item, "supportsImages"),
        temperature: item
            .get("temperature")
            .and_then(Value::as_f64)
            .map(|temperature| temperature as f32),
        reasoning_efforts,
        default_reasoning_effort,
    })
}

fn is_chat_model(id: &str) -> bool {
    !id.to_ascii_lowercase().contains("image") && id != "completion-gf" && id != "nes-gf"
}

fn bool_field(item: &Value, key: &str) -> bool {
    item.get(key).and_then(Value::as_bool) == Some(true)
}

fn number_field(item: &Value, key: &str) -> Option<u64> {
    match item.get(key)? {
        Value::Number(number) => number.as_u64().or_else(|| {
            number
                .as_f64()
                .filter(|value| *value >= 0.0)
                .map(|value| value as u64)
        }),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }
}

fn parse_reasoning_effort(name: &str) -> Option<ReasoningEffort> {
    name.trim()
        .to_ascii_lowercase()
        .parse::<ReasoningEffort>()
        .ok()
        .filter(|effort| *effort != ReasoningEffort::None)
}

/// Builds the CodeBuddy client headers for one chat request. Headers from settings take
/// precedence over the defaults with the same name.
pub fn chat_headers(
    conversation_id: Option<&str>,
    custom_headers: &CustomHeaders,
) -> Result<CustomHeaders> {
    let conversation_id = conversation_id
        .map(|id| id.replace('-', ""))
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| Uuid::new_v4().simple().to_string());
    let defaults = [
        ("accept", "application/json, text/event-stream".to_string()),
        ("user-agent", USER_AGENT.to_string()),
        ("x-codebuddy-request", "1".to_string()),
        ("x-agent-intent", "craft".to_string()),
        ("x-agent-purpose", "conversation".to_string()),
        ("x-private-data", "false".to_string()),
        ("x-ide-type", "CLI".to_string()),
        ("x-ide-name", "CLI".to_string()),
        ("x-ide-version", CLI_VERSION.to_string()),
        ("x-conversation-id", conversation_id),
        (
            "x-conversation-request-id",
            Uuid::new_v4().simple().to_string(),
        ),
        (
            "x-conversation-message-id",
            Uuid::new_v4().simple().to_string(),
        ),
        ("x-request-id", Uuid::new_v4().to_string()),
    ];

    let mut headers = Vec::with_capacity(defaults.len() + custom_headers.iter().len());
    for (name, value) in defaults {
        let name = HeaderName::from_static(name);
        if custom_headers.iter().any(|(custom, _)| *custom == name) {
            continue;
        }
        let value = HeaderValue::from_str(&value)
            .with_context(|| format!("invalid value for CodeBuddy header `{name}`"))?;
        headers.push((name, value));
    }
    headers.extend(
        custom_headers
            .iter()
            .map(|(name, value)| (name.clone(), value.clone())),
    );
    Ok(CustomHeaders::new(headers))
}

/// CodeBuddy sends `finish_reason: ""` on intermediate chunks. Read as a real finish reason it
/// would end the turn early and drop tool calls that are still streaming.
pub fn normalize_stream_event(event: &mut ResponseStreamEvent) {
    for choice in &mut event.choices {
        if choice
            .finish_reason
            .as_deref()
            .is_some_and(|reason| reason.trim().is_empty())
        {
            choice.finish_reason = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn user_agent_names_cli_version() {
        assert_eq!(
            USER_AGENT,
            format!("CLI/{CLI_VERSION} CodeBuddy/{CLI_VERSION}")
        );
    }

    #[test]
    fn normalizes_configured_urls() {
        for url in [
            "https://copilot.tencent.com/v2",
            "https://copilot.tencent.com/v2/",
            "https://copilot.tencent.com/v2/chat/completions",
        ] {
            assert_eq!(chat_api_url(url), "https://copilot.tencent.com/v2");
            assert_eq!(product_origin(url), "https://copilot.tencent.com");
        }
        assert_eq!(
            product_origin("https://copilot.tencent.com/v1"),
            "https://copilot.tencent.com"
        );
    }

    #[test]
    fn prefers_last_login_account() {
        let body = json!({
            "data": {
                "accounts": [
                    { "uid": "first", "enterpriseId": "e1" },
                    { "uid": " ", "enterpriseId": "e2", "lastLogin": true },
                    { "uid": "recent", "enterpriseId": "e3", "lastLogin": true },
                ]
            }
        });
        assert_eq!(
            identity_from_accounts(&body),
            Some(Identity {
                uid: "recent".into(),
                enterprise_id: "e3".into(),
            })
        );

        let body = json!({ "data": { "accounts": [{ "uid": "only", "enterpriseId": "e" }] } });
        assert_eq!(
            identity_from_accounts(&body).map(|identity| identity.uid),
            Some("only".into())
        );
        assert_eq!(identity_from_accounts(&json!({ "data": {} })), None);
    }

    #[test]
    fn parses_catalog() {
        let payload = json!({
            "agents": [{ "name": "cli", "models": ["glm-5", "claude-sonnet"] }],
            "models": [
                {
                    "id": "claude-sonnet",
                    "name": "Claude Sonnet",
                    "maxInputTokens": 200000,
                    "maxOutputTokens": "64000",
                    "supportsToolCall": true,
                    "supportsImages": true,
                    "reasoning": { "defaultEffort": "high", "supportedEfforts": ["low", "high", "bogus"] }
                },
                { "id": "default-image-generation" },
                { "id": "nes-gf" },
                { "id": "deepseek-v3", "supportsTools": true, "temperature": 0.6 },
                { "id": "GLM-5", "supportsReasoning": true },
                { "id": "glm-5", "name": "duplicate" },
                { "name": "missing id" }
            ]
        });
        let models = models_from_config(&payload);
        let ids: Vec<&str> = models.iter().map(|model| model.id.as_str()).collect();
        assert_eq!(ids, ["GLM-5", "claude-sonnet", "deepseek-v3"]);

        let glm = &models[0];
        assert_eq!(glm.reasoning_efforts, FALLBACK_REASONING_EFFORTS.to_vec());
        assert_eq!(glm.default_reasoning_effort, None);
        assert!(!glm.supports_tools);

        let claude = &models[1];
        assert_eq!(claude.display_name, "Claude Sonnet");
        assert_eq!(claude.max_input_tokens, 200_000);
        assert_eq!(claude.max_output_tokens, Some(64_000));
        assert!(claude.supports_tools && claude.supports_images);
        assert_eq!(
            claude.reasoning_efforts,
            vec![ReasoningEffort::Low, ReasoningEffort::High]
        );
        assert_eq!(claude.default_reasoning_effort, Some(ReasoningEffort::High));

        let deepseek = &models[2];
        assert!(!deepseek.supports_reasoning());
        assert_eq!(deepseek.temperature, Some(0.6));
        assert_eq!(deepseek.max_output_tokens, None);
    }

    #[test]
    fn builds_chat_headers() -> Result<()> {
        let custom = CustomHeaders::new(vec![(
            HeaderName::from_static("x-ide-name"),
            HeaderValue::from_static("Zed"),
        )]);
        let headers = chat_headers(Some("0b3c-41aa"), &custom)?;
        let value = |name: &str| {
            headers
                .iter()
                .filter(|(header, _)| header.as_str() == name)
                .map(|(_, value)| value.to_str().unwrap_or_default().to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(value("x-conversation-id"), ["0b3c41aa"]);
        assert_eq!(value("x-ide-name"), ["Zed"]);
        assert_eq!(value("user-agent"), [USER_AGENT]);
        assert_eq!(value("x-request-id").len(), 1);

        let headers = chat_headers(None, &CustomHeaders::default())?;
        let conversation_id = headers
            .iter()
            .find(|(header, _)| header.as_str() == "x-conversation-id")
            .map(|(_, value)| value.len());
        assert_eq!(conversation_id, Some(32));
        Ok(())
    }

    #[test]
    fn clears_empty_finish_reason() -> Result<()> {
        let mut event: ResponseStreamEvent = serde_json::from_value(json!({
            "choices": [
                { "index": 0, "delta": { "content": "hi" }, "finish_reason": "" },
                { "index": 1, "delta": null, "finish_reason": "tool_calls" }
            ]
        }))?;
        normalize_stream_event(&mut event);
        assert_eq!(event.choices[0].finish_reason, None);
        assert_eq!(
            event.choices[1].finish_reason.as_deref(),
            Some("tool_calls")
        );
        Ok(())
    }
}
