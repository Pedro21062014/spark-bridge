use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

fn ngrok_binary_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "ngrok.exe"
    } else {
        "ngrok"
    }
}

fn ngrok_download_url() -> Result<&'static str, String> {
    // ngrok v3 stable (canal oficial Equinox, verificado em ngrok.com/docs).
    if cfg!(target_os = "windows") {
        if cfg!(target_arch = "x86_64") {
            Ok("https://bin.equinox.io/c/bNyj1mQVY4c/ngrok-v3-stable-windows-amd64.zip")
        } else {
            Err("Unsupported Windows architecture for ngrok auto-download".into())
        }
    } else if cfg!(target_os = "macos") {
        if cfg!(target_arch = "aarch64") {
            Ok("https://bin.equinox.io/c/bNyj1mQVY4c/ngrok-v3-stable-darwin-arm64.zip")
        } else {
            Ok("https://bin.equinox.io/c/bNyj1mQVY4c/ngrok-v3-stable-darwin-amd64.zip")
        }
    } else if cfg!(target_os = "linux") {
        if cfg!(target_arch = "aarch64") {
            Ok("https://bin.equinox.io/c/bNyj1mQVY4c/ngrok-v3-stable-linux-arm64.tgz")
        } else {
            Ok("https://bin.equinox.io/c/bNyj1mQVY4c/ngrok-v3-stable-linux-amd64.tgz")
        }
    } else {
        Err("Unsupported OS for ngrok auto-download".into())
    }
}

pub async fn ensure_ngrok(app_data_dir: &PathBuf) -> Result<PathBuf, String> {
    // 1. Check if ngrok is in PATH
    let bin_name = ngrok_binary_name();
    if let Ok(status) = Command::new(bin_name)
        .arg("version")
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

    // 3. Download from Equinox (ngrok official CDN)
    let url = ngrok_download_url()?;
    let resp = reqwest::get(url)
        .await
        .map_err(|e| format!("Failed to download ngrok: {}", e))?;

    if !resp.status().is_success() {
        return Err(format!("Download failed with status: {}", resp.status()));
    }

    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("Failed to read download bytes: {}", e))?;

    if url.ends_with(".zip") {
        // Windows/macOS: extrai o binario do zip com a crate `zip`.
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
            .map_err(|e| format!("Failed to open ngrok zip: {}", e))?;
        let mut file = archive
            .by_name(bin_name)
            .map_err(|e| format!("ngrok binary not found in zip: {}", e))?;
        let mut out = std::fs::File::create(&local_bin).map_err(|e| e.to_string())?;
        std::io::copy(&mut file, &mut out).map_err(|e| e.to_string())?;
    } else {
        // Linux: extrai o .tgz com o `tar` do sistema.
        let tgz_path = app_data_dir.join("ngrok.tgz");
        std::fs::write(&tgz_path, &bytes).map_err(|e| e.to_string())?;
        let status = Command::new("tar")
            .args([
                "-xzf",
                tgz_path.to_str().unwrap_or("ngrok.tgz"),
                "-C",
                app_data_dir.to_str().unwrap_or("."),
            ])
            .status()
            .await
            .map_err(|e| format!("Failed to extract ngrok.tgz: {}", e))?;
        let _ = std::fs::remove_file(&tgz_path);
        if !status.success() || !local_bin.exists() {
            return Err("Failed to extract ngrok archive".into());
        }
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

/// Extrai a URL publica (preferindo HTTPS) da resposta de
/// GET http://127.0.0.1:4040/api/tunnels do ngrok.
pub fn parse_ngrok_public_url(v: &serde_json::Value) -> Option<String> {
    let tunnels = v.get("tunnels")?.as_array()?;
    for t in tunnels {
        if t.get("proto").and_then(|p| p.as_str()) == Some("https") {
            if let Some(u) = t.get("public_url").and_then(|u| u.as_str()) {
                return Some(u.to_string());
            }
        }
    }
    tunnels
        .first()?
        .get("public_url")
        .and_then(|u| u.as_str())
        .map(|s| s.to_string())
}

async fn drain_stream<R>(
    reader: R,
    logs: Arc<Mutex<Vec<String>>>,
) where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut lines = BufReader::new(reader).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let mut guard = logs.lock().await;
        guard.push(line);
        if guard.len() > 30 {
            guard.remove(0);
        }
    }
}

pub async fn start_ngrok_tunnel(
    bin_path: &PathBuf,
    port: u16,
    authtoken: &str,
    domain: &str,
) -> Result<(Child, String), String> {
    let mut cmd = Command::new(bin_path);
    cmd.arg("http")
        .arg(port.to_string())
        .arg("--authtoken")
        .arg(authtoken)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let domain = domain.trim();
    if !domain.is_empty() {
        cmd.arg("--domain").arg(domain);
    }

    #[cfg(target_os = "windows")]
    {
        #[allow(unused_imports)]
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Failed to start ngrok: {}", e))?;

    // Drena stdout/stderr guardando as ultimas linhas (ajuda a diagnosticar
    // erros de authtoken/dominio) e evitando que o pipe trave o processo.
    let logs = Arc::new(Mutex::new(Vec::<String>::new()));
    if let Some(stdout) = child.stdout.take() {
        let logs_clone = logs.clone();
        tokio::spawn(drain_stream(stdout, logs_clone));
    }
    if let Some(stderr) = child.stderr.take() {
        let logs_clone = logs.clone();
        tokio::spawn(drain_stream(stderr, logs_clone));
    }

    // Consulta a API local do ngrok ate a URL publica aparecer.
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| e.to_string())?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);

    loop {
        if let Ok(Some(status)) = child.try_wait() {
            let tail = logs.lock().await.join(" | ");
            return Err(format!(
                "O processo ngrok encerrou antes de manter o túnel (status {}). Logs: {}",
                status,
                if tail.is_empty() { "(sem logs)".to_string() } else { tail }
            ));
        }

        if let Ok(resp) = client.get("http://127.0.0.1:4040/api/tunnels").send().await {
            if let Ok(json) = resp.json::<serde_json::Value>().await {
                if let Some(url) = parse_ngrok_public_url(&json) {
                    return Ok((child, url));
                }
            }
        }

        if std::time::Instant::now() > deadline {
            let _ = child.kill().await;
            let tail = logs.lock().await.join(" | ");
            return Err(format!(
                "Timeout aguardando a URL publica do ngrok (30s). Verifique o authtoken e o dominio. Logs: {}",
                if tail.is_empty() { "(sem logs)".to_string() } else { tail }
            ));
        }

        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::parse_ngrok_public_url;

    #[test]
    fn parse_prefers_https() {
        let v = serde_json::json!({
            "tunnels": [
                {"proto": "http", "public_url": "http://abc.ngrok-free.app"},
                {"proto": "https", "public_url": "https://abc.ngrok-free.app"}
            ]
        });
        assert_eq!(
            parse_ngrok_public_url(&v).as_deref(),
            Some("https://abc.ngrok-free.app")
        );
    }

    #[test]
    fn parse_empty() {
        let v = serde_json::json!({"tunnels": []});
        assert_eq!(parse_ngrok_public_url(&v), None);
        let v2 = serde_json::json!({});
        assert_eq!(parse_ngrok_public_url(&v2), None);
    }
}
