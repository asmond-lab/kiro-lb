use base64::Engine;
use rand::RngCore;
use std::path::Path;

const EXAMPLE: &str = include_str!("../.env.example");

fn random_bytes(n: usize) -> Vec<u8> {
    let mut buf = vec![0u8; n];
    rand::thread_rng().fill_bytes(&mut buf);
    buf
}

fn render_default(api_key: &str, password: &str) -> String {
    EXAMPLE
        .lines()
        .map(|line| match line.split_once('=').map(|(k, _)| k.trim()) {
            Some("PROXY_API_KEY") => format!("PROXY_API_KEY=\"{api_key}\""),
            Some("DASHBOARD_PASSWORD") => format!("DASHBOARD_PASSWORD=\"{password}\""),
            _ => line.to_owned(),
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

pub struct Generated {
    pub api_key: String,
    pub password: String,
}

fn configured_by_environment() -> bool {
    ["PROXY_API_KEY", "INFERX_CONTROL_TOKEN"]
        .iter()
        .any(|name| std::env::var(name).is_ok_and(|v| !v.trim().is_empty()))
}

pub fn ensure_env() -> std::io::Result<Option<Generated>> {
    if dotenvy::dotenv().is_ok() || configured_by_environment() {
        return Ok(None);
    }
    let dir = std::env::current_dir()?;
    let example = dir.join(".env.example");
    if !example.exists() {
        std::fs::write(&example, EXAMPLE)?;
    }
    let api_key = hex::encode(random_bytes(32));
    let password = base64::engine::general_purpose::STANDARD.encode(random_bytes(32));
    write_private(&dir.join(".env"), &render_default(&api_key, &password))?;
    dotenvy::dotenv().map_err(std::io::Error::other)?;
    Ok(Some(Generated { api_key, password }))
}

fn write_private(path: &Path, contents: &str) -> std::io::Result<()> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    use std::io::Write;
    opts.open(path)?.write_all(contents.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_environment_configured_deployment_writes_nothing() {
        let dir =
            std::env::temp_dir().join(format!("kirolb-boot-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let previous = std::env::current_dir().unwrap();
        std::env::set_current_dir(&dir).unwrap();
        std::env::set_var("PROXY_API_KEY", "from-environment");
        let generated = ensure_env().unwrap();
        let wrote = dir.join(".env").exists() || dir.join(".env.example").exists();
        std::env::set_current_dir(previous).unwrap();
        std::env::remove_var("PROXY_API_KEY");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(generated.is_none());
        assert!(!wrote);
    }
}
