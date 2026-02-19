//! Self-update: fetches the latest release from GitHub and installs binaries in-place.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

const REPO: &str = "ardabotai/agentbook";
const GITHUB_API: &str = "https://api.github.com";

/// Binaries bundled in each release tarball.
const BUNDLED_BINS: &[&str] = &["agentbook", "agentbook-tui", "agentbook-node", "agentbook-agent"];

/// Detect the target triple for the current platform.
fn current_target() -> Result<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Ok("aarch64-apple-darwin"),
        ("macos", "x86_64") => Ok("x86_64-apple-darwin"),
        ("linux", "aarch64") => Ok("aarch64-unknown-linux-gnu"),
        ("linux", "x86_64") => Ok("x86_64-unknown-linux-gnu"),
        (os, arch) => bail!("unsupported platform: {os}/{arch}"),
    }
}

/// Run `agentbook update [--yes]`.
pub async fn cmd_update(yes: bool) -> Result<()> {
    let current_version = env!("CARGO_PKG_VERSION");
    let target = current_target()?;

    println!("Current version : v{current_version}");
    println!("Platform        : {target}");
    println!("Checking GitHub releases…");

    let client = reqwest::Client::builder()
        .user_agent(format!("agentbook/{current_version}"))
        .build()?;

    // Fetch latest release metadata.
    let release_url = format!("{GITHUB_API}/repos/{REPO}/releases/latest");
    let release: serde_json::Value = client
        .get(&release_url)
        .send()
        .await
        .context("failed to reach GitHub API")?
        .error_for_status()
        .context("GitHub API returned an error")?
        .json()
        .await
        .context("failed to parse GitHub API response")?;

    let tag = release["tag_name"]
        .as_str()
        .context("missing tag_name in release")?;
    let latest_version = tag.trim_start_matches('v');

    println!("Latest version  : {tag}");

    if latest_version == current_version {
        println!("Already up to date.");
        return Ok(());
    }

    // Find the asset matching our platform.
    let asset_name = format!("agentbook-{tag}-{target}.tar.gz");
    let assets = release["assets"]
        .as_array()
        .context("missing assets in release")?;
    let asset_url = assets
        .iter()
        .find_map(|a| {
            let name = a["name"].as_str()?;
            if name == asset_name {
                a["browser_download_url"].as_str().map(str::to_string)
            } else {
                None
            }
        })
        .with_context(|| format!("no release asset found for {asset_name}"))?;

    if !yes {
        eprint!("Update to {tag}? [y/N] ");
        let mut input = String::new();
        std::io::stdin()
            .read_line(&mut input)
            .context("failed to read input")?;
        if !matches!(input.trim().to_lowercase().as_str(), "y" | "yes") {
            println!("Cancelled.");
            return Ok(());
        }
    }

    // Determine install directory from the location of the running binary.
    let exe_path = std::env::current_exe().context("failed to determine current exe path")?;
    let install_dir = exe_path
        .parent()
        .context("could not determine install directory")?
        .to_path_buf();

    println!("Downloading {asset_name}…");
    let tarball = download(&client, &asset_url).await?;

    println!("Installing to {}…", install_dir.display());
    let result = extract_and_install(&tarball, &install_dir);
    let _ = std::fs::remove_file(&tarball); // clean up temp tarball regardless
    result?;

    println!("Done! agentbook updated to {tag}.");

    // Check if the node daemon is running; if so, offer to restart it.
    let socket_path = agentbook::client::default_socket_path();
    let node_running = agentbook::client::NodeClient::connect(&socket_path).await.is_ok();

    if node_running {
        // Determine whether auth can be handled non-interactively (via 1Password).
        let state_dir = agentbook_mesh::state_dir::default_state_dir()
            .unwrap_or_else(|_| PathBuf::from("."));
        let op_title = agentbook_wallet::onepassword::item_title_from_state_dir(&state_dir);
        let can_auto_restart = agentbook_wallet::onepassword::has_op_cli()
            && op_title
                .as_ref()
                .map(|t| agentbook_wallet::onepassword::has_agentbook_item(t))
                .unwrap_or(false);

        let stop = if yes {
            true
        } else {
            eprint!("Node daemon is running with the old binary. Stop it now? [Y/n] ");
            let mut input = String::new();
            std::io::stdin()
                .read_line(&mut input)
                .context("failed to read input")?;
            !matches!(input.trim().to_lowercase().as_str(), "n" | "no")
        };

        if stop {
            println!("Stopping node daemon…");
            if let Ok(mut client) = agentbook::client::NodeClient::connect(&socket_path).await {
                let _ = client.request(agentbook::protocol::Request::Shutdown).await;
                tokio::time::sleep(std::time::Duration::from_millis(800)).await;
            }

            if can_auto_restart {
                println!("Restarting node daemon via 1Password…");
                let node_bin = install_dir.join("agentbook-node");
                let node_bin = if node_bin.exists() { node_bin } else { PathBuf::from("agentbook-node") };
                let child = std::process::Command::new(&node_bin)
                    .arg("--socket").arg(&socket_path)
                    .arg("--notify-ready")
                    .stdout(std::process::Stdio::piped())
                    .spawn()
                    .with_context(|| format!("failed to spawn {}", node_bin.display()))?;

                let stdout = child.stdout.expect("piped stdout");
                use std::io::BufRead;
                let mut got_ready = false;
                for line in std::io::BufReader::new(stdout).lines() {
                    match line {
                        Ok(l) if l.trim() == "READY" => { got_ready = true; break; }
                        Ok(_) => continue,
                        Err(_) => break,
                    }
                }
                if got_ready {
                    println!("Node daemon restarted.");
                } else {
                    println!("Node launched — run `agentbook up` if it doesn't respond.");
                }
            } else {
                // Node requires interactive TOTP auth — user must restart manually.
                println!("Node stopped. Restart it when ready (you'll be prompted for your authenticator code):");
                println!("  agentbook up");
            }
        } else {
            println!("Node still running the old binary — restart it when ready:");
            println!("  agentbook down && agentbook up");
        }
    }

    Ok(())
}

/// Download a URL into a temp file, returning the temp file path.
async fn download(client: &reqwest::Client, url: &str) -> Result<PathBuf> {
    use std::io::Write;

    // Use a named temp file so we can pass its path to `tar`.
    let mut tmp = tempfile::Builder::new()
        .suffix(".tar.gz")
        .tempfile()
        .context("failed to create temp file")?;

    let mut resp = client
        .get(url)
        .send()
        .await
        .context("download request failed")?
        .error_for_status()
        .context("download returned error status")?;

    let total = resp.content_length();
    let mut downloaded: u64 = 0;

    while let Some(chunk) = resp.chunk().await.context("error reading download chunk")? {
        tmp.write_all(&chunk).context("failed to write chunk")?;
        downloaded += chunk.len() as u64;
        if let Some(t) = total {
            let pct = downloaded * 100 / t;
            eprint!("\r  {downloaded}/{t} bytes ({pct}%)   ");
        }
    }
    tmp.flush().context("failed to flush temp file")?;
    eprintln!(); // newline after progress

    // Persist the temp file so it survives this function's scope.
    let (_, path) = tmp
        .keep()
        .context("failed to persist temp file")?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_target_returns_valid_triple_on_supported_platform() {
        // This test runs on the CI platform we actually build on.
        // If the platform is unsupported, current_target() returns Err.
        match current_target() {
            Ok(triple) => {
                // Must be one of the four supported triples.
                let valid = [
                    "aarch64-apple-darwin",
                    "x86_64-apple-darwin",
                    "aarch64-unknown-linux-gnu",
                    "x86_64-unknown-linux-gnu",
                ];
                assert!(
                    valid.contains(&triple),
                    "unexpected target triple: {triple}"
                );
                // Sanity-check it matches the current OS.
                let os = std::env::consts::OS;
                if os == "macos" {
                    assert!(triple.ends_with("apple-darwin"));
                } else if os == "linux" {
                    assert!(triple.ends_with("linux-gnu"));
                }
            }
            Err(e) => {
                // Unsupported platform — just verify the error message is informative.
                assert!(e.to_string().contains("unsupported platform"));
            }
        }
    }

    #[test]
    fn bundled_bins_non_empty() {
        assert!(!BUNDLED_BINS.is_empty());
        assert!(BUNDLED_BINS.contains(&"agentbook"));
        assert!(BUNDLED_BINS.contains(&"agentbook-node"));
    }
}

/// Extract `BUNDLED_BINS` from `tarball` and atomically replace each in `install_dir`.
fn extract_and_install(tarball: &Path, install_dir: &Path) -> Result<()> {
    let tmp_dir = tempfile::tempdir().context("failed to create temp dir for extraction")?;

    // Shell out to `tar` — universally available on our target platforms.
    let status = std::process::Command::new("tar")
        .args(["xzf"])
        .arg(tarball)
        .arg("-C")
        .arg(tmp_dir.path())
        .status()
        .context("failed to run tar")?;

    if !status.success() {
        bail!("tar extraction failed with status {status}");
    }

    for bin in BUNDLED_BINS {
        let src = tmp_dir.path().join(bin);
        if !src.exists() {
            // Not all releases may bundle every binary; skip gracefully.
            continue;
        }

        let dest = install_dir.join(bin);
        // Write to a sibling temp file first, then atomically rename.
        let staging = install_dir.join(format!(".{bin}.new"));
        std::fs::copy(&src, &staging)
            .with_context(|| format!("failed to copy {bin} to staging"))?;

        // Preserve executable permission.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o755))
                .with_context(|| format!("failed to chmod {bin}"))?;
        }

        std::fs::rename(&staging, &dest)
            .with_context(|| format!("failed to install {bin} to {}", dest.display()))?;

        println!("  ✓ {}", dest.display());
    }

    Ok(())
}
