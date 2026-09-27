//! Safe, reversible client configuration for Codex CLI and Claude Code.

use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;
use uuid::Uuid;

const DEFAULT_BASE_URL: &str = "http://127.0.0.1:8000";
const DEFAULT_MODEL: &str = "claude-sonnet-4.6";
const DEFAULT_KEY_ENV: &str = "KIROLB_API_KEY";

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

#[derive(Default, Deserialize, Serialize)]
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
    recoverable_sha256: Vec<String>,
}

struct PlannedWrite {
    client: ClientKind,
    content: String,
    snapshot: Snapshot,
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
         setup and diagnose only call /health and /v1/models; they do not run inference."
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
    match url.path().trim_end_matches('/') {
        "" => url.set_path("/"),
        "/v1" => url.set_path("/"),
        _ => return Err("base URL path must be empty, /, or /v1".into()),
    }
    Ok(url)
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
    let key = read_key(&options)?;
    run_diagnostic(&options.base_url, &key)?;
    let _lock = StateLock::acquire()?;
    let mut state = load_state()?;
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
    save_state(&state)?;
    for plan in plans {
        atomic_write(&plan.snapshot.path, plan.content.as_bytes())?;
    }
    for client in &options.clients {
        if let Some(snapshot) = state.clients.get_mut(client.id()) {
            snapshot.recoverable_sha256.clear();
        }
    }
    save_state(&state)?;
    println!("Client configuration installed. No inference request was made.");
    println!("Run `kirolb client status` to inspect it or `kirolb client restore` to undo it.");
    if options.clients.contains(&ClientKind::Codex) {
        println!(
            "Start Codex with `codex --profile kirolb` and {} set in its environment.",
            options.key_env
        );
    }
    if options.clients.contains(&ClientKind::Claude) {
        println!("Start Claude Code with `claude`; use `/status` to confirm routing.");
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
        let http = Client::builder()
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| format!("cannot create diagnostic client: {e}"))?;
        let health = base_url
            .join("health")
            .map_err(|_| "cannot construct health URL".to_owned())?;
        let response = http
            .get(health)
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
    reject_symlink_parent(&path)?;
    reject_symlink(&path)?;
    let current = read_optional(&path)?;
    let previous = state.clients.get(client.id());
    let (original, original_mode, mut recoverable_sha256) = if let Some(snapshot) = previous {
        let current_hash = current.as_deref().map(hash);
        if snapshot.path != path || !snapshot_accepts(snapshot, current_hash.as_deref()) {
            return Err(format!(
                "{} configuration changed after setup; restore or reconcile it manually before setup",
                client.id()
            ));
        }
        let mut hashes = snapshot.recoverable_sha256.clone();
        hashes.push(snapshot.installed_sha256.clone());
        (snapshot.original.clone(), snapshot.original_mode, hashes)
    } else {
        (current, file_mode(&path)?, Vec::new())
    };
    let content = match client {
        ClientKind::Codex => codex_config(options),
        ClientKind::Claude => claude_config(original.as_deref(), options, key)?,
    };
    recoverable_sha256.sort();
    recoverable_sha256.dedup();
    Ok(PlannedWrite {
        client,
        snapshot: Snapshot {
            path,
            original,
            original_mode,
            installed_sha256: hash(&content),
            recoverable_sha256,
        },
        content,
    })
}

fn codex_config(options: &Options) -> String {
    let base = options.base_url.join("v1").expect("validated base URL");
    format!(
        "# Generated by kirolb client setup. Use: codex --profile kirolb\n\
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
    env.insert(
        "ANTHROPIC_BASE_URL".into(),
        Value::String(options.base_url.as_str().trim_end_matches('/').to_owned()),
    );
    env.insert("ANTHROPIC_AUTH_TOKEN".into(), Value::String(key.to_owned()));
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
                let state = match current_hash.as_deref() {
                    Some(value) if value == snapshot.installed_sha256 => "installed",
                    value if snapshot_accepts(snapshot, value) => "recovery pending",
                    _ => "modified (restore blocked)",
                };
                println!("{}: {state} at {}", client.id(), snapshot.path.display());
            }
        }
    }
    Ok(())
}

fn restore(clients: Vec<ClientKind>) -> Result<(), String> {
    let _lock = StateLock::acquire()?;
    let mut state = load_state()?;
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
        reject_symlink_parent(&snapshot.path)?;
        reject_symlink(&snapshot.path)?;
        let current = read_optional(&snapshot.path)?;
        let current_hash = current.as_deref().map(hash);
        if !snapshot_accepts(snapshot, current_hash.as_deref()) {
            return Err(format!(
                "{} configuration changed after setup; refusing destructive restore",
                client.id()
            ));
        }
    }
    for client in clients {
        let Some(snapshot) = state.clients.remove(client.id()) else {
            println!("{}: nothing to restore", client.id());
            continue;
        };
        match snapshot.original {
            Some(content) => {
                atomic_write(&snapshot.path, content.as_bytes())?;
                set_file_mode(&snapshot.path, snapshot.original_mode)?;
            }
            None if snapshot.path.exists() => fs::remove_file(&snapshot.path)
                .map_err(|e| format!("cannot remove {}: {e}", snapshot.path.display()))?,
            None => {}
        }
        println!("{}: restored", client.id());
    }
    save_state(&state)
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
    let home = std::env::var_os("HOME").ok_or_else(|| "HOME is not set".to_owned())?;
    let home = PathBuf::from(home);
    if !home.is_absolute() {
        return Err("HOME must be an absolute path".into());
    }
    Ok(home.join(fallback))
}

fn state_path() -> Result<PathBuf, String> {
    Ok(env_dir("KIROLB_CLIENT_STATE_DIR", ".kirolb")?.join("client-setup.json"))
}

fn load_state() -> Result<State, String> {
    let path = state_path()?;
    reject_symlink(&path)?;
    let Some(content) = read_optional(&path)? else {
        return Ok(State {
            version: 1,
            clients: BTreeMap::new(),
        });
    };
    let state: State = serde_json::from_str(&content)
        .map_err(|_| "client setup restoration state is malformed".to_owned())?;
    if state.version != 1 {
        return Err("client setup restoration state has an unsupported version".into());
    }
    Ok(state)
}

fn save_state(state: &State) -> Result<(), String> {
    let path = state_path()?;
    if state.clients.is_empty() {
        if path.exists() {
            fs::remove_file(&path).map_err(|e| format!("cannot remove {}: {e}", path.display()))?;
        }
        return Ok(());
    }
    let mut content = serde_json::to_vec_pretty(state)
        .map_err(|e| format!("cannot serialize restoration state: {e}"))?;
    content.push(b'\n');
    atomic_write(&path, &content)
}

fn read_optional(path: &Path) -> Result<Option<String>, String> {
    match fs::read_to_string(path) {
        Ok(content) => Ok(Some(content)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("cannot read {}: {error}", path.display())),
    }
}

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

fn snapshot_accepts(snapshot: &Snapshot, current_hash: Option<&str>) -> bool {
    match current_hash {
        Some(value) => {
            value == snapshot.installed_sha256
                || snapshot.recoverable_sha256.iter().any(|hash| hash == value)
                || snapshot
                    .original
                    .as_deref()
                    .is_some_and(|original| hash(original) == value)
        }
        None => snapshot.original.is_none(),
    }
}

fn atomic_write(path: &Path, content: &[u8]) -> Result<(), String> {
    reject_symlink(path)?;
    let parent = path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", path.display()))?;
    if !parent.exists() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        set_dir_private(parent)?;
    }
    let temporary = parent.join(format!(".kirolb-{}.tmp", Uuid::new_v4()));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temporary)
            .map_err(|e| format!("cannot create temporary file: {e}"))?;
        file.write_all(content)
            .and_then(|_| file.sync_all())
            .map_err(|e| format!("cannot write temporary file: {e}"))?;
        replace_atomic(&temporary, path)?;
        sync_directory(parent)?;
        set_file_private(path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<(), String> {
    fs::File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|e| format!("cannot sync {}: {e}", path.display()))
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(not(windows))]
fn replace_atomic(temporary: &Path, path: &Path) -> Result<(), String> {
    fs::rename(temporary, path)
        .map_err(|e| format!("cannot atomically replace {}: {e}", path.display()))
}

#[cfg(windows)]
fn replace_atomic(temporary: &Path, path: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let from: Vec<u16> = temporary.as_os_str().encode_wide().chain(Some(0)).collect();
    let to: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    // MoveFileExW replaces an existing destination without an unlink window.
    let result = unsafe {
        MoveFileExW(
            from.as_ptr(),
            to.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        Err(format!(
            "cannot atomically replace {}: {}",
            path.display(),
            io::Error::last_os_error()
        ))
    } else {
        Ok(())
    }
}

#[cfg(unix)]
fn set_dir_private(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|e| format!("cannot secure {}: {e}", path.display()))
}

#[cfg(not(unix))]
fn set_dir_private(_path: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(unix)]
fn set_file_private(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .map_err(|e| format!("cannot secure {}: {e}", path.display()))
}

#[cfg(unix)]
fn file_mode(path: &Path) -> Result<Option<u32>, String> {
    use std::os::unix::fs::MetadataExt;
    match fs::metadata(path) {
        Ok(metadata) => Ok(Some(metadata.mode() & 0o7777)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!(
            "cannot inspect permissions on {}: {error}",
            path.display()
        )),
    }
}

#[cfg(not(unix))]
fn file_mode(_path: &Path) -> Result<Option<u32>, String> {
    Ok(None)
}

#[cfg(unix)]
fn set_file_mode(path: &Path, mode: Option<u32>) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    if let Some(mode) = mode {
        fs::set_permissions(path, fs::Permissions::from_mode(mode))
            .map_err(|e| format!("cannot restore permissions on {}: {e}", path.display()))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn set_file_mode(_path: &Path, _mode: Option<u32>) -> Result<(), String> {
    Ok(())
}

struct StateLock {
    file: fs::File,
}

impl StateLock {
    fn acquire() -> Result<Self, String> {
        let path = state_path()?.with_extension("lock");
        let parent = path.parent().expect("state lock parent");
        if !parent.exists() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
            set_dir_private(parent)?;
        }
        reject_symlink_parent(&path)?;
        reject_symlink(&path)?;
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options
            .open(&path)
            .map_err(|e| format!("cannot open client setup lock: {e}"))?;
        file.lock()
            .map_err(|e| format!("cannot lock client setup state: {e}"))?;
        Ok(Self { file })
    }
}

impl Drop for StateLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

#[cfg(not(unix))]
fn set_file_private(_path: &Path) -> Result<(), String> {
    Ok(())
}

fn hash(content: &str) -> String {
    hex::encode(Sha256::digest(content.as_bytes()))
}
