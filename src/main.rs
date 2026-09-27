#![allow(clippy::result_large_err)]

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use axum::body::Body;
use axum::http::{header, Method, StatusCode, Uri};
use axum::response::Response;
use axum::routing::{delete, get, post};
use axum::Router;
use include_dir::{include_dir, Dir};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicI64};
use std::sync::Arc;
use std::time::Duration;

use kiro_lb::app::{self, cors_preflight, AppState};
use kiro_lb::pool::AccountManager;
use kiro_lb::routes_dashboard as d;
use kiro_lb::routes_v1 as v1;
use kiro_lb::upstream::http::{self as up, Transport};
use kiro_lb::{config, dashboard_store, settings, store, tokenizer};

static STATIC: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/static");

async fn static_file(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = match path {
        "" => "index.html",
        "favicon.svg" => "kiro-icon.svg",
        p => p,
    };
    match STATIC.get_file(path) {
        Some(f) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            let cache = if path.starts_with("assets/") || path.starts_with("fonts/") {
                "public, max-age=31536000, immutable"
            } else {
                "no-cache"
            };
            Response::builder()
                .header(header::CONTENT_TYPE, mime.as_ref())
                .header(header::CACHE_CONTROL, cache)
                .body(Body::from(f.contents()))
                .unwrap()
        }
        None => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"detail":"Not Found"}"#))
            .unwrap(),
    }
}

fn router(state: app::Shared) -> Router {
    Router::new()
        .route("/v1/messages", post(v1::messages).options(cors_preflight))
        .route(
            "/v1/messages/count_tokens",
            post(v1::count_tokens).options(cors_preflight),
        )
        .route(
            "/v1/chat/completions",
            post(v1::chat_completions).options(cors_preflight),
        )
        .route("/v1/responses", post(v1::responses).options(cors_preflight))
        .route("/v1/models", get(v1::models).options(cors_preflight))
        .route("/v1/models/{id}", get(v1::model).options(cors_preflight))
        .route("/health", get(v1::health))
        .route("/healthz", get(v1::healthz))
        .route("/docs", get(kiro_lb::docs::swagger))
        .route("/openapi.json", get(kiro_lb::docs::openapi))
        .route("/metrics", get(d::metrics))
        .route("/api/dashboard/login", post(d::login))
        .route("/api/dashboard/logout", post(d::logout))
        .route("/api/dashboard/keys", get(d::list_keys).post(d::create_key))
        .route("/api/dashboard/keys/usage", get(d::key_usage))
        .route(
            "/api/dashboard/keys/{id}",
            delete(d::delete_key).patch(d::rename_key),
        )
        .route("/api/dashboard/accounts/usage", get(d::account_usage))
        .route("/api/dashboard/overview", get(d::overview))
        .route(
            "/api/dashboard/accounts",
            get(d::accounts).post(d::register_account),
        )
        .route(
            "/api/dashboard/accounts/refresh-usage",
            post(d::refresh_usage),
        )
        .route(
            "/api/dashboard/accounts/device-login",
            post(d::start_device_login),
        )
        .route(
            "/api/dashboard/accounts/device-login/{id}",
            get(d::poll_device_login).delete(d::cancel_device_login),
        )
        .route(
            "/api/dashboard/accounts/device-login/{id}/register",
            post(d::register_device_login),
        )
        .route("/api/dashboard/accounts/{label}", delete(d::delete_account))
        .route(
            "/api/dashboard/accounts/{label}/enabled",
            post(d::set_enabled),
        )
        .route("/api/dashboard/request-rate", get(d::request_rate))
        .route(
            "/api/dashboard/endpoints",
            get(d::get_endpoints).put(d::put_endpoints),
        )
        .route("/api/dashboard/endpoints/test", post(d::test_endpoints))
        .route("/api/dashboard/endpoints/ping", post(d::ping_endpoints))
        .route("/api/dashboard/request-logs", get(d::request_logs))
        .route(
            "/api/dashboard/request-logs/{id}",
            get(d::request_log_detail),
        )
        .route("/api/dashboard/data", get(d::data_overview))
        .route("/api/dashboard/data/clear", post(d::clear_data))
        .route(
            "/api/dashboard/proxies",
            get(d::get_proxies).put(d::put_proxies),
        )
        .route("/api/dashboard/concurrency", get(d::concurrency))
        .route(
            "/api/dashboard/tunables",
            get(d::get_tunables).put(d::put_tunables),
        )
        .route("/api/dashboard/model-costs", get(d::model_costs_view))
        .route(
            "/api/dashboard/agent-mode",
            get(d::get_agent_mode).put(d::put_agent_mode),
        )
        .route(
            "/api/dashboard/prompt-filter",
            get(d::get_prompt_filter).put(d::put_prompt_filter),
        )
        .route("/api/dashboard/models", get(d::dashboard_models))
        .route("/_internal/handoff/quiesce", post(d::handoff_quiesce))
        .route("/_internal/handoff/activate", post(d::handoff_activate))
        .route("/_internal/handoff/ready", get(d::handoff_ready))
        .fallback(|method: Method, uri: Uri| async move {
            if method == Method::GET || method == Method::HEAD {
                static_file(uri).await
            } else {
                app::detail(404, "Not Found")
            }
        })
        .layer(axum::extract::DefaultBodyLimit::max(64 * 1024 * 1024))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            app::data_plane_middleware,
        ))
        .layer(axum::middleware::map_response(
            |mut r: Response| async move {
                r.headers_mut()
                    .insert("access-control-allow-origin", "*".parse().unwrap());
                r
            },
        ))
        .with_state(state)
}

fn spawn_background(state: app::Shared) {
    let cfg = config::get();
    let s = state.clone();
    let interval = cfg.state_save_interval_seconds.max(1) as u64;
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(interval)).await;
            if s.pool.is_dirty() {
                let p = s.pool.clone();
                let _ = tokio::task::spawn_blocking(move || p.save_state()).await;
            }
        }
    });
    if cfg.usage_refresh_interval_seconds > 0 {
        let s = state.clone();
        let every = cfg.usage_refresh_interval_seconds.max(60) as u64;
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(every)).await;
                if s.quiesced.load(std::sync::atomic::Ordering::SeqCst) {
                    continue;
                }
                d::refresh_all_usage(&s).await;
            }
        });
    }
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(3600)).await;
            let _ = tokio::task::spawn_blocking(dashboard_store::prune_request_logs).await;
        }
    });
    let s = state;
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(30)).await;
            let pool = s.pool.clone();
            let _ = tokio::task::spawn_blocking(move || {
                let rows = pool.drain_unsaved_observations();
                if !dashboard_store::record_rate_observations(&rows) {
                    pool.restore_unsaved_observations(rows);
                }
                dashboard_store::prune_rate_observations();
                dashboard_store::flush_key_model_usage();
            })
            .await;
        }
    });
}

fn parse_args() -> (String, u16) {
    let cfg = config::get();
    let (mut host, mut port) = (cfg.server_host.clone(), cfg.server_port);
    let args: Vec<String> = std::env::args().collect();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--host" if i + 1 < args.len() => {
                host = args[i + 1].clone();
                i += 1;
            }
            "--port" if i + 1 < args.len() => {
                port = args[i + 1].parse().unwrap_or(port);
                i += 1;
            }
            "-h" | "--help" => {
                println!("kirolb [--host HOST] [--port PORT]\n       kirolb replay <capture-dir>");
                std::process::exit(0);
            }
            _ => {}
        }
        i += 1;
    }
    (host, port)
}

fn main() {
    let generated = match kiro_lb::bootstrap::ensure_env() {
        Ok(g) => g,
        Err(e) => {
            eprintln!("Cannot create .env: {e}");
            std::process::exit(1);
        }
    };
    let cfg = config::get();
    let filter = std::env::var("RUST_LOG").unwrap_or_else(|_| {
        match cfg.log_level.as_str() {
            "TRACE" => "trace",
            "DEBUG" => "debug",
            "WARNING" | "WARN" => "warn",
            "ERROR" | "CRITICAL" => "error",
            _ => "info",
        }
        .into()
    });
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(filter))
        .with_target(false)
        .compact()
        .init();
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("replay") {
        std::process::exit(kiro_lb::debug::replay_cli(&args[2..]));
    }
    let (host, port) = parse_args();
    let addr: SocketAddr = format!("{host}:{port}")
        .parse()
        .unwrap_or_else(|_| SocketAddr::from(([0, 0, 0, 0], port)));
    print_banner(&addr);
    if let Some(g) = &generated {
        println!("  No .env found: created .env and .env.example with fresh credentials.");
        println!("  PROXY_API_KEY:      {}", g.api_key);
        println!("  DASHBOARD_PASSWORD: {}", g.password);
        println!();
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(serve(host, port));
}

async fn serve(host: String, port: u16) {
    let cfg = config::get();
    if cfg.first_token_timeout >= cfg.streaming_read_timeout {
        tracing::warn!(
            "FIRST_TOKEN_TIMEOUT ({}s) >= STREAMING_READ_TIMEOUT ({}s); the first should be lower",
            cfg.first_token_timeout,
            cfg.streaming_read_timeout
        );
    }
    if let Err(e) = tokio::task::spawn_blocking(store::initialize)
        .await
        .unwrap()
    {
        tracing::error!(
            "Cannot initialize the store at {}: {e}",
            store::database_path().display()
        );
        std::process::exit(1);
    }
    tokio::task::spawn_blocking(|| {
        settings::load_all();
        up::load_proxies();
        tokenizer::warm_up();
    })
    .await
    .unwrap();
    let quiesced = !store::can_write_runtime_state();
    let http = up::build_client(None);
    let pool = AccountManager::new(http.clone());
    {
        let p = pool.clone();
        tokio::task::spawn_blocking(move || {
            p.load_credentials();
            p.load_state();
        })
        .await
        .unwrap();
    }
    if pool.accounts().is_empty() {
        tracing::warn!(
            "No account in the store yet. Open the dashboard and add one with device login."
        );
    }
    let mut initialized = false;
    for a in pool.accounts() {
        if pool.initialize_account(&a.id).await {
            initialized = true;
            break;
        }
    }
    if !initialized {
        tracing::warn!("No account initialized at startup; they will be retried on first use");
    }
    let observations = dashboard_store::load_rate_observations(
        store::now_f64() - cfg.rate_estimate_window_seconds as f64,
    );
    pool.load_observations(observations);
    let state = Arc::new(AppState {
        pool: pool.clone(),
        transport: Arc::new(Transport {
            shared: http.clone(),
        }),
        http: http.clone(),
        started_at: store::now_f64(),
        quiesced: AtomicBool::new(quiesced),
        inflight: AtomicI64::new(0),
        drained: tokio::sync::Notify::new(),
    });
    if !quiesced {
        let s = state.clone();
        tokio::spawn(async move {
            d::refresh_all_usage(&s).await;
        });
    }
    spawn_background(state.clone());
    let addr: SocketAddr = format!("{host}:{port}")
        .parse()
        .unwrap_or_else(|_| SocketAddr::from(([0, 0, 0, 0], port)));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .unwrap_or_else(|e| {
            tracing::error!("Cannot bind {addr}: {e}");
            std::process::exit(1);
        });
    tracing::info!(
        "kiro-lb {} listening on http://{addr} with {} account(s){}",
        config::APP_VERSION,
        pool.accounts().len(),
        if quiesced {
            " (quiesced: not the active writer)"
        } else {
            ""
        }
    );
    let app = router(state.clone()).into_make_service_with_connect_info::<SocketAddr>();
    let _ = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown())
        .await;
    tracing::info!("Shutting down: final flush");
    let p = pool.clone();
    let _ = tokio::task::spawn_blocking(move || {
        let rows = p.drain_unsaved_observations();
        dashboard_store::record_rate_observations(&rows);
        dashboard_store::flush_key_model_usage();
        p.save_state();
    })
    .await;
}

async fn shutdown() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = term => {},
    }
}

fn print_banner(addr: &SocketAddr) {
    let host = if addr.ip().is_unspecified() {
        "127.0.0.1".to_owned()
    } else {
        addr.ip().to_string()
    };
    let base = format!("http://{host}:{}", addr.port());
    let rust = "\x1b[38;2;222;165;132m";
    let ghost = "\x1b[38;2;200;160;255m";
    let (bold, dim, green, cyan, reset) = ("\x1b[1m", "\x1b[2m", "\x1b[32m", "\x1b[36m", "\x1b[0m");
    let art = [
        "\u{2800}\u{2800}\u{2800}\u{2800}\u{2800}\u{2880}\u{28F4}\u{28FF}\u{28FF}\u{28FF}\u{28E6}\u{2800}",
        "\u{2800}\u{2800}\u{2800}\u{2800}\u{28F0}\u{28FF}\u{285F}\u{28BB}\u{28FF}\u{285F}\u{28BB}\u{28E7}",
        "\u{2800}\u{2800}\u{2800}\u{28F0}\u{28FF}\u{28FF}\u{28C7}\u{28F8}\u{28FF}\u{28C7}\u{28F8}\u{28FF}",
        "\u{2800}\u{2800}\u{28F4}\u{28FF}\u{28FF}\u{28FF}\u{28FF}\u{28FF}\u{28FF}\u{28FF}\u{28FF}\u{28FF}",
        "\u{28E0}\u{28FE}\u{28FF}\u{28FF}\u{28FF}\u{28FF}\u{28FF}\u{28FF}\u{28FF}\u{28FF}\u{28FF}\u{2807}",
        "\u{28BF}\u{287F}\u{28BF}\u{28FF}\u{28FF}\u{28FF}\u{28FF}\u{28FF}\u{28FF}\u{28FF}\u{287F}\u{2800}",
        "\u{2800}\u{2800}\u{2808}\u{283F}\u{283F}\u{280B}\u{2819}\u{28BF}\u{28FF}\u{287F}\u{2801}\u{2800}",
    ];
    let text = [
        String::new(),
        format!(
            "{bold}kiro-lb v{}{reset} {rust}{bold}[Rust Version]{reset}",
            config::APP_VERSION
        ),
        format!("Server running at: {ghost}{base}{reset}"),
        String::new(),
        String::new(),
        format!("API Docs:      {ghost}{base}/docs{reset}"),
        format!("Health Check:  {ghost}{base}/health{reset}"),
    ];
    let wide: Vec<String> = art.iter().map(|r| r.to_string()).collect();
    let art_width = wide[0].chars().count();
    let rule =
        "\u{2500}".repeat(art_width + 6 + "Health Check:  ".len() + base.len() + "/health".len());
    println!();
    for (a, t) in wide.iter().zip(text.iter()) {
        println!("  {ghost}{a}{reset}      {t}");
    }
    println!();
    println!("  {dim}{rule}{reset}");
    println!("  \u{1F4AC} Found a bug? Need help? Have questions?");
    println!("  {green}\u{279C}{reset}  {cyan}https://github.com/minpeter/kiro-lb/issues{reset}");
    println!("  {dim}{rule}{reset}");
    println!();
}
