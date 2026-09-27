use serde_json::Value;
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
#[cfg(target_os = "linux")]
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
#[cfg(target_os = "linux")]
use std::process::Child;
use std::process::{Command, Output, Stdio};
use std::thread;
#[cfg(target_os = "linux")]
use std::time::{Duration, Instant};
use uuid::Uuid;

const KEY: &str = "test-only-not-a-real-credential";

struct TestHome {
    path: PathBuf,
}

impl TestHome {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("kirolb-client-test-{}", Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self { path }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_kirolb"));
        command
            .env_clear()
            .env("HOME", &self.path)
            .env("USERPROFILE", &self.path)
            .env("KIROLB_API_KEY", KEY)
            .current_dir(&self.path);
        command
    }

    fn codex(&self) -> PathBuf {
        self.path.join(".codex/kirolb.config.toml")
    }

    fn claude(&self) -> PathBuf {
        self.path.join(".claude/settings.json")
    }

    fn state(&self) -> PathBuf {
        self.path.join(".kirolb/client-setup.json")
    }
}

impl Drop for TestHome {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[cfg(target_os = "linux")]
fn gateway(requests: usize) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    let handle = thread::spawn(move || {
        for stream in listener.incoming().take(requests) {
            let mut stream = stream.unwrap();
            let mut request = vec![0; 8192];
            let read = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..read]);
            let (status, body) = if request.starts_with("GET /health ") {
                ("200 OK", r#"{"status":"healthy"}"#)
            } else if request.starts_with("GET /v1/models ")
                && request.contains(&format!("authorization: Bearer {KEY}"))
            {
                ("200 OK", r#"{"data":[{"id":"claude-sonnet-4.6"}]}"#)
            } else {
                ("401 Unauthorized", r#"{"error":"unauthorized"}"#)
            };
            write!(
                stream,
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        }
    });
    (address, handle)
}

fn run(home: &TestHome, arguments: &[&str]) -> Output {
    home.command().args(arguments).output().unwrap()
}

#[cfg(target_os = "linux")]
fn wait_bounded(mut child: Child) -> Output {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            child
                .stdout
                .take()
                .unwrap()
                .read_to_end(&mut stdout)
                .unwrap();
            child
                .stderr
                .take()
                .unwrap()
                .read_to_end(&mut stderr)
                .unwrap();
            return Output {
                status,
                stdout,
                stderr,
            };
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("client command did not finish within three seconds");
        }
        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(target_os = "linux")]
fn run_bounded(home: &TestHome, arguments: &[&str]) -> Output {
    let mut command = home.command();
    command
        .args(arguments)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    wait_bounded(command.spawn().unwrap())
}

#[cfg(target_os = "linux")]
fn setup(home: &TestHome, base_url: &str, client: &str) -> Output {
    run(home, &["client", "setup", client, "--base-url", base_url])
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[cfg(target_os = "linux")]
#[test]
fn setup_is_repeatable_preserves_settings_and_restores_exact_bytes_and_mode() {
    let home = TestHome::new();
    fs::create_dir(home.path.join(".claude")).unwrap();
    let original = "{\n  \"theme\": \"dark\",\n  \"env\": {\"CUSTOM\": \"kept\", \"ANTHROPIC_API_KEY\": \"old-key\", \"ANTHROPIC_CUSTOM_HEADERS\": \"X-Tenant: kept\\nx-api-key: stale\"}\n}\n";
    fs::write(home.claude(), original).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(home.claude(), fs::Permissions::from_mode(0o640)).unwrap();
    }
    #[cfg(unix)]
    let original_directory_mode = {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(home.path.join(".claude"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777
    };
    let (url, server) = gateway(4);
    let first = setup(&home, &url, "all");
    assert!(first.status.success(), "{}", text(&first));
    let second = setup(&home, &url, "all");
    assert!(second.status.success(), "{}", text(&second));
    server.join().unwrap();

    let settings: Value =
        serde_json::from_str(&fs::read_to_string(home.claude()).unwrap()).unwrap();
    assert_eq!(settings["theme"], "dark");
    assert_eq!(settings["env"]["CUSTOM"], "kept");
    assert_eq!(settings["env"]["ANTHROPIC_BASE_URL"], url);
    assert_eq!(settings["env"]["ANTHROPIC_API_KEY"], "");
    assert_eq!(settings["env"]["ANTHROPIC_AUTH_TOKEN"], KEY);
    assert_eq!(
        settings["env"]["ANTHROPIC_CUSTOM_HEADERS"],
        "X-Tenant: kept\n"
    );
    let codex = fs::read_to_string(home.codex()).unwrap();
    assert!(codex.contains("wire_api = \"responses\""));
    assert!(codex.contains("env_key = \"KIROLB_API_KEY\""));
    assert!(!codex.contains(KEY));
    assert!(!text(&first).contains(KEY));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(home.claude()).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(home.state()).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(home.path.join(".claude"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            original_directory_mode
        );
    }

    let restored = run(&home, &["client", "restore", "all"]);
    assert!(restored.status.success(), "{}", text(&restored));
    assert_eq!(fs::read_to_string(home.claude()).unwrap(), original);
    assert!(!home.codex().exists());
    assert!(!home.state().exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(home.claude()).unwrap().permissions().mode() & 0o777,
            0o640
        );
    }
    let again = run(&home, &["client", "restore", "all"]);
    assert!(again.status.success(), "{}", text(&again));
}

#[cfg(target_os = "linux")]
#[test]
fn restore_distinguishes_a_preexisting_empty_file_from_an_absent_file() {
    let home = TestHome::new();
    fs::create_dir(home.path.join(".codex")).unwrap();
    fs::write(home.codex(), "").unwrap();
    let (url, server) = gateway(2);
    let installed = setup(&home, &url, "all");
    assert!(installed.status.success(), "{}", text(&installed));
    server.join().unwrap();
    let restored = run(&home, &["client", "restore", "all"]);
    assert!(restored.status.success(), "{}", text(&restored));
    assert!(home.codex().is_file());
    assert_eq!(fs::read_to_string(home.codex()).unwrap(), "");
    assert!(!home.claude().exists());
}

#[cfg(target_os = "linux")]
#[test]
fn restore_refuses_edits_made_after_setup_without_exposing_the_key() {
    let home = TestHome::new();
    let (url, server) = gateway(2);
    let installed = setup(&home, &url, "all");
    assert!(installed.status.success(), "{}", text(&installed));
    server.join().unwrap();
    fs::write(home.claude(), "{\"userEdit\":true}\n").unwrap();
    let restored = run(&home, &["client", "restore", "all"]);
    assert!(!restored.status.success());
    assert!(text(&restored).contains("refusing destructive restore"));
    assert!(!text(&restored).contains(KEY));
    assert!(home.codex().exists());
    assert_eq!(
        fs::read_to_string(home.claude()).unwrap(),
        "{\"userEdit\":true}\n"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn restore_refuses_permission_edits_made_after_setup() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let home = TestHome::new();
    let (url, server) = gateway(2);
    let installed = setup(&home, &url, "all");
    assert!(installed.status.success(), "{}", text(&installed));
    server.join().unwrap();
    fs::set_permissions(home.claude(), fs::Permissions::from_mode(0o640)).unwrap();

    let restored = run(&home, &["client", "restore", "all"]);

    assert!(!restored.status.success());
    assert!(text(&restored).contains("refusing destructive restore"));
    assert_eq!(fs::metadata(home.claude()).unwrap().mode() & 0o7777, 0o640);
    assert!(home.codex().exists());
    assert!(home.state().exists());
}

#[cfg(target_os = "linux")]
#[test]
fn unavailable_gateway_and_malformed_settings_leave_clients_untouched() {
    let home = TestHome::new();
    let unavailable = setup(&home, "http://127.0.0.1:1", "all");
    assert!(!unavailable.status.success());
    assert!(!home.codex().exists());
    assert!(!home.claude().exists());
    assert!(!home.state().exists());

    fs::create_dir(home.path.join(".claude")).unwrap();
    fs::write(home.claude(), "not json\n").unwrap();
    let (url, server) = gateway(2);
    let malformed = setup(&home, &url, "all");
    assert!(!malformed.status.success());
    server.join().unwrap();
    assert!(text(&malformed).contains("malformed JSON"));
    assert!(!home.codex().exists());
    assert_eq!(fs::read_to_string(home.claude()).unwrap(), "not json\n");
    assert!(!home.state().exists());
}

#[cfg(target_os = "linux")]
#[test]
fn oversized_recovery_journal_is_rejected_before_client_mutation() {
    let home = TestHome::new();
    fs::create_dir(home.path.join(".codex")).unwrap();
    fs::create_dir(home.path.join(".claude")).unwrap();
    let codex_original = "c".repeat(4_300_000);
    let claude_original = format!(r#"{{"padding":"{}"}}"#, "d".repeat(4_300_000));
    fs::write(home.codex(), &codex_original).unwrap();
    fs::write(home.claude(), &claude_original).unwrap();
    let (url, server) = gateway(2);

    let output = setup(&home, &url, "all");

    server.join().unwrap();
    assert!(!output.status.success());
    assert!(text(&output).contains("client-setup.json is too large"));
    assert_eq!(fs::read_to_string(home.codex()).unwrap(), codex_original);
    assert_eq!(fs::read_to_string(home.claude()).unwrap(), claude_original);
    assert!(!home.state().exists());
    assert!(fs::read_dir(home.path.join(".kirolb"))
        .unwrap()
        .next()
        .is_none());
}

#[cfg(unix)]
#[cfg(target_os = "linux")]
#[test]
fn setup_refuses_symlink_targets_and_keeps_the_referent_unchanged() {
    use std::os::unix::fs::symlink;
    let home = TestHome::new();
    fs::create_dir(home.path.join(".claude")).unwrap();
    let referent = home.path.join("real-settings.json");
    fs::write(&referent, "{\"safe\":true}\n").unwrap();
    symlink(&referent, home.claude()).unwrap();
    let (url, server) = gateway(2);
    let output = setup(&home, &url, "all");
    assert!(!output.status.success());
    server.join().unwrap();
    assert!(text(&output).contains("refusing to use symlink"));
    assert_eq!(fs::read_to_string(referent).unwrap(), "{\"safe\":true}\n");
    assert!(!home.codex().exists());
}

#[cfg(target_os = "linux")]
#[test]
fn recovery_journal_can_restore_the_old_bytes_after_an_interrupted_install() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let home = TestHome::new();
    fs::create_dir(home.path.join(".claude")).unwrap();
    let original = "{\"before\":true}\n";
    fs::write(home.claude(), original).unwrap();
    let original_mode = fs::metadata(home.claude()).unwrap().mode() & 0o7777;
    let (url, server) = gateway(2);
    let output = setup(&home, &url, "all");
    assert!(output.status.success(), "{}", text(&output));
    server.join().unwrap();

    // This is the observable on-disk state if setup stops after journaling but
    // before replacing the Claude settings file.
    fs::write(home.claude(), original).unwrap();
    fs::set_permissions(home.claude(), fs::Permissions::from_mode(original_mode)).unwrap();
    let restored = run(&home, &["client", "restore", "all"]);
    assert!(restored.status.success(), "{}", text(&restored));
    assert_eq!(fs::read_to_string(home.claude()).unwrap(), original);
    assert!(!home.codex().exists());
}

#[cfg(target_os = "linux")]
#[test]
fn parallel_setup_is_serialized_and_both_runs_succeed() {
    let home = TestHome::new();
    let (url, server) = gateway(4);
    let mut first = home.command();
    first
        .args(["client", "setup", "all", "--base-url", &url])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut second = home.command();
    second
        .args(["client", "setup", "all", "--base-url", &url])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let first = first.spawn().unwrap();
    let second = second.spawn().unwrap();
    let first = first.wait_with_output().unwrap();
    let second = second.wait_with_output().unwrap();
    server.join().unwrap();
    assert!(first.status.success(), "{}", text(&first));
    assert!(second.status.success(), "{}", text(&second));
    assert!(home.codex().exists());
    assert!(home.claude().exists());
    let status = run(&home, &["client", "status", "all"]);
    assert!(status.status.success());
    assert!(String::from_utf8_lossy(&status.stdout).contains("codex: installed"));
    assert!(String::from_utf8_lossy(&status.stdout).contains("claude: installed"));
}

#[cfg(target_os = "linux")]
#[test]
fn status_rejects_special_client_and_journal_files_without_blocking() {
    use std::ffi::CString;
    use std::os::unix::fs::symlink;

    let home = TestHome::new();
    let (url, server) = gateway(2);
    let installed = setup(&home, &url, "all");
    assert!(installed.status.success(), "{}", text(&installed));
    server.join().unwrap();
    let settings = fs::read(home.claude()).unwrap();

    fs::remove_file(home.claude()).unwrap();
    let fifo = CString::new(home.claude().as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    let fifo_status = run_bounded(&home, &["client", "status", "all"]);
    assert!(!fifo_status.status.success());
    assert!(text(&fifo_status).contains("not a regular file"));

    fs::remove_file(home.claude()).unwrap();
    symlink("/dev/zero", home.claude()).unwrap();
    let device_status = run_bounded(&home, &["client", "status", "all"]);
    assert!(!device_status.status.success());
    assert!(text(&device_status).contains("symlink"));

    fs::remove_file(home.claude()).unwrap();
    fs::write(home.claude(), settings).unwrap();
    fs::remove_file(home.state()).unwrap();
    let fifo = CString::new(home.state().as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    let journal_status = run_bounded(&home, &["client", "status", "all"]);
    assert!(!journal_status.status.success());
    assert!(text(&journal_status).contains("not a regular file"));
}

#[test]
fn diagnose_rejects_malformed_discovery_without_writing_configuration() {
    let home = TestHome::new();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = thread::spawn(move || {
        for (index, stream) in listener.incoming().take(2).enumerate() {
            let mut stream = stream.unwrap();
            let mut request = [0; 2048];
            let _ = stream.read(&mut request);
            let body = if index == 0 {
                r#"{"status":"healthy"}"#
            } else {
                "{}"
            };
            write!(stream, "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).unwrap();
        }
    });
    let output = run(&home, &["client", "diagnose", "--base-url", &url]);
    server.join().unwrap();
    assert!(!output.status.success());
    assert!(text(&output).contains("no data array"));
    assert!(!home.codex().exists());
    assert!(!home.claude().exists());
    assert!(!home.state().exists());
    assert!(!home.path.join(".env").exists());
}

#[cfg(target_os = "linux")]
#[test]
fn plaintext_loopback_diagnostic_ignores_environment_proxies() {
    let home = TestHome::new();
    let (url, server) = gateway(2);
    let output = home
        .command()
        .env("HTTP_PROXY", "http://127.0.0.1:1")
        .env("http_proxy", "http://127.0.0.1:1")
        .env("ALL_PROXY", "http://127.0.0.1:1")
        .env("all_proxy", "http://127.0.0.1:1")
        .env("NO_PROXY", "")
        .env("no_proxy", "")
        .args(["client", "diagnose", "--base-url", &url])
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", text(&output));
    server.join().unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn setup_rejects_unusable_model_discovery_without_writing_configuration() {
    let home = TestHome::new();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = thread::spawn(move || {
        for (index, stream) in listener.incoming().take(2).enumerate() {
            let mut stream = stream.unwrap();
            let mut request = [0; 2048];
            let _ = stream.read(&mut request);
            let body = if index == 0 {
                r#"{"status":"healthy"}"#
            } else {
                r#"{"data":[null,{}, {"id":""}, {"id":7}]}"#
            };
            write!(stream, "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).unwrap();
        }
    });

    let output = setup(&home, &url, "all");

    server.join().unwrap();
    assert!(!output.status.success());
    assert!(text(&output).contains("returned no usable models"));
    assert!(!home.codex().exists());
    assert!(!home.claude().exists());
    assert!(!home.state().exists());
}

#[cfg(target_os = "linux")]
#[test]
fn plaintext_setup_rejects_unsafe_inherited_proxy_before_writing() {
    let home = TestHome::new();
    let (url, server) = gateway(2);
    let output = home
        .command()
        .env("HTTP_PROXY", "http://127.0.0.1:1")
        .env("NO_PROXY", "localhost")
        .args(["client", "setup", "all", "--base-url", &url])
        .output()
        .unwrap();

    server.join().unwrap();
    assert!(!output.status.success());
    assert!(text(&output).contains("without an unambiguous NO_PROXY/no_proxy"));
    assert!(!home.codex().exists());
    assert!(!home.claude().exists());
    assert!(!home.state().exists());
}

#[cfg(target_os = "linux")]
#[test]
fn plaintext_setup_accepts_proxy_with_matching_bypass() {
    let home = TestHome::new();
    let (url, server) = gateway(2);
    let host = url
        .strip_prefix("http://")
        .unwrap()
        .split(':')
        .next()
        .unwrap();
    let output = home
        .command()
        .env("HTTP_PROXY", "http://127.0.0.1:1")
        .env("NO_PROXY", host)
        .args(["client", "setup", "all", "--base-url", &url])
        .output()
        .unwrap();

    server.join().unwrap();
    assert!(output.status.success(), "{}", text(&output));
    assert!(home.codex().exists());
    assert!(home.claude().exists());
    assert!(home.state().exists());
}

#[cfg(target_os = "linux")]
#[test]
fn plaintext_setup_honors_preserved_claude_proxy_overrides() {
    let home = TestHome::new();
    fs::create_dir(home.path.join(".claude")).unwrap();
    let original =
        r#"{"env":{"HTTP_PROXY":"http://127.0.0.1:1","NO_PROXY":"localhost"},"theme":"dark"}"#;
    fs::write(home.claude(), original).unwrap();
    let (url, server) = gateway(2);
    let output = home
        .command()
        .env("NO_PROXY", "127.0.0.1")
        .args(["client", "setup", "claude", "--base-url", &url])
        .output()
        .unwrap();

    server.join().unwrap();
    assert!(!output.status.success());
    assert!(text(&output).contains("claude runtime has an HTTP proxy"));
    assert_eq!(fs::read_to_string(home.claude()).unwrap(), original);
    assert!(!home.state().exists());
}

#[cfg(windows)]
#[test]
fn status_uses_userprofile_when_home_is_absent() {
    let home = TestHome::new();
    let output = home
        .command()
        .env_remove("HOME")
        .args(["client", "status", "all"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", text(&output));
}

#[cfg(not(target_os = "linux"))]
#[test]
fn setup_and_restore_fail_closed_without_writing_files() {
    let home = TestHome::new();

    for arguments in [
        &["client", "setup", "all"][..],
        &["client", "restore", "all"][..],
    ] {
        let output = run(&home, arguments);
        assert!(!output.status.success());
        assert!(text(&output).contains("supported only on Linux"));
    }
    assert!(!home.codex().exists());
    assert!(!home.claude().exists());
    assert!(!home.state().exists());
}

#[cfg(target_os = "linux")]
#[test]
fn malformed_url_and_secret_are_rejected_without_printing_the_secret() {
    let home = TestHome::new();
    let url = run(
        &home,
        &[
            "client",
            "setup",
            "--base-url",
            "https://user:pass@example.test/path",
        ],
    );
    assert!(!url.status.success());
    assert!(text(&url).contains("base URL must"));

    let mut command = home.command();
    let output = command
        .env("KIROLB_API_KEY", "line-one\nline-two")
        .args(["client", "diagnose"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!text(&output).contains("line-one"));
}
