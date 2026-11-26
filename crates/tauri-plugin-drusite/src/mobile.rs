// SPDX-License-Identifier: AGPL-3.0-or-later

//! Phones: the Swift and Kotlin halves of the plugin.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use drusite_core::auth::AuthError;
use drusite_dpop::Jwk;
use serde::{Deserialize, Serialize};
use tauri::Runtime;
use tauri::plugin::PluginHandle;
use url::Url;

use crate::{Error, Result};

/// The device key's alias in the platform key store.
const ALIAS: &str = "device";

#[derive(Serialize)]
struct AliasArgs<'a> {
    alias: &'a str,
}

#[derive(Serialize)]
struct SignArgs<'a> {
    alias: &'a str,
    /// Base64.
    message: String,
}

/// A key as the platform describes it: iOS gives the SEC1 point, Android x and y.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NativeKey {
    public_key: Option<String>,
    x: Option<String>,
    y: Option<String>,
}

#[derive(Deserialize)]
struct Signature {
    /// Base64 DER.
    signature: String,
}

fn decode(text: &str) -> Result<Vec<u8>> {
    STANDARD
        .decode(text)
        .map_err(|e| Error::Platform(format!("the key store answered badly: {e}")))
}

/// The device key in the Secure Enclave, StrongBox or the TEE. The app only ever holds its
/// public half; signing is a call to the platform.
pub(crate) struct DeviceKey<R: Runtime> {
    handle: PluginHandle<R>,
    jwk: Jwk,
}

impl<R: Runtime> DeviceKey<R> {
    pub(crate) fn load(handle: PluginHandle<R>) -> Result<Self> {
        let key: NativeKey = handle.run_mobile_plugin("deviceKey", AliasArgs { alias: ALIAS })?;
        let jwk = match (&key.public_key, &key.x, &key.y) {
            (Some(point), _, _) => Jwk::from_sec1(&decode(point)?)?,
            (None, Some(x), Some(y)) => Jwk::from_xy(&decode(x)?, &decode(y)?)?,
            _ => {
                return Err(Error::Platform(
                    "the device key has no public key".to_owned(),
                ));
            }
        };
        Ok(Self { handle, jwk })
    }
}

impl<R: Runtime> drusite_core::auth::DeviceKey for DeviceKey<R> {
    fn jwk(&self) -> Jwk {
        self.jwk.clone()
    }

    fn sign(&self, message: &[u8]) -> Result<Vec<u8>, AuthError> {
        let signed: Signature = self
            .handle
            .run_mobile_plugin(
                "sign",
                SignArgs {
                    alias: ALIAS,
                    message: STANDARD.encode(message),
                },
            )
            .map_err(|e| AuthError::Refused(format!("the device key didn't sign: {e}")))?;
        STANDARD
            .decode(signed.signature)
            .map_err(|_| AuthError::Malformed)
    }
}

#[cfg(target_os = "android")]
pub(crate) fn store_key<R: Runtime>(handle: &PluginHandle<R>) -> Result<drusite_core::StoreKey> {
    #[derive(Deserialize)]
    struct Key {
        /// Hex.
        key: String,
    }
    let key: Key = handle.run_mobile_plugin("storeKey", ())?;
    Ok(drusite_core::StoreKey::from_hex(&key.key)?)
}

pub(crate) fn device_name<R: Runtime>(handle: &PluginHandle<R>) -> String {
    #[derive(Deserialize)]
    struct Name {
        name: String,
    }
    handle
        .run_mobile_plugin::<Name>("deviceName", ())
        .map(|n| n.name)
        .unwrap_or_else(|_| std::env::consts::OS.to_owned())
}

#[derive(Serialize)]
struct AuthenticateArgs<'a> {
    url: &'a str,
    scheme: &'a str,
}

#[derive(Deserialize)]
struct Callback {
    url: Option<String>,
    cancelled: Option<bool>,
}

/// Opens `url` in the platform's sign-in browser and waits for its redirect to the app's scheme.
pub(crate) async fn authenticate<R: Runtime>(handle: PluginHandle<R>, url: &Url) -> Result<Url> {
    let url = url.to_string();
    // The platform call blocks until the browser comes back, so it waits off the async threads.
    let answer = tokio::task::spawn_blocking(move || {
        handle.run_mobile_plugin::<Callback>(
            "authenticate",
            AuthenticateArgs {
                url: &url,
                scheme: "dev.drusite.app",
            },
        )
    })
    .await
    .map_err(|e| Error::Platform(e.to_string()))??;
    if answer.cancelled == Some(true) {
        return Err(Error::Cancelled);
    }
    let callback = answer
        .url
        .ok_or_else(|| Error::Platform("the browser came back with nothing".to_owned()))?;
    Url::parse(&callback).map_err(|e| Error::Platform(format!("odd redirect {callback}: {e}")))
}

pub(crate) fn cancel<R: Runtime>(handle: &PluginHandle<R>) {
    let _ = handle.run_mobile_plugin::<serde::de::IgnoredAny>("cancelAuthentication", ());
}

/// On Android 17 and later, asks for the local-network permission when `server` is on the
/// local network, since connections there are otherwise dropped without an error.
#[cfg(target_os = "android")]
pub(crate) async fn local_network<R: Runtime>(
    handle: &PluginHandle<R>,
    server: &str,
) -> Result<()> {
    #[derive(Deserialize)]
    struct Answer {
        granted: bool,
    }
    if !crate::on_local_network(server).await {
        return Ok(());
    }
    let handle = handle.clone();
    // The platform call blocks until the person answers, so it waits off the async threads.
    let answer: Answer =
        tokio::task::spawn_blocking(move || handle.run_mobile_plugin("localNetwork", ()))
            .await
            .map_err(|e| Error::Platform(e.to_string()))??;
    if answer.granted {
        Ok(())
    } else {
        Err(Error::Platform(
            "Drusite needs the “local network” permission to reach a server on your network. \
             Allow it in Settings › Apps › Drusite › Permissions, then sign in again."
                .to_owned(),
        ))
    }
}
