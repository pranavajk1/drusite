// SPDX-License-Identifier: AGPL-3.0-or-later

//! A browser for tests: keeps cookies, follows nothing by itself.

use std::collections::HashMap;

use reqwest::StatusCode;
use reqwest::header::{HeaderMap, LOCATION, SET_COOKIE};
use serde_json::Value;
use url::Url;

/// A browser: follows nothing by itself, keeps cookies, sends Origin like a real one.
pub struct Browser {
    pub http: reqwest::Client,
    pub base: Url,
    pub cookies: HashMap<String, String>,
}

impl Browser {
    pub fn new(base: &Url) -> Self {
        Self {
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap(),
            base: base.clone(),
            cookies: HashMap::new(),
        }
    }

    pub fn url(&self, path: &str) -> Url {
        self.base.join(path).unwrap()
    }

    pub fn cookie_header(&self) -> String {
        self.cookies
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("; ")
    }

    pub fn absorb(&mut self, headers: &HeaderMap) {
        for value in headers.get_all(SET_COOKIE) {
            let text = value.to_str().unwrap();
            let (pair, attrs) = text.split_once(';').unwrap_or((text, ""));
            let (name, value) = pair.split_once('=').unwrap();
            if attrs.contains("Max-Age=0") {
                self.cookies.remove(name);
            } else {
                self.cookies.insert(name.to_owned(), value.to_owned());
            }
        }
    }

    pub async fn get(&mut self, url: &Url) -> reqwest::Response {
        let response = self
            .http
            .get(url.clone())
            .header("cookie", self.cookie_header())
            .send()
            .await
            .unwrap();
        self.absorb(response.headers());
        response
    }

    pub async fn post(&mut self, path: &str, body: Value) -> (StatusCode, Value) {
        let origin = self.base.origin().ascii_serialization();
        self.post_from(path, body, &origin).await
    }

    pub async fn post_from(
        &mut self,
        path: &str,
        body: Value,
        origin: &str,
    ) -> (StatusCode, Value) {
        let response = self
            .http
            .post(self.url(path))
            .header("cookie", self.cookie_header())
            .header("origin", origin)
            .json(&body)
            .send()
            .await
            .unwrap();
        self.absorb(response.headers());
        let status = response.status();
        (status, response.json().await.unwrap_or(Value::Null))
    }

    pub async fn delete(&mut self, path: &str) -> StatusCode {
        let response = self
            .http
            .delete(self.url(path))
            .header("cookie", self.cookie_header())
            .header("origin", self.base.origin().ascii_serialization())
            .send()
            .await
            .unwrap();
        response.status()
    }
}

pub fn location(response: &reqwest::Response, base: &Url) -> Url {
    base.join(response.headers()[LOCATION].to_str().unwrap())
        .unwrap()
}
