// SPDX-License-Identifier: AGPL-3.0-or-later

//! The public host's own listener: plain HTTP behind a reverse proxy, or TLS with a certificate
//! from files (`[publish] tls_cert`, `tls_key`), read again when they change, so a renewal
//! (certbot, cert-manager, an ACME client) needs no restart.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use anyhow::Context as _;
use axum::Router;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder;
use rustls::ServerConfig;
use rustls::pki_types::pem::PemObject as _;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

/// How often the files are looked at for a renewal.
const RECHECK: Duration = Duration::from_secs(60);

fn load(cert: &Path, key: &Path) -> anyhow::Result<Arc<CertifiedKey>> {
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_file_iter(cert)
        .with_context(|| format!("reading {}", cert.display()))?
        .collect::<Result<_, _>>()
        .with_context(|| format!("reading the certificates in {}", cert.display()))?;
    anyhow::ensure!(!certs.is_empty(), "{} holds no certificate", cert.display());
    let key = PrivateKeyDer::from_pem_file(key)
        .with_context(|| format!("reading the private key in {}", key.display()))?;
    let provider = rustls::crypto::aws_lc_rs::default_provider();
    Ok(Arc::new(
        CertifiedKey::from_der(certs, key, &provider)
            .context("the key doesn't fit the certificate")?,
    ))
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// The certificate, reloaded when its files change.
#[derive(Debug)]
struct Reloading {
    cert: PathBuf,
    key: PathBuf,
    current: Mutex<(Arc<CertifiedKey>, Instant, Option<SystemTime>)>,
}

impl ResolvesServerCert for Reloading {
    fn resolve(&self, _hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        let mut current = self.current.lock().ok()?;
        if current.1.elapsed() >= RECHECK {
            current.1 = Instant::now();
            let stamp = modified(&self.cert).max(modified(&self.key));
            if stamp != current.2 {
                match load(&self.cert, &self.key) {
                    Ok(key) => {
                        tracing::info!(cert = %self.cert.display(), "public host certificate reloaded");
                        *current = (key, Instant::now(), stamp);
                    }
                    Err(error) => {
                        tracing::warn!(%error, "reloading the public host's certificate; keeping the old one");
                    }
                }
            }
        }
        Some(Arc::clone(&current.0))
    }
}

/// A TLS acceptor for the certificate and key files.
pub fn acceptor(cert: &Path, key: &Path) -> anyhow::Result<TlsAcceptor> {
    let first = load(cert, key)?;
    let resolver = Reloading {
        cert: cert.to_owned(),
        key: key.to_owned(),
        current: Mutex::new((first, Instant::now(), modified(cert).max(modified(key)))),
    };
    let mut config = ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .context("TLS protocol versions")?
    .with_no_client_auth()
    .with_cert_resolver(Arc::new(resolver));
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(TlsAcceptor::from(Arc::new(config)))
}

/// Serves `app` over TLS on `listener` until `shutdown` resolves. Each connection's peer
/// address reaches the handlers as [`axum::extract::ConnectInfo`].
pub async fn serve_tls(
    listener: TcpListener,
    acceptor: TlsAcceptor,
    app: Router,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) {
    tokio::pin!(shutdown);
    loop {
        let (stream, peer) = tokio::select! {
            () = &mut shutdown => break,
            accepted = listener.accept() => match accepted {
                Ok(a) => a,
                Err(error) => {
                    tracing::warn!(%error, "accepting a connection on the public host");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
            },
        };
        let acceptor = acceptor.clone();
        let app = app.clone();
        tokio::spawn(async move {
            // A client that never finishes its handshake doesn't hold the connection open.
            let tls = match tokio::time::timeout(Duration::from_secs(10), acceptor.accept(stream))
                .await
            {
                Ok(Ok(s)) => s,
                Ok(Err(error)) => {
                    tracing::debug!(%error, %peer, "TLS handshake failed");
                    return;
                }
                Err(_) => return,
            };
            let service = hyper::service::service_fn(
                move |mut request: hyper::Request<hyper::body::Incoming>| {
                    request
                        .extensions_mut()
                        .insert(axum::extract::ConnectInfo::<SocketAddr>(peer));
                    let app = app.clone();
                    async move {
                        use tower::ServiceExt as _;
                        app.oneshot(request.map(axum::body::Body::new)).await
                    }
                },
            );
            if let Err(error) = Builder::new(TokioExecutor::new())
                .serve_connection(TokioIo::new(tls), service)
                .await
            {
                tracing::debug!(%error, %peer, "public host connection ended");
            }
        });
    }
}
