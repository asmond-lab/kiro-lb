//! Dashboard-adjustable settings: environment default, persisted override, in-memory cache.
//! Reads are on the request path and never touch SQLite.

use parking_lot::RwLock;
use serde_json::{json, Value};

use crate::{config, store};

#[derive(Debug)]
pub struct InvalidSetting(pub String);

impl std::fmt::Display for InvalidSetting {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

fn bounded_int(raw: &Value, low: i64, high: i64) -> Result<i64, InvalidSetting> {
    if raw.is_boolean() {
        return Err(InvalidSetting("expected a number, got a boolean".into()));
    }
    let number = match raw {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f.trunc() as i64)),
        Value::String(s) => s.trim().parse::<i64>().ok(),
        _ => None,
    }
    .ok_or_else(|| InvalidSetting(format!("expected a whole number, got {raw}")))?;
    if !(low..=high).contains(&number) {
        return Err(InvalidSetting(format!("must be between {low} and {high}")));
    }
    Ok(number)
}

pub const LOAD_BALANCING_STRATEGIES: [&str; 4] = ["weighted", "sticky", "most_credits", "session"];

#[derive(Clone, Debug)]
pub struct Tunables {
    pub token_refresh_seconds: i64,
    pub load_balancing: String,
    pub max_concurrency: i64,
    pub max_account_concurrency: i64,
    pub queue_timeout_seconds: i64,
}

impl Tunables {
    fn defaults() -> Tunables {
        let cfg = config::get();
        Tunables {
            token_refresh_seconds: cfg.token_refresh_threshold,
            load_balancing: if cfg.quota_weighted_routing {
                "session"
            } else {
                "sticky"
            }
            .into(),
            max_concurrency: 0,
            max_account_concurrency: 0,
            queue_timeout_seconds: 30,
        }
    }
}

pub enum TunableKey {
    TokenRefreshSeconds,
    LoadBalancing,
    MaxConcurrency,
    MaxAccountConcurrency,
    QueueTimeoutSeconds,
}

impl TunableKey {
    pub const ALL: [TunableKey; 5] = [
        TunableKey::TokenRefreshSeconds,
        TunableKey::LoadBalancing,
        TunableKey::MaxConcurrency,
        TunableKey::MaxAccountConcurrency,
        TunableKey::QueueTimeoutSeconds,
    ];

    pub fn store_key(&self) -> &'static str {
        match self {
            TunableKey::TokenRefreshSeconds => "token_refresh_seconds",
            TunableKey::LoadBalancing => "load_balancing",
            TunableKey::MaxConcurrency => "max_concurrency",
            TunableKey::MaxAccountConcurrency => "max_account_concurrency",
            TunableKey::QueueTimeoutSeconds => "queue_timeout_seconds",
        }
    }

    pub fn api_key(&self) -> &'static str {
        match self {
            TunableKey::TokenRefreshSeconds => "tokenRefreshSeconds",
            TunableKey::LoadBalancing => "loadBalancing",
            TunableKey::MaxConcurrency => "maxConcurrency",
            TunableKey::MaxAccountConcurrency => "maxAccountConcurrency",
            TunableKey::QueueTimeoutSeconds => "queueTimeoutSeconds",
        }
    }

    fn coerce(&self, raw: &Value) -> Result<Value, InvalidSetting> {
        Ok(match self {
            TunableKey::TokenRefreshSeconds => json!(bounded_int(raw, 60, 3600)?),
            TunableKey::MaxConcurrency => json!(bounded_int(raw, 0, 512)?),
            TunableKey::MaxAccountConcurrency => json!(bounded_int(raw, 0, 128)?),
            TunableKey::QueueTimeoutSeconds => json!(bounded_int(raw, 1, 600)?),
            TunableKey::LoadBalancing => {
                let s = raw
                    .as_str()
                    .ok_or_else(|| InvalidSetting("expected a string".into()))?
                    .trim();
                if !LOAD_BALANCING_STRATEGIES.contains(&s) {
                    let allowed: Vec<String> = LOAD_BALANCING_STRATEGIES
                        .iter()
                        .map(|v| format!("'{v}'"))
                        .collect();
                    return Err(InvalidSetting(format!(
                        "must be one of {}",
                        allowed.join(", ")
                    )));
                }
                json!(s)
            }
        })
    }

    fn apply(&self, t: &mut Tunables, v: &Value) {
        match self {
            TunableKey::TokenRefreshSeconds => {
                t.token_refresh_seconds = v.as_i64().unwrap_or(t.token_refresh_seconds)
            }
            TunableKey::LoadBalancing => {
                t.load_balancing = v.as_str().unwrap_or(&t.load_balancing).to_owned()
            }
            TunableKey::MaxConcurrency => t.max_concurrency = v.as_i64().unwrap_or(0),
            TunableKey::MaxAccountConcurrency => {
                t.max_account_concurrency = v.as_i64().unwrap_or(0)
            }
            TunableKey::QueueTimeoutSeconds => t.queue_timeout_seconds = v.as_i64().unwrap_or(30),
        }
    }
}

static TUNABLES: RwLock<Option<Tunables>> = RwLock::new(None);

pub fn tunables() -> Tunables {
    if let Some(t) = TUNABLES.read().as_ref() {
        return t.clone();
    }
    let mut guard = TUNABLES.write();
    guard.get_or_insert_with(Tunables::defaults).clone()
}

pub fn load_tunables() {
    let mut t = Tunables::defaults();
    for key in TunableKey::ALL {
        if let Some(stored) = store::load_setting(key.store_key()) {
            match key.coerce(&stored) {
                Ok(v) => key.apply(&mut t, &v),
                Err(e) => tracing::warn!("[Tunables] Ignoring persisted {}: {e}", key.store_key()),
            }
        }
    }
    *TUNABLES.write() = Some(t);
}

pub fn set_tunable(key: &TunableKey, raw: &Value) -> Result<Result<(), String>, InvalidSetting> {
    let value = key.coerce(raw)?;
    if let Err(e) = store::save_setting(key.store_key(), &value) {
        return Ok(Err(e.to_string()));
    }
    let mut guard = TUNABLES.write();
    let t = guard.get_or_insert_with(Tunables::defaults);
    key.apply(t, &value);
    tracing::info!("[Tunables] {} set to {value}", key.store_key());
    Ok(Ok(()))
}

pub fn tunables_snapshot() -> Value {
    let t = tunables();
    json!({
        "tokenRefreshSeconds": t.token_refresh_seconds,
        "loadBalancing": t.load_balancing,
        "loadBalancingOptions": LOAD_BALANCING_STRATEGIES,
        "maxConcurrency": t.max_concurrency,
        "maxAccountConcurrency": t.max_account_concurrency,
        "queueTimeoutSeconds": t.queue_timeout_seconds,
    })
}

// ----- agent task mode --------------------------------------------------------------------

pub const AGENT_MODES: [&str; 4] = ["", "vibe", "spec", "task"];
static AGENT_MODE: RwLock<Option<String>> = RwLock::new(None);

fn agent_env_default() -> String {
    let candidate = config::get().agent_task_type.trim().to_owned();
    if !AGENT_MODES.contains(&candidate.as_str()) {
        tracing::warn!(
            "[AgentMode] Ignoring unknown KIRO_AGENT_TASK_TYPE={candidate:?}; omitting the field"
        );
        return String::new();
    }
    candidate
}

pub fn agent_mode() -> String {
    if let Some(m) = AGENT_MODE.read().as_ref() {
        return m.clone();
    }
    AGENT_MODE
        .write()
        .get_or_insert_with(agent_env_default)
        .clone()
}

pub fn validate_agent_mode(value: &Value) -> Result<String, InvalidSetting> {
    match value {
        Value::Null => Ok(String::new()),
        Value::String(s) => {
            let candidate = s.trim();
            if AGENT_MODES.contains(&candidate) {
                Ok(candidate.to_owned())
            } else {
                let known: Vec<String> = AGENT_MODES.iter().map(|m| format!("'{m}'")).collect();
                Err(InvalidSetting(format!(
                    "unknown mode '{candidate}'; allowed values are {}",
                    known.join(", ")
                )))
            }
        }
        _ => Err(InvalidSetting("mode must be a string".into())),
    }
}

pub fn load_agent_mode() {
    let mut resolved = agent_env_default();
    if let Some(stored) = store::load_setting("agent_task_type") {
        match validate_agent_mode(&stored) {
            Ok(m) => resolved = m,
            Err(e) => tracing::warn!("[AgentMode] Ignoring persisted mode: {e}"),
        }
    }
    *AGENT_MODE.write() = Some(resolved);
}

pub fn set_agent_mode(value: &Value) -> Result<Result<String, String>, InvalidSetting> {
    let mode = validate_agent_mode(value)?;
    if let Err(e) = store::save_setting("agent_task_type", &json!(mode)) {
        return Ok(Err(e.to_string()));
    }
    *AGENT_MODE.write() = Some(mode.clone());
    Ok(Ok(mode))
}

// ----- prompt filter flags ----------------------------------------------------------------

#[derive(Clone, Copy)]
pub struct PromptFilterFlags {
    pub condense: bool,
    pub shorten_tools: bool,
}

static PROMPT_FLAGS: RwLock<Option<PromptFilterFlags>> = RwLock::new(None);

fn prompt_defaults() -> PromptFilterFlags {
    let cfg = config::get();
    PromptFilterFlags {
        condense: cfg.condense_claude_prompt,
        shorten_tools: cfg.shorten_claude_tools,
    }
}

pub fn prompt_flags() -> PromptFilterFlags {
    if let Some(f) = *PROMPT_FLAGS.read() {
        return f;
    }
    *PROMPT_FLAGS.write().get_or_insert_with(prompt_defaults)
}

pub fn load_prompt_flags() {
    let mut flags = prompt_defaults();
    if let Some(Value::Bool(b)) = store::load_setting("condense_claude_prompt") {
        flags.condense = b;
    }
    if let Some(Value::Bool(b)) = store::load_setting("shorten_claude_tools") {
        flags.shorten_tools = b;
    }
    *PROMPT_FLAGS.write() = Some(flags);
}

pub fn set_prompt_flag(key: &str, value: bool) -> Result<(), String> {
    store::save_setting(key, &json!(value)).map_err(|e| e.to_string())?;
    let mut guard = PROMPT_FLAGS.write();
    let flags = guard.get_or_insert_with(prompt_defaults);
    match key {
        "condense_claude_prompt" => flags.condense = value,
        "shorten_claude_tools" => flags.shorten_tools = value,
        _ => {}
    }
    tracing::info!("[PromptFilter] {key} = {value}");
    Ok(())
}

// ----- endpoint settings ------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct EndpointSettings {
    pub rotation: bool,
    pub order: Vec<String>,
    pub cooldown_seconds: f64,
}

impl EndpointSettings {
    pub fn as_json(&self) -> Value {
        json!({"rotation": self.rotation, "order": self.order, "cooldownSeconds": self.cooldown_seconds})
    }
}

fn known_keys(keys: &[String]) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for k in keys {
        let k = k.trim();
        if !k.is_empty()
            && crate::upstream::endpoints::by_key(k).is_some()
            && !seen.iter().any(|s| s == k)
        {
            seen.push(k.to_owned());
        }
    }
    seen
}

fn endpoint_env_defaults() -> EndpointSettings {
    let cfg = config::get();
    let mut order = known_keys(&cfg.endpoint_order);
    if order.is_empty() {
        order = crate::upstream::endpoints::ENDPOINTS
            .iter()
            .map(|e| e.key.to_owned())
            .collect();
    }
    EndpointSettings {
        rotation: cfg.endpoint_rotation,
        order,
        cooldown_seconds: cfg.endpoint_cooldown_seconds,
    }
}

pub fn validate_endpoints(
    rotation: &Value,
    order: &Value,
    cooldown: &Value,
) -> Result<EndpointSettings, InvalidSetting> {
    let rotation = rotation
        .as_bool()
        .ok_or_else(|| InvalidSetting("rotation must be a boolean".into()))?;
    let list = order
        .as_array()
        .ok_or_else(|| InvalidSetting("order must be a list of endpoint keys".into()))?;
    let unknown: Vec<&Value> = list
        .iter()
        .filter(|k| {
            k.as_str()
                .is_none_or(|s| crate::upstream::endpoints::by_key(s.trim()).is_none())
        })
        .collect();
    if !unknown.is_empty() {
        let mut known: Vec<&str> = crate::upstream::endpoints::ENDPOINTS
            .iter()
            .map(|e| e.key)
            .collect();
        known.sort();
        return Err(InvalidSetting(format!(
            "unknown endpoint keys: {}; known keys are {}",
            Value::Array(unknown.into_iter().cloned().collect()),
            known.join(", ")
        )));
    }
    let keys: Vec<String> = list
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect();
    let resolved = known_keys(&keys);
    if resolved.is_empty() {
        return Err(InvalidSetting(
            "at least one endpoint must stay enabled".into(),
        ));
    }
    let cooldown = match cooldown {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
    .ok_or_else(|| InvalidSetting("cooldownSeconds must be a number".into()))?;
    if !(0.0..=3600.0).contains(&cooldown) {
        return Err(InvalidSetting(
            "cooldownSeconds must be between 0.0 and 3600.0".into(),
        ));
    }
    Ok(EndpointSettings {
        rotation,
        order: resolved,
        cooldown_seconds: cooldown,
    })
}

static ENDPOINTS: RwLock<Option<EndpointSettings>> = RwLock::new(None);

pub fn endpoint_settings() -> EndpointSettings {
    if let Some(s) = ENDPOINTS.read().as_ref() {
        return s.clone();
    }
    ENDPOINTS
        .write()
        .get_or_insert_with(endpoint_env_defaults)
        .clone()
}

pub fn load_endpoint_settings() {
    let defaults = endpoint_env_defaults();
    let mut settings = defaults.clone();
    if let Some(Value::Object(p)) = store::load_setting("endpoints") {
        let rotation = p
            .get("rotation")
            .cloned()
            .unwrap_or(json!(defaults.rotation));
        let order = p.get("order").cloned().unwrap_or(json!(defaults.order));
        let cooldown = p
            .get("cooldownSeconds")
            .cloned()
            .unwrap_or(json!(defaults.cooldown_seconds));
        match validate_endpoints(&rotation, &order, &cooldown) {
            Ok(s) => settings = s,
            Err(e) => tracing::warn!("[Endpoints] Ignoring persisted settings: {e}"),
        }
    }
    *ENDPOINTS.write() = Some(settings);
}

pub fn update_endpoints(
    rotation: &Value,
    order: &Value,
    cooldown: &Value,
) -> Result<Result<EndpointSettings, String>, InvalidSetting> {
    let settings = validate_endpoints(rotation, order, cooldown)?;
    if let Err(e) = store::save_setting("endpoints", &settings.as_json()) {
        return Ok(Err(e.to_string()));
    }
    *ENDPOINTS.write() = Some(settings.clone());
    Ok(Ok(settings))
}

pub fn load_all() {
    load_endpoint_settings();
    load_prompt_flags();
    load_agent_mode();
    load_tunables();
}

pub fn set_prompt_flag_for_test(condense: bool, shorten_tools: bool) {
    *PROMPT_FLAGS.write() = Some(PromptFilterFlags {
        condense,
        shorten_tools,
    });
}
