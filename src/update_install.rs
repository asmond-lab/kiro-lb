use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tempfile::TempDir;
use tokio::process::Command;

const RELEASE_BASE: &str = "https://github.com/minpeter/kiro-lb/releases/download";
const MAX_CHECKSUMS: u64 = 1024 * 1024;
const MAX_ASSET: u64 = 256 * 1024 * 1024;
const MAX_EXECUTABLE: u64 = 256 * 1024 * 1024;
const NETWORK_TIMEOUT: Duration = Duration::from_secs(60);
const SMOKE_TIMEOUT: Duration = Duration::from_secs(5);

pub fn asset_name() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => Some("kirolb-windows-x64.exe"),
        ("windows", "aarch64") => Some("kirolb-windows-arm64.exe"),
        ("linux", "x86_64") => Some("kirolb-linux-x64.tar.gz"),
        ("linux", "aarch64") => Some("kirolb-linux-arm64.tar.gz"),
        _ => None,
    }
}

pub struct PreparedUpdate {
    executable: PathBuf,
    candidate: PathBuf,
    staging: TempDir,
}

pub struct InstalledUpdate {
    pub executable: PathBuf,
    pub backup: PathBuf,
}

pub async fn prepare(
    client: &reqwest::Client,
    executable: PathBuf,
    tag: &str,
) -> Result<PreparedUpdate, String> {
    let version = valid_tag(tag)?;
    let asset =
        asset_name().ok_or_else(|| "updates are not supported on this platform".to_owned())?;
    let parent = executable
        .parent()
        .ok_or_else(|| "executable path has no parent directory".to_owned())?;
    let staging = tempfile::Builder::new()
        .prefix(".kirolb-update-")
        .tempdir_in(parent)
        .map_err(|e| format!("cannot stage update beside executable: {e}"))?;

    let sums = download_bytes(
        client,
        &format!("{RELEASE_BASE}/{tag}/SHA256SUMS"),
        MAX_CHECKSUMS,
    )
    .await?;
    let expected = checksum_for(&sums, asset)?;
    let downloaded = staging.path().join(asset);
    let actual = download_file(
        client,
        &format!("{RELEASE_BASE}/{tag}/{asset}"),
        &downloaded,
        MAX_ASSET,
    )
    .await?;
    verify_checksum(actual, expected, asset)?;

    let candidate = if let Some(bare) = asset.strip_suffix(".tar.gz") {
        let output = staging.path().join("candidate");
        tokio::task::spawn_blocking(move || {
            extract_executable(&downloaded, bare, &output)?;
            Ok::<_, String>(output)
        })
        .await
        .map_err(|e| e.to_string())??
    } else {
        downloaded
    };

    smoke_test(&candidate, &version).await?;
    Ok(PreparedUpdate {
        executable,
        candidate,
        staging,
    })
}

impl PreparedUpdate {
    pub fn replace(self) -> Result<InstalledUpdate, String> {
        let backup = backup_path(&self.executable)?;
        atomic_copy(&self.executable, &backup)
            .map_err(|e| format!("failed to create update backup: {e}"))?;

        if let Err(error) = self_replace::self_replace(&self.candidate) {
            if !self.executable.exists() {
                atomic_copy(&backup, &self.executable).map_err(|restore| {
                    format!("replacement failed ({error}); restoring original failed: {restore}")
                })?;
            }
            return Err(format!("failed to replace executable: {error}"));
        }

        // Keep the TempDir alive until self_replace has finished with its candidate.
        drop(self.staging);
        Ok(InstalledUpdate {
            executable: self.executable,
            backup,
        })
    }
}

impl InstalledUpdate {
    pub fn restore(&self) -> Result<(), String> {
        // Use the saved installation path: current_exe may now refer to the
        // relocated/deleted OLD image, especially on Windows.
        atomic_copy(&self.backup, &self.executable)
            .map_err(|e| format!("failed to restore previous executable: {e}"))
    }
}

fn valid_tag(tag: &str) -> Result<String, String> {
    let raw = tag
        .strip_prefix('v')
        .ok_or_else(|| "release tag must have the form vX.Y.Z".to_owned())?;
    let version = semver::Version::parse(raw)
        .map_err(|_| "release tag must have the form vX.Y.Z".to_owned())?;
    if !version.pre.is_empty()
        || !version.build.is_empty()
        || tag != format!("v{}.{}.{}", version.major, version.minor, version.patch)
    {
        return Err("release tag must be a stable vX.Y.Z tag".to_owned());
    }
    Ok(format!(
        "{}.{}.{}",
        version.major, version.minor, version.patch
    ))
}

async fn response(client: &reqwest::Client, url: &str) -> Result<reqwest::Response, String> {
    client
        .get(url)
        .timeout(NETWORK_TIMEOUT)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| format!("failed to download release asset: {e}"))
}

async fn download_bytes(
    client: &reqwest::Client,
    url: &str,
    limit: u64,
) -> Result<Vec<u8>, String> {
    let mut stream = response(client, url).await?.bytes_stream();
    let mut result = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("failed while downloading release asset: {e}"))?;
        if result.len() as u64 + chunk.len() as u64 > limit {
            return Err("release asset exceeds size limit".to_owned());
        }
        result.extend_from_slice(&chunk);
    }
    Ok(result)
}

async fn download_file(
    client: &reqwest::Client,
    url: &str,
    path: &Path,
    limit: u64,
) -> Result<[u8; 32], String> {
    let mut response = response(client, url).await?;
    if response
        .content_length()
        .is_some_and(|length| length > limit)
    {
        return Err("release asset exceeds size limit".to_owned());
    }
    let mut file = tokio::fs::File::create(path)
        .await
        .map_err(|e| format!("failed to create staged asset: {e}"))?;
    let mut hash = Sha256::new();
    let mut size = 0_u64;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| format!("failed while downloading release asset: {e}"))?
    {
        size = size.saturating_add(chunk.len() as u64);
        if size > limit {
            return Err("release asset exceeds size limit".to_owned());
        }
        tokio::io::AsyncWriteExt::write_all(&mut file, &chunk)
            .await
            .map_err(|e| format!("failed to write staged asset: {e}"))?;
        hash.update(&chunk);
    }
    tokio::io::AsyncWriteExt::flush(&mut file)
        .await
        .map_err(|e| format!("failed to flush staged asset: {e}"))?;
    Ok(hash.finalize().into())
}

fn checksum_for(contents: &[u8], asset: &str) -> Result<[u8; 32], String> {
    let text = std::str::from_utf8(contents).map_err(|_| "SHA256SUMS is not UTF-8".to_owned())?;
    let mut found = None;
    for line in text.lines() {
        let Some((hash, name)) = line.split_once(' ') else {
            continue;
        };
        let name = name.trim_start_matches([' ', '*']);
        if name != asset {
            continue;
        }
        if found.is_some() {
            return Err(format!("SHA256SUMS contains duplicate entries for {asset}"));
        }
        let bytes = hex::decode(hash).map_err(|_| format!("invalid checksum for {asset}"))?;
        let value: [u8; 32] = bytes
            .try_into()
            .map_err(|_| format!("invalid checksum for {asset}"))?;
        found = Some(value);
    }
    found.ok_or_else(|| format!("SHA256SUMS does not contain {asset}"))
}

fn verify_checksum(actual: [u8; 32], expected: [u8; 32], asset: &str) -> Result<(), String> {
    if actual == expected {
        Ok(())
    } else {
        Err(format!("checksum mismatch for {asset}"))
    }
}

fn extract_executable(archive: &Path, expected: &str, output: &Path) -> Result<(), String> {
    let file = File::open(archive).map_err(|e| format!("failed to open update archive: {e}"))?;
    let decoder = flate2::read::GzDecoder::new(file).take(MAX_EXECUTABLE + 1024 * 1024);
    let mut archive = tar::Archive::new(decoder);
    let mut found = false;
    for item in archive
        .entries()
        .map_err(|e| format!("invalid update archive: {e}"))?
    {
        let entry = item.map_err(|e| format!("invalid update archive entry: {e}"))?;
        if entry.path_bytes().as_ref() != expected.as_bytes() {
            continue;
        }
        if found {
            return Err("update archive contains duplicate executable entries".to_owned());
        }
        if !entry.header().entry_type().is_file() {
            return Err("update archive executable is not a regular file".to_owned());
        }
        if entry.size() > MAX_EXECUTABLE {
            return Err("update executable exceeds size limit".to_owned());
        }
        let mut destination =
            File::create(output).map_err(|e| format!("failed to create staged executable: {e}"))?;
        io::copy(&mut entry.take(MAX_EXECUTABLE + 1), &mut destination)
            .map_err(|e| format!("failed to extract update executable: {e}"))?;
        destination
            .flush()
            .map_err(|e| format!("failed to flush staged executable: {e}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(output, fs::Permissions::from_mode(0o755))
                .map_err(|e| format!("failed to mark staged executable executable: {e}"))?;
        }
        found = true;
    }
    if !found {
        return Err("update archive is missing its executable".to_owned());
    }
    Ok(())
}

async fn smoke_test(candidate: &Path, version: &str) -> Result<(), String> {
    let mut command = Command::new(candidate);
    command
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    let child = command
        .spawn()
        .map_err(|e| format!("failed to start staged executable: {e}"))?;
    let output = tokio::time::timeout(SMOKE_TIMEOUT, child.wait_with_output())
        .await
        .map_err(|_| "staged executable version check timed out".to_owned())?
        .map_err(|e| format!("staged executable version check failed: {e}"))?;
    if !output.status.success() || output.stdout != format!("kirolb {version}\n").as_bytes() {
        return Err("staged executable reported an unexpected version".to_owned());
    }
    Ok(())
}

fn backup_path(executable: &Path) -> Result<PathBuf, String> {
    let name = executable
        .file_name()
        .ok_or_else(|| "executable path has no file name".to_owned())?;
    let mut backup_name = name.to_os_string();
    backup_name.push(".previous");
    Ok(executable.with_file_name(backup_name))
}

fn atomic_copy(source: &Path, destination: &Path) -> io::Result<()> {
    let parent = destination
        .parent()
        .ok_or_else(|| io::Error::other("destination has no parent"))?;
    let mut temporary = tempfile::Builder::new()
        .prefix(".kirolb-copy-")
        .tempfile_in(parent)?;
    let mut input = File::open(source)?;
    io::copy(&mut input, &mut temporary)?;
    temporary.flush()?;
    temporary.as_file().sync_all()?;
    let permissions = fs::metadata(source)?.permissions();
    fs::set_permissions(temporary.path(), permissions)?;
    temporary.persist(destination).map_err(|e| e.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_rejects_mismatch_and_ambiguity() {
        let asset = "kirolb-linux-x64.tar.gz";
        let good = "11".repeat(32);
        assert_eq!(
            checksum_for(format!("{good}  {asset}\n").as_bytes(), asset).unwrap(),
            [0x11; 32]
        );
        assert!(checksum_for(format!("{good}  other\n").as_bytes(), asset).is_err());
        assert!(checksum_for(
            format!("{good}  {asset}\n{good} *{asset}\n").as_bytes(),
            asset
        )
        .is_err());
        assert!(verify_checksum([0x11; 32], [0x22; 32], asset).is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn archive_rejects_nonregular_and_duplicate_targets() {
        fn archive(path: &Path, duplicate: bool, symlink: bool) {
            let file = File::create(path).unwrap();
            let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
            let mut tar = tar::Builder::new(encoder);
            // A raw traversal entry must never be unpacked, even when a valid
            // executable appears later in the same archive.
            let mut traversal = tar::Header::new_gnu();
            traversal.as_mut_bytes()[..10].copy_from_slice(b"../outside");
            traversal.set_size(4);
            traversal.set_cksum();
            tar.append(&traversal, &b"evil"[..]).unwrap();
            let mut header = tar::Header::new_gnu();
            if symlink {
                header.set_entry_type(tar::EntryType::Symlink);
                header.set_link_name("elsewhere").unwrap();
                header.set_size(0);
                header.set_cksum();
                tar.append_data(&mut header, "kirolb-linux-x64", io::empty())
                    .unwrap();
            } else {
                for _ in 0..=usize::from(duplicate) {
                    let bytes = b"binary";
                    let mut header = tar::Header::new_gnu();
                    header.set_size(bytes.len() as u64);
                    header.set_mode(0o755);
                    header.set_cksum();
                    tar.append_data(&mut header, "kirolb-linux-x64", &bytes[..])
                        .unwrap();
                }
            }
            tar.finish().unwrap();
        }
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("asset.tar.gz");
        let output = dir.path().join("candidate");
        archive(&input, false, true);
        assert!(extract_executable(&input, "kirolb-linux-x64", &output).is_err());
        archive(&input, true, false);
        assert!(extract_executable(&input, "kirolb-linux-x64", &output).is_err());
        archive(&input, false, false);
        extract_executable(&input, "kirolb-linux-x64", &output).unwrap();
        assert_eq!(fs::read(&output).unwrap(), b"binary");
        assert!(!dir.path().parent().unwrap().join("outside").exists());
        assert!(extract_executable(&input, "missing", &output).is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn smoke_test_rejects_wrong_and_hanging_versions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let wrong = dir.path().join("wrong");
        fs::write(&wrong, "#!/bin/sh\nprintf 'kirolb 9.9.9\\n'\n").unwrap();
        fs::set_permissions(&wrong, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(smoke_test(&wrong, "1.2.3").await.is_err());
        smoke_test(&wrong, "9.9.9").await.unwrap();

        let hanging = dir.path().join("hanging");
        fs::write(&hanging, "#!/bin/sh\nexec sleep 30\n").unwrap();
        fs::set_permissions(&hanging, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(smoke_test(&hanging, "1.2.3")
            .await
            .unwrap_err()
            .contains("timed out"));
    }

    #[cfg(unix)]
    #[test]
    fn executable_replacement_and_restore_in_disposable_child() {
        use std::os::unix::fs::PermissionsExt;
        if std::env::var_os("KIROLB_TEST_REPLACE_CHILD").is_some() {
            let executable = std::env::current_exe().unwrap();
            let before = Sha256::digest(fs::read(&executable).unwrap());
            let staging = tempfile::tempdir_in(executable.parent().unwrap()).unwrap();
            let candidate = staging.path().join("new");
            fs::write(&candidate, "#!/bin/sh\nprintf 'new executable runs\\n'\n").unwrap();
            fs::set_permissions(&candidate, fs::Permissions::from_mode(0o755)).unwrap();
            let installed = PreparedUpdate {
                executable,
                candidate,
                staging,
            }
            .replace()
            .unwrap();
            assert_eq!(Sha256::digest(fs::read(&installed.backup).unwrap()), before);
            let output = std::process::Command::new(&installed.executable)
                .output()
                .unwrap();
            assert!(output.status.success());
            assert_eq!(output.stdout, b"new executable runs\n");
            installed.restore().unwrap();
            assert_eq!(
                Sha256::digest(fs::read(&installed.executable).unwrap()),
                before
            );
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let executable = dir.path().join("disposable-test-runner");
        fs::copy(std::env::current_exe().unwrap(), &executable).unwrap();
        let output = std::process::Command::new(executable)
            .env("KIROLB_TEST_REPLACE_CHILD", "1")
            .args([
                "--exact",
                "update_install::tests::executable_replacement_and_restore_in_disposable_child",
                "--nocapture",
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
