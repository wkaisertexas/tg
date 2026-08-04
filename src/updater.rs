use anyhow::{Context, Result, bail};
use reqwest::blocking::Client;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

pub fn update() -> Result<()> {
    let repository = std::env::var("TG_REPOSITORY")
        .ok()
        .or_else(|| option_env!("TG_REPOSITORY").map(str::to_owned))
        .context("release repository is unknown; set TG_REPOSITORY=owner/repository")?;
    let client = Client::builder().user_agent("tg-self-updater").build()?;
    let release: Release = client
        .get(format!(
            "https://api.github.com/repos/{repository}/releases/latest"
        ))
        .send()?
        .error_for_status()?
        .json()?;
    if release.tag_name.trim_start_matches('v') == env!("CARGO_PKG_VERSION") {
        println!("tg {} is already current", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    let asset_name = format!("tg-{}", release_target()?);
    let binary_asset = find_asset(&release.assets, &asset_name)?;
    let checksum_asset = find_asset(&release.assets, &format!("{asset_name}.sha256"))?;
    let binary = download(&client, &binary_asset.browser_download_url)?;
    let checksum = String::from_utf8(download(&client, &checksum_asset.browser_download_url)?)?;
    verify_checksum(&binary, &checksum)?;

    let executable = std::env::current_exe()?.canonicalize()?;
    install(&executable, &binary).map_err(|error| {
        if error
            .downcast_ref::<std::io::Error>()
            .is_some_and(is_permission_error)
        {
            anyhow::anyhow!("run sudo tg update to update")
        } else {
            error
        }
    })?;
    println!("updated tg to {}", release.tag_name);
    Ok(())
}

fn find_asset<'a>(assets: &'a [Asset], name: &str) -> Result<&'a Asset> {
    assets
        .iter()
        .find(|asset| asset.name == name)
        .with_context(|| format!("release does not contain {name}"))
}

fn download(client: &Client, url: &str) -> Result<Vec<u8>> {
    Ok(client
        .get(url)
        .send()?
        .error_for_status()?
        .bytes()?
        .to_vec())
}

fn verify_checksum(binary: &[u8], checksum_file: &str) -> Result<()> {
    let expected = checksum_file
        .split_whitespace()
        .next()
        .context("empty checksum asset")?;
    let actual = format!("{:x}", Sha256::digest(binary));
    if actual != expected.to_ascii_lowercase() {
        bail!("downloaded binary checksum did not match the release")
    }
    Ok(())
}

fn install(executable: &Path, binary: &[u8]) -> Result<()> {
    let parent = executable
        .parent()
        .context("executable has no parent directory")?;
    let temporary = parent.join(format!(".tg-update-{}", std::process::id()));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)
        .with_context(|| format!("cannot write update beside {}", executable.display()))?;
    file.write_all(binary)?;
    file.sync_all()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o755))?;
    }
    if let Err(error) = fs::rename(&temporary, executable) {
        let _ = fs::remove_file(&temporary);
        return Err(error.into());
    }
    Ok(())
}

fn is_permission_error(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::PermissionDenied
}

fn release_target() -> Result<&'static str> {
    match (std::env::consts::ARCH, std::env::consts::OS) {
        ("aarch64", "macos") => Ok("aarch64-apple-darwin"),
        ("x86_64", "macos") => Ok("x86_64-apple-darwin"),
        ("aarch64", "linux") => Ok("aarch64-unknown-linux-gnu"),
        ("x86_64", "linux") => Ok("x86_64-unknown-linux-gnu"),
        (architecture, operating_system) => {
            bail!("no release build for {architecture}-{operating_system}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifies_release_checksums() {
        let bytes = b"tg binary";
        let checksum = format!("{:x}  tg\n", Sha256::digest(bytes));
        verify_checksum(bytes, &checksum).unwrap();
        assert!(verify_checksum(bytes, "0000  tg").is_err());
    }

    #[test]
    fn installs_an_executable_atomically() {
        let temp = tempfile::tempdir().unwrap();
        let executable = temp.path().join("tg");
        fs::write(&executable, b"old").unwrap();
        install(&executable, b"new").unwrap();
        assert_eq!(fs::read(executable).unwrap(), b"new");
    }
}
