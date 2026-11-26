// SPDX-License-Identifier: AGPL-3.0-or-later

//! Sending email over SMTP: magic links, for now.

use std::sync::{Arc, Mutex};

use lettre::message::Mailbox;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport as _, Message, Tokio1Executor};

use crate::config::{SmtpConfig, SmtpTls};

/// A message as sent, for tests.
#[derive(Debug, Clone)]
pub struct SentMail {
    pub to: String,
    pub subject: String,
    pub body: String,
}

#[derive(Debug, Clone)]
pub enum Mailer {
    /// No `[smtp]`: nothing needs email.
    None,
    Smtp {
        transport: AsyncSmtpTransport<Tokio1Executor>,
        from: Mailbox,
    },
    /// Keeps what it "sends", for tests.
    Memory(Arc<Mutex<Vec<SentMail>>>),
}

impl Mailer {
    pub fn from_config(config: Option<&SmtpConfig>) -> anyhow::Result<Self> {
        let Some(config) = config else {
            return Ok(Self::None);
        };
        let mut builder = match config.tls {
            SmtpTls::Tls => AsyncSmtpTransport::<Tokio1Executor>::relay(&config.host)?,
            SmtpTls::Starttls => {
                AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&config.host)?
            }
            SmtpTls::None => AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&config.host),
        };
        if let Some(port) = config.port {
            builder = builder.port(port);
        }
        if let (Some(user), Some(password)) = (&config.username, &config.password) {
            builder =
                builder.credentials(Credentials::new(user.clone(), password.expose().to_owned()));
        }
        let from: Mailbox = config
            .from
            .parse()
            .map_err(|e| anyhow::anyhow!("[smtp] from {:?} isn't an address: {e}", config.from))?;
        Ok(Self::Smtp {
            transport: builder.build(),
            from,
        })
    }

    pub fn memory() -> (Self, Arc<Mutex<Vec<SentMail>>>) {
        let sent = Arc::default();
        (Self::Memory(Arc::clone(&sent)), sent)
    }

    pub async fn send(&self, to: &str, subject: &str, body: String) -> anyhow::Result<()> {
        match self {
            Self::None => anyhow::bail!("email isn't configured ([smtp])"),
            Self::Memory(sent) => {
                sent.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(SentMail {
                        to: to.to_owned(),
                        subject: subject.to_owned(),
                        body,
                    });
                Ok(())
            }
            Self::Smtp { transport, from } => {
                let message = Message::builder()
                    .from(from.clone())
                    .to(to.parse()?)
                    .subject(subject)
                    .header(lettre::message::header::ContentType::TEXT_PLAIN)
                    .body(body)?;
                transport.send(message).await?;
                Ok(())
            }
        }
    }

    /// Checks the SMTP server answers, at start-up.
    pub async fn check(&self) -> anyhow::Result<()> {
        if let Self::Smtp { transport, .. } = self
            && !transport.test_connection().await?
        {
            anyhow::bail!("the SMTP server didn't answer");
        }
        Ok(())
    }
}
