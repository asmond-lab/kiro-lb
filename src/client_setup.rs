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
    expected: ExpectedFile,
}

#[derive(Clone)]
struct ExpectedFile {
    content: Option<String>,
    #[cfg(unix)]
    parent_identity: FileIdentity,
    #[cfg(unix)]
    file_identity: Option<FileIdentity>,
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
        conditional_write(
            &plan.snapshot.path,
            plan.content.as_bytes(),
            &plan.expected,
            None,
        )?;
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
            "Start Codex 0.134.0 or later with `codex --profile kirolb` and {} set in its environment.",
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
            .redirect(reqwest::redirect::Policy::none())
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
        if !snapshot_accepts(snapshot, current_hash.as_deref()) {
            return Err(format!(
                "{} configuration changed after setup; refusing destructive restore",
                client.id()
            ));
        }
        plans.push((*client, snapshot.clone(), expected));
    }
    for client in clients {
        let Some((_, snapshot, expected)) = plans
            .iter()
            .find(|(planned, _, _)| *planned == client)
            .cloned()
        else {
            println!("{}: nothing to restore", client.id());
            continue;
        };
        match snapshot.original {
            Some(content) => {
                conditional_write(
                    &snapshot.path,
                    content.as_bytes(),
                    &expected,
                    snapshot.original_mode,
                )?;
            }
            None if expected.content.is_some() => conditional_remove(&snapshot.path, &expected)?,
            None => {}
        }
        state.clients.remove(client.id());
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
    let home =
        crate::store::home_dir().ok_or_else(|| "user home directory is not set".to_owned())?;
    if !home.is_absolute() {
        return Err("user home directory must be an absolute path".into());
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
            content: inspected.as_ref().map(|(content, _)| content.clone()),
            parent_identity: FileIdentity {
                device: parent_metadata.dev(),
                inode: parent_metadata.ino(),
            },
            file_identity: inspected.map(|(_, identity)| identity),
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
fn read_at(parent: &fs::File, path: &Path) -> Result<Option<(String, FileIdentity)>, String> {
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::fs::MetadataExt;

    let name = leaf_name(path)?;
    let descriptor = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
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
    let mut file = unsafe { fs::File::from_raw_fd(descriptor) };
    let metadata = file
        .metadata()
        .map_err(|e| format!("cannot inspect {}: {e}", path.display()))?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(format!("{} is not a regular file", path.display()));
    }
    let mut content = String::new();
    file.read_to_string(&mut content)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    Ok(Some((
        content,
        FileIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        },
    )))
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
fn expected_matches(actual: &Option<(String, FileIdentity)>, expected: &ExpectedFile) -> bool {
    match (actual, expected.content.as_deref(), expected.file_identity) {
        (None, None, None) => true,
        (Some((actual_content, actual_identity)), Some(content), Some(identity)) => {
            actual_content == content && *actual_identity == identity
        }
        _ => false,
    }
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
        libc::renameat2(
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
) -> Result<(std::ffi::CString, FileIdentity), String> {
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

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
        name,
        FileIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        },
    ))
}

#[cfg(target_os = "linux")]
fn conditional_write(
    path: &Path,
    content: &[u8],
    expected: &ExpectedFile,
    mode: Option<u32>,
) -> Result<(), String> {
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
    let (temporary, staged_identity) = create_staged(&parent, path, content, mode)?;
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
                let matches =
                    old.as_ref()
                        .ok()
                        .and_then(Option::as_ref)
                        .is_some_and(|(actual, identity)| {
                            actual == expected_content && Some(*identity) == expected.file_identity
                        });
                if matches {
                    Ok(())
                } else {
                    let installed = read_at(&parent, path);
                    let can_rollback = installed
                        .as_ref()
                        .ok()
                        .and_then(Option::as_ref)
                        .is_some_and(|(actual, identity)| {
                            actual.as_bytes() == content && *identity == staged_identity
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
    result
}

#[cfg(target_os = "linux")]
fn conditional_remove(path: &Path, expected: &ExpectedFile) -> Result<(), String> {
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
    let matches = moved
        .as_ref()
        .ok()
        .and_then(Option::as_ref)
        .is_some_and(|(actual, identity)| {
            expected.content.as_deref() == Some(actual) && expected.file_identity == Some(*identity)
        });
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
        .map_err(|e| format!("cannot sync parent of {}: {e}", path.display()))
}

#[cfg(not(target_os = "linux"))]
fn conditional_write(
    path: &Path,
    content: &[u8],
    expected: &ExpectedFile,
    mode: Option<u32>,
) -> Result<(), String> {
    let current = inspect_expected(path, false)?;
    if current.content != expected.content {
        return Err(format!(
            "{} changed after planning; refusing update",
            path.display()
        ));
    }
    #[cfg(unix)]
    if current.parent_identity != expected.parent_identity
        || current.file_identity != expected.file_identity
    {
        return Err(format!(
            "{} identity changed after planning; refusing update",
            path.display()
        ));
    }
    atomic_write(path, content)?;
    set_file_mode(path, mode)
}

#[cfg(not(target_os = "linux"))]
fn conditional_remove(path: &Path, expected: &ExpectedFile) -> Result<(), String> {
    let current = inspect_expected(path, false)?;
    if current.content != expected.content {
        return Err(format!(
            "{} changed after planning; refusing removal",
            path.display()
        ));
    }
    #[cfg(unix)]
    if current.parent_identity != expected.parent_identity
        || current.file_identity != expected.file_identity
    {
        return Err(format!(
            "{} identity changed after planning; refusing removal",
            path.display()
        ));
    }
    fs::remove_file(path).map_err(|e| format!("cannot remove {}: {e}", path.display()))
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

#[cfg(all(unix, not(target_os = "linux")))]
fn set_file_mode(path: &Path, mode: Option<u32>) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    if let Some(mode) = mode {
        fs::set_permissions(path, fs::Permissions::from_mode(mode))
            .map_err(|e| format!("cannot restore permissions on {}: {e}", path.display()))?;
    }
    Ok(())
}

#[cfg(all(not(unix), not(target_os = "linux")))]
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
