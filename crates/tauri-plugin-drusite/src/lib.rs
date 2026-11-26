// SPDX-License-Identifier: AGPL-3.0-or-later

//! The platform side of the Drusite app, as a Tauri plugin the app calls from Rust:
//!
//! - **the device key** that signs DPoP proofs: in the Secure Enclave on iOS and in StrongBox or
//!   the TEE on Android, where it can't be exported. On macOS, for now, a software key kept in
//!   the Keychain (the Secure Enclave needs a signed app with a keychain entitlement);
//! - **the store key** that opens the encrypted local store: in the Keychain on Apple platforms,
//!   and on Android wrapped by a Keystore key;
//! - **signing in through the system browser**: `ASWebAuthenticationSession` on iOS, the
//!   browser and an intent filter for the `dev.drusite.app:` scheme on Android, and a loopback
//!   redirect (RFC 8252) on desktops.
//!
//! On iOS it also hides WKWebView's form accessory bar (the ^ ⌄ ✓ strip above the keyboard),
//! which native editors don't show.

use std::sync::Arc;

use drusite_core::StoreKey;
use drusite_core::account::SharedKey;
use tauri::plugin::{Builder, TauriPlugin};
use tauri::{Manager, Runtime};
use url::Url;

#[cfg(desktop)]
mod desktop;
#[cfg(mobile)]
mod mobile;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Platform(String),
    #[error("signing in was cancelled")]
    Cancelled,
    #[error(transparent)]
    Core(#[from] drusite_core::Error),
    #[error(transparent)]
    Dpop(#[from] drusite_dpop::DpopError),
    #[cfg(mobile)]
    #[error(transparent)]
    Plugin(#[from] tauri::plugin::mobile::PluginInvokeError),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// The plugin's state, reached through [`DrusiteExt::drusite`].
pub struct Drusite<R: Runtime> {
    #[cfg(mobile)]
    handle: tauri::plugin::PluginHandle<R>,
    #[cfg(desktop)]
    _runtime: std::marker::PhantomData<fn() -> R>,
}

/// Where a sign-in comes back to, and the means to wait for it.
pub struct Redirect<R: Runtime> {
    uri: String,
    #[cfg(desktop)]
    listener: tokio::net::TcpListener,
    #[cfg(mobile)]
    handle: tauri::plugin::PluginHandle<R>,
    #[cfg(desktop)]
    _runtime: std::marker::PhantomData<fn() -> R>,
}

impl<R: Runtime> std::fmt::Debug for Drusite<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Drusite")
    }
}

impl<R: Runtime> std::fmt::Debug for Redirect<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Redirect").field("uri", &self.uri).finish()
    }
}

impl<R: Runtime> Redirect<R> {
    /// The `redirect_uri` to sign in with.
    pub fn uri(&self) -> &str {
        &self.uri
    }

    /// Opens `url` in the system browser and waits for the browser to come back. Dropping the
    /// future gives up waiting; on phones, call [`Drusite::cancel_sign_in`] too, to close the
    /// browser sheet.
    pub async fn open(self, url: &Url) -> Result<Url> {
        #[cfg(desktop)]
        {
            desktop::open(self.listener, url).await
        }
        #[cfg(mobile)]
        {
            mobile::authenticate(self.handle, url).await
        }
    }
}

impl<R: Runtime> Drusite<R> {
    /// The device key, made on first use.
    pub fn device_key(&self) -> Result<SharedKey> {
        #[cfg(desktop)]
        {
            Ok(Arc::new(desktop::device_key()?))
        }
        #[cfg(mobile)]
        {
            Ok(Arc::new(mobile::DeviceKey::load(self.handle.clone())?))
        }
    }

    /// The key the local store is encrypted with, made on first use.
    pub fn store_key(&self) -> Result<StoreKey> {
        #[cfg(target_vendor = "apple")]
        {
            Ok(drusite_core::key::keychain::load_or_create(
                drusite_core::key::keychain::SERVICE,
                "workspace",
            )?)
        }
        #[cfg(target_os = "android")]
        {
            mobile::store_key(&self.handle)
        }
        #[cfg(not(any(target_vendor = "apple", target_os = "android")))]
        {
            Err(Error::Platform(
                "this platform has no store key support yet".to_owned(),
            ))
        }
    }

    /// How the server should list this device.
    pub fn device_name(&self) -> String {
        #[cfg(desktop)]
        {
            desktop::device_name()
        }
        #[cfg(mobile)]
        {
            mobile::device_name(&self.handle)
        }
    }

    /// Gets ready to sign in: picks the redirect the browser will come back to.
    pub async fn redirect(&self) -> Result<Redirect<R>> {
        #[cfg(desktop)]
        {
            let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
                .await
                .map_err(|e| Error::Platform(format!("can't listen for the sign-in: {e}")))?;
            let port = listener
                .local_addr()
                .map_err(|e| Error::Platform(e.to_string()))?
                .port();
            Ok(Redirect {
                uri: drusite_core::auth::loopback_redirect(port),
                listener,
                _runtime: std::marker::PhantomData,
            })
        }
        #[cfg(mobile)]
        {
            Ok(Redirect {
                uri: drusite_core::auth::APP_REDIRECT.to_owned(),
                handle: self.handle.clone(),
            })
        }
    }

    /// Closes a sign-in sheet still open on a phone. Nothing to do on desktops.
    pub fn cancel_sign_in(&self) {
        #[cfg(mobile)]
        mobile::cancel(&self.handle);
    }
}

/// [`Drusite`] from anything that reaches the app.
pub trait DrusiteExt<R: Runtime> {
    fn drusite(&self) -> &Drusite<R>;
}

impl<R: Runtime, T: Manager<R>> DrusiteExt<R> for T {
    fn drusite(&self) -> &Drusite<R> {
        self.state::<Drusite<R>>().inner()
    }
}

#[cfg(target_os = "ios")]
tauri::ios_plugin_binding!(init_plugin_drusite);

pub fn init<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("drusite")
        .setup(|app, api| {
            #[cfg(target_os = "android")]
            let handle = api.register_android_plugin("dev.drusite.plugin", "DrusitePlugin")?;
            #[cfg(target_os = "ios")]
            let handle = api.register_ios_plugin(init_plugin_drusite)?;
            #[cfg(desktop)]
            let _ = api;
            app.manage(Drusite::<R> {
                #[cfg(mobile)]
                handle,
                #[cfg(desktop)]
                _runtime: std::marker::PhantomData,
            });
            Ok(())
        })
        .build()
}
