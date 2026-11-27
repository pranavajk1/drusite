// SPDX-License-Identifier: AGPL-3.0-or-later

//! What can go wrong talking to a provider, in words a person can act on.

/// A provider call that failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AiError {
    #[error("can't reach the provider: {0}")]
    Network(String),
    #[error("the provider refused the key: {0}")]
    Unauthorized(String),
    #[error("the provider doesn't know that: {0}")]
    NotFound(String),
    #[error("the provider says to slow down: {0}")]
    RateLimited(String),
    #[error("the provider refused the request ({status}): {message}")]
    Refused { status: u16, message: String },
    #[error("the provider answered oddly: {0}")]
    Malformed(String),
    #[error("{0}")]
    Unsupported(String),
    #[error("{0}")]
    Invalid(String),
}

impl AiError {
    pub(crate) fn network(e: &reqwest::Error) -> Self {
        let mut message = e.to_string();
        // reqwest's message names the URL, which may carry a key in its query for some APIs.
        if let Some(url) = e.url() {
            message = message.replace(url.as_str(), url.host_str().unwrap_or("the provider"));
        }
        if e.is_timeout() {
            return Self::Network("it took too long to answer".to_owned());
        }
        Self::Network(message)
    }

    /// The error for an answer with status `status` and body `body`.
    pub(crate) fn from_status(status: u16, body: &str) -> Self {
        let message = message_of(body);
        match status {
            401 | 403 => Self::Unauthorized(message),
            404 => Self::NotFound(message),
            429 => Self::RateLimited(message),
            _ => Self::Refused { status, message },
        }
    }
}

/// The human part of an error body: vendors put it in different places.
pub(crate) fn message_of(body: &str) -> String {
    let parsed: Option<serde_json::Value> = serde_json::from_str(body).ok();
    let found = parsed.as_ref().and_then(|v| {
        let candidates = [
            &v["error"]["message"],
            &v["error"],
            &v["message"],
            &v["detail"],
            &v["detail"][0]["msg"],
        ];
        candidates
            .into_iter()
            .find_map(|c| c.as_str().map(str::to_owned))
    });
    let mut message = found.unwrap_or_else(|| body.trim().to_owned());
    if message.is_empty() {
        message = "no reason given".to_owned();
    }
    if message.len() > 300 {
        let mut end = 300;
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        message.truncate(end);
        message.push('…');
    }
    message
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_each_vendors_message() {
        // OpenAI, Anthropic, Google, OpenRouter.
        assert_eq!(
            message_of(r#"{"error":{"message":"bad key","type":"x"}}"#),
            "bad key"
        );
        // Ollama, LM Studio.
        assert_eq!(
            message_of(r#"{"error":"model not found"}"#),
            "model not found"
        );
        // Mistral.
        assert_eq!(message_of(r#"{"message":"Unauthorized"}"#), "Unauthorized");
        // FastAPI-style servers (vLLM's validation errors).
        assert_eq!(
            message_of(r#"{"detail":[{"msg":"field required"}]}"#),
            "field required"
        );
        assert_eq!(message_of("  Bad Gateway \n"), "Bad Gateway");
        assert_eq!(message_of(""), "no reason given");
        assert!(matches!(
            AiError::from_status(401, "{}"),
            AiError::Unauthorized(_)
        ));
        assert!(matches!(
            AiError::from_status(429, ""),
            AiError::RateLimited(_)
        ));
    }
}
