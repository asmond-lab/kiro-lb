//! Safe, reversible client configuration for Codex CLI and Claude Code.

use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;
use uuid::Uuid;

const DEFAULT_BASE_URL: &str = "http://127.0.0.1:8000";
const DEFAULT_MODEL: &str = "claude-sonnet-4.6";
const DEFAULT_KEY_ENV: &str = "KIROLB_API_KEY";
const MAX_CLIENT_FILE_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum ClientKind {
    Codex,
    Claude,
}

impl ClientKind {
    fn id(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
        }
    }
}

#[derive(Clone, Default, Deserialize, Serialize)]
struct State {
    version: u8,
    clients: BTreeMap<String, Snapshot>,
}

#[derive(Clone, Deserialize, Serialize)]
struct Snapshot {
    path: PathBuf,
    original: Option<String>,
    #[serde(default)]
    original_mode: Option<u32>,
    installed_sha256: String,
    #[serde(default)]
    installed_mode: Option<u32>,
    #[serde(default)]
    recoverable_sha256: Vec<String>,
}

struct PlannedWrite {
    client: ClientKind,
    content: String,
    snapshot: Snapshot,
    expected: ExpectedFile,
}

struct AppliedChange {
    path: PathBuf,
    before: ExpectedFile,
    after: ExpectedFile,
}

#[derive(Clone, Debug)]
struct ExpectedFile {
    content: Option<String>,
    #[cfg(unix)]
    parent_identity: FileIdentity,
    #[cfg(unix)]
    file_identity: Option<FileIdentity>,
    file_mode: Option<u32>,
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileIdentity {
    device: u64,
    inode: u64,
}

struct Options {
    clients: Vec<ClientKind>,
    base_url: Url,
    model: String,
    key_env: String,
    key_stdin: bool,
}

pub fn cli(args: &[String]) -> i32 {
    match run(args) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("Client setup failed: {error}");
            1
        }
    }
}

fn run(args: &[String]) -> Result<(), String> {
    let Some(command) = args.first().map(String::as_str) else {
        print_help();
        return Err("missing client command".into());
    };
    if matches!(command, "-h" | "--help" | "help") {
        print_help();
        return Ok(());
    }
    match command {
        "setup" => setup(parse_options(&args[1..])?),
        "diagnose" => diagnose(parse_options(&args[1..])?),
        "status" => status(parse_clients_only(&args[1..])?),
        "restore" => restore(parse_clients_only(&args[1..])?),
        _ => Err(format!("unknown client command {command:?}")),
    }
}

fn print_help() {
    println!(
        "kirolb client setup [codex|claude|all] [--base-url URL] [--model MODEL] [--api-key-env NAME|--api-key-stdin]\n\
         kirolb client diagnose [--base-url URL] [--api-key-env NAME|--api-key-stdin]\n\
         kirolb client status [codex|claude|all]\n\
         kirolb client restore [codex|claude|all]\n\n\
         setup and diagnose only call /health and /v1/models; they do not run inference.\n\
         Automatic setup and restore are supported on Linux only."
    );
}

fn parse_options(args: &[String]) -> Result<Options, String> {
    let mut clients = None;
    let mut base_url = DEFAULT_BASE_URL.to_owned();
    let mut model = DEFAULT_MODEL.to_owned();
    let mut key_env = DEFAULT_KEY_ENV.to_owned();
    let mut key_stdin = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "codex" | "claude" | "all" if clients.is_none() => {
                clients = Some(parse_client(&args[index])?);
            }
            "--base-url" | "--model" | "--api-key-env" => {
                let flag = args[index].as_str();
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| format!("{flag} requires a value"))?;
                match flag {
                    "--base-url" => base_url = value.clone(),
                    "--model" => model = value.clone(),
                    _ => key_env = value.clone(),
                }
                index += 1;
            }
            "--api-key-stdin" if !key_stdin => key_stdin = true,
            other => return Err(format!("unknown or repeated argument {other:?}")),
        }
        index += 1;
    }
    if key_stdin && key_env != DEFAULT_KEY_ENV {
        return Err("--api-key-stdin and --api-key-env cannot be combined".into());
    }
    if model.trim().is_empty() || model.chars().any(char::is_control) {
        return Err("model must be a non-empty single-line value".into());
    }
    if key_env.is_empty()
        || !key_env
            .chars()
            .all(|c| c == '_' || c.is_ascii_alphanumeric())
    {
        return Err("API key environment variable name is invalid".into());
    }
    Ok(Options {
        clients: clients.unwrap_or_else(all_clients),
        base_url: normalize_base_url(&base_url)?,
        model,
        key_env,
        key_stdin,
    })
}

fn parse_clients_only(args: &[String]) -> Result<Vec<ClientKind>, String> {
    match args {
        [] => Ok(all_clients()),
        [client] => parse_client(client),
        _ => Err("expected at most one of codex, claude, or all".into()),
    }
}

fn parse_client(value: &str) -> Result<Vec<ClientKind>, String> {
    match value {
        "codex" => Ok(vec![ClientKind::Codex]),
        "claude" => Ok(vec![ClientKind::Claude]),
        "all" => Ok(all_clients()),
        _ => Err(format!("unknown client {value:?}")),
    }
}

fn all_clients() -> Vec<ClientKind> {
    vec![ClientKind::Codex, ClientKind::Claude]
}

fn normalize_base_url(raw: &str) -> Result<Url, String> {
    let mut url = Url::parse(raw).map_err(|_| "base URL is not a valid URL".to_owned())?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(
            "base URL must be an HTTP(S) origin without credentials, query, or fragment".into(),
        );
    }
    if url.scheme() == "http"
        && !url
            .host_str()
            .is_some_and(|host| host.eq_ignore_ascii_case("localhost") || is_loopback(host))
    {
        return Err(
            "base URL must use HTTPS unless its host is localhost or a loopback address".into(),
        );
    }
    match url.path().trim_end_matches('/') {
        "" => url.set_path("/"),
        "/v1" => url.set_path("/"),
        _ => return Err("base URL path must be empty, /, or /v1".into()),
    }
    Ok(url)
}

fn is_loopback(host: &str) -> bool {
    host.trim_matches(['[', ']'])
        .parse::<std::net::IpAddr>()
        .is_ok_and(|address| address.is_loopback())
}

fn read_key(options: &Options) -> Result<String, String> {
    let key = if options.key_stdin {
        let mut value = String::new();
        io::stdin()
            .read_to_string(&mut value)
            .map_err(|e| format!("cannot read API key from stdin: {e}"))?;
        value.trim_end_matches(['\r', '\n']).to_owned()
    } else {
        std::env::var(&options.key_env).map_err(|_| format!("{} is not set", options.key_env))?
    };
    if key.is_empty() || key.contains(['\r', '\n']) {
        return Err("API key must be a non-empty single-line value".into());
    }
    Ok(key)
}

fn setup(options: Options) -> Result<(), String> {
    require_mutation_platform()?;
    let key = read_key(&options)?;
    run_diagnostic(&options.base_url, &key)?;
    let lock = StateLock::acquire()?;
    let mut state = load_state_locked(&lock)?;
    let plans: Vec<PlannedWrite> = options
        .clients
        .iter()
        .map(|client| plan_install(*client, &options, &key, &state))
        .collect::<Result<_, _>>()?;
    for plan in &plans {
        state
            .clients
            .insert(plan.client.id().to_owned(), plan.snapshot.clone());
    }
    // The private state file is a recovery journal. If the process stops after
    // this point, restore accepts either the old bytes or the staged bytes.
    save_state_locked(&lock, &state)?;
    commit_setup_plans(&lock, &plans, &mut state, |_| {}, |_, _| {})?;
    println!("Client configuration installed. No inference request was made.");
    println!("Run `kirolb client status` to inspect it or `kirolb client restore` to undo it.");
    if options.clients.contains(&ClientKind::Codex) {
        println!(
            "Start Codex 0.134.0 or later with `codex --profile kirolb` and {} set in its environment.",
            options.key_env
        );
    }
    if options.clients.contains(&ClientKind::Claude) {
        println!("Start Claude Code with `claude`; use `/status` to confirm routing.");
    }
    Ok(())
}

fn commit_setup_plans(
    lock: &StateLock,
    plans: &[PlannedWrite],
    state: &mut State,
    mut before_commit: impl FnMut(usize),
    mut after_commit: impl FnMut(usize, &Path),
) -> Result<(), String> {
    let recovery_state = state.clone();
    let mut applied = Vec::new();
    for (index, plan) in plans.iter().enumerate() {
        if let Err(error) = lock.verify_directory_path() {
            return abort_transaction(lock, &recovery_state, &applied, error);
        }
        before_commit(index);
        let installed = match conditional_write(
            &plan.snapshot.path,
            plan.content.as_bytes(),
            &plan.expected,
            Some(0o600),
        ) {
            Ok(installed) => installed,
            Err(error) => {
                return abort_transaction(lock, &recovery_state, &applied, error);
            }
        };
        applied.push(AppliedChange {
            path: plan.snapshot.path.clone(),
            before: plan.expected.clone(),
            after: installed,
        });
        after_commit(index, &plan.snapshot.path);
        if let Err(error) = lock.verify_directory_path() {
            return abort_transaction(lock, &recovery_state, &applied, error);
        }
    }
    for plan in plans {
        if let Some(snapshot) = state.clients.get_mut(plan.client.id()) {
            snapshot.recoverable_sha256.clear();
        }
    }
    if let Err(error) = save_state_locked(lock, state) {
        return abort_transaction(lock, &recovery_state, &applied, error);
    }
    Ok(())
}

fn diagnose(options: Options) -> Result<(), String> {
    let key = read_key(&options)?;
    run_diagnostic(&options.base_url, &key)
}

fn run_diagnostic(base_url: &Url, key: &str) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("cannot start diagnostic runtime: {e}"))?;
    runtime.block_on(async {
        let mut builder = Client::builder()
            .connect_timeout(Duration::from_secs(3))
            .redirect(reqwest::redirect::Policy::none());
        if base_url.scheme() == "http" {
            builder = builder.no_proxy();
        }
        let http = builder
            .build()
            .map_err(|e| format!("cannot create diagnostic client: {e}"))?;
        let health = base_url
            .join("health")
            .map_err(|_| "cannot construct health URL".to_owned())?;
        let response = http
            .get(health)
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .map_err(|e| format!("gateway health check failed: {e}"))?;
        if !response.status().is_success() {
            return Err(format!("gateway health check returned HTTP {}", response.status()));
        }
        let health_json: Value = response
            .json()
            .await
            .map_err(|_| "gateway health check returned malformed JSON".to_owned())?;
        if health_json.get("status").and_then(Value::as_str) != Some("healthy") {
            return Err("gateway health check did not report healthy status".into());
        }
        let models = base_url
            .join("v1/models")
            .map_err(|_| "cannot construct models URL".to_owned())?;
        let response = http
            .get(models)
            .bearer_auth(key)
            .timeout(Duration::from_secs(20))
            .send()
            .await
            .map_err(|e| format!("gateway model discovery failed: {e}"))?;
        if !response.status().is_success() {
            return Err(format!(
                "gateway model discovery returned HTTP {} (credential rejected or catalog unavailable)",
                response.status()
            ));
        }
        let document: Value = response
            .json()
            .await
            .map_err(|_| "gateway model discovery returned malformed JSON".to_owned())?;
        let count = document
            .get("data")
            .and_then(Value::as_array)
            .ok_or_else(|| "gateway model discovery response has no data array".to_owned())?
            .len();
        if count == 0 {
            return Err("gateway model discovery returned no models; add a serving account first".into());
        }
        println!("Gateway is healthy; authentication succeeded; discovered {count} model(s).");
        println!("Diagnostic complete. No inference request was made.");
        Ok(())
    })
}

fn plan_install(
    client: ClientKind,
    options: &Options,
    key: &str,
    state: &State,
) -> Result<PlannedWrite, String> {
    let path = client_path(client)?;
    let expected = inspect_expected(&path, true)?;
    let current = expected.content.clone();
    let previous = state.clients.get(client.id());
    let (original, original_mode, mut recoverable_sha256) = if let Some(snapshot) = previous {
        let current_hash = current.as_deref().map(hash);
        if snapshot.path != path
            || !snapshot_accepts(snapshot, current_hash.as_deref(), expected.file_mode)
        {
            return Err(format!(
                "{} configuration changed after setup; restore or reconcile it manually before setup",
                client.id()
            ));
        }
        let mut hashes = snapshot.recoverable_sha256.clone();
        hashes.push(snapshot.installed_sha256.clone());
        (snapshot.original.clone(), snapshot.original_mode, hashes)
    } else {
        (current, expected.file_mode, Vec::new())
    };
    let content = match client {
        ClientKind::Codex => codex_config(options),
        ClientKind::Claude => claude_config(original.as_deref(), options, key)?,
    };
    ensure_file_size(&path, content.len())?;
    recoverable_sha256.sort();
    recoverable_sha256.dedup();
    Ok(PlannedWrite {
        client,
        snapshot: Snapshot {
            path,
            original,
            original_mode,
            installed_sha256: hash(&content),
            installed_mode: Some(0o600),
            recoverable_sha256,
        },
        content,
        expected,
    })
}

fn codex_config(options: &Options) -> String {
    let base = options.base_url.join("v1").expect("validated base URL");
    format!(
        "# Generated by kirolb client setup for Codex 0.134.0 or later.\n\
         # Use: codex --profile kirolb\n\
         model = {}\n\
         model_provider = \"kirolb\"\n\
         web_search = \"disabled\"\n\n\
         [model_providers.kirolb]\n\
         name = \"kiro-lb\"\n\
         base_url = {}\n\
         env_key = {}\n\
         env_key_instructions = {}\n\
         wire_api = \"responses\"\n\
         requires_openai_auth = false\n\
         supports_websockets = false\n\
         supports_standalone_web_search = false\n",
        quoted(&options.model),
        quoted(base.as_str()),
        quoted(&options.key_env),
        quoted(&format!("Set {} to a kiro-lb API key", options.key_env)),
    )
}

fn quoted(value: &str) -> String {
    serde_json::to_string(value).expect("string serialization")
}

fn claude_config(original: Option<&str>, options: &Options, key: &str) -> Result<String, String> {
    let mut document: Value = match original {
        Some(content) => serde_json::from_str(content)
            .map_err(|_| "existing Claude Code settings contain malformed JSON".to_owned())?,
        None => json!({}),
    };
    let root = document
        .as_object_mut()
        .ok_or_else(|| "existing Claude Code settings must be a JSON object".to_owned())?;
    let env = root.entry("env").or_insert_with(|| json!({}));
    let env = env
        .as_object_mut()
        .ok_or_else(|| "existing Claude Code env setting must be a JSON object".to_owned())?;
    let custom_headers = effective_custom_headers(
        env.get("ANTHROPIC_CUSTOM_HEADERS"),
        std::env::var("ANTHROPIC_CUSTOM_HEADERS").ok().as_deref(),
    )?;
    env.insert(
        "ANTHROPIC_BASE_URL".into(),
        Value::String(options.base_url.as_str().trim_end_matches('/').to_owned()),
    );
    env.insert("ANTHROPIC_API_KEY".into(), Value::String(String::new()));
    env.insert("ANTHROPIC_AUTH_TOKEN".into(), Value::String(key.to_owned()));
    env.insert(
        "ANTHROPIC_CUSTOM_HEADERS".into(),
        Value::String(custom_headers),
    );
    env.insert(
        "CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY".into(),
        Value::String("1".into()),
    );
    serde_json::to_string_pretty(&document)
        .map(|mut content| {
            content.push('\n');
            content
        })
        .map_err(|e| format!("cannot serialize Claude Code settings: {e}"))
}

fn effective_custom_headers(
    configured: Option<&Value>,
    inherited: Option<&str>,
) -> Result<String, String> {
    let headers = match configured {
        Some(Value::String(value)) => value.as_str(),
        Some(_) => {
            return Err(
                "existing Claude Code ANTHROPIC_CUSTOM_HEADERS setting must be a string".into(),
            );
        }
        None => inherited.unwrap_or_default(),
    };
    Ok(headers
        .split_inclusive('\n')
        .filter(|line| {
            let line = line.trim_end_matches(['\r', '\n']);
            let Some((name, _)) = line.split_once(':') else {
                return true;
            };
            !matches!(
                name.trim().to_ascii_lowercase().as_str(),
                "authorization" | "x-api-key"
            )
        })
        .collect())
}

fn status(clients: Vec<ClientKind>) -> Result<(), String> {
    let state = load_state()?;
    for client in clients {
        match state.clients.get(client.id()) {
            None => println!("{}: not managed", client.id()),
            Some(snapshot) => {
                if snapshot.path != client_path(client)? {
                    return Err(format!(
                        "{} restoration state does not match the current configuration directory",
                        client.id()
                    ));
                }
                let current = read_optional(&snapshot.path)?;
                let current_hash = current.as_deref().map(hash);
                let current_mode = readonly_file_mode(&snapshot.path)?;
                let state = match current_hash.as_deref() {
                    Some(value) if snapshot_matches_installed(snapshot, value, current_mode) => {
                        "installed"
                    }
                    value if snapshot_accepts(snapshot, value, current_mode) => "recovery pending",
                    _ => "modified (restore blocked)",
                };
                println!("{}: {state} at {}", client.id(), snapshot.path.display());
            }
        }
    }
    Ok(())
}

fn restore(clients: Vec<ClientKind>) -> Result<(), String> {
    require_mutation_platform()?;
    let lock = StateLock::acquire()?;
    let mut state = load_state_locked(&lock)?;
    let previous_state = state.clone();
    let mut plans = Vec::new();
    for client in &clients {
        let Some(snapshot) = state.clients.get(client.id()) else {
            continue;
        };
        if snapshot.path != client_path(*client)? {
            return Err(format!(
                "{} restoration state does not match the current configuration directory",
                client.id()
            ));
        }
        let expected = inspect_expected(&snapshot.path, false)?;
        let current = expected.content.as_deref();
        let current_hash = current.map(hash);
        if !snapshot_accepts(snapshot, current_hash.as_deref(), expected.file_mode) {
            return Err(format!(
                "{} configuration changed after setup; refusing destructive restore",
                client.id()
            ));
        }
        plans.push((*client, snapshot.clone(), expected));
    }
    for client in &clients {
        if !plans.iter().any(|(planned, _, _)| planned == client) {
            println!("{}: nothing to restore", client.id());
        }
    }
    commit_restore_plans(&lock, &plans, &mut state, &previous_state, |_| {})?;
    for (client, _, _) in plans {
        println!("{}: restored", client.id());
    }
    Ok(())
}

fn commit_restore_plans(
    lock: &StateLock,
    plans: &[(ClientKind, Snapshot, ExpectedFile)],
    state: &mut State,
    previous_state: &State,
    mut before_commit: impl FnMut(usize),
) -> Result<(), String> {
    let mut applied = Vec::new();
    for (index, (client, snapshot, expected)) in plans.iter().enumerate() {
        if let Err(error) = lock.verify_directory_path() {
            return abort_transaction(lock, previous_state, &applied, error);
        }
        before_commit(index);
        match snapshot.original.as_deref() {
            Some(content) => {
                let restored = match conditional_write(
                    &snapshot.path,
                    content.as_bytes(),
                    expected,
                    snapshot.original_mode,
                ) {
                    Ok(restored) => restored,
                    Err(error) => {
                        return abort_transaction(lock, previous_state, &applied, error);
                    }
                };
                applied.push(AppliedChange {
                    path: snapshot.path.clone(),
                    before: expected.clone(),
                    after: restored,
                });
            }
            None if expected.content.is_some() => {
                let restored = match conditional_remove(&snapshot.path, expected) {
                    Ok(restored) => restored,
                    Err(error) => {
                        return abort_transaction(lock, previous_state, &applied, error);
                    }
                };
                applied.push(AppliedChange {
                    path: snapshot.path.clone(),
                    before: expected.clone(),
                    after: restored,
                });
            }
            None => {}
        }
        if let Err(error) = lock.verify_directory_path() {
            return abort_transaction(lock, previous_state, &applied, error);
        }
        state.clients.remove(client.id());
    }
    if let Err(error) = save_state_locked(lock, state) {
        return abort_transaction(lock, previous_state, &applied, error);
    }
    Ok(())
}

fn client_path(client: ClientKind) -> Result<PathBuf, String> {
    match client {
        ClientKind::Codex => Ok(env_dir("CODEX_HOME", ".codex")?.join("kirolb.config.toml")),
        ClientKind::Claude => Ok(env_dir("CLAUDE_CONFIG_DIR", ".claude")?.join("settings.json")),
    }
}

fn env_dir(variable: &str, fallback: &str) -> Result<PathBuf, String> {
    if let Some(value) = std::env::var_os(variable) {
        if value.is_empty() {
            return Err(format!("{variable} is empty"));
        }
        let path = PathBuf::from(value);
        if !path.is_absolute() {
            return Err(format!("{variable} must be an absolute path"));
        }
        return Ok(path);
    }
    let home = client_home_dir()
        .ok_or_else(|| "user home directory is not set to an absolute native path".to_owned())?;
    Ok(home.join(fallback))
}

fn client_home_dir() -> Option<PathBuf> {
    first_absolute_path([std::env::var_os("HOME"), std::env::var_os("USERPROFILE")])
}

fn first_absolute_path(
    values: impl IntoIterator<Item = Option<std::ffi::OsString>>,
) -> Option<PathBuf> {
    values
        .into_iter()
        .flatten()
        .map(PathBuf::from)
        .find(|path| path.is_absolute())
}

fn state_path() -> Result<PathBuf, String> {
    Ok(env_dir("KIROLB_CLIENT_STATE_DIR", ".kirolb")?.join("client-setup.json"))
}

fn load_state() -> Result<State, String> {
    let path = state_path()?;
    let Some(content) = read_optional(&path)? else {
        return Ok(State {
            version: 1,
            clients: BTreeMap::new(),
        });
    };
    parse_state(&content)
}

fn parse_state(content: &str) -> Result<State, String> {
    let state: State = serde_json::from_str(content)
        .map_err(|_| "client setup restoration state is malformed".to_owned())?;
    if state.version != 1 {
        return Err("client setup restoration state has an unsupported version".into());
    }
    Ok(state)
}

#[cfg(unix)]
fn read_optional(path: &Path) -> Result<Option<String>, String> {
    let parent_path = path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", path.display()))?;
    match fs::symlink_metadata(parent_path) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "cannot inspect parent of {}: {error}",
                path.display()
            ));
        }
    }
    let parent = open_parent_secure(path, false)?;
    read_at(&parent, path).map(|value| value.map(|(content, _, _)| content))
}

#[cfg(not(unix))]
fn read_optional(path: &Path) -> Result<Option<String>, String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err(format!("{} is not a regular file", path.display()));
        }
        Ok(metadata) if metadata.len() > MAX_CLIENT_FILE_BYTES => {
            return Err(format!("{} is too large", path.display()));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("cannot inspect {}: {error}", path.display())),
    }
    let file = fs::File::open(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let metadata = file
        .metadata()
        .map_err(|e| format!("cannot inspect {}: {e}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!("{} is not a regular file", path.display()));
    }
    if metadata.len() > MAX_CLIENT_FILE_BYTES {
        return Err(format!("{} is too large", path.display()));
    }
    read_bounded(file, path).map(Some)
}

#[cfg(not(unix))]
fn reject_symlink(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(format!("refusing to use symlink {}", path.display()))
        }
        Ok(metadata) if !metadata.is_file() => Err(format!("{} is not a file", path.display())),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("cannot inspect {}: {error}", path.display())),
    }
}

#[cfg(not(unix))]
fn reject_symlink_parent(path: &Path) -> Result<(), String> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    match fs::symlink_metadata(parent) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(format!(
            "refusing to use symlink directory {}",
            parent.display()
        )),
        Ok(metadata) if !metadata.is_dir() => {
            Err(format!("{} is not a directory", parent.display()))
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("cannot inspect {}: {error}", parent.display())),
    }
}

#[cfg(not(unix))]
fn set_dir_private(_path: &Path) -> Result<(), String> {
    Ok(())
}

fn snapshot_accepts(
    snapshot: &Snapshot,
    current_hash: Option<&str>,
    current_mode: Option<u32>,
) -> bool {
    match current_hash {
        Some(value) => {
            snapshot_matches_installed(snapshot, value, current_mode)
                || (snapshot.recoverable_sha256.iter().any(|hash| hash == value)
                    && snapshot_mode_matches(snapshot.installed_mode, current_mode))
                || (snapshot
                    .original
                    .as_deref()
                    .is_some_and(|original| hash(original) == value)
                    && snapshot_mode_matches(snapshot.original_mode, current_mode))
        }
        None => snapshot.original.is_none(),
    }
}

fn snapshot_matches_installed(
    snapshot: &Snapshot,
    current_hash: &str,
    current_mode: Option<u32>,
) -> bool {
    current_hash == snapshot.installed_sha256
        && snapshot_mode_matches(snapshot.installed_mode, current_mode)
}

#[cfg(unix)]
fn snapshot_mode_matches(expected: Option<u32>, current: Option<u32>) -> bool {
    expected.is_none_or(|mode| Some(mode) == current)
}

#[cfg(not(unix))]
fn snapshot_mode_matches(_expected: Option<u32>, _current: Option<u32>) -> bool {
    true
}

fn abort_transaction(
    lock: &StateLock,
    recovery_state: &State,
    applied: &[AppliedChange],
    error: String,
) -> Result<(), String> {
    let rollback = rollback_changes(applied);
    let journal = save_state_locked(lock, recovery_state)
        .or_else(|_| save_recovery_state(lock, recovery_state));
    match (rollback, journal) {
        (Ok(()), Ok(())) => Err(error),
        (rollback, journal) => {
            let mut details = Vec::new();
            if let Err(rollback_error) = rollback {
                details.push(format!("client rollback failed: {rollback_error}"));
            }
            if let Err(journal_error) = journal {
                details.push(format!("journal rollback failed: {journal_error}"));
            }
            Err(format!("{error}; {}", details.join("; ")))
        }
    }
}

fn rollback_changes(applied: &[AppliedChange]) -> Result<(), String> {
    let mut errors = Vec::new();
    for change in applied.iter().rev() {
        let result = match change.before.content.as_deref() {
            Some(content) => conditional_write(
                &change.path,
                content.as_bytes(),
                &change.after,
                change.before.file_mode,
            )
            .map(|_| ()),
            None => conditional_remove(&change.path, &change.after).map(|_| ()),
        };
        if let Err(error) = result {
            errors.push(error);
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

#[cfg(unix)]
fn readonly_file_mode(path: &Path) -> Result<Option<u32>, String> {
    use std::os::unix::fs::MetadataExt;

    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => Ok(None),
        Ok(metadata) => Ok(Some(metadata.mode() & 0o7777)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("cannot inspect {}: {error}", path.display())),
    }
}

#[cfg(not(unix))]
fn readonly_file_mode(_path: &Path) -> Result<Option<u32>, String> {
    Ok(None)
}

#[cfg(target_os = "linux")]
fn require_mutation_platform() -> Result<(), String> {
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn require_mutation_platform() -> Result<(), String> {
    Err(
        "automatic client setup and restore are supported only on Linux; diagnose and status remain available"
            .into(),
    )
}

fn inspect_expected(path: &Path, create_parent: bool) -> Result<ExpectedFile, String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        let parent = open_parent_secure(path, create_parent)?;
        let parent_metadata = parent
            .metadata()
            .map_err(|e| format!("cannot inspect {}: {e}", path.display()))?;
        let inspected = read_at(&parent, path)?;
        Ok(ExpectedFile {
            content: inspected.as_ref().map(|(content, _, _)| content.clone()),
            parent_identity: FileIdentity {
                device: parent_metadata.dev(),
                inode: parent_metadata.ino(),
            },
            file_identity: inspected.as_ref().map(|(_, identity, _)| *identity),
            file_mode: inspected.map(|(_, _, mode)| mode),
        })
    }
    #[cfg(not(unix))]
    {
        let parent = path
            .parent()
            .ok_or_else(|| format!("{} has no parent directory", path.display()))?;
        if create_parent && !parent.exists() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
            set_dir_private(parent)?;
        }
        reject_symlink_parent(path)?;
        reject_symlink(path)?;
        Ok(ExpectedFile {
            content: read_optional(path)?,
            file_mode: None,
        })
    }
}

#[cfg(unix)]
fn open_parent_secure(path: &Path, create: bool) -> Result<fs::File, String> {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;
    use std::path::Component;

    if !path.is_absolute() {
        return Err(format!("{} must be an absolute path", path.display()));
    }
    let parent = path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", path.display()))?;
    let mut directory = fs::File::open("/").map_err(|e| format!("cannot open /: {e}"))?;
    for component in parent.components() {
        let Component::Normal(name) = component else {
            if matches!(component, Component::RootDir) {
                continue;
            }
            return Err(format!("unsafe path component in {}", path.display()));
        };
        let name = CString::new(name.as_bytes())
            .map_err(|_| format!("invalid path component in {}", path.display()))?;
        let open = || unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        let mut descriptor = open();
        if descriptor < 0 && create && io::Error::last_os_error().kind() == io::ErrorKind::NotFound
        {
            let created = unsafe { libc::mkdirat(directory.as_raw_fd(), name.as_ptr(), 0o700) };
            if created < 0 && io::Error::last_os_error().kind() != io::ErrorKind::AlreadyExists {
                return Err(format!(
                    "cannot create directory component in {}: {}",
                    path.display(),
                    io::Error::last_os_error()
                ));
            }
            descriptor = open();
        }
        if descriptor < 0 {
            return Err(format!(
                "cannot safely open parent of {}: {}",
                path.display(),
                io::Error::last_os_error()
            ));
        }
        directory = unsafe { fs::File::from_raw_fd(descriptor) };
    }
    Ok(directory)
}

#[cfg(unix)]
fn leaf_name(path: &Path) -> Result<std::ffi::CString, String> {
    use std::os::unix::ffi::OsStrExt;
    let name = path
        .file_name()
        .ok_or_else(|| format!("{} has no file name", path.display()))?;
    std::ffi::CString::new(name.as_bytes())
        .map_err(|_| format!("invalid file name {}", path.display()))
}

#[cfg(unix)]
fn read_at(parent: &fs::File, path: &Path) -> Result<Option<(String, FileIdentity, u32)>, String> {
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::fs::MetadataExt;

    let name = leaf_name(path)?;
    let descriptor = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NONBLOCK | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if descriptor < 0 {
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::NotFound {
            return Ok(None);
        }
        if error.raw_os_error() == Some(libc::ELOOP) {
            return Err(format!("refusing to use symlink {}", path.display()));
        }
        return Err(format!("cannot safely read {}: {error}", path.display()));
    }
    let file = unsafe { fs::File::from_raw_fd(descriptor) };
    let metadata = file
        .metadata()
        .map_err(|e| format!("cannot inspect {}: {e}", path.display()))?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(format!("{} is not a regular file", path.display()));
    }
    if metadata.len() > MAX_CLIENT_FILE_BYTES {
        return Err(format!("{} is too large", path.display()));
    }
    let content = read_bounded(file, path)?;
    Ok(Some((
        content,
        FileIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        },
        metadata.mode() & 0o7777,
    )))
}

fn read_bounded(file: fs::File, path: &Path) -> Result<String, String> {
    let mut content = Vec::new();
    file.take(MAX_CLIENT_FILE_BYTES + 1)
        .read_to_end(&mut content)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    if content.len() as u64 > MAX_CLIENT_FILE_BYTES {
        return Err(format!("{} is too large", path.display()));
    }
    String::from_utf8(content).map_err(|_| format!("{} is not valid UTF-8", path.display()))
}

fn ensure_file_size(path: &Path, size: usize) -> Result<(), String> {
    if size as u64 > MAX_CLIENT_FILE_BYTES {
        Err(format!("{} is too large", path.display()))
    } else {
        Ok(())
    }
}

#[cfg(unix)]
fn verify_parent(parent: &fs::File, path: &Path, expected: &ExpectedFile) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;
    let metadata = parent
        .metadata()
        .map_err(|e| format!("cannot inspect parent of {}: {e}", path.display()))?;
    let actual = FileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    };
    if actual != expected.parent_identity {
        return Err(format!(
            "parent directory of {} changed after planning; refusing update",
            path.display()
        ));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn verify_parent_path(
    parent: &fs::File,
    path: &Path,
    expected: &ExpectedFile,
) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;

    verify_parent(parent, path, expected)?;
    let current = open_parent_secure(path, false)?;
    let metadata = current
        .metadata()
        .map_err(|e| format!("cannot inspect parent of {}: {e}", path.display()))?;
    let identity = FileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    };
    if identity != expected.parent_identity {
        return Err(format!(
            "parent directory of {} changed during update; refusing mutation",
            path.display()
        ));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn expected_matches(actual: &Option<(String, FileIdentity, u32)>, expected: &ExpectedFile) -> bool {
    match (
        actual,
        expected.content.as_deref(),
        expected.file_identity,
        expected.file_mode,
    ) {
        (None, None, None, None) => true,
        (
            Some((actual_content, actual_identity, actual_mode)),
            Some(content),
            Some(identity),
            Some(mode),
        ) => actual_content == content && *actual_identity == identity && *actual_mode == mode,
        _ => false,
    }
}

#[cfg(target_os = "linux")]
fn file_matches(
    actual: &Option<(String, FileIdentity, u32)>,
    content: &[u8],
    identity: FileIdentity,
    mode: u32,
) -> bool {
    actual
        .as_ref()
        .is_some_and(|(actual_content, actual_identity, actual_mode)| {
            actual_content.as_bytes() == content
                && *actual_identity == identity
                && *actual_mode == mode
        })
}

#[cfg(target_os = "linux")]
fn rename_at(
    parent: &fs::File,
    from: &std::ffi::CStr,
    to: &std::ffi::CStr,
    flags: libc::c_uint,
) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            parent.as_raw_fd(),
            from.as_ptr(),
            parent.as_raw_fd(),
            to.as_ptr(),
            flags,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(target_os = "linux")]
fn create_staged(
    parent: &fs::File,
    path: &Path,
    content: &[u8],
    mode: Option<u32>,
) -> Result<(std::ffi::CString, FileIdentity, u32), String> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    ensure_file_size(path, content.len())?;
    create_staged_inner(parent, path, |file| {
        file.write_all(content)
            .map_err(|e| format!("cannot stage {}: {e}", path.display()))?;
        if let Some(mode) = mode {
            file.set_permissions(fs::Permissions::from_mode(mode))
                .map_err(|e| format!("cannot stage permissions for {}: {e}", path.display()))?;
        }
        file.sync_all()
            .map_err(|e| format!("cannot stage {}: {e}", path.display()))?;
        let metadata = file
            .metadata()
            .map_err(|e| format!("cannot inspect staged file for {}: {e}", path.display()))?;
        Ok((
            FileIdentity {
                device: metadata.dev(),
                inode: metadata.ino(),
            },
            metadata.mode() & 0o7777,
        ))
    })
}

#[cfg(target_os = "linux")]
fn create_staged_inner(
    parent: &fs::File,
    path: &Path,
    finish: impl FnOnce(&mut fs::File) -> Result<(FileIdentity, u32), String>,
) -> Result<(std::ffi::CString, FileIdentity, u32), String> {
    use std::os::fd::{AsRawFd, FromRawFd};

    let name = std::ffi::CString::new(format!(".kirolb-{}.tmp", Uuid::new_v4()))
        .expect("generated temporary name");
    let descriptor = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0o600,
        )
    };
    if descriptor < 0 {
        return Err(format!(
            "cannot stage {}: {}",
            path.display(),
            io::Error::last_os_error()
        ));
    }
    let mut file = unsafe { fs::File::from_raw_fd(descriptor) };
    match finish(&mut file) {
        Ok((identity, staged_mode)) => Ok((name, identity, staged_mode)),
        Err(error) => {
            let cleanup = unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), 0) };
            if cleanup < 0 && io::Error::last_os_error().kind() != io::ErrorKind::NotFound {
                Err(format!(
                    "{error}; cannot remove failed staged file for {}: {}",
                    path.display(),
                    io::Error::last_os_error()
                ))
            } else {
                Err(error)
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn conditional_write(
    path: &Path,
    content: &[u8],
    expected: &ExpectedFile,
    mode: Option<u32>,
) -> Result<ExpectedFile, String> {
    conditional_write_inner(path, content, expected, mode, || {})
}

#[cfg(target_os = "linux")]
fn conditional_write_inner(
    path: &Path,
    content: &[u8],
    expected: &ExpectedFile,
    mode: Option<u32>,
    after_mutation: impl FnOnce(),
) -> Result<ExpectedFile, String> {
    use std::os::fd::AsRawFd;

    let parent = open_parent_secure(path, false)?;
    verify_parent(&parent, path, expected)?;
    let target = leaf_name(path)?;
    if !expected_matches(&read_at(&parent, path)?, expected) {
        return Err(format!(
            "{} changed after planning; refusing update",
            path.display()
        ));
    }
    let (temporary, staged_identity, staged_mode) = create_staged(&parent, path, content, mode)?;
    let temporary_path = path.with_file_name(temporary.to_string_lossy().as_ref());
    let result = match expected.content.as_deref() {
        None => rename_at(&parent, &temporary, &target, libc::RENAME_NOREPLACE).map_err(|e| {
            format!(
                "{} changed after planning; refusing update: {e}",
                path.display()
            )
        }),
        Some(expected_content) => {
            if let Err(error) = rename_at(&parent, &temporary, &target, libc::RENAME_EXCHANGE) {
                Err(format!(
                    "cannot atomically update {}: {error}",
                    path.display()
                ))
            } else {
                let old = read_at(&parent, &temporary_path);
                let matches = old.as_ref().ok().and_then(Option::as_ref).is_some_and(
                    |(actual, identity, actual_mode)| {
                        actual == expected_content
                            && Some(*identity) == expected.file_identity
                            && Some(*actual_mode) == expected.file_mode
                    },
                );
                if matches {
                    Ok(())
                } else {
                    let installed = read_at(&parent, path);
                    let can_rollback = installed
                        .as_ref()
                        .ok()
                        .and_then(Option::as_ref)
                        .is_some_and(|(actual, identity, actual_mode)| {
                            actual.as_bytes() == content
                                && *identity == staged_identity
                                && *actual_mode == staged_mode
                        });
                    if can_rollback {
                        rename_at(&parent, &temporary, &target, libc::RENAME_EXCHANGE).map_err(
                        |e| {
                            format!(
                                "cannot roll back conflicted update of {}; conflicting bytes were preserved at {}: {e}",
                                path.display(), temporary_path.display()
                            )
                        },
                    )?;
                        Err(format!(
                            "{} changed after planning; refusing update",
                            path.display()
                        ))
                    } else {
                        return Err(format!(
                            "{} changed during commit; conflicting bytes were preserved at {}",
                            path.display(),
                            temporary_path.display()
                        ));
                    }
                }
            }
        }
    };
    if result.is_ok() {
        after_mutation();
        if let Err(error) = verify_parent_path(&parent, path, expected) {
            let rollback = if expected.content.is_some() {
                rename_at(&parent, &temporary, &target, libc::RENAME_EXCHANGE)
                    .map_err(|e| format!("cannot roll back update of {}: {e}", path.display()))?;
                let restored = read_at(&parent, path)?;
                let staged = read_at(&parent, &temporary_path)?;
                if expected_matches(&restored, expected)
                    && file_matches(&staged, content, staged_identity, staged_mode)
                {
                    let removed =
                        unsafe { libc::unlinkat(parent.as_raw_fd(), temporary.as_ptr(), 0) };
                    if removed == 0 {
                        Ok(())
                    } else {
                        Err(format!(
                            "cannot remove rolled-back staged file for {}: {}",
                            path.display(),
                            io::Error::last_os_error()
                        ))
                    }
                } else {
                    let _ = rename_at(&parent, &temporary, &target, libc::RENAME_EXCHANGE);
                    Err(format!(
                        "cannot roll back update of {}; a concurrent edit was preserved",
                        path.display()
                    ))
                }
            } else {
                rename_at(&parent, &target, &temporary, libc::RENAME_NOREPLACE)
                    .map_err(|e| format!("cannot roll back update of {}: {e}", path.display()))?;
                let staged = read_at(&parent, &temporary_path)?;
                if file_matches(&staged, content, staged_identity, staged_mode) {
                    let removed =
                        unsafe { libc::unlinkat(parent.as_raw_fd(), temporary.as_ptr(), 0) };
                    if removed == 0 {
                        Ok(())
                    } else {
                        Err(format!(
                            "cannot remove rolled-back staged file for {}: {}",
                            path.display(),
                            io::Error::last_os_error()
                        ))
                    }
                } else {
                    let _ = rename_at(&parent, &temporary, &target, libc::RENAME_NOREPLACE);
                    Err(format!(
                        "cannot roll back update of {}; a concurrent edit was preserved",
                        path.display()
                    ))
                }
            };
            if let Err(rollback_error) = rollback {
                return Err(format!("{error}; {rollback_error}"));
            }
            parent.sync_all().map_err(|e| {
                format!("cannot sync rolled-back parent of {}: {e}", path.display())
            })?;
            return Err(error);
        }
    }
    let unlink = unsafe { libc::unlinkat(parent.as_raw_fd(), temporary.as_ptr(), 0) };
    if unlink < 0 && io::Error::last_os_error().kind() != io::ErrorKind::NotFound {
        return Err(format!(
            "cannot remove staged file for {}: {}",
            path.display(),
            io::Error::last_os_error()
        ));
    }
    parent
        .sync_all()
        .map_err(|e| format!("cannot sync parent of {}: {e}", path.display()))?;
    result?;
    Ok(ExpectedFile {
        content: Some(
            String::from_utf8(content.to_vec())
                .map_err(|_| format!("generated content for {} is not UTF-8", path.display()))?,
        ),
        parent_identity: expected.parent_identity,
        file_identity: Some(staged_identity),
        file_mode: Some(staged_mode),
    })
}

#[cfg(target_os = "linux")]
fn conditional_remove(path: &Path, expected: &ExpectedFile) -> Result<ExpectedFile, String> {
    conditional_remove_inner(path, expected, || {})
}

#[cfg(target_os = "linux")]
fn conditional_remove_inner(
    path: &Path,
    expected: &ExpectedFile,
    after_mutation: impl FnOnce(),
) -> Result<ExpectedFile, String> {
    use std::os::fd::AsRawFd;

    let parent = open_parent_secure(path, false)?;
    verify_parent(&parent, path, expected)?;
    let target = leaf_name(path)?;
    if !expected_matches(&read_at(&parent, path)?, expected) {
        return Err(format!(
            "{} changed after planning; refusing removal",
            path.display()
        ));
    }
    let temporary = std::ffi::CString::new(format!(".kirolb-{}.tmp", Uuid::new_v4()))
        .expect("generated temporary name");
    let temporary_path = path.with_file_name(temporary.to_string_lossy().as_ref());
    rename_at(&parent, &target, &temporary, libc::RENAME_NOREPLACE).map_err(|e| {
        format!(
            "{} changed after planning; refusing removal: {e}",
            path.display()
        )
    })?;
    let moved = read_at(&parent, &temporary_path);
    let matches = moved.as_ref().ok().and_then(Option::as_ref).is_some_and(
        |(actual, identity, actual_mode)| {
            expected.content.as_deref() == Some(actual)
                && expected.file_identity == Some(*identity)
                && expected.file_mode == Some(*actual_mode)
        },
    );
    if !matches {
        if rename_at(&parent, &temporary, &target, libc::RENAME_NOREPLACE).is_err() {
            return Err(format!(
                "{} changed during removal; conflicting bytes were preserved at {}",
                path.display(),
                temporary_path.display()
            ));
        }
        return Err(format!(
            "{} changed after planning; refusing removal",
            path.display()
        ));
    }
    after_mutation();
    if let Err(error) = verify_parent_path(&parent, path, expected) {
        if let Err(rollback_error) = rename_at(&parent, &temporary, &target, libc::RENAME_NOREPLACE)
        {
            return Err(format!(
                "{error}; cannot roll back removal of {}: {rollback_error}",
                path.display()
            ));
        }
        parent
            .sync_all()
            .map_err(|e| format!("cannot sync rolled-back parent of {}: {e}", path.display()))?;
        return Err(error);
    }
    let result = unsafe { libc::unlinkat(parent.as_raw_fd(), temporary.as_ptr(), 0) };
    if result < 0 {
        return Err(format!(
            "cannot remove {}: {}",
            path.display(),
            io::Error::last_os_error()
        ));
    }
    parent
        .sync_all()
        .map_err(|e| format!("cannot sync parent of {}: {e}", path.display()))?;
    Ok(ExpectedFile {
        content: None,
        parent_identity: expected.parent_identity,
        file_identity: None,
        file_mode: None,
    })
}

#[cfg(not(target_os = "linux"))]
fn conditional_write(
    _path: &Path,
    _content: &[u8],
    _expected: &ExpectedFile,
    _mode: Option<u32>,
) -> Result<ExpectedFile, String> {
    require_mutation_platform()?;
    unreachable!()
}

#[cfg(not(target_os = "linux"))]
fn conditional_remove(_path: &Path, _expected: &ExpectedFile) -> Result<ExpectedFile, String> {
    require_mutation_platform()?;
    unreachable!()
}

#[cfg(target_os = "linux")]
struct StateLock {
    file: fs::File,
    file_identity: FileIdentity,
    directory: fs::File,
    directory_identity: FileIdentity,
    _lock_directory: fs::File,
    lock_directory_identity: FileIdentity,
    state_directory: PathBuf,
    state_file: PathBuf,
}

#[cfg(target_os = "linux")]
impl StateLock {
    fn acquire() -> Result<Self, String> {
        Self::acquire_at(state_path()?)
    }

    fn acquire_at(state_file: PathBuf) -> Result<Self, String> {
        use std::os::fd::{AsRawFd, FromRawFd};
        use std::os::unix::fs::MetadataExt;

        let state_directory = state_file
            .parent()
            .ok_or_else(|| format!("{} has no parent directory", state_file.display()))?
            .to_owned();
        let lock_directory = open_parent_secure(&state_directory, true)?;
        let lock_metadata = lock_directory
            .metadata()
            .map_err(|e| format!("cannot inspect client setup lock directory: {e}"))?;
        let lock_directory_identity = FileIdentity {
            device: lock_metadata.dev(),
            inode: lock_metadata.ino(),
        };
        let lock_name =
            std::ffi::CString::new(".kirolb-client-setup.lock").expect("static lock file name");
        let descriptor = unsafe {
            libc::openat(
                lock_directory.as_raw_fd(),
                lock_name.as_ptr(),
                libc::O_RDWR | libc::O_CREAT | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                0o600,
            )
        };
        if descriptor < 0 {
            return Err(format!(
                "cannot safely open client setup lock: {}",
                io::Error::last_os_error()
            ));
        }
        let file = unsafe { fs::File::from_raw_fd(descriptor) };
        let file_metadata = file
            .metadata()
            .map_err(|e| format!("cannot inspect client setup lock: {e}"))?;
        if !file_metadata.is_file() {
            return Err("client setup lock is not a regular file".into());
        }
        let file_identity = FileIdentity {
            device: file_metadata.dev(),
            inode: file_metadata.ino(),
        };
        file.lock()
            .map_err(|e| format!("cannot lock client setup state: {e}"))?;
        let directory = open_parent_secure(&state_file, true)?;
        let metadata = directory
            .metadata()
            .map_err(|e| format!("cannot inspect state directory: {e}"))?;
        let directory_identity = FileIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        };
        let lock = Self {
            file,
            file_identity,
            directory,
            directory_identity,
            _lock_directory: lock_directory,
            lock_directory_identity,
            state_directory,
            state_file,
        };
        lock.verify_directory_path()?;
        Ok(lock)
    }

    fn verify_directory_path(&self) -> Result<(), String> {
        use std::os::unix::fs::MetadataExt;

        self.verify_lock_directory_path()?;
        let current = open_parent_secure(&self.state_file, false)?;
        let metadata = current
            .metadata()
            .map_err(|e| format!("cannot inspect state directory: {e}"))?;
        let identity = FileIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        };
        if identity != self.directory_identity {
            return Err(
                "client setup state directory changed while locked; refusing mutation".into(),
            );
        }
        Ok(())
    }

    fn verify_lock_directory_path(&self) -> Result<(), String> {
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::MetadataExt;

        let current = open_parent_secure(&self.state_directory, false)?;
        let metadata = current
            .metadata()
            .map_err(|e| format!("cannot inspect client setup lock directory: {e}"))?;
        let identity = FileIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        };
        if identity != self.lock_directory_identity {
            return Err(
                "client setup lock directory changed while locked; refusing mutation".into(),
            );
        }
        let lock_name =
            std::ffi::CString::new(".kirolb-client-setup.lock").expect("static lock file name");
        let mut lock_stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        let result = unsafe {
            libc::fstatat(
                self._lock_directory.as_raw_fd(),
                lock_name.as_ptr(),
                lock_stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if result < 0 {
            return Err(format!(
                "client setup lock path changed while locked; refusing mutation: {}",
                io::Error::last_os_error()
            ));
        }
        let lock_stat = unsafe { lock_stat.assume_init() };
        let lock_identity = FileIdentity {
            device: lock_stat.st_dev,
            inode: lock_stat.st_ino,
        };
        if lock_stat.st_mode & libc::S_IFMT != libc::S_IFREG || lock_identity != self.file_identity
        {
            return Err("client setup lock path changed while locked; refusing mutation".into());
        }
        Ok(())
    }
}

#[cfg(target_os = "linux")]
impl Drop for StateLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

#[cfg(target_os = "linux")]
fn load_state_locked(lock: &StateLock) -> Result<State, String> {
    lock.verify_directory_path()?;
    match read_at(&lock.directory, &lock.state_file)? {
        Some((content, _, _)) => parse_state(&content),
        None => Ok(State {
            version: 1,
            clients: BTreeMap::new(),
        }),
    }
}

#[cfg(target_os = "linux")]
fn save_state_locked(lock: &StateLock, state: &State) -> Result<(), String> {
    use std::os::fd::AsRawFd;

    lock.verify_directory_path()?;
    let target = leaf_name(&lock.state_file)?;
    if state.clients.is_empty() {
        let result = unsafe { libc::unlinkat(lock.directory.as_raw_fd(), target.as_ptr(), 0) };
        if result < 0 && io::Error::last_os_error().kind() != io::ErrorKind::NotFound {
            return Err(format!(
                "cannot remove {}: {}",
                lock.state_file.display(),
                io::Error::last_os_error()
            ));
        }
    } else {
        let mut content = serde_json::to_vec_pretty(state)
            .map_err(|e| format!("cannot serialize restoration state: {e}"))?;
        content.push(b'\n');
        let (temporary, _, _) =
            create_staged(&lock.directory, &lock.state_file, &content, Some(0o600))?;
        if let Err(error) = rename_at(&lock.directory, &temporary, &target, 0) {
            let _ = unsafe { libc::unlinkat(lock.directory.as_raw_fd(), temporary.as_ptr(), 0) };
            return Err(format!(
                "cannot save {}: {error}",
                lock.state_file.display()
            ));
        }
    }
    lock.directory
        .sync_all()
        .map_err(|e| format!("cannot sync state directory: {e}"))?;
    lock.verify_directory_path()
}

#[cfg(target_os = "linux")]
fn save_recovery_state(lock: &StateLock, state: &State) -> Result<(), String> {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::MetadataExt;

    if state.clients.is_empty() {
        return Ok(());
    }
    lock.verify_lock_directory_path()?;
    let directory = open_parent_secure(&lock.state_file, false)?;
    let metadata = directory
        .metadata()
        .map_err(|e| format!("cannot inspect replacement state directory: {e}"))?;
    let identity = FileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    };
    if read_at(&directory, &lock.state_file)?.is_some() {
        return Err(format!(
            "refusing to overwrite recovery state at {}",
            lock.state_file.display()
        ));
    }
    let mut content = serde_json::to_vec_pretty(state)
        .map_err(|e| format!("cannot serialize restoration state: {e}"))?;
    content.push(b'\n');
    let target = leaf_name(&lock.state_file)?;
    let (temporary, _, _) = create_staged(&directory, &lock.state_file, &content, Some(0o600))?;
    if let Err(error) = rename_at(&directory, &temporary, &target, libc::RENAME_NOREPLACE) {
        let _ = unsafe { libc::unlinkat(directory.as_raw_fd(), temporary.as_ptr(), 0) };
        return Err(format!(
            "cannot preserve recovery state at {}: {error}",
            lock.state_file.display()
        ));
    }
    directory
        .sync_all()
        .map_err(|e| format!("cannot sync replacement state directory: {e}"))?;
    lock.verify_lock_directory_path()?;
    let current = open_parent_secure(&lock.state_file, false)?;
    let current_metadata = current
        .metadata()
        .map_err(|e| format!("cannot inspect replacement state directory: {e}"))?;
    if identity
        != (FileIdentity {
            device: current_metadata.dev(),
            inode: current_metadata.ino(),
        })
    {
        return Err("replacement state directory changed while preserving recovery state".into());
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
struct StateLock;

#[cfg(not(target_os = "linux"))]
impl StateLock {
    fn acquire() -> Result<Self, String> {
        require_mutation_platform()?;
        Ok(Self)
    }

    fn verify_directory_path(&self) -> Result<(), String> {
        require_mutation_platform()
    }
}

#[cfg(not(target_os = "linux"))]
fn load_state_locked(_lock: &StateLock) -> Result<State, String> {
    require_mutation_platform()?;
    unreachable!()
}

#[cfg(not(target_os = "linux"))]
fn save_state_locked(_lock: &StateLock, _state: &State) -> Result<(), String> {
    require_mutation_platform()
}

#[cfg(not(target_os = "linux"))]
fn save_recovery_state(_lock: &StateLock, _state: &State) -> Result<(), String> {
    require_mutation_platform()
}

fn hash(content: &str) -> String {
    hex::encode(Sha256::digest(content.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    fn temporary_directory() -> PathBuf {
        let path = std::env::temp_dir().join(format!("kirolb-client-unit-{}", Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        path
    }

    #[test]
    fn plaintext_base_urls_are_limited_to_loopback_hosts() {
        assert!(normalize_base_url("http://localhost:8000").is_ok());
        assert!(normalize_base_url("http://127.255.0.1:8000").is_ok());
        assert!(normalize_base_url("http://[::1]:8000").is_ok());
        assert!(normalize_base_url("https://gateway.example:8443").is_ok());
        assert!(normalize_base_url("http://gateway.example:8000").is_err());
        assert!(normalize_base_url("http://192.168.1.10:8000").is_err());
    }

    #[test]
    fn home_selection_skips_an_unusable_first_candidate() {
        let selected = first_absolute_path([
            Some(std::ffi::OsString::from("relative-home")),
            Some(std::env::current_dir().unwrap().into_os_string()),
        ])
        .unwrap();
        assert!(selected.is_absolute());
    }

    #[test]
    fn a_recoverable_hash_is_not_the_installed_hash() {
        let snapshot = Snapshot {
            path: PathBuf::from("unused"),
            original: None,
            original_mode: None,
            installed_sha256: "new".into(),
            installed_mode: None,
            recoverable_sha256: vec!["old".into()],
        };

        assert!(!snapshot_matches_installed(&snapshot, "old", None));
        assert!(snapshot_accepts(&snapshot, Some("old"), None));
    }

    #[test]
    fn claude_config_clears_a_competing_api_key() {
        let options = Options {
            clients: vec![ClientKind::Claude],
            base_url: Url::parse("https://gateway.example/").unwrap(),
            model: DEFAULT_MODEL.into(),
            key_env: DEFAULT_KEY_ENV.into(),
            key_stdin: false,
        };
        let generated = claude_config(
            Some(
                r#"{"env":{"ANTHROPIC_API_KEY":"old-key","ANTHROPIC_CUSTOM_HEADERS":"X-Tenant: kept\nx-api-key: stale\r\nAuthorization: Basic stale\nX-Trace: kept\n","CUSTOM":"kept"}}"#,
            ),
            &options,
            "gateway-key",
        )
        .unwrap();
        let document: Value = serde_json::from_str(&generated).unwrap();

        assert_eq!(document["env"]["ANTHROPIC_API_KEY"], "");
        assert_eq!(document["env"]["ANTHROPIC_AUTH_TOKEN"], "gateway-key");
        assert_eq!(
            document["env"]["ANTHROPIC_CUSTOM_HEADERS"],
            "X-Tenant: kept\nX-Trace: kept\n"
        );
        assert_eq!(document["env"]["CUSTOM"], "kept");
    }

    #[test]
    fn inherited_custom_headers_keep_metadata_but_drop_credentials() {
        let headers = effective_custom_headers(
            None,
            Some("X-Tenant: kept\nX-Api-Key: stale\nAuthorization: Bearer stale"),
        )
        .unwrap();

        assert_eq!(headers, "X-Tenant: kept\n");
    }

    #[cfg(windows)]
    #[test]
    fn windows_home_selection_falls_back_from_posix_home_to_userprofile() {
        let selected = first_absolute_path([
            Some(std::ffi::OsString::from("/c/Users/example")),
            Some(std::ffi::OsString::from(r"C:\Users\example")),
        ])
        .unwrap();
        assert_eq!(selected, PathBuf::from(r"C:\Users\example"));
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn automatic_mutation_fails_closed_off_linux() {
        let error = require_mutation_platform().unwrap_err();
        assert!(error.contains("supported only on Linux"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn conditional_write_refuses_a_post_plan_edit() {
        let directory = temporary_directory();
        let path = directory.join("settings.json");
        fs::write(&path, "before").unwrap();
        let expected = inspect_expected(&path, false).unwrap();
        fs::write(&path, "user edit").unwrap();

        let error = conditional_write(&path, b"generated", &expected, None).unwrap_err();

        assert!(error.contains("changed after planning"));
        assert_eq!(fs::read_to_string(&path).unwrap(), "user edit");
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn conditional_remove_refuses_a_post_plan_edit() {
        let directory = temporary_directory();
        let path = directory.join("settings.json");
        fs::write(&path, "installed").unwrap();
        let expected = inspect_expected(&path, false).unwrap();
        fs::write(&path, "user edit").unwrap();

        let error = conditional_remove(&path, &expected).unwrap_err();

        assert!(error.contains("changed after planning"));
        assert_eq!(fs::read_to_string(&path).unwrap(), "user edit");
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn conditional_write_refuses_a_post_plan_permission_edit() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let directory = temporary_directory();
        let path = directory.join("settings.json");
        fs::write(&path, "before").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let expected = inspect_expected(&path, false).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();

        let error = conditional_write(&path, b"generated", &expected, None).unwrap_err();

        assert!(error.contains("changed after planning"));
        assert_eq!(fs::read_to_string(&path).unwrap(), "before");
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o7777, 0o640);
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn conditional_remove_refuses_a_post_plan_permission_edit() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let directory = temporary_directory();
        let path = directory.join("settings.json");
        fs::write(&path, "installed").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let expected = inspect_expected(&path, false).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();

        let error = conditional_remove(&path, &expected).unwrap_err();

        assert!(error.contains("changed after planning"));
        assert_eq!(fs::read_to_string(&path).unwrap(), "installed");
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o7777, 0o640);
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn transaction_rollback_preserves_a_concurrent_client_edit() {
        let directory = temporary_directory();
        let path = directory.join("settings.json");
        fs::write(&path, "before").unwrap();
        let before = inspect_expected(&path, false).unwrap();
        let after = conditional_write(&path, b"installed", &before, Some(0o600)).unwrap();
        fs::write(&path, "concurrent edit").unwrap();

        let error = rollback_changes(&[AppliedChange {
            path: path.clone(),
            before,
            after,
        }])
        .unwrap_err();

        assert!(error.contains("changed after planning"));
        assert_eq!(fs::read_to_string(&path).unwrap(), "concurrent edit");
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn failed_staging_removes_its_private_temporary_file() {
        let directory = temporary_directory();
        let path = directory.join("settings.json");
        let parent = open_parent_secure(&path, false).unwrap();

        let error = create_staged_inner(&parent, &path, |file| {
            file.write_all(b"sensitive staged content").unwrap();
            Err("simulated staging failure".into())
        })
        .unwrap_err();

        assert_eq!(error, "simulated staging failure");
        assert!(fs::read_dir(&directory).unwrap().next().is_none());
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn read_at_rejects_a_fifo_without_waiting_for_a_writer() {
        use std::os::unix::ffi::OsStrExt;

        let directory = temporary_directory();
        let path = directory.join("settings.json");
        let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let parent = open_parent_secure(&path, false).unwrap();

        let error = read_at(&parent, &path).unwrap_err();

        assert!(error.contains("not a regular file"));
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn state_lock_refuses_mutation_after_its_path_is_replaced() {
        let directory = temporary_directory();
        let state_file = directory.join("state/client-setup.json");
        let lock_path = directory.join(".kirolb-client-setup.lock");
        let first_lock = StateLock::acquire_at(state_file.clone()).unwrap();
        fs::remove_file(&lock_path).unwrap();
        let second_lock = StateLock::acquire_at(state_file).unwrap();

        let error = first_lock.verify_directory_path().unwrap_err();

        assert!(error.contains("lock path changed while locked"));
        second_lock.verify_directory_path().unwrap();
        drop(second_lock);
        drop(first_lock);
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn state_lock_refuses_to_write_after_its_directory_is_replaced() {
        let directory = temporary_directory();
        let state_directory = directory.join("state");
        let moved_directory = directory.join("moved-state");
        let state_file = state_directory.join("client-setup.json");
        let first_lock = StateLock::acquire_at(state_file.clone()).unwrap();
        let first_state = test_state("first");
        save_state_locked(&first_lock, &first_state).unwrap();

        fs::rename(&state_directory, &moved_directory).unwrap();
        fs::create_dir(&state_directory).unwrap();
        let replacement_state = test_state("replacement");

        let error = save_state_locked(&first_lock, &test_state("stale writer")).unwrap_err();
        assert!(error.contains("state directory changed while locked"));
        assert!(!state_file.exists());
        let moved: State = serde_json::from_str(
            &fs::read_to_string(moved_directory.join("client-setup.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(moved.clients["codex"].original.as_deref(), Some("first"));

        save_recovery_state(&first_lock, &replacement_state).unwrap();
        let replacement: State =
            serde_json::from_str(&fs::read_to_string(&state_file).unwrap()).unwrap();
        assert_eq!(
            replacement.clients["codex"].original.as_deref(),
            Some("replacement")
        );
        drop(first_lock);
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn setup_rolls_back_all_clients_when_state_directory_changes_after_verification() {
        for swap_index in [0, 1] {
            let directory = temporary_directory();
            let state_directory = directory.join("state");
            let moved_directory = directory.join("moved-state");
            let state_file = state_directory.join("client-setup.json");
            let first_path = directory.join("first.conf");
            let second_path = directory.join("second.conf");
            fs::write(&first_path, "first before").unwrap();
            fs::write(&second_path, "second before").unwrap();
            let plans = vec![
                test_plan(ClientKind::Codex, first_path.clone(), "first installed"),
                test_plan(ClientKind::Claude, second_path.clone(), "second installed"),
            ];
            let previous_state = State {
                version: 1,
                clients: BTreeMap::new(),
            };
            let mut state = previous_state.clone();
            for plan in &plans {
                state
                    .clients
                    .insert(plan.client.id().to_owned(), plan.snapshot.clone());
            }
            let lock = StateLock::acquire_at(state_file.clone()).unwrap();
            save_state_locked(&lock, &state).unwrap();

            let error = commit_setup_plans(
                &lock,
                &plans,
                &mut state,
                |index| {
                    if index == swap_index {
                        fs::rename(&state_directory, &moved_directory).unwrap();
                        fs::create_dir(&state_directory).unwrap();
                    }
                },
                |_, _| {},
            )
            .unwrap_err();

            assert!(error.contains("state directory changed while locked"));
            assert_eq!(fs::read_to_string(&first_path).unwrap(), "first before");
            assert_eq!(fs::read_to_string(&second_path).unwrap(), "second before");
            let recovery: State =
                serde_json::from_str(&fs::read_to_string(&state_file).unwrap()).unwrap();
            assert_eq!(recovery.clients.len(), 2);
            assert!(moved_directory.join("client-setup.json").exists());
            drop(lock);
            fs::remove_dir_all(directory).unwrap();
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn setup_preserves_recovery_when_directory_and_client_change_during_commit() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let directory = temporary_directory();
        let state_directory = directory.join("state");
        let moved_directory = directory.join("moved-state");
        let state_file = state_directory.join("client-setup.json");
        let client_path = directory.join("client.conf");
        fs::write(&client_path, "before").unwrap();
        let plans = vec![test_plan(
            ClientKind::Codex,
            client_path.clone(),
            "installed",
        )];
        let previous_state = State {
            version: 1,
            clients: BTreeMap::new(),
        };
        let mut state = previous_state.clone();
        state
            .clients
            .insert("codex".into(), plans[0].snapshot.clone());
        let lock = StateLock::acquire_at(state_file.clone()).unwrap();
        save_state_locked(&lock, &state).unwrap();

        let error = commit_setup_plans(
            &lock,
            &plans,
            &mut state,
            |_| {
                fs::rename(&state_directory, &moved_directory).unwrap();
                fs::create_dir(&state_directory).unwrap();
            },
            |_, path| {
                fs::write(path, "concurrent edit").unwrap();
                fs::set_permissions(path, fs::Permissions::from_mode(0o640)).unwrap();
            },
        )
        .unwrap_err();

        assert!(error.contains("client rollback failed"));
        assert_eq!(fs::read_to_string(&client_path).unwrap(), "concurrent edit");
        assert_eq!(fs::metadata(&client_path).unwrap().mode() & 0o7777, 0o640);
        let recovery: State =
            serde_json::from_str(&fs::read_to_string(&state_file).unwrap()).unwrap();
        assert_eq!(
            recovery.clients["codex"].original.as_deref(),
            Some("before")
        );
        assert_eq!(
            recovery.clients["codex"].installed_sha256,
            hash("installed")
        );
        drop(lock);
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn abort_preserves_prepared_journal_for_an_unreported_committed_mutation() {
        let directory = temporary_directory();
        let state_file = directory.join("state/client-setup.json");
        let client_path = directory.join("client.conf");
        fs::write(&client_path, "before").unwrap();
        let plan = test_plan(ClientKind::Codex, client_path.clone(), "installed");
        let recovery_state = State {
            version: 1,
            clients: BTreeMap::from([("codex".into(), plan.snapshot.clone())]),
        };
        let lock = StateLock::acquire_at(state_file.clone()).unwrap();
        save_state_locked(&lock, &recovery_state).unwrap();

        conditional_write(&client_path, b"installed", &plan.expected, Some(0o600)).unwrap();
        let error = abort_transaction(
            &lock,
            &recovery_state,
            &[],
            "simulated post-commit durability failure".into(),
        )
        .unwrap_err();

        assert!(error.contains("simulated post-commit durability failure"));
        assert_eq!(fs::read_to_string(&client_path).unwrap(), "installed");
        let preserved: State =
            serde_json::from_str(&fs::read_to_string(&state_file).unwrap()).unwrap();
        assert_eq!(
            preserved.clients["codex"].original.as_deref(),
            Some("before")
        );
        assert_eq!(
            preserved.clients["codex"].installed_sha256,
            hash("installed")
        );
        let current = inspect_expected(&client_path, false).unwrap();
        assert!(snapshot_accepts(
            &preserved.clients["codex"],
            Some(&hash(current.content.as_deref().unwrap())),
            current.file_mode,
        ));
        drop(lock);
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn restore_rolls_back_all_clients_when_state_directory_changes_after_verification() {
        let directory = temporary_directory();
        let state_directory = directory.join("state");
        let moved_directory = directory.join("moved-state");
        let state_file = state_directory.join("client-setup.json");
        let first_path = directory.join("first.conf");
        let second_path = directory.join("second.conf");
        fs::write(&first_path, "first installed").unwrap();
        fs::write(&second_path, "second installed").unwrap();
        let first = test_snapshot(&first_path, Some("first before"), "first installed");
        let second = test_snapshot(&second_path, None, "second installed");
        let previous_state = State {
            version: 1,
            clients: BTreeMap::from([
                (ClientKind::Codex.id().to_owned(), first.clone()),
                (ClientKind::Claude.id().to_owned(), second.clone()),
            ]),
        };
        let mut state = previous_state.clone();
        let plans = vec![
            (
                ClientKind::Codex,
                first,
                inspect_expected(&first_path, false).unwrap(),
            ),
            (
                ClientKind::Claude,
                second,
                inspect_expected(&second_path, false).unwrap(),
            ),
        ];
        let lock = StateLock::acquire_at(state_file.clone()).unwrap();
        save_state_locked(&lock, &state).unwrap();

        let error = commit_restore_plans(&lock, &plans, &mut state, &previous_state, |index| {
            if index == 1 {
                fs::rename(&state_directory, &moved_directory).unwrap();
                fs::create_dir(&state_directory).unwrap();
            }
        })
        .unwrap_err();

        assert!(error.contains("state directory changed while locked"));
        assert_eq!(fs::read_to_string(&first_path).unwrap(), "first installed");
        assert_eq!(
            fs::read_to_string(&second_path).unwrap(),
            "second installed"
        );
        let recovery: State =
            serde_json::from_str(&fs::read_to_string(&state_file).unwrap()).unwrap();
        assert_eq!(recovery.clients.len(), 2);
        assert!(moved_directory.join("client-setup.json").exists());
        drop(lock);
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(target_os = "linux")]
    fn test_plan(client: ClientKind, path: PathBuf, installed: &str) -> PlannedWrite {
        let expected = inspect_expected(&path, false).unwrap();
        PlannedWrite {
            client,
            content: installed.to_owned(),
            snapshot: test_snapshot(&path, expected.content.as_deref(), installed),
            expected,
        }
    }

    #[cfg(target_os = "linux")]
    fn test_snapshot(path: &Path, original: Option<&str>, installed: &str) -> Snapshot {
        Snapshot {
            path: path.to_owned(),
            original: original.map(str::to_owned),
            original_mode: original.map(|_| 0o644),
            installed_sha256: hash(installed),
            installed_mode: Some(0o600),
            recoverable_sha256: Vec::new(),
        }
    }

    #[cfg(target_os = "linux")]
    fn test_state(original: &str) -> State {
        State {
            version: 1,
            clients: BTreeMap::from([(
                "codex".to_owned(),
                Snapshot {
                    path: PathBuf::from("/tmp/test-config"),
                    original: Some(original.to_owned()),
                    original_mode: Some(0o600),
                    installed_sha256: hash("installed"),
                    installed_mode: Some(0o600),
                    recoverable_sha256: Vec::new(),
                },
            )]),
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn conditional_write_preserves_a_concurrent_edit_when_parent_path_moves() {
        let directory = temporary_directory();
        let config = directory.join("config");
        let moved_config = directory.join("moved-config");
        fs::create_dir(&config).unwrap();
        let path = config.join("settings.json");
        fs::write(&path, "before").unwrap();
        let expected = inspect_expected(&path, false).unwrap();

        let error = conditional_write_inner(&path, b"generated", &expected, Some(0o600), || {
            fs::write(&path, "concurrent edit").unwrap();
            fs::rename(&config, &moved_config).unwrap();
            fs::create_dir(&config).unwrap();
            fs::write(&path, "replacement").unwrap();
        })
        .unwrap_err();

        assert!(error.contains("parent directory"));
        assert_eq!(fs::read_to_string(&path).unwrap(), "replacement");
        assert_eq!(
            fs::read_to_string(moved_config.join("settings.json")).unwrap(),
            "concurrent edit"
        );
        let preserved = fs::read_dir(&moved_config)
            .unwrap()
            .filter_map(Result::ok)
            .find(|entry| entry.file_name() != "settings.json")
            .unwrap();
        assert_eq!(fs::read_to_string(preserved.path()).unwrap(), "before");
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn conditional_write_removes_a_new_file_when_parent_path_moves() {
        let directory = temporary_directory();
        let config = directory.join("config");
        let moved_config = directory.join("moved-config");
        fs::create_dir(&config).unwrap();
        let path = config.join("settings.json");
        let expected = inspect_expected(&path, false).unwrap();

        let error = conditional_write_inner(&path, b"generated", &expected, Some(0o600), || {
            fs::rename(&config, &moved_config).unwrap();
            fs::create_dir(&config).unwrap();
            fs::write(&path, "replacement").unwrap();
        })
        .unwrap_err();

        assert!(error.contains("parent directory"));
        assert_eq!(fs::read_to_string(&path).unwrap(), "replacement");
        assert!(fs::read_dir(&moved_config).unwrap().next().is_none());
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn conditional_remove_rolls_back_when_parent_path_moves_during_commit() {
        let directory = temporary_directory();
        let config = directory.join("config");
        let moved_config = directory.join("moved-config");
        fs::create_dir(&config).unwrap();
        let path = config.join("settings.json");
        fs::write(&path, "installed").unwrap();
        let expected = inspect_expected(&path, false).unwrap();

        let error = conditional_remove_inner(&path, &expected, || {
            fs::rename(&config, &moved_config).unwrap();
            fs::create_dir(&config).unwrap();
            fs::write(&path, "replacement").unwrap();
        })
        .unwrap_err();

        assert!(error.contains("parent directory"));
        assert_eq!(fs::read_to_string(&path).unwrap(), "replacement");
        assert_eq!(
            fs::read_to_string(moved_config.join("settings.json")).unwrap(),
            "installed"
        );
        assert_eq!(fs::read_dir(&moved_config).unwrap().count(), 1);
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn conditional_write_refuses_an_ancestor_symlink_swap() {
        use std::os::unix::fs::symlink;

        let directory = temporary_directory();
        let home = directory.join("home");
        let config = home.join(".claude");
        fs::create_dir_all(&config).unwrap();
        let path = config.join("settings.json");
        fs::write(&path, "before").unwrap();
        let expected = inspect_expected(&path, false).unwrap();

        let moved_home = directory.join("moved-home");
        let attacker_home = directory.join("attacker-home");
        fs::create_dir_all(attacker_home.join(".claude")).unwrap();
        let attacker_path = attacker_home.join(".claude/settings.json");
        fs::write(&attacker_path, "do not touch").unwrap();
        fs::rename(&home, &moved_home).unwrap();
        symlink(&attacker_home, &home).unwrap();

        assert!(conditional_write(&path, b"generated", &expected, None).is_err());
        assert_eq!(fs::read_to_string(attacker_path).unwrap(), "do not touch");
        assert_eq!(
            fs::read_to_string(moved_home.join(".claude/settings.json")).unwrap(),
            "before"
        );
        fs::remove_dir_all(directory).unwrap();
    }
}
