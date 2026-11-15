use regex::Regex;
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

fn cloudflared_binary_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "cloudflared.exe"
    } else {
        "cloudflared"
    }
}

fn cloudflared_download_url() -> Result<&'static str, String> {
    if cfg!(target_os = "windows") {
        if cfg!(target_arch = "x86_64") {
            Ok("https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-windows-amd64.exe")
        } else {
            Ok("https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-windows-386.exe")
        }
    } else if cfg!(target_os = "macos") {
        if cfg!(target_arch = "aarch64") {
            Ok("https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-darwin-arm64.tgz")
        } else {
            Ok("https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-darwin-amd64.tgz")
        }
    } else if cfg!(target_os = "linux") {
        if cfg!(target_arch = "aarch64") {
            Ok("https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-linux-arm64")
        } else {
            Ok("https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-linux-amd64")
        }
    } else {
        Err("Unsupported OS for cloudflared auto-download".into())
    }
}

pub async fn ensure_cloudflared(app_data_dir: &PathBuf) -> Result<PathBuf, String> {
    // 1. Check if cloudflared is in PATH
    let bin_name = cloudflared_binary_name();
    if let Ok(status) = Command::new(bin_name)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
    {
        if status.success() {
            return Ok(PathBuf::from(bin_name));
        }
    }

    // 2. Check in app_data_dir
    std::fs::create_dir_all(app_data_dir).map_err(|e| e.to_string())?;
    let local_bin = app_data_dir.join(bin_name);
    if local_bin.exists() {
        return Ok(local_bin);
    }

    // 3. Download from GitHub releases
    let url = cloudflared_download_url()?;
    let resp = reqwest::get(url)
        .await
        .map_err(|e| format!("Failed to download cloudflared: {}", e))?;

    if !resp.status().is_success() {
        return Err(format!("Download failed with status: {}", resp.status()));
    }

    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("Failed to read download bytes: {}", e))?;

    if url.ends_with(".tgz") {
        let tgz_path = app_data_dir.join("cloudflared.tgz");
        std::fs::write(&tgz_path, &bytes).map_err(|e| e.to_string())?;
        let status = Command::new("tar")
            .args([
                "-xzf",
                tgz_path.to_str().unwrap_or("cloudflared.tgz"),
                "-C",
                app_data_dir.to_str().unwrap_or("."),
            ])
            .status()
            .await
            .map_err(|e| format!("Failed to extract cloudflared.tgz: {}", e))?;
        let _ = std::fs::remove_file(&tgz_path);
        if !status.success() {
            return Err("Failed to extract cloudflared archive".into());
        }
    } else {
        std::fs::write(&local_bin, &bytes).map_err(|e| e.to_string())?;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(&local_bin) {
            let mut perms = meta.permissions();
            perms.set_mode(0o755);
            let _ = std::fs::set_permissions(&local_bin, perms);
        }
    }

    Ok(local_bin)
}

pub async fn start_quick_tunnel(
    bin_path: &PathBuf,
    port: u16,
) -> Result<(Child, String), String> {
    let mut cmd = Command::new(bin_path);
    cmd.args(["tunnel", "--url", &format!("http://127.0.0.1:{}", port)])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    #[cfg(target_os = "windows")]
    {
        #[allow(unused_imports)]
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Failed to start cloudflared: {}", e))?;

    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "Failed to capture cloudflared stderr".to_string())?;

    let re = Regex::new(r"https://[a-zA-Z0-9-]+\.trycloudflare\.com").unwrap();
    let mut reader = BufReader::new(stderr).lines();

    let url = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        while let Ok(Some(line)) = reader.next_line().await {
            if let Some(m) = re.find(&line) {
                return Some(m.as_str().to_string());
            }
        }
        None
    })
    .await
    .map_err(|_| "Timed out waiting for Cloudflare Tunnel URL".to_string())?
    .ok_or_else(|| "Cloudflare Tunnel exited before providing a URL".to_string())?;

    Ok((child, url))
}
