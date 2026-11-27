// SPDX-License-Identifier: AGPL-3.0-or-later

//! Desktops: a software device key, the browser, and a loopback redirect.

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpListener;
use url::Url;

use crate::{Error, Result};

/// The device key. On macOS it is kept in the Keychain, this device only; it is a software key
/// until the app is signed and can use the Secure Enclave.
pub(crate) fn device_key() -> Result<drusite_dpop::SoftwareKey> {
    #[cfg(target_os = "macos")]
    {
        let secret = drusite_core::key::keychain::secret(
            "dev.drusite.device",
            "dpop",
            "Drusite device key",
        )?;
        Ok(drusite_dpop::SoftwareKey::from_bytes(&secret)?)
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err(Error::Platform(
            "this platform has no device key store yet".to_owned(),
        ))
    }
}

pub(crate) fn device_name() -> String {
    #[cfg(target_os = "macos")]
    {
        let name = std::process::Command::new("scutil")
            .args(["--get", "ComputerName"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty());
        name.unwrap_or_else(|| "Mac".to_owned())
    }
    #[cfg(not(target_os = "macos"))]
    {
        std::env::consts::OS.to_owned()
    }
}

/// Opens `url` in the default browser.
fn open_browser(url: &Url) -> Result<()> {
    #[cfg(target_os = "macos")]
    let mut command = std::process::Command::new("open");
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut c = std::process::Command::new("rundll32");
        c.arg("url.dll,FileProtocolHandler");
        c
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let mut command = std::process::Command::new("xdg-open");
    let status = command
        .arg(url.as_str())
        .status()
        .map_err(|e| Error::Platform(format!("can't open the browser: {e}")))?;
    if status.success() {
        Ok(())
    } else {
        Err(Error::Platform(format!(
            "can't open the browser ({status})"
        )))
    }
}

const DONE_PAGE: &str = "<!doctype html><meta charset=utf-8><meta name=color-scheme content=\"light dark\">\
<title>Drusite</title><style>\
body{margin:0;min-height:100vh;display:grid;place-items:center;background:#f8f7fa;color:#2b2930;\
font:15px/1.55 ui-sans-serif,-apple-system,BlinkMacSystemFont,\"Segoe UI\",system-ui,sans-serif}\
main{max-width:22rem;padding:32px;border-radius:12px;background:#fff;text-align:center;\
box-shadow:0 24px 48px rgb(24 18 38/.08),0 0 0 1px rgb(24 18 38/.06)}\
h1{font-size:20px;font-weight:600;margin:0 0 4px}p{margin:0;color:#6f6c78}\
@media(prefers-color-scheme:dark){body{background:#1b1a1f;color:#e8e6ed}\
main{background:#26252c;box-shadow:0 0 0 1px rgb(255 255 255/.07)}p{color:#a19eaa}}\
</style><main><h1>Back to Drusite</h1><p>You're signed in. You can close this tab.</p></main>";

/// Opens the browser at `url` and waits for its redirect to the loopback port.
pub(crate) async fn open(listener: TcpListener, url: &Url) -> Result<Url> {
    open_browser(url)?;
    let port = listener
        .local_addr()
        .map_err(|e| Error::Platform(e.to_string()))?
        .port();
    loop {
        let (mut stream, _) = listener
            .accept()
            .await
            .map_err(|e| Error::Platform(e.to_string()))?;
        // One request line is all that matters; browsers send it in the first packet.
        let mut buf = vec![0u8; 8192];
        let n =
            match tokio::time::timeout(std::time::Duration::from_secs(10), stream.read(&mut buf))
                .await
            {
                Ok(Ok(n)) => n,
                _ => continue,
            };
        let head = String::from_utf8_lossy(&buf[..n]);
        let target = head
            .lines()
            .next()
            .and_then(|line| line.strip_prefix("GET "))
            .and_then(|rest| rest.split(' ').next());
        let callback = target
            .filter(|t| t.starts_with("/oauth/callback"))
            .and_then(|t| Url::parse(&format!("http://127.0.0.1:{port}{t}")).ok());
        let Some(callback) = callback else {
            let _ = stream
                .write_all(
                    b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                )
                .await;
            continue;
        };
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/html; charset=utf-8\r\ncontent-length: {}\r\n\
             cache-control: no-store\r\nconnection: close\r\n\r\n{DONE_PAGE}",
            DONE_PAGE.len()
        );
        let _ = stream.write_all(response.as_bytes()).await;
        let _ = stream.shutdown().await;
        return Ok(callback);
    }
}
