//! Per-account token lifecycle: Kiro Desktop refresh and AWS SSO OIDC, credentials
//! from the internal store, an external JSON file or a kiro-cli SQLite database,
//! and a cross-process refresh lease so blue/green slots never refresh twice.

use parking_lot::Mutex;
use regex::Regex;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::{config, settings, store};

const SQLITE_TOKEN_KEYS: [&str; 3] = [
    "kirocli:social:token",
    "kirocli:odic:token",
    "codewhisperer:odic:token",
];
const SQLITE_REGISTRATION_KEYS: [&str; 2] = [
    "kirocli:odic:device-registration",
    "codewhisperer:odic:device-registration",
];

/// Holds the durable refresh lease and releases it on drop, so a cancelled
/// refresh (client disconnect, aborted task) cannot strand the lease.
pub struct RefreshLease {
    pub account: String,
    pub owner: String,
}

impl Drop for RefreshLease {
    fn drop(&mut self) {
        store::release_refresh_lease(&self.account, &self.owner);
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AuthType {
    KiroDesktop,
    AwsSsoOidc,
}

#[derive(Debug)]
pub enum AuthError {
    CredentialDead { account: String, status: u16 },
    Http { status: u16, body: String },
    Other(String),
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuthError::CredentialDead { account, status } => {
                write!(f, "Refresh token for {account} was rejected by the auth host (HTTP {status}); re-login required")
            }
            AuthError::Http { status, .. } => write!(f, "token refresh failed with HTTP {status}"),
            AuthError::Other(m) => f.write_str(m),
        }
    }
}

pub fn is_credential_dead_status(status: u16) -> bool {
    matches!(status, 400 | 401 | 403)
}

#[derive(Default, Clone)]
struct Creds {
    refresh_token: Option<String>,
    access_token: Option<String>,
    profile_arn: Option<String>,
    sso_region: Option<String>,
    detected_api_region: Option<String>,
    client_id: Option<String>,
    client_secret: Option<String>,
    expires_at: Option<f64>,
}

pub fn parse_iso(value: &str) -> Option<f64> {
    static FRAC: OnceLock<Regex> = OnceLock::new();
    let s = value.trim().replace('Z', "+00:00");
    let s = FRAC
        .get_or_init(|| Regex::new(r"(\.\d{6})\d+").unwrap())
        .replace(&s, "$1")
        .into_owned();
    let (date, rest) = s.split_once('T').or_else(|| s.split_once(' '))?;
    let d: Vec<i64> = date
        .split('-')
        .map(|x| x.parse().ok())
        .collect::<Option<_>>()?;
    if d.len() != 3 {
        return None;
    }
    let (time, offset) = match rest.find(['+', '-']) {
        Some(i) => (&rest[..i], Some(&rest[i..])),
        None => (rest, None),
    };
    let mut t = time.split(':');
    let h: i64 = t.next()?.parse().ok()?;
    let m: i64 = t.next()?.parse().ok()?;
    let sec: f64 = t.next().map(|x| x.parse().ok()).unwrap_or(Some(0.0))?;
    let off = match offset {
        Some(o) => {
            let sign = if o.starts_with('-') { -1 } else { 1 };
            let o = &o[1..];
            let (oh, om) = o
                .split_once(':')
                .unwrap_or((o.get(..2)?, o.get(2..).unwrap_or("0")));
            sign * (oh.parse::<i64>().ok()? * 3600 + om.parse::<i64>().unwrap_or(0) * 60)
        }
        None => 0,
    };
    let (y, mo) = if d[1] <= 2 {
        (d[0] - 1, d[1] + 9)
    } else {
        (d[0], d[1] - 3)
    };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * mo + 2) / 5 + d[2] - 1;
    let days = era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468;
    Some((days * 86400 + h * 3600 + m * 60 - off) as f64 + sec)
}

pub fn iso_from_epoch(ts: f64) -> String {
    let secs = ts.floor() as i64;
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    let micros = ((ts - secs as f64) * 1e6).round() as i64;
    let frac = if micros > 0 {
        format!(".{micros:06}")
    } else {
        String::new()
    };
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}{frac}+00:00",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn s(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(Value::as_str).map(str::to_owned)
}

impl Creds {
    fn load_document(&mut self, data: &Value) {
        if let Some(v) = data.get("refreshToken") {
            self.refresh_token = v.as_str().map(str::to_owned);
        }
        if let Some(v) = data.get("accessToken") {
            self.access_token = v.as_str().map(str::to_owned);
        }
        if let Some(v) = data.get("profileArn") {
            self.profile_arn = v.as_str().map(str::to_owned);
        }
        if let Some(r) = s(data, "region") {
            self.sso_region = Some(r.clone());
            self.detected_api_region = Some(r);
        }
        if let Some(h) = s(data, "clientIdHash") {
            self.load_enterprise_registration(&h);
        }
        if let Some(v) = s(data, "clientId") {
            self.client_id = Some(v);
        }
        if let Some(v) = s(data, "clientSecret") {
            self.client_secret = Some(v);
        }
        if let Some(e) = s(data, "expiresAt") {
            match parse_iso(&e) {
                Some(t) => self.expires_at = Some(t),
                None => tracing::warn!("Failed to parse expiresAt"),
            }
        }
    }

    fn load_enterprise_registration(&mut self, hash: &str) {
        let Some(home) = store::home_dir() else {
            return;
        };
        let path = home
            .join(".aws")
            .join("sso")
            .join("cache")
            .join(format!("{hash}.json"));
        match std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        {
            Some(d) => {
                if let Some(v) = s(&d, "clientId") {
                    self.client_id = Some(v);
                }
                if let Some(v) = s(&d, "clientSecret") {
                    self.client_secret = Some(v);
                }
            }
            None => tracing::warn!(
                "Enterprise device registration file not found: {}",
                path.display()
            ),
        }
    }

    fn load_sqlite(&mut self, db_path: &str) {
        let path = PathBuf::from(store::expand_home(db_path));
        if !path.exists() {
            tracing::warn!("SQLite database not found: {db_path}");
            return;
        }
        let Ok(conn) = rusqlite::Connection::open_with_flags(
            &path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        ) else {
            tracing::error!("SQLite error loading credentials from {db_path}");
            return;
        };
        let get = |key: &str| -> Option<Value> {
            conn.query_row("SELECT value FROM auth_kv WHERE key = ?1", [key], |r| {
                r.get::<_, String>(0)
            })
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
        };
        if let Some(token) = SQLITE_TOKEN_KEYS.iter().find_map(|k| get(k)) {
            if let Some(v) = s(&token, "access_token") {
                self.access_token = Some(v);
            }
            if let Some(v) = s(&token, "refresh_token") {
                self.refresh_token = Some(v);
            }
            if let Some(v) = s(&token, "profile_arn") {
                self.profile_arn = Some(v);
            }
            if let Some(v) = s(&token, "region") {
                self.sso_region = Some(v);
            }
            if let Some(e) = s(&token, "expires_at") {
                self.expires_at = parse_iso(&e).or(self.expires_at);
            }
        }
        if let Some(reg) = SQLITE_REGISTRATION_KEYS.iter().find_map(|k| get(k)) {
            if let Some(v) = s(&reg, "client_id") {
                self.client_id = Some(v);
            }
            if let Some(v) = s(&reg, "client_secret") {
                self.client_secret = Some(v);
            }
            if self.sso_region.is_none() {
                self.sso_region = s(&reg, "region");
            }
        }
        let profile: Option<Value> = conn
            .query_row(
                "SELECT value FROM state WHERE key = 'api.codewhisperer.profile'",
                [],
                |r| r.get::<_, String>(0),
            )
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok());
        if let Some(arn) = profile
            .as_ref()
            .and_then(|p| s(p, "arn"))
            .filter(|a| !a.is_empty())
        {
            if self.profile_arn.is_none() {
                self.profile_arn = Some(arn.clone());
            }
            if let Some(r) = region_from_arn(&arn) {
                self.detected_api_region = Some(r);
            }
        }
    }
}

pub fn region_from_arn(arn: &str) -> Option<String> {
    static R: OnceLock<Regex> = OnceLock::new();
    let part = arn.split(':').nth(3).filter(|p| !p.is_empty())?;
    R.get_or_init(|| Regex::new(r"^[a-z]+-[a-z]+-\d+$").unwrap())
        .is_match(part)
        .then(|| part.to_owned())
}

pub enum Source {
    Internal(String),
    File(String),
    Sqlite(String),
}

pub struct KiroAuth {
    source: Source,
    creds: Mutex<Creds>,
    refresh_lock: tokio::sync::Mutex<()>,
    auth_type: AuthType,
    refresh_url: String,
    pub api_region: String,
    pub api_host: String,
    pub q_host: String,
    http: reqwest::Client,
}

impl KiroAuth {
    pub fn new(
        source: Source,
        region: &str,
        api_region: Option<&str>,
        http: reqwest::Client,
    ) -> KiroAuth {
        let mut c = Creds::default();
        match &source {
            Source::Internal(id) => {
                c.load_document(&store::load_internal_credential(id).unwrap_or(json!({})))
            }
            Source::Sqlite(p) => c.load_sqlite(p),
            Source::File(p) => match std::fs::read_to_string(store::expand_home(p))
                .ok()
                .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            {
                Some(d) => c.load_document(&d),
                None => tracing::warn!("Credentials file not found: {p}"),
            },
        }
        let mut auth = KiroAuth {
            source,
            creds: Mutex::new(c),
            refresh_lock: tokio::sync::Mutex::new(()),
            auth_type: AuthType::KiroDesktop,
            refresh_url: String::new(),
            api_region: String::new(),
            api_host: String::new(),
            q_host: String::new(),
            http,
        };
        auth.apply_overlay();
        let c = auth.creds.lock().clone();
        auth.auth_type = if c.client_id.is_some() && c.client_secret.is_some() {
            AuthType::AwsSsoOidc
        } else {
            AuthType::KiroDesktop
        };
        let final_region = api_region
            .map(str::to_owned)
            .or(c.detected_api_region.clone())
            .or(c.sso_region.clone())
            .unwrap_or_else(|| region.to_owned());
        let builder_id = auth.auth_type == AuthType::AwsSsoOidc && c.profile_arn.is_none();
        auth.refresh_url = config::kiro_refresh_url(c.sso_region.as_deref().unwrap_or(region));
        auth.api_host = config::kiro_api_host(&final_region);
        auth.q_host = config::kiro_q_host(&final_region, builder_id);
        auth.api_region = final_region;
        auth
    }

    fn external_account_id(&self) -> Option<String> {
        match &self.source {
            Source::Internal(_) => None,
            Source::File(p) | Source::Sqlite(p) => {
                let expanded = store::expand_home(p);
                Some(
                    std::fs::canonicalize(&expanded)
                        .map(|x| x.to_string_lossy().trim_start_matches(r"\\?\").to_owned())
                        .unwrap_or(expanded),
                )
            }
        }
    }

    fn lease_account_id(&self) -> Option<String> {
        match &self.source {
            Source::Internal(id) => Some(id.clone()),
            _ => self.external_account_id(),
        }
    }

    fn apply_overlay(&self) {
        let Some(id) = self.external_account_id() else {
            return;
        };
        let Some(overlay) = store::load_internal_credential(&id) else {
            return;
        };
        let mut c = self.creds.lock();
        let Some(refresh) = s(&overlay, "refreshToken") else {
            return;
        };
        let fresher = Some(&refresh) == c.refresh_token.as_ref()
            || matches!((s(&overlay, "expiresAt").and_then(|e| parse_iso(&e)), c.expires_at), (Some(o), Some(cur)) if o > cur);
        if fresher {
            c.load_document(&overlay);
        }
    }

    pub fn auth_type(&self) -> AuthType {
        self.auth_type
    }

    pub fn profile_arn(&self) -> Option<String> {
        self.creds
            .lock()
            .profile_arn
            .clone()
            .filter(|p| !p.is_empty())
    }

    pub fn request_profile_arn(&self) -> Option<String> {
        self.profile_arn().or_else(|| {
            (self.auth_type == AuthType::AwsSsoOidc)
                .then(|| config::KIRO_BUILDER_ID_PROFILE_ARN.to_owned())
        })
    }

    pub fn generation_url(&self) -> String {
        format!("{}/", self.api_host)
    }

    pub fn expires_at(&self) -> Option<f64> {
        self.creds.lock().expires_at
    }

    fn expiring_soon(&self) -> bool {
        self.creds
            .lock()
            .expires_at
            .is_none_or(|e| e <= now() + settings::tunables().token_refresh_seconds as f64)
    }

    fn expired(&self) -> bool {
        self.creds.lock().expires_at.is_none_or(|e| now() >= e)
    }

    fn cached_token(&self) -> Option<String> {
        let c = self.creds.lock();
        c.access_token.clone().filter(|t| !t.is_empty())
    }

    pub async fn access_token(&self) -> Result<String, AuthError> {
        if let Some(t) = self.cached_token().filter(|_| !self.expiring_soon()) {
            return Ok(t);
        }
        let _guard = self.refresh_lock.lock().await;
        if let Some(t) = self.cached_token().filter(|_| !self.expiring_soon()) {
            return Ok(t);
        }
        if let Source::Sqlite(p) = &self.source {
            self.creds.lock().load_sqlite(p);
            self.apply_overlay();
            if let Some(t) = self.cached_token().filter(|_| !self.expiring_soon()) {
                return Ok(t);
            }
        }
        match self.refresh_with_lease(false).await {
            Ok(()) => {}
            Err(AuthError::Http { status: 400, .. })
                if matches!(self.source, Source::Sqlite(_)) =>
            {
                if let Some(t) = self.cached_token().filter(|_| !self.expired()) {
                    tracing::warn!("Using existing access_token until it expires. Run 'kiro-cli login' when convenient.");
                    return Ok(t);
                }
                return Err(AuthError::Other("Token expired and refresh failed. Please run 'kiro-cli login' to refresh your credentials.".into()));
            }
            Err(e) => return Err(self.dead(e)),
        }
        self.cached_token()
            .ok_or_else(|| AuthError::Other("Failed to obtain access token".into()))
    }

    pub async fn force_refresh(&self) -> Result<String, AuthError> {
        let _guard = self.refresh_lock.lock().await;
        self.refresh_with_lease(true)
            .await
            .map_err(|e| self.dead(e))?;
        self.cached_token()
            .ok_or_else(|| AuthError::Other("Failed to obtain access token".into()))
    }

    fn dead(&self, e: AuthError) -> AuthError {
        match e {
            AuthError::Http { status, .. } if is_credential_dead_status(status) => {
                let account = self
                    .lease_account_id()
                    .unwrap_or_else(|| "refresh_token account".into());
                tracing::error!("Refresh token for {account} was rejected by the auth host (HTTP {status}); the credential cannot be renewed and needs a re-login.");
                AuthError::CredentialDead { account, status }
            }
            other => other,
        }
    }

    async fn refresh_with_lease(&self, force: bool) -> Result<(), AuthError> {
        let Some(account) = self.lease_account_id() else {
            return self.refresh_request().await;
        };
        let previous = self.cached_token();
        let wait = std::env::var("KIRO_REFRESH_LEASE_WAIT_SECONDS")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(75.0);
        let deadline = tokio::time::Instant::now() + Duration::from_secs_f64(wait);
        let _lease = loop {
            let a = account.clone();
            if let Some(lease) = tokio::task::spawn_blocking(move || {
                store::try_acquire_refresh_lease(&a, 60.0)
                    .map(|owner| RefreshLease { account: a, owner })
            })
            .await
            .ok()
            .flatten()
            {
                break lease;
            }
            if tokio::time::Instant::now() >= deadline {
                if let Some(latest) = store::load_internal_credential(&account) {
                    self.creds.lock().load_document(&latest);
                }
                if self.cached_token().is_some() && !self.expired() {
                    return Ok(());
                }
                tracing::warn!("Refresh lease for account {account} not acquired in {wait}s; not refreshing without ownership");
                return Err(AuthError::Other(
                    "Credential refresh is owned by another slot; try again shortly".into(),
                ));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        async {
            if let Some(latest) = store::load_internal_credential(&account) {
                self.creds.lock().load_document(&latest);
            }
            let renewed_elsewhere = self.cached_token() != previous;
            if self.cached_token().is_some()
                && !self.expiring_soon()
                && (!force || renewed_elsewhere)
            {
                return Ok(());
            }
            self.refresh_request().await
        }
        .await
    }

    async fn refresh_request(&self) -> Result<(), AuthError> {
        if let Source::Internal(id) = &self.source {
            let doc = store::load_internal_credential(id)
                .ok_or_else(|| AuthError::Other(format!("Unknown internal account: {id}")))?;
            self.creds.lock().load_document(&doc);
        }
        let first = match self.auth_type {
            AuthType::AwsSsoOidc => self.do_oidc_refresh().await,
            AuthType::KiroDesktop => self.do_desktop_refresh().await,
        };
        match first {
            Err(AuthError::Http { status: 400, .. }) if self.reload_raw_external() => {
                tracing::warn!(
                    "Token refresh failed with 400; retrying with raw external credentials"
                );
                match self.auth_type {
                    AuthType::AwsSsoOidc => self.do_oidc_refresh().await,
                    AuthType::KiroDesktop => self.do_desktop_refresh().await,
                }
            }
            other => other,
        }
    }

    fn reload_raw_external(&self) -> bool {
        match &self.source {
            Source::Sqlite(p) => {
                self.creds.lock().load_sqlite(p);
                true
            }
            Source::File(p) => {
                if let Some(d) = std::fs::read_to_string(store::expand_home(p))
                    .ok()
                    .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                {
                    self.creds.lock().load_document(&d);
                }
                true
            }
            Source::Internal(_) => false,
        }
    }

    async fn post(
        &self,
        url: &str,
        body: Value,
        headers: &[(&str, String)],
    ) -> Result<Value, AuthError> {
        let mut req = self
            .http
            .post(url)
            .timeout(Duration::from_secs(30))
            .json(&body);
        for (k, v) in headers {
            req = req.header(*k, v);
        }
        let resp = req.send().await.map_err(|e| {
            tracing::error!("Token refresh request failed: {e}");
            AuthError::Other("token refresh request failed".into())
        })?;
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        if status != 200 {
            tracing::error!("Token refresh failed: status={status}");
            return Err(AuthError::Http { status, body: text });
        }
        serde_json::from_str(&text).map_err(|e| {
            tracing::error!("Token refresh returned invalid JSON: {e}");
            AuthError::Other("token refresh returned invalid JSON".into())
        })
    }

    async fn do_desktop_refresh(&self) -> Result<(), AuthError> {
        let refresh = self
            .creds
            .lock()
            .refresh_token
            .clone()
            .ok_or_else(|| AuthError::Other("Refresh token is not set".into()))?;
        tracing::info!("Refreshing Kiro token via Kiro Desktop Auth...");
        let ua = format!("KiroIDE-0.7.45-{}", crate::utils::machine_fingerprint());
        let data = self
            .post(
                &self.refresh_url,
                json!({"refreshToken": refresh}),
                &[("User-Agent", ua)],
            )
            .await?;
        let access = s(&data, "accessToken")
            .ok_or_else(|| AuthError::Other("Response does not contain accessToken".into()))?;
        let expires_in = data
            .get("expiresIn")
            .and_then(Value::as_f64)
            .unwrap_or(3600.0);
        {
            let mut c = self.creds.lock();
            c.access_token = Some(access);
            if let Some(r) = s(&data, "refreshToken") {
                c.refresh_token = Some(r);
            }
            if let Some(p) = s(&data, "profileArn") {
                c.profile_arn = Some(p);
            }
            c.expires_at = Some(now().floor() + expires_in - 60.0);
        }
        self.persist();
        Ok(())
    }

    async fn do_oidc_refresh(&self) -> Result<(), AuthError> {
        let c = self.creds.lock().clone();
        let refresh = c
            .refresh_token
            .ok_or_else(|| AuthError::Other("Refresh token is not set".into()))?;
        let client_id = c.client_id.ok_or_else(|| {
            AuthError::Other("Client ID is not set (required for AWS SSO OIDC)".into())
        })?;
        let secret = c.client_secret.ok_or_else(|| {
            AuthError::Other("Client secret is not set (required for AWS SSO OIDC)".into())
        })?;
        tracing::info!("Refreshing Kiro token via AWS SSO OIDC...");
        let url = config::aws_sso_oidc_url(c.sso_region.as_deref().unwrap_or(config::REGION));
        let data = self
            .post(&url, json!({"grantType": "refresh_token", "clientId": client_id, "clientSecret": secret, "refreshToken": refresh}), &[])
            .await?;
        let access = s(&data, "accessToken").ok_or_else(|| {
            AuthError::Other("AWS SSO OIDC response does not contain accessToken".into())
        })?;
        let expires_in = data
            .get("expiresIn")
            .and_then(Value::as_f64)
            .unwrap_or(3600.0);
        {
            let mut c = self.creds.lock();
            c.access_token = Some(access);
            if let Some(r) = s(&data, "refreshToken") {
                c.refresh_token = Some(r);
            }
            c.expires_at = Some(now() + expires_in - 60.0);
        }
        self.persist();
        Ok(())
    }

    fn persist(&self) {
        let c = self.creds.lock().clone();
        let expires = c.expires_at.map(iso_from_epoch);
        match &self.source {
            Source::Internal(id) => {
                let mut doc = store::load_internal_credential(id).unwrap_or(json!({}));
                doc["accessToken"] = json!(c.access_token);
                doc["refreshToken"] = json!(c.refresh_token);
                doc["expiresAt"] = json!(expires);
                if let Some(p) = c.profile_arn.filter(|p| !p.is_empty()) {
                    doc["profileArn"] = json!(p);
                }
                if let Err(e) = store::save_internal_credential(id, &doc) {
                    tracing::error!("Could not persist refreshed credential for {id}: {e}");
                }
            }
            Source::File(_) | Source::Sqlite(_) => {
                let Some(id) = self.external_account_id() else {
                    return;
                };
                let mut doc = json!({"accessToken": c.access_token, "refreshToken": c.refresh_token, "expiresAt": expires});
                if let Some(p) = c.profile_arn.filter(|p| !p.is_empty()) {
                    doc["profileArn"] = json!(p);
                }
                let payload = doc.to_string();
                let updated = store::with(|conn| {
                    store::require_runtime_writer(conn)?;
                    conn.execute(
                        "UPDATE account_sources SET credential_json = ?1 WHERE account_id = ?2",
                        rusqlite::params![payload, id],
                    )
                })
                .unwrap_or(0);
                if updated == 0 {
                    tracing::warn!(
                        "Could not persist credential overlay for unregistered account {id}"
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_round_trip() {
        let t = parse_iso("2026-09-26T15:54:25+00:00").unwrap();
        assert_eq!(iso_from_epoch(t), "2026-09-26T15:54:25+00:00");
        assert_eq!(parse_iso("2026-09-26T15:54:25Z"), Some(t));
        assert_eq!(parse_iso("2026-09-26T12:54:25-03:00"), Some(t));
        assert_eq!(
            parse_iso("2026-09-26T15:54:25.123456789Z").map(|v| (v * 1e6).round() / 1e6),
            Some(t + 0.123456)
        );
    }
}
