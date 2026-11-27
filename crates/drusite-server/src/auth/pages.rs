// SPDX-License-Identifier: AGPL-3.0-or-later

//! The sign-in and account pages, built into the binary. One small page and script serve
//! `/login`, `/account` and the links from emails; they talk to the JSON endpoints.

use axum::http::{HeaderName, HeaderValue, header};
use axum::response::{IntoResponse, Response};
use axum::{Router, routing::get};

use crate::app::AppState;

const HTML: &str = include_str!("../../assets/signin.html");
const JS: &str = include_str!("../../assets/signin.js");
const CSS: &str = include_str!("../../assets/signin.css");

/// Nothing but this server's own script and style, and no framing.
const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; \
                   img-src 'self' data:; form-action 'self'; frame-ancestors 'none'; base-uri 'none'";

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/login", get(page))
        .route("/account", get(page))
        .route("/auth/magic", get(page))
        .route("/auth/recover", get(page))
        .route(
            "/assets/signin.js",
            get(|| async { asset(JS, "text/javascript; charset=utf-8") }),
        )
        .route(
            "/assets/signin.css",
            get(|| async { asset(CSS, "text/css; charset=utf-8") }),
        )
}

pub(super) fn security_headers() -> [(HeaderName, HeaderValue); 5] {
    [
        (
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(CSP),
        ),
        (
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ),
        (
            header::REFERRER_POLICY,
            HeaderValue::from_static("no-referrer"),
        ),
        (header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY")),
        (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
    ]
}

async fn page() -> Response {
    (
        security_headers(),
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/html; charset=utf-8"),
        )],
        HTML,
    )
        .into_response()
}

fn asset(body: &'static str, content_type: &'static str) -> Response {
    (
        [
            (header::CONTENT_TYPE, HeaderValue::from_static(content_type)),
            (
                header::X_CONTENT_TYPE_OPTIONS,
                HeaderValue::from_static("nosniff"),
            ),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-cache")),
        ],
        body,
    )
        .into_response()
}

pub(super) fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// A page with a title and a message, for errors that end in the browser.
pub(super) fn message_page(title: &str, message: &str) -> String {
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>{t}</title><link rel=\"stylesheet\" href=\"/assets/signin.css\"></head>\
         <body><main><h1>{t}</h1><p>{m}</p></main></body></html>",
        t = escape(title),
        m = escape(message),
    )
}

/// The consent page for an agent. `choices` is the workspace checkboxes, already escaped.
pub(super) fn consent_page(
    agent: &str,
    request: &str,
    choices: &str,
    read_checked: &str,
    write_checked: &str,
) -> String {
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>Connect an agent</title><link rel=\"stylesheet\" href=\"/assets/signin.css\">\
         </head><body><main><h1>Connect {a}?</h1>\
         <p class=\"muted\">{a} wants to use your Drusite notes. Choose what it may do. You can \
         see everything it changes, undo it, and disconnect it from your account page.</p>\
         <form method=\"post\" action=\"/oauth/consent\" class=\"consent\">\
         <input type=\"hidden\" name=\"request\" value=\"{r}\">\
         <fieldset><legend>Workspaces</legend>{choices}</fieldset>\
         <fieldset><legend>Access</legend>\
         <label class=\"choice\"><input type=\"radio\" name=\"access\" value=\"read\"{read_checked}> \
         Read only</label>\
         <label class=\"choice\"><input type=\"radio\" name=\"access\" value=\"write\"{write_checked}> \
         Read, add and edit pages</label>\
         <label class=\"choice\"><input type=\"checkbox\" name=\"journal\"> \
         Edit journal entries too (otherwise it can only read them)</label>\
         <p class=\"muted\">Agents can never delete pages.</p></fieldset>\
         <div class=\"actions\"><button type=\"submit\" name=\"decision\" value=\"deny\">Don't allow\
         </button> <button type=\"submit\" name=\"decision\" value=\"allow\" class=\"primary\">\
         Allow</button></div></form></main></body></html>",
        a = escape(agent),
        r = escape(request),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_are_escaped() {
        let page = message_page("<b>", "a \"quote\" & <script>");
        assert!(!page.contains("<script>"));
        assert!(page.contains("&lt;script&gt;"));
    }
}
