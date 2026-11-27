// SPDX-License-Identifier: AGPL-3.0-or-later

//! Publish to web, through the server: the server keeps who may publish, the links and the
//! passwords, and writes the share state into the workspace document so every device shows it.

use drusite_model::PageId;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::Supervisor;
use super::ask::Failure;
use crate::api::{ExpiryChange, PasswordChange, ShareChange, ShareInfo};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ServerShare {
    published: bool,
    url: Option<String>,
    include_subpages: bool,
    allow_indexing: bool,
    expires_at: Option<i64>,
    expired: bool,
    has_password: bool,
    can_publish: bool,
}

impl From<ServerShare> for ShareInfo {
    fn from(s: ServerShare) -> Self {
        Self {
            published: s.published,
            url: s.url,
            include_subpages: s.include_subpages,
            allow_indexing: s.allow_indexing,
            expires_at: s.expires_at,
            expired: s.expired,
            has_password: s.has_password,
            can_publish: s.can_publish,
            available: true,
            message: None,
        }
    }
}

/// The body of a change, as the server takes it: absent keeps, `null` removes.
fn body(change: &ShareChange) -> Value {
    let mut m = Map::new();
    if let Some(v) = change.include_subpages {
        m.insert("includeSubpages".into(), json!(v));
    }
    if let Some(v) = change.allow_indexing {
        m.insert("allowIndexing".into(), json!(v));
    }
    match &change.expiry {
        Some(ExpiryChange::Set { at }) => {
            m.insert("expiresAt".into(), json!(at));
        }
        Some(ExpiryChange::Clear) => {
            m.insert("expiresAt".into(), Value::Null);
        }
        None => {}
    }
    match &change.password {
        Some(PasswordChange::Set { password }) => {
            m.insert("password".into(), json!(password));
        }
        Some(PasswordChange::Clear) => {
            m.insert("password".into(), Value::Null);
        }
        None => {}
    }
    Value::Object(m)
}

/// Words for the person when a call fails.
fn words(f: Failure) -> String {
    match f.code.as_deref() {
        Some("404" | "not_found") => {
            "This server doesn't know this page, or is older than publishing; it may need updating."
                .to_owned()
        }
        _ => f.message,
    }
}

impl Supervisor {
    fn share_path(&self, page: PageId, rest: &str) -> Result<String, String> {
        self.ws_path(&format!("pages/{page}/share{rest}"))
    }

    /// How a page is shared, as the server says.
    pub async fn share(&self, page: PageId) -> Result<ShareInfo, String> {
        let path = self.share_path(page, "")?;
        let s: ServerShare = self.call("GET", &path, None).await.map_err(words)?;
        Ok(s.into())
    }

    pub async fn publish(&self, page: PageId, change: &ShareChange) -> Result<ShareInfo, String> {
        let path = self.share_path(page, "")?;
        let s: ServerShare = self
            .call("PUT", &path, Some(body(change)))
            .await
            .map_err(words)?;
        Ok(s.into())
    }

    pub async fn unpublish(&self, page: PageId) -> Result<ShareInfo, String> {
        let path = self.share_path(page, "")?;
        let s: ServerShare = self.call("DELETE", &path, None).await.map_err(words)?;
        Ok(s.into())
    }

    pub async fn regenerate_share_link(&self, page: PageId) -> Result<ShareInfo, String> {
        let path = self.share_path(page, "/regenerate")?;
        let s: ServerShare = self.call("POST", &path, None).await.map_err(words)?;
        Ok(s.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_change_says_only_what_changes() {
        assert_eq!(body(&ShareChange::default()), json!({}));
        let all = ShareChange {
            include_subpages: Some(true),
            allow_indexing: Some(false),
            expiry: Some(ExpiryChange::Clear),
            password: Some(PasswordChange::Set {
                password: "pw12".into(),
            }),
        };
        assert_eq!(
            body(&all),
            json!({"includeSubpages": true, "allowIndexing": false, "expiresAt": null, "password": "pw12"})
        );
    }
}
