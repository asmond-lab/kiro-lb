//! Best-effort release checks, cached independently of dashboard polling.

use semver::Version;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::time::{Duration, Instant};

use crate::{app::Shared, config::APP_VERSION};

const LATEST_RELEASE: &str = "https://api.github.com/repos/minpeter/kiro-lb/releases/latest";
const CHECK_TIMEOUT: Duration = Duration::from_secs(5);
const CHECK_INTERVAL: Duration = Duration::from_secs(3600);
const MIN_CHECK_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Default)]
pub struct UpdateChecker {
    pub info: parking_lot::RwLock<VersionInfo>,
    last_check: tokio::sync::Mutex<Option<Instant>>,
    pub installation: parking_lot::RwLock<Installation>,
    pub restart: tokio::sync::Notify,
    pub pending: parking_lot::Mutex<Option<crate::update_install::InstalledUpdate>>,
}

impl UpdateChecker {
    pub async fn refresh(&self, client: &reqwest::Client) -> VersionInfo {
        self.refresh_from(client, LATEST_RELEASE).await
    }

    async fn refresh_from(&self, client: &reqwest::Client, endpoint: &str) -> VersionInfo {
        // Serializing checks and reusing recent results coalesces concurrent
        // manual/background requests, including failed checks. Cancelling a
        // caller drops this guard, so later callers can retry normally.
        let mut last_check = self.last_check.lock().await;
        if last_check.is_some_and(|at| at.elapsed() < MIN_CHECK_INTERVAL) {
            return self.info.read().clone();
        }
        let info = match check(client, endpoint, APP_VERSION, CHECK_TIMEOUT).await {
            Ok(info) => info,
            Err(error) => {
                tracing::debug!("Could not check for kiro-lb updates: {error}");
                VersionInfo {
                    status: UpdateStatus::Unavailable,
                    ..Default::default()
                }
            }
        };
        *self.info.write() = info.clone();
        *last_check = Some(Instant::now());
        info
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallStatus {
    Idle,
    Downloading,
    Restarting,
    Failed,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Installation {
    pub status: InstallStatus,
    pub version: Option<String>,
    pub error: Option<String>,
    pub disabled_reason: Option<&'static str>,
}

impl Default for Installation {
    fn default() -> Self {
        let disabled_reason = if std::env::var("KIRO_LB_CONTAINER").as_deref() == Ok("1")
            || std::path::Path::new("/.dockerenv").exists()
            || std::path::Path::new("/run/.containerenv").exists()
        {
            Some("container")
        } else if std::env::var("KIRO_SLOT").is_ok_and(|slot| !slot.is_empty()) {
            Some("managed")
        } else if crate::update_install::asset_name().is_none() {
            Some("unsupported")
        } else if cfg!(debug_assertions) {
            Some("development")
        } else {
            None
        };
        Self {
            status: InstallStatus::Idle,
            version: None,
            error: None,
            disabled_reason,
        }
    }
}

/// Admit one explicitly confirmed install, independent of the HTTP connection.
pub fn start_install(state: Shared, version: &str) -> Result<Installation, &'static str> {
    let info = state.version.info.read().clone();
    let mut install = state.version.installation.write();
    if install.disabled_reason.is_some() {
        return Err("This deployment must be updated externally");
    }
    if matches!(
        install.status,
        InstallStatus::Downloading | InstallStatus::Restarting
    ) {
        return Err("An update is already in progress");
    }
    if info.status != UpdateStatus::UpdateAvailable || info.latest.as_deref() != Some(version) {
        return Err("The available version changed; check for updates and confirm again");
    }
    install.status = InstallStatus::Downloading;
    install.version = Some(version.to_owned());
    install.error = None;
    let accepted = install.clone();
    drop(install);
    let tag = format!("v{version}");
    tokio::spawn(async move {
        let result = async {
            let executable = std::env::current_exe()
                .and_then(|p| p.canonicalize())
                .map_err(|e| e.to_string())?;
            let prepared = crate::update_install::prepare(&state.http, executable, &tag).await?;
            let updating = state.clone();
            tokio::task::spawn_blocking(move || {
                let result = prepared.replace();
                // Windows may already have renamed the running image before
                // replacement fails. Restoring the original path does not fix
                // current_exe() for this process; another attempt could update
                // the relocated image instead of the installed executable.
                if cfg!(windows) && result.is_err() {
                    updating.version.installation.write().disabled_reason =
                        Some("restart_required");
                }
                result
            })
            .await
            .map_err(|e| e.to_string())?
        }
        .await;
        match result {
            Ok(installed) => {
                tracing::info!(
                    "Installed {tag}; draining requests before restart. Backup: {}",
                    installed.backup.display()
                );
                *state.version.pending.lock() = Some(installed);
                state.version.installation.write().status = InstallStatus::Restarting;
                state.version.restart.notify_one();
            }
            Err(error) => {
                tracing::warn!("Update installation failed: {error}");
                let mut install = state.version.installation.write();
                install.status = InstallStatus::Failed;
                install.error = Some(error);
            }
        }
    });
    Ok(accepted)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateStatus {
    Checking,
    Latest,
    UpdateAvailable,
    Ahead,
    Unavailable,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionInfo {
    pub current: String,
    pub latest: Option<String>,
    pub status: UpdateStatus,
    pub release_url: Option<String>,
}

impl Default for VersionInfo {
    fn default() -> Self {
        Self {
            current: APP_VERSION.to_owned(),
            latest: None,
            status: UpdateStatus::Checking,
            release_url: None,
        }
    }
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    draft: bool,
    prerelease: bool,
}

impl Release {
    fn version_info(self, current: &str) -> Result<VersionInfo, String> {
        let latest = self.tag_name.strip_prefix('v').unwrap_or(&self.tag_name);
        let latest_version = Version::parse(latest).map_err(|e| e.to_string())?;
        if self.draft || self.prerelease || !latest_version.pre.is_empty() {
            return Err("Latest release is not a stable published version".into());
        }
        let current_version = Version::parse(current).map_err(|e| e.to_string())?;
        let status = match current_version.cmp_precedence(&latest_version) {
            Ordering::Less => UpdateStatus::UpdateAvailable,
            Ordering::Equal => UpdateStatus::Latest,
            Ordering::Greater => UpdateStatus::Ahead,
        };
        Ok(VersionInfo {
            current: current.to_owned(),
            latest: Some(latest.to_owned()),
            status,
            release_url: Some(format!(
                "https://github.com/minpeter/kiro-lb/releases/tag/{}",
                self.tag_name
            )),
        })
    }
}

async fn check(
    client: &reqwest::Client,
    endpoint: &str,
    current: &str,
    timeout: Duration,
) -> Result<VersionInfo, String> {
    let release = client
        .get(endpoint)
        .header("accept", "application/vnd.github+json")
        .header("user-agent", concat!("kiro-lb/", env!("CARGO_PKG_VERSION")))
        .timeout(timeout)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| e.to_string())?
        .json::<Release>()
        .await
        .map_err(|e| e.to_string())?;
    release.version_info(current)
}

/// Run after the listener is bound. GitHub failures never prevent serving requests.
pub async fn run(state: Shared) {
    let mut notified = None;
    loop {
        let info = state.version.refresh(&state.http).await;
        if info.status == UpdateStatus::UpdateAvailable && info.latest != notified {
            tracing::warn!(
                "kiro-lb update available: v{} -> v{}. Download: {}",
                info.current,
                info.latest.as_deref().unwrap_or_default(),
                info.release_url.as_deref().unwrap_or_default(),
            );
            notified = info.latest.clone();
        }
        tokio::time::sleep(CHECK_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{http::StatusCode, response::IntoResponse, routing::get, Router};

    fn release(tag: &str) -> Release {
        Release {
            tag_name: tag.into(),
            draft: false,
            prerelease: false,
        }
    }

    #[test]
    fn compares_semver_precedence_not_text_or_build_metadata() {
        for (current, tag, expected) in [
            ("0.2.9", "v0.2.10", UpdateStatus::UpdateAvailable),
            ("0.9.9", "v0.10.0", UpdateStatus::UpdateAvailable),
            ("0.99.9", "v1.0.0", UpdateStatus::UpdateAvailable),
            ("0.2.1", "v0.2.1", UpdateStatus::Latest),
            ("0.2.1+local", "0.2.1", UpdateStatus::Latest),
            ("0.2.2-rc.1", "v0.2.2", UpdateStatus::UpdateAvailable),
            ("0.3.0", "v0.2.1", UpdateStatus::Ahead),
        ] {
            let info = release(tag).version_info(current).unwrap();
            assert_eq!(info.status, expected, "{current} vs {tag}");
            assert_eq!(info.current, current);
            assert_eq!(info.latest.as_deref(), Some(tag.trim_start_matches('v')));
            assert_eq!(
                info.release_url.unwrap(),
                format!("https://github.com/minpeter/kiro-lb/releases/tag/{tag}")
            );
        }
    }

    #[test]
    fn rejects_invalid_or_unpublished_release_versions() {
        for tag in ["nightly", "v1.2", "v01.2.3", "v1.2.3-rc.1"] {
            assert!(release(tag).version_info("0.2.1").is_err(), "{tag}");
        }
        for (draft, prerelease) in [(true, false), (false, true)] {
            let r = Release {
                draft,
                prerelease,
                ..release("v1.2.3")
            };
            assert!(r.version_info("0.2.1").is_err());
        }
    }

    #[tokio::test]
    async fn handles_http_errors_bad_json_and_slow_responses() {
        let app = Router::new()
            .route("/latest", get(|headers: axum::http::HeaderMap| async move {
                assert!(headers["user-agent"].to_str().unwrap().starts_with("kiro-lb/"));
                assert_eq!(headers["accept"], "application/vnd.github+json");
                axum::Json(serde_json::json!({"tag_name": "v0.2.10", "draft": false, "prerelease": false}))
            }))
            .route("/limited", get(|| async { StatusCode::FORBIDDEN }))
            .route("/missing", get(|| async { StatusCode::NOT_FOUND }))
            .route("/invalid", get(|| async { "not json" }))
            .route("/slow", get(|| async {
                // Send headers immediately but stall the body: the deadline must
                // cover reading JSON, not just connecting and receiving headers.
                let stream = futures_util::stream::once(async {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    Ok::<_, std::io::Error>("{}")
                });
                axum::body::Body::from_stream(stream).into_response()
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async { axum::serve(listener, app).await.unwrap() });
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let info = check(&client, &format!("{base}/latest"), "0.2.9", CHECK_TIMEOUT)
            .await
            .unwrap();
        assert_eq!(info.status, UpdateStatus::UpdateAvailable);
        for path in ["limited", "missing", "invalid", "slow"] {
            assert!(
                check(
                    &client,
                    &format!("{base}/{path}"),
                    "0.2.9",
                    Duration::from_millis(100)
                )
                .await
                .is_err(),
                "{path}"
            );
        }
        server.abort();
    }

    #[tokio::test]
    async fn concurrent_checks_share_results_until_the_cooldown_expires_including_failures() {
        use std::sync::{
            atomic::{AtomicBool, AtomicUsize, Ordering},
            Arc,
        };

        let calls = Arc::new(AtomicUsize::new(0));
        let failing = Arc::new(AtomicBool::new(false));
        let (count, fail) = (calls.clone(), failing.clone());
        let app = Router::new().route("/", get(move || {
            let (count, fail) = (count.clone(), fail.clone());
            async move {
                count.fetch_add(1, Ordering::SeqCst);
                if fail.load(Ordering::SeqCst) {
                    StatusCode::TOO_MANY_REQUESTS.into_response()
                } else {
                    axum::Json(serde_json::json!({"tag_name": "v99.0.0", "draft": false, "prerelease": false})).into_response()
                }
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async { axum::serve(listener, app).await.unwrap() });
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let checker = UpdateChecker::default();
        let (a, b) = tokio::join!(
            checker.refresh_from(&client, &url),
            checker.refresh_from(&client, &url)
        );
        assert_eq!(a.status, UpdateStatus::UpdateAvailable);
        assert_eq!(b.latest.as_deref(), Some("99.0.0"));
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        *checker.last_check.lock().await =
            Some(Instant::now() - (MIN_CHECK_INTERVAL - Duration::from_secs(1)));
        checker.refresh_from(&client, &url).await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        *checker.last_check.lock().await = Some(Instant::now() - MIN_CHECK_INTERVAL);
        failing.store(true, Ordering::SeqCst);
        let failed = checker.refresh_from(&client, &url).await;
        assert_eq!(failed.status, UpdateStatus::Unavailable);
        assert!(failed.latest.is_none());
        assert!(checker.info.read().release_url.is_none());
        let again = checker.refresh_from(&client, &url).await;
        assert_eq!(again.status, UpdateStatus::Unavailable);
        assert_eq!(calls.load(Ordering::SeqCst), 2);

        *checker.last_check.lock().await = Some(Instant::now() - MIN_CHECK_INTERVAL);
        failing.store(false, Ordering::SeqCst);
        assert_eq!(
            checker.refresh_from(&client, &url).await.status,
            UpdateStatus::UpdateAvailable
        );
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        server.abort();
    }

    #[tokio::test]
    async fn cancelling_a_check_releases_the_gate_without_caching_a_false_result() {
        let checker = UpdateChecker::default();
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        // The listening socket never responds; cancel after the request starts.
        let mut pending = Box::pin(checker.refresh_from(&client, &url));
        tokio::select! {
            result = &mut pending => panic!("check unexpectedly completed: {result:?}"),
            accepted = listener.accept() => { let _socket = accepted.unwrap(); },
        }
        drop(pending);
        assert!(checker.last_check.try_lock().unwrap().is_none());
        assert_eq!(checker.info.read().status, UpdateStatus::Checking);

        let app = Router::new().route("/", get(|| async { StatusCode::NOT_FOUND }));
        let server = tokio::spawn(async { axum::serve(listener, app).await.unwrap() });
        let result = checker.refresh_from(&client, &url).await;
        assert_eq!(result.status, UpdateStatus::Unavailable);
        assert!(checker.last_check.try_lock().unwrap().is_some());
        server.abort();
    }
}
