//! Account pool: lazy initialization, quota-weighted selection, the circuit breaker,
//! quarantines, rate observations, and session affinity.
//!
//! `load_balancing=session` pins a conversation to one account: Kiro keeps a prompt
//! cache per account, and a warm account answered in ~1.6s where a cold one took
//! 2.4-6s. Affinity is a preference, never an exclusion: health and quarantine checks
//! still run first, and on failover the session moves with the request.

use parking_lot::Mutex;
use rand::Rng;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::auth::{AuthError, AuthType, KiroAuth, Source};
use crate::errors::{is_suspension_error, ErrorType};
use crate::model_resolver::{self, normalize_model_name, ModelInfoCache, ModelSupport};
use crate::{config, settings, store};

pub fn account_label(id: &str) -> String {
    hex::encode(Sha256::digest(id.as_bytes()))[..12].to_owned()
}

pub fn format_duration(s: f64) -> String {
    if s < 60.0 {
        format!("{}s", s as i64)
    } else if s < 3600.0 {
        format!("{}m", (s / 60.0) as i64)
    } else if s < 86400.0 {
        format!("{}h", (s / 3600.0) as i64)
    } else {
        format!("{}d", (s / 86400.0) as i64)
    }
}

#[derive(Default, Clone, Copy)]
pub struct AccountStats {
    pub total: i64,
    pub success: i64,
    pub failed: i64,
}

#[derive(Default)]
pub struct AccountState {
    pub failures: i64,
    pub last_failure_time: f64,
    pub rate_limited_until: f64,
    pub quota_exhausted_until: f64,
    pub suspended_until: f64,
    pub auth_dead_until: f64,
    pub models_cached_at: f64,
    pub quota_headroom: Option<f64>,
    pub quota_resets_at: f64,
    pub quota_overage_enabled: Option<bool>,
    pub stats: AccountStats,
    pub sessions: i64,
}

pub struct Account {
    pub id: String,
    pub config: Value,
    pub auth: Mutex<Option<Arc<KiroAuth>>>,
    pub models: Arc<ModelInfoCache>,
    pub state: Mutex<AccountState>,
    init: tokio::sync::Mutex<()>,
    init_retry_at: Mutex<Option<Instant>>,
    init_scheduled: std::sync::atomic::AtomicBool,
    models_refresh: tokio::sync::Mutex<()>,
    models_refresh_scheduled: std::sync::atomic::AtomicBool,
}

impl Account {
    pub fn auth(&self) -> Option<Arc<KiroAuth>> {
        self.auth.lock().clone()
    }
}

pub fn is_quota_depleted(s: &AccountState) -> bool {
    s.quota_headroom.is_some_and(|h| h <= 0.0) && s.quota_overage_enabled == Some(false)
}

fn cooling_remaining(s: &AccountState, now: f64) -> f64 {
    if s.failures <= 0 {
        return 0.0;
    }
    let cfg = config::get();
    let mult = 2f64
        .powi((s.failures - 1).min(60) as i32)
        .min(cfg.account_max_backoff_multiplier);
    cfg.account_recovery_timeout as f64 * mult - (now - s.last_failure_time)
}

pub fn routing_state(a: &Account, now: f64) -> (&'static str, i64) {
    let s = a.state.lock();
    for (until, name) in [
        (s.auth_dead_until, "auth_dead"),
        (s.suspended_until, "suspended"),
        (s.quota_exhausted_until, "quota_exhausted"),
        (s.rate_limited_until, "rate_limited"),
    ] {
        if until - now > 0.0 {
            return (name, (until - now) as i64);
        }
    }
    let cool = cooling_remaining(&s, now);
    if cool > 0.0 {
        return ("cooling_down", cool as i64);
    }
    if a.auth.lock().is_none() {
        return ("uninitialized", 0);
    }
    if is_quota_depleted(&s) {
        let r = s.quota_resets_at - now;
        return ("quota_depleted", if r > 0.0 { r as i64 } else { 0 });
    }
    ("available", 0)
}

#[derive(Clone)]
pub struct RateObservation {
    pub at: f64,
    pub account_id: String,
    pub rpm: i64,
    pub rejected: bool,
    pub outcome: String,
}

struct SessionEntry {
    account: String,
    touched: Instant,
}

#[derive(Default)]
struct PoolInner {
    order: Vec<String>,
    accounts: HashMap<String, Arc<Account>>,
    model_to_accounts: HashMap<String, Vec<String>>,
    current_index: usize,
    observations: VecDeque<RateObservation>,
    unsaved: Vec<RateObservation>,
    sessions: HashMap<u64, SessionEntry>,
}

/// Per-account bound for one warm-up initialization.
pub const WARM_UP_ACCOUNT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

pub struct AccountManager {
    inner: Mutex<PoolInner>,
    dirty: std::sync::atomic::AtomicBool,
    http: reqwest::Client,
    mutations: tokio::sync::Mutex<()>,
    warm: tokio::sync::Mutex<()>,
    warm_failed_at: Mutex<Option<std::time::Instant>>,
}

/// After a warm-up that initialized nothing, discovery answers "not ready"
/// immediately for this long instead of sweeping dead accounts on every poll.
pub const WARM_UP_RETRY_AFTER: std::time::Duration = std::time::Duration::from_secs(10);

impl AccountManager {
    pub fn new(http: reqwest::Client) -> Arc<AccountManager> {
        Arc::new(AccountManager {
            inner: Mutex::new(PoolInner::default()),
            dirty: false.into(),
            http,
            mutations: tokio::sync::Mutex::new(()),
            warm: tokio::sync::Mutex::new(()),
            warm_failed_at: Mutex::new(None),
        })
    }

    /// True when at least one account has initialized auth and a model list,
    /// which is what model discovery and handoff readiness need.
    pub fn catalog_ready(&self) -> bool {
        self.accounts().iter().any(|a| a.auth.lock().is_some())
    }

    /// Initializes every uninitialized account concurrently, each bounded by
    /// `per_account`, so a dead or slow account never holds up a healthy one.
    /// Single-flight: a caller that arrives while a warm-up runs waits for it
    /// instead of starting another. Only the runtime writer may call this; it
    /// can refresh credentials through the lease.
    pub async fn warm_up(self: &Arc<Self>, per_account: std::time::Duration) {
        let _flight = self.warm.lock().await;
        let pending: Vec<Arc<Account>> = self
            .accounts()
            .into_iter()
            .filter(|a| {
                a.auth.lock().is_none()
                    && !a.init_scheduled.load(std::sync::atomic::Ordering::Acquire)
            })
            .collect();
        let tasks: Vec<_> = pending
            .into_iter()
            .map(|a| {
                let pool = self.clone();
                tokio::spawn(async move {
                    pool.initialize_for_maintenance(&a, per_account).await;
                })
            })
            .collect();
        for t in tasks {
            let _ = t.await;
        }
        *self.warm_failed_at.lock() =
            (!self.catalog_ready() && !self.accounts().is_empty()).then(std::time::Instant::now);
    }

    /// Waits up to `limit` for a warm-up in progress (or starts one) so model
    /// discovery right after activation does not return an empty list.
    pub async fn ensure_catalog(self: &Arc<Self>, limit: std::time::Duration) -> bool {
        if self.catalog_ready() || self.accounts().is_empty() {
            return true;
        }
        let recently_failed = self
            .warm_failed_at
            .lock()
            .is_some_and(|t| t.elapsed() < WARM_UP_RETRY_AFTER);
        if recently_failed && self.warm.try_lock().is_ok() {
            return false;
        }
        let pool = self.clone();
        let _ = tokio::time::timeout(
            limit,
            async move { pool.warm_up(WARM_UP_ACCOUNT_TIMEOUT).await },
        )
        .await;
        self.catalog_ready()
    }

    /// Held across a whole account read/check/mutate/persist sequence
    /// (register, delete, enable, disable) so two operators cannot both pass
    /// the last-account check against the same snapshot.
    pub async fn lock_mutations(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.mutations.lock().await
    }

    fn mark_dirty(&self) {
        self.dirty.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn load_credentials(&self) {
        let sources = store::load_account_sources();
        let mut inner = self.inner.lock();
        for entry in sources {
            if entry.get("enabled").and_then(Value::as_bool) == Some(false) {
                continue;
            }
            let Some(kind) = entry.get("type").and_then(Value::as_str) else {
                tracing::warn!("Invalid credential entry (missing type)");
                continue;
            };
            let ids: Vec<String> = match kind {
                "internal" => entry
                    .get("id")
                    .and_then(Value::as_str)
                    .map(|s| vec![s.to_owned()])
                    .unwrap_or_default(),
                "refresh_token" => {
                    if entry
                        .get("refresh_token")
                        .and_then(Value::as_str)
                        .is_none_or(str::is_empty)
                    {
                        tracing::warn!("Invalid credential entry (type=refresh_token requires refresh_token field)");
                        continue;
                    }
                    vec![store::account_id_for_entry(&entry)]
                }
                "json" | "sqlite" => {
                    let Some(path) = entry.get("path").and_then(Value::as_str) else {
                        tracing::warn!("Invalid credential entry (type={kind} requires path)");
                        continue;
                    };
                    let expanded = std::path::PathBuf::from(store::expand_home(path));
                    if expanded.is_dir() {
                        std::fs::read_dir(&expanded)
                            .into_iter()
                            .flatten()
                            .flatten()
                            .map(|e| e.path())
                            .filter(|p| p.is_file() && valid_credential_file(p, kind))
                            .map(|p| {
                                std::fs::canonicalize(&p)
                                    .unwrap_or(p)
                                    .to_string_lossy()
                                    .trim_start_matches(r"\\?\")
                                    .to_owned()
                            })
                            .collect()
                    } else if expanded.is_file() {
                        vec![store::account_id_for_entry(&entry)]
                    } else {
                        tracing::warn!("Credential path not found: {path}");
                        vec![]
                    }
                }
                _ => vec![],
            };
            for id in ids {
                if inner.accounts.contains_key(&id) {
                    continue;
                }
                inner.order.push(id.clone());
                inner.accounts.insert(
                    id.clone(),
                    Arc::new(Account {
                        id,
                        config: entry.clone(),
                        auth: Mutex::new(None),
                        models: Arc::new(ModelInfoCache::new()),
                        state: Mutex::new(AccountState::default()),
                        init: tokio::sync::Mutex::new(()),
                        init_retry_at: Mutex::new(None),
                        init_scheduled: false.into(),
                        models_refresh: tokio::sync::Mutex::new(()),
                        models_refresh_scheduled: false.into(),
                    }),
                );
            }
        }
        tracing::info!("Loaded {} account(s) from credentials", inner.order.len());
    }

    pub fn load_state(&self) {
        if let Some(state) = store::load_runtime_state() {
            let mut inner = self.inner.lock();
            inner.current_index = state
                .get("current_account_index")
                .and_then(Value::as_u64)
                .unwrap_or(0) as usize;
            if let Some(Value::Object(m)) = state.get("model_to_accounts") {
                for (model, data) in m {
                    let list = data
                        .get("accounts")
                        .and_then(Value::as_array)
                        .map(|a| {
                            a.iter()
                                .filter_map(|v| v.as_str().map(str::to_owned))
                                .collect()
                        })
                        .unwrap_or_default();
                    inner.model_to_accounts.insert(model.clone(), list);
                }
            }
            if let Some(Value::Object(accts)) = state.get("accounts") {
                for (id, d) in accts {
                    let Some(a) = inner.accounts.get(id) else {
                        continue;
                    };
                    let mut s = a.state.lock();
                    let f = |k: &str| d.get(k).and_then(Value::as_f64).unwrap_or(0.0);
                    s.failures = d.get("failures").and_then(Value::as_i64).unwrap_or(0);
                    s.last_failure_time = f("last_failure_time");
                    s.quota_exhausted_until = f("quota_exhausted_until");
                    s.suspended_until = f("suspended_until");
                    s.auth_dead_until = f("auth_dead_until");
                    s.models_cached_at = f("models_cached_at");
                    let st = d.get("stats").cloned().unwrap_or(json!({}));
                    s.stats = AccountStats {
                        total: st
                            .get("total_requests")
                            .and_then(Value::as_i64)
                            .unwrap_or(0),
                        success: st
                            .get("successful_requests")
                            .and_then(Value::as_i64)
                            .unwrap_or(0),
                        failed: st
                            .get("failed_requests")
                            .and_then(Value::as_i64)
                            .unwrap_or(0),
                    };
                }
            }
        }
        self.seed_quota();
    }

    fn seed_quota(&self) {
        let headroom = store::load_quota_headroom();
        let period = store::load_quota_period();
        let inner = self.inner.lock();
        for (id, h) in headroom {
            if let Some(a) = inner.accounts.get(&id) {
                a.state.lock().quota_headroom = Some(h.clamp(0.0, 1.0));
            }
        }
        for (id, (reset, overage)) in period {
            if let Some(a) = inner.accounts.get(&id) {
                let mut s = a.state.lock();
                s.quota_resets_at = reset.unwrap_or(0.0);
                s.quota_overage_enabled = overage;
            }
        }
    }

    pub fn reload_durable_state(&self) {
        {
            let mut inner = self.inner.lock();
            let observations = std::mem::take(&mut inner.observations);
            *inner = PoolInner {
                observations,
                ..Default::default()
            };
        }
        self.load_credentials();
        self.load_state();
    }

    pub fn state_document(&self) -> Value {
        let inner = self.inner.lock();
        let accounts: serde_json::Map<String, Value> = inner
            .order
            .iter()
            .filter_map(|id| inner.accounts.get(id).map(|a| (id, a)))
            .map(|(id, a)| {
                let s = a.state.lock();
                (
                    id.clone(),
                    json!({
                        "failures": s.failures, "last_failure_time": s.last_failure_time,
                        "quota_exhausted_until": s.quota_exhausted_until, "suspended_until": s.suspended_until,
                        "auth_dead_until": s.auth_dead_until, "models_cached_at": s.models_cached_at,
                        "stats": {"total_requests": s.stats.total, "successful_requests": s.stats.success, "failed_requests": s.stats.failed},
                    }),
                )
            })
            .collect();
        let models: serde_json::Map<String, Value> = inner
            .model_to_accounts
            .iter()
            .map(|(m, l)| (m.clone(), json!({"accounts": l})))
            .collect();
        json!({"current_account_index": inner.current_index, "accounts": accounts, "model_to_accounts": models})
    }

    pub fn save_state(&self) -> bool {
        let doc = self.state_document();
        let written = store::save_runtime_state(&doc);
        if written {
            self.dirty
                .store(false, std::sync::atomic::Ordering::Relaxed);
        }
        written
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn accounts(&self) -> Vec<Arc<Account>> {
        let inner = self.inner.lock();
        inner
            .order
            .iter()
            .filter_map(|id| inner.accounts.get(id).cloned())
            .collect()
    }

    pub fn get(&self, id: &str) -> Option<Arc<Account>> {
        self.inner.lock().accounts.get(id).cloned()
    }

    pub fn remove_account(&self, id: &str) {
        let mut inner = self.inner.lock();
        inner.accounts.remove(id);
        let pos = inner.order.iter().position(|x| x == id);
        inner.order.retain(|x| x != id);
        for list in inner.model_to_accounts.values_mut() {
            list.retain(|x| x != id);
        }
        inner.model_to_accounts.retain(|_, l| !l.is_empty());
        inner.observations.retain(|o| o.account_id != id);
        inner.unsaved.retain(|o| o.account_id != id);
        inner.sessions.retain(|_, e| e.account != id);
        let n = inner.order.len();
        inner.current_index = match (n, pos) {
            (0, _) => 0,
            (_, Some(p)) if p < inner.current_index => inner.current_index - 1,
            _ => inner.current_index.min(n - 1),
        };
        drop(inner);
        self.mark_dirty();
    }

    async fn initialize(&self, a: &Arc<Account>) -> bool {
        let _guard = a.init.lock().await;
        if a.auth.lock().is_some() {
            return true;
        }
        let cfg = &a.config;
        let region = cfg
            .get("region")
            .and_then(Value::as_str)
            .unwrap_or(config::REGION)
            .to_owned();
        let api_region = cfg
            .get("api_region")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let source = match cfg.get("type").and_then(Value::as_str) {
            Some("internal") | Some("refresh_token") => Source::Internal(a.id.clone()),
            Some("sqlite") => Source::Sqlite(a.id.clone()),
            Some("json") => Source::File(a.id.clone()),
            other => {
                tracing::error!("Unknown credential type: {other:?}");
                return false;
            }
        };
        let (http, id) = (self.http.clone(), a.id.clone());
        let auth = match tokio::task::spawn_blocking(move || {
            KiroAuth::new(source, &region, api_region.as_deref(), http)
        })
        .await
        {
            Ok(v) => Arc::new(v),
            Err(_) => return false,
        };
        match auth.access_token().await {
            Ok(_) => {}
            Err(AuthError::CredentialDead { status, .. }) => {
                self.commit_credential_dead(a, status);
                return false;
            }
            Err(e) => {
                tracing::error!("Failed to initialize account {id}: {e}");
                return false;
            }
        }
        let models = crate::model_catalog::fetch_available_models(&auth, &self.http).await;
        match models {
            Some(m) => a.models.update(m),
            None => a.models.seed_fallback(),
        }
        let available = model_resolver::available_models(&a.models);
        let mut inner = self.inner.lock();
        if !inner
            .accounts
            .get(&id)
            .is_some_and(|live| Arc::ptr_eq(live, a))
        {
            tracing::info!(
                "Discarding initialization of {id}: it was removed or replaced while initializing"
            );
            return false;
        }
        *a.auth.lock() = Some(auth);
        a.state.lock().models_cached_at = store::now_f64();
        for m in &available {
            let list = inner.model_to_accounts.entry(m.clone()).or_default();
            if !list.contains(&id) {
                list.push(id.clone());
            }
        }
        drop(inner);
        self.mark_dirty();
        tracing::info!("Initialized account: {id} ({} models)", available.len());
        true
    }

    pub async fn initialize_account(&self, id: &str) -> bool {
        match self.get(id) {
            Some(a) => self.initialize(&a).await,
            None => false,
        }
    }

    async fn initialize_for_maintenance(&self, a: &Arc<Account>, timeout: Duration) -> bool {
        let result = tokio::time::timeout(timeout, self.initialize(a)).await;
        let initialized = matches!(result, Ok(true));
        *a.init_retry_at.lock() = (!initialized).then(|| Instant::now() + WARM_UP_RETRY_AFTER);
        if result.is_err() {
            tracing::warn!(
                "Account {} did not initialize within {timeout:?}; it will be retried in the background",
                a.id
            );
        }
        initialized
    }

    /// Single-flight per account: selectors that see the same expired cache
    /// queue on one refresh and reuse its result instead of each calling
    /// ListAvailableModels.
    pub async fn refresh_models(&self, a: &Arc<Account>) {
        let http = self.http.clone();
        self.refresh_models_with(a, move |auth| {
            let http = http.clone();
            async move { crate::model_catalog::fetch_available_models(&auth, &http).await }
        })
        .await;
    }

    pub async fn refresh_models_with<F, Fut>(&self, a: &Arc<Account>, fetch: F)
    where
        F: FnOnce(Arc<KiroAuth>) -> Fut,
        Fut: std::future::Future<Output = Option<Vec<Value>>>,
    {
        let _flight = a.models_refresh.lock().await;
        let ttl = if a.models.is_authoritative() {
            config::get().account_cache_ttl as f64
        } else {
            config::MODEL_CACHE_TTL as f64
        };
        let cached = a.state.lock().models_cached_at;
        if cached > 0.0 && store::now_f64() - cached <= ttl {
            return;
        }
        let Some(auth) = a.auth() else { return };
        let refresh_revision = a.models.refresh_revision();
        let refreshed = fetch(auth).await;
        match refreshed {
            Some(m) => a.models.update_after_refresh(m, refresh_revision),
            None => a.models.seed_fallback(),
        }
        a.state.lock().models_cached_at = store::now_f64();
        self.mark_dirty();
    }

    fn schedule_account_maintenance(self: &Arc<Self>) {
        // Hold an idle warm-up lock through scheduling so startup warm-up and
        // background recovery cannot enqueue consecutive attempts.
        let warm_idle = self.warm.try_lock().ok();
        let now = store::now_f64();
        for a in self.accounts() {
            if a.auth.lock().is_none() {
                if warm_idle.is_none() {
                    continue;
                }
                if a.state.lock().auth_dead_until > now {
                    continue;
                }
                let retry_due = a
                    .init_retry_at
                    .lock()
                    .is_none_or(|retry_at| Instant::now() >= retry_at);
                if retry_due
                    && a.init_scheduled
                        .compare_exchange(
                            false,
                            true,
                            std::sync::atomic::Ordering::AcqRel,
                            std::sync::atomic::Ordering::Acquire,
                        )
                        .is_ok()
                {
                    let (pool, account) = (self.clone(), a.clone());
                    tokio::spawn(async move {
                        pool.initialize_for_maintenance(&account, WARM_UP_ACCOUNT_TIMEOUT)
                            .await;
                        account
                            .init_scheduled
                            .store(false, std::sync::atomic::Ordering::Release);
                    });
                }
                continue;
            }
            let ttl = if a.models.is_authoritative() {
                config::get().account_cache_ttl as f64
            } else {
                config::MODEL_CACHE_TTL as f64
            };
            let cached = a.state.lock().models_cached_at;
            if cached <= 0.0
                || now - cached <= ttl
                || a.models_refresh_scheduled
                    .compare_exchange(
                        false,
                        true,
                        std::sync::atomic::Ordering::AcqRel,
                        std::sync::atomic::Ordering::Acquire,
                    )
                    .is_err()
            {
                continue;
            }
            let (pool, account) = (self.clone(), a.clone());
            tokio::spawn(async move {
                pool.refresh_models(&account).await;
                account
                    .models_refresh_scheduled
                    .store(false, std::sync::atomic::Ordering::Release);
            });
        }
    }

    fn routing_weight(s: &AccountState) -> f64 {
        let cfg = config::get();
        match s.quota_headroom {
            None => cfg.unknown_quota_weight.max(config::MINIMUM_ROUTING_WEIGHT),
            Some(h) if h <= 0.0 => cfg
                .depleted_quota_weight
                .max(config::MINIMUM_ROUTING_WEIGHT),
            Some(h) => h.max(config::MINIMUM_ROUTING_WEIGHT),
        }
    }

    fn candidate_order(&self, model: &str, session: Option<u64>) -> Vec<Arc<Account>> {
        let inner = self.inner.lock();
        let ids = inner.order.clone();
        if ids.is_empty() {
            return vec![];
        }
        let strategy = settings::tunables().load_balancing;
        let rotate = |start: usize| {
            (0..ids.len())
                .map(|o| ids[(start + o) % ids.len()].clone())
                .collect::<Vec<_>>()
        };
        let weighted = |inner: &PoolInner| {
            let mut rng = rand::thread_rng();
            let mut keyed: Vec<(f64, String)> = ids
                .iter()
                .map(|id| {
                    let w = inner
                        .accounts
                        .get(id)
                        .map(|a| Self::routing_weight(&a.state.lock()))
                        .unwrap_or(config::MINIMUM_ROUTING_WEIGHT);
                    let e: f64 = -(1.0 - rng.gen::<f64>()).ln();
                    (e / w, id.clone())
                })
                .collect();
            keyed.sort_by(|a, b| a.0.total_cmp(&b.0));
            keyed.into_iter().map(|(_, id)| id).collect::<Vec<_>>()
        };
        let mut pinned = None;
        let ordered: Vec<String> = if !config::get().quota_weighted_routing || strategy == "sticky"
        {
            rotate(inner.current_index)
        } else if strategy == "most_credits" {
            let mut v = ids.clone();
            v.sort_by(|a, b| {
                let wa = inner
                    .accounts
                    .get(a)
                    .map(|x| Self::routing_weight(&x.state.lock()))
                    .unwrap_or(0.0);
                let wb = inner
                    .accounts
                    .get(b)
                    .map(|x| Self::routing_weight(&x.state.lock()))
                    .unwrap_or(0.0);
                wb.total_cmp(&wa)
            });
            v
        } else if let Some(key) = session.filter(|_| strategy == "session") {
            let ttl = Duration::from_secs(config::get().session_affinity_ttl_seconds);
            let pinned_id = inner
                .sessions
                .get(&key)
                .filter(|e| e.touched.elapsed() < ttl)
                .map(|e| e.account.clone());
            let mut rest = weighted(&inner);
            if let Some(p) = pinned_id.filter(|p| inner.accounts.contains_key(p)) {
                pinned = Some(p.clone());
                rest.retain(|x| *x != p);
                rest.insert(0, p);
            }
            rest
        } else {
            weighted(&inner)
        };
        let mut accounts: Vec<Arc<Account>> = ordered
            .into_iter()
            .filter_map(|id| inner.accounts.get(&id).cloned())
            .collect();
        let pinned = pinned.and_then(|id| {
            accounts
                .iter()
                .position(|a| a.id == id && a.models.support(model) != ModelSupport::Unsupported)
                .map(|index| accounts.remove(index))
        });
        let pinned = pinned.and_then(|a| {
            if crate::upstream::http::account_concurrency_load(&a.id)
                .is_none_or(|(held, limit)| held < limit)
            {
                Some(a)
            } else {
                accounts.push(a);
                None
            }
        });
        accounts.sort_by_key(|a| {
            let support = a.models.support(model);
            let load = crate::upstream::http::account_concurrency_load(&a.id)
                .map(|(held, limit)| (held >= limit, held))
                .unwrap_or((false, 0));
            (load.0, support, load.1)
        });
        if let Some(pinned) = pinned {
            accounts.insert(0, pinned);
        }
        accounts
    }

    pub async fn next_account(
        self: &Arc<Self>,
        model: &str,
        exclude: &HashSet<String>,
        session: Option<u64>,
    ) -> Option<Arc<Account>> {
        self.schedule_account_maintenance();
        if let Some(a) = self.select(model, exclude, session, false).await {
            return Some(a);
        }
        let any_depleted = self
            .accounts()
            .iter()
            .any(|a| is_quota_depleted(&a.state.lock()));
        if !any_depleted {
            return None;
        }
        let a = self.select(model, exclude, session, true).await;
        if let Some(a) = &a {
            tracing::warn!("Routing to {} despite usage reporting its quota spent: no other account is eligible", a.id);
        }
        a
    }

    async fn select(
        &self,
        model: &str,
        exclude: &HashSet<String>,
        session: Option<u64>,
        last_resort: bool,
    ) -> Option<Arc<Account>> {
        let candidates = self.candidate_order(model, session);
        let single = candidates.len() == 1;
        let cfg = config::get();
        let mut unsupported = None;
        for a in candidates {
            if exclude.contains(&a.id) {
                continue;
            }
            let now = store::now_f64();
            if !single {
                let s = a.state.lock();
                if s.auth_dead_until > now
                    || s.suspended_until > now
                    || s.quota_exhausted_until > now
                    || s.rate_limited_until > now
                {
                    continue;
                }
                if !last_resort && is_quota_depleted(&s) {
                    continue;
                }
                if cooling_remaining(&s, now) > 0.0 {
                    if rand::thread_rng().gen::<f64>() > cfg.account_probabilistic_retry_chance {
                        continue;
                    }
                    tracing::info!("Probabilistic retry for broken account {}", a.id);
                }
            }
            if a.auth.lock().is_none() && !self.initialize(&a).await {
                a.state.lock().failures += 1;
                self.mark_dirty();
                continue;
            }
            let still_member = self
                .inner
                .lock()
                .accounts
                .get(&a.id)
                .is_some_and(|live| Arc::ptr_eq(live, &a));
            if still_member && a.auth.lock().is_some() {
                if a.models.support(model) == ModelSupport::Unsupported {
                    unsupported.get_or_insert(a);
                } else {
                    return Some(a);
                }
            }
        }
        unsupported
    }

    pub fn pin_session(&self, session: Option<u64>, account_id: &str) {
        let Some(key) = session else { return };
        if settings::tunables().load_balancing != "session" {
            return;
        }
        let mut inner = self.inner.lock();
        Self::pin_session_locked(&mut inner, key, account_id);
    }

    fn pin_session_locked(inner: &mut PoolInner, key: u64, account_id: &str) {
        let cap = config::get().session_affinity_capacity.max(1);
        let previous = inner.sessions.get(&key).map(|e| e.account.clone());
        if previous.as_deref() == Some(account_id) {
            if let Some(e) = inner.sessions.get_mut(&key) {
                e.touched = Instant::now();
            }
            return;
        }
        if inner.sessions.len() >= cap {
            let ttl = Duration::from_secs(config::get().session_affinity_ttl_seconds);
            inner.sessions.retain(|_, e| e.touched.elapsed() < ttl);
            if inner.sessions.len() >= cap {
                if let Some(oldest) = inner
                    .sessions
                    .iter()
                    .min_by_key(|(_, e)| e.touched)
                    .map(|(k, _)| *k)
                {
                    inner.sessions.remove(&oldest);
                }
            }
        }
        inner.sessions.insert(
            key,
            SessionEntry {
                account: account_id.to_owned(),
                touched: Instant::now(),
            },
        );
        if let Some(prev) = previous.and_then(|p| inner.accounts.get(&p).cloned()) {
            prev.state.lock().sessions -= 1;
        }
        if let Some(a) = inner.accounts.get(account_id) {
            a.state.lock().sessions += 1;
        }
    }

    pub fn session_counts(&self) -> HashMap<String, i64> {
        let inner = self.inner.lock();
        let ttl = Duration::from_secs(config::get().session_affinity_ttl_seconds);
        let mut out: HashMap<String, i64> = HashMap::new();
        for e in inner
            .sessions
            .values()
            .filter(|e| e.touched.elapsed() < ttl)
        {
            *out.entry(e.account.clone()).or_default() += 1;
        }
        out
    }

    fn record_event_locked(inner: &mut PoolInner, id: &str, outcome: &str) {
        let at = store::now_f64();
        let cutoff = at - config::get().rate_window_seconds as f64;
        let rpm = inner
            .observations
            .iter()
            .rev()
            .take_while(|o| o.at > cutoff)
            .filter(|o| o.account_id == id)
            .count() as i64
            + 1;
        let obs = RateObservation {
            at,
            account_id: id.to_owned(),
            rpm,
            rejected: outcome == "rate_limited",
            outcome: outcome.to_owned(),
        };
        inner.observations.push_back(obs.clone());
        inner.unsaved.push(obs);
    }

    pub fn report_success(&self, id: &str, model: &str) {
        let Some(a) = self.get(id) else { return };
        self.commit_success(&a, model, None);
    }

    /// Commits a successful request only if its original account object is
    /// still the live pool member. Membership, affinity, model evidence and
    /// account statistics change under one pool lock so a same-ID replacement
    /// cannot receive an old request's completion.
    pub fn commit_success(&self, a: &Arc<Account>, model: &str, session: Option<u64>) -> bool {
        let mut inner = self.inner.lock();
        if !inner
            .accounts
            .get(&a.id)
            .is_some_and(|live| Arc::ptr_eq(live, a))
        {
            return false;
        }
        a.models.record_supported(model);
        {
            let mut s = a.state.lock();
            s.failures = 0;
            s.rate_limited_until = 0.0;
            s.auth_dead_until = 0.0;
            if s.suspended_until > 0.0 {
                s.suspended_until = 0.0;
                tracing::info!("Account {} is serving again; suspension lifted", a.id);
            }
            if s.quota_exhausted_until > 0.0 {
                s.quota_exhausted_until = 0.0;
                tracing::info!(
                    "Account {} is serving again; quota quarantine cleared",
                    a.id
                );
            }
            s.stats.total += 1;
            s.stats.success += 1;
        }
        Self::record_event_locked(&mut inner, &a.id, "success");
        let normalized = normalize_model_name(model);
        let list = inner.model_to_accounts.entry(normalized).or_default();
        if !list.iter().any(|x| x == &a.id) {
            list.push(a.id.clone());
        }
        if let Some(i) = inner.order.iter().position(|x| x == &a.id) {
            inner.current_index = i;
        }
        if settings::tunables().load_balancing == "session" {
            if let Some(key) = session {
                Self::pin_session_locked(&mut inner, key, &a.id);
            }
        }
        drop(inner);
        self.mark_dirty();
        true
    }

    fn quota_quarantine_until(s: &AccountState, now: f64) -> f64 {
        let cfg = config::get();
        let floor = now + cfg.account_quota_quarantine as f64;
        if s.quota_resets_at <= 0.0 {
            return floor;
        }
        let target = s.quota_resets_at + cfg.account_quota_reset_margin as f64;
        floor.max(target.min(now + cfg.account_quota_quarantine_max as f64))
    }

    pub fn report_failure(
        &self,
        id: &str,
        model: &str,
        error_type: ErrorType,
        status: u16,
        reason: Option<&str>,
        message: Option<&str>,
    ) {
        let Some(a) = self.get(id) else { return };
        self.commit_failure(&a, model, error_type, status, reason, message);
    }

    pub fn commit_failure(
        &self,
        a: &Arc<Account>,
        model: &str,
        error_type: ErrorType,
        status: u16,
        reason: Option<&str>,
        message: Option<&str>,
    ) -> bool {
        let mut inner = self.inner.lock();
        if !inner
            .accounts
            .get(&a.id)
            .is_some_and(|live| Arc::ptr_eq(live, a))
        {
            return false;
        }
        let cfg = config::get();
        let now = store::now_f64();
        let outcome = {
            let mut s = a.state.lock();
            if reason == Some("INVALID_MODEL_ID") {
                a.models.record_unsupported(model);
                s.stats.total += 1;
                tracing::warn!("Model '{model}' not available on account {}: status={status}, reason=INVALID_MODEL_ID", a.id);
                None
            } else if reason == Some("USER_REQUEST_RATE_EXCEEDED") {
                s.rate_limited_until = now + cfg.account_rate_limit_cooldown as f64;
                s.stats.total += 1;
                s.stats.failed += 1;
                tracing::warn!("Account {} rate limited: status={status}, cooldown={} (failures unchanged at {})", a.id, format_duration(cfg.account_rate_limit_cooldown as f64), s.failures);
                Some("rate_limited")
            } else if is_suspension_error(status, message, reason) {
                s.suspended_until = now + cfg.account_suspension_quarantine as f64;
                s.stats.total += 1;
                s.stats.failed += 1;
                tracing::error!(
                    "Account {} is SUSPENDED upstream: status={status}; excluded for {}",
                    a.id,
                    format_duration(cfg.account_suspension_quarantine as f64)
                );
                Some("suspended")
            } else if reason == Some("MONTHLY_REQUEST_COUNT") {
                s.quota_exhausted_until = Self::quota_quarantine_until(&s, now);
                s.stats.total += 1;
                s.stats.failed += 1;
                tracing::warn!(
                    "Account {} monthly quota exhausted; excluded for {}",
                    a.id,
                    format_duration((s.quota_exhausted_until - now).max(0.0))
                );
                Some("quota_exhausted")
            } else {
                if error_type == ErrorType::Recoverable {
                    s.failures += 1;
                    s.last_failure_time = now;
                    tracing::warn!(
                        "Account {} failure #{}: status={status}, reason={reason:?}",
                        a.id,
                        s.failures
                    );
                }
                s.stats.total += 1;
                s.stats.failed += 1;
                Some("failure")
            }
        };
        if let Some(o) = outcome {
            Self::record_event_locked(&mut inner, &a.id, o);
        }
        drop(inner);
        self.mark_dirty();
        true
    }

    pub fn report_credential_dead(&self, id: &str, status: u16) {
        let Some(a) = self.get(id) else { return };
        self.commit_credential_dead(&a, status);
    }

    pub fn commit_credential_dead(&self, a: &Arc<Account>, status: u16) -> bool {
        let mut inner = self.inner.lock();
        if !inner
            .accounts
            .get(&a.id)
            .is_some_and(|live| Arc::ptr_eq(live, a))
        {
            return false;
        }
        let now = store::now_f64();
        let already = {
            let mut s = a.state.lock();
            let already = s.auth_dead_until > now;
            s.auth_dead_until = now + config::get().account_auth_dead_quarantine as f64;
            s.stats.total += 1;
            s.stats.failed += 1;
            already
        };
        if !already {
            Self::record_event_locked(&mut inner, &a.id, "auth_dead");
            tracing::error!("Account {} credential is DEAD (HTTP {status} from the auth host); re-register or re-login to restore it.", a.id);
        }
        drop(inner);
        self.mark_dirty();
        true
    }

    pub fn set_quota(
        &self,
        id: &str,
        headroom: Option<f64>,
        resets_at: Option<f64>,
        overage: Option<bool>,
    ) {
        let Some(a) = self.get(id) else { return };
        let mut s = a.state.lock();
        s.quota_headroom = headroom.map(|h| h.clamp(0.0, 1.0));
        s.quota_resets_at = resets_at
            .filter(|r| r.is_finite() && *r > 0.0)
            .unwrap_or(0.0);
        s.quota_overage_enabled = overage;
    }

    pub fn drain_unsaved_observations(&self) -> Vec<RateObservation> {
        std::mem::take(&mut self.inner.lock().unsaved)
    }

    pub fn restore_unsaved_observations(&self, mut rows: Vec<RateObservation>) {
        let mut inner = self.inner.lock();
        rows.append(&mut inner.unsaved);
        inner.unsaved = rows;
    }

    pub fn load_observations(&self, rows: Vec<RateObservation>) {
        let mut inner = self.inner.lock();
        for r in rows.into_iter().rev() {
            inner.observations.push_front(r);
        }
    }

    fn prune_observations(inner: &mut PoolInner, now: f64) {
        let cutoff = now - config::get().rate_estimate_window_seconds as f64;
        while inner.observations.front().is_some_and(|o| o.at < cutoff) {
            inner.observations.pop_front();
        }
    }

    pub fn estimate_rate_limit(&self, id: &str, now: f64) -> Value {
        let inner = self.inner.lock();
        estimate(&inner.observations, id, now)
    }

    pub fn request_rate_series(&self, window: i64, bucket: i64) -> Value {
        let now = store::now_f64();
        let bucket = bucket.max(1);
        let latest = (now as i64 / bucket) * bucket;
        let count = (window / bucket).max(1);
        let starts: Vec<i64> = (0..count).rev().map(|o| latest - o * bucket).collect();
        let index: HashMap<i64, usize> = starts.iter().enumerate().map(|(i, s)| (*s, i)).collect();
        let accounts = self.accounts();
        let mut inner = self.inner.lock();
        Self::prune_observations(&mut inner, now);
        let mut by_account: HashMap<&str, Vec<&RateObservation>> = HashMap::new();
        for o in &inner.observations {
            by_account.entry(o.account_id.as_str()).or_default().push(o);
        }
        let series: Vec<Value> = accounts
            .iter()
            .map(|a| {
                let n = count as usize;
                let (mut ok, mut rl, mut fail, mut peak) = (vec![0i64; n], vec![0i64; n], vec![0i64; n], vec![0i64; n]);
                for o in by_account.get(a.id.as_str()).into_iter().flatten() {
                    let Some(&b) = index.get(&((o.at as i64 / bucket) * bucket)) else { continue };
                    match o.outcome.as_str() {
                        "success" => ok[b] += 1,
                        "rate_limited" => rl[b] += 1,
                        _ => fail[b] += 1,
                    }
                    peak[b] = peak[b].max(o.rpm);
                }
                let mut v = json!({"account": account_label(&a.id), "success": ok, "rateLimited": rl, "failure": fail, "peakRpm": peak, "routingState": routing_state(a, now).0});
                if let (Value::Object(m), Value::Object(e)) = (&mut v, estimate(&inner.observations, &a.id, now)) {
                    m.extend(e);
                }
                v
            })
            .collect();
        json!({"bucketSeconds": bucket, "bucketStarts": starts, "rateWindowSeconds": config::get().rate_window_seconds, "accounts": series})
    }

    pub fn first_initialized(&self) -> Option<Arc<Account>> {
        self.accounts()
            .into_iter()
            .find(|a| a.auth.lock().is_some())
    }

    pub fn all_available_models(&self) -> Vec<String> {
        let mut set: std::collections::BTreeSet<String> = Default::default();
        for a in self.accounts().iter().filter(|a| a.auth.lock().is_some()) {
            set.extend(model_resolver::available_models(&a.models));
        }
        set.into_iter().collect()
    }

    pub fn auth_type_of(a: &Account) -> Option<AuthType> {
        a.auth().map(|x| x.auth_type())
    }
}

fn estimate(observations: &VecDeque<RateObservation>, id: &str, now: f64) -> Value {
    let window = config::get().rate_estimate_window_seconds;
    let cutoff = now - window as f64;
    let samples: Vec<&RateObservation> = observations
        .iter()
        .filter(|o| o.account_id == id && o.at >= cutoff)
        .collect();
    let served_peak = samples
        .iter()
        .filter(|o| !o.rejected)
        .map(|o| o.rpm)
        .max()
        .unwrap_or(0);
    let rejections: Vec<i64> = samples
        .iter()
        .filter(|o| o.rejected)
        .map(|o| o.rpm)
        .collect();
    let informative: Vec<i64> = rejections
        .iter()
        .copied()
        .filter(|r| *r >= served_peak)
        .collect();
    let limit = informative.iter().copied().min();
    let reason = if limit.is_some() {
        Value::Null
    } else if !rejections.is_empty() {
        json!("rejections seen only below the rate this account serves cleanly")
    } else {
        json!("no rate rejection observed yet")
    };
    json!({
        "limitRpm": limit, "limitUnknownReason": reason, "safeRpm": served_peak,
        "limitPrecisionRpm": limit.map(|l| (l - served_peak).max(0)),
        "rateLimitSamples": rejections.len(), "informativeSamples": informative.len(), "estimateWindowSeconds": window,
    })
}

fn valid_credential_file(p: &std::path::Path, kind: &str) -> bool {
    match kind {
        "json" => std::fs::read_to_string(p)
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .is_some_and(|d| d.get("refreshToken").is_some() || d.get("clientId").is_some()),
        "sqlite" => {
            rusqlite::Connection::open_with_flags(p, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .ok()
                .and_then(|c| {
                    c.query_row(
                        "SELECT name FROM sqlite_master WHERE type='table' AND name='auth_kv'",
                        [],
                        |r| r.get::<_, String>(0),
                    )
                    .ok()
                })
                .is_some()
        }
        _ => false,
    }
}

/// Session key: system text plus the first user message. The same conversation
/// keeps both constant across turns; distinct conversations differ.
pub fn session_key(system: &str, first_user: &str) -> Option<u64> {
    if system.is_empty() && first_user.is_empty() {
        return None;
    }
    let digest = Sha256::new()
        .chain_update(system.as_bytes())
        .chain_update([0u8])
        .chain_update(first_user.as_bytes())
        .finalize();
    Some(u64::from_be_bytes(digest[..8].try_into().unwrap()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(id: &str) -> Arc<Account> {
        Arc::new(Account {
            id: id.into(),
            config: json!({}),
            auth: Mutex::new(None),
            models: Arc::new(ModelInfoCache::new()),
            state: Mutex::new(AccountState::default()),
            init: tokio::sync::Mutex::new(()),
            init_retry_at: Mutex::new(None),
            init_scheduled: false.into(),
            models_refresh: tokio::sync::Mutex::new(()),
            models_refresh_scheduled: false.into(),
        })
    }

    #[test]
    fn affinity_lookup_does_not_extend_a_near_expiry_pin() {
        assert_eq!(settings::tunables().load_balancing, "session");
        let pool = AccountManager::new(reqwest::Client::new());
        let a = account("a");
        let touched = Instant::now()
            - Duration::from_secs(config::get().session_affinity_ttl_seconds.saturating_sub(1));
        {
            let mut inner = pool.inner.lock();
            inner.order.push(a.id.clone());
            inner.accounts.insert(a.id.clone(), a);
            inner.sessions.insert(
                7,
                SessionEntry {
                    account: "a".into(),
                    touched,
                },
            );
        }

        pool.candidate_order("model", Some(7));
        pool.candidate_order("model", Some(7));

        assert_eq!(pool.inner.lock().sessions.get(&7).unwrap().touched, touched);
    }

    #[test]
    fn old_completion_cannot_mutate_or_pin_a_same_id_replacement() {
        assert_eq!(settings::tunables().load_balancing, "session");
        let pool = AccountManager::new(reqwest::Client::new());
        let old = account("a");
        let replacement = account("a");
        replacement.state.lock().failures = 3;
        {
            let mut inner = pool.inner.lock();
            inner.order.push("a".into());
            inner.accounts.insert("a".into(), replacement.clone());
        }

        assert!(!pool.commit_success(&old, "model", Some(9)));
        assert_eq!(replacement.state.lock().failures, 3);
        assert_eq!(replacement.models.support("model"), ModelSupport::Unknown);
        assert!(pool.session_counts().is_empty());

        assert!(pool.commit_success(&replacement, "model", Some(9)));
        assert_eq!(replacement.state.lock().failures, 0);
        assert_eq!(replacement.models.support("model"), ModelSupport::Supported);
        assert_eq!(pool.session_counts().get("a"), Some(&1));
    }

    #[test]
    fn old_failures_cannot_mutate_a_same_id_replacement() {
        let pool = AccountManager::new(reqwest::Client::new());
        let old = account("a");
        let replacement = account("a");
        replacement.state.lock().failures = 3;
        {
            let mut inner = pool.inner.lock();
            inner.order.push("a".into());
            inner.accounts.insert("a".into(), replacement.clone());
        }

        assert!(!pool.commit_failure(
            &old,
            "model",
            ErrorType::Fatal,
            400,
            Some("INVALID_MODEL_ID"),
            None,
        ));
        assert!(!pool.commit_failure(
            &old,
            "model",
            ErrorType::Recoverable,
            429,
            Some("USER_REQUEST_RATE_EXCEEDED"),
            None,
        ));
        assert!(!pool.commit_failure(
            &old,
            "model",
            ErrorType::Recoverable,
            402,
            Some("MONTHLY_REQUEST_COUNT"),
            None,
        ));
        assert!(!pool.commit_credential_dead(&old, 401));

        let state = replacement.state.lock();
        assert_eq!(state.failures, 3);
        assert_eq!(state.rate_limited_until, 0.0);
        assert_eq!(state.quota_exhausted_until, 0.0);
        assert_eq!(state.auth_dead_until, 0.0);
        assert_eq!(state.stats.total, 0);
        drop(state);
        assert_eq!(replacement.models.support("model"), ModelSupport::Unknown);
        assert!(pool.inner.lock().observations.is_empty());
    }
}
