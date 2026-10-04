use std::process::Command;

#[test]
fn bootstrap_respects_environment_and_existing_files_without_printing_secrets() {
    for (control, proxy, existing, generates) in [
        (Some("inferx-test-control"), None, false, false),
        (Some("  inferx-test-control  "), None, false, false),
        (None, Some("proxy-test-key"), false, false),
        (None, None, false, true),
        (Some(""), None, false, true),
        (Some(" \t "), Some(""), false, true),
        (None, Some(" \t "), false, true),
        (None, None, true, false),
        (Some("inferx-test-control"), None, true, false),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let env_path = dir.path().join(".env");
        let original =
            "PROXY_API_KEY=existing-test-key\nDASHBOARD_PASSWORD=existing-test-password\n";
        if existing {
            std::fs::write(&env_path, original).unwrap();
        }
        // Exit after the startup banner, before binding a port or starting
        // background tasks. No real credentials or network services are used.
        let blocked_store = dir.path().join("not-a-directory");
        std::fs::write(&blocked_store, "fixture").unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_kirolb"));
        command
            .env_clear()
            .current_dir(dir.path())
            .env("DASHBOARD_DATA_DIR", &blocked_store)
            .env("SERVER_HOST", "127.0.0.1");
        if let Some(value) = control {
            command.env("INFERX_CONTROL_TOKEN", value);
        }
        if let Some(value) = proxy {
            command.env("PROXY_API_KEY", value);
        }
        let output = command.output().unwrap();
        assert_eq!(output.status.code(), Some(101));
        let stdout = String::from_utf8(output.stdout).unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stderr.contains("cannot open"));
        assert_eq!(env_path.exists(), existing || generates);
        assert_eq!(dir.path().join(".env.example").exists(), generates);
        assert_eq!(stdout.contains("created .env"), generates);
        for secret in [
            "inferx-test-control",
            "proxy-test-key",
            "existing-test-key",
            "existing-test-password",
        ] {
            assert!(!stdout.contains(secret) && !stderr.contains(secret));
        }
        if existing || generates {
            let contents = std::fs::read_to_string(&env_path).unwrap();
            if existing {
                assert!(contents == original, "existing .env must be unchanged");
            } else {
                for name in ["PROXY_API_KEY", "DASHBOARD_PASSWORD"] {
                    let secret = contents
                        .lines()
                        .find_map(|line| line.strip_prefix(&format!("{name}=")))
                        .unwrap()
                        .trim_matches('"');
                    assert!(!secret.is_empty());
                    assert!(
                        !stdout.contains(secret) && !stderr.contains(secret),
                        "generated credential must not be printed"
                    );
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    assert_eq!(
                        std::fs::metadata(&env_path).unwrap().permissions().mode() & 0o777,
                        0o600
                    );
                }
            }
        }
    }
}
