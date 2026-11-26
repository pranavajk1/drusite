// SPDX-License-Identifier: AGPL-3.0-or-later

//! Migrations, row-level security, workspace keys and account administration, on a real
//! PostgreSQL.

#![allow(clippy::unwrap_used)] // helpers outside #[test] functions

mod common;

use drusite_model::{UserId, WorkspaceId};
use drusite_server::account::{self, AccountError, hash_token};
use drusite_server::crypto::LocalMasterKeys;
use drusite_server::db::Db;
use drusite_server::keys::{KeyError, KeySource, Keyring};
use sqlx::Row as _;

async fn add_user(db: &Db, email: &str) -> UserId {
    let id = UserId::new();
    sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
        .bind(id.as_uuid())
        .bind(email)
        .execute(db.pool())
        .await
        .unwrap();
    id
}

/// Creates a workspace the way the server will: the workspace, its owner, its first key.
async fn add_workspace(db: &Db, keys: &Keyring, owner: UserId) -> WorkspaceId {
    let ws = WorkspaceId::new();
    let mut tx = db.as_user(owner).await.unwrap();
    sqlx::query("INSERT INTO workspaces (id, created_by) VALUES ($1, $2)")
        .bind(ws.as_uuid())
        .bind(owner.as_uuid())
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO workspace_members (workspace_id, user_id, role) VALUES ($1, $2, 'owner')",
    )
    .bind(ws.as_uuid())
    .bind(owner.as_uuid())
    .execute(&mut *tx)
    .await
    .unwrap();
    keys.create(&mut tx, ws).await.unwrap();
    tx.commit().await.unwrap();
    ws
}

fn local_keyring(entries: &str) -> Keyring {
    Keyring::new(KeySource::Local(LocalMasterKeys::parse(entries).unwrap()))
}

fn is_rls_violation(e: &sqlx::Error) -> bool {
    // 42501: insufficient_privilege, which is what a failed WITH CHECK raises.
    e.as_database_error()
        .and_then(|d| d.code())
        .is_some_and(|c| c == "42501")
}

async fn count(tx: &mut sqlx::PgConnection, sql: &'static str) -> i64 {
    sqlx::query(sql).fetch_one(tx).await.unwrap().get(0)
}

#[tokio::test]
async fn migrations_apply_once_and_preflight_guards_rls() {
    let Some(t) = common::fresh_db().await else {
        return;
    };
    assert!(t.db.migrations_current().await.unwrap());
    t.db.migrate().await.unwrap();

    // A superuser skips row-level security, so the server refuses one unless told otherwise.
    let err = t.admin.preflight(false).await.unwrap_err();
    assert!(err.to_string().contains("BYPASSRLS"), "{err}");
    t.admin.preflight(true).await.unwrap();
}

#[tokio::test]
async fn row_level_security_keeps_workspaces_apart() {
    let Some(t) = common::fresh_db().await else {
        return;
    };
    let keys = local_keyring(&LocalMasterKeys::generate_entry().unwrap());
    let alice = add_user(&t.db, "alice@example.com").await;
    let bob = add_user(&t.db, "bob@example.com").await;
    let alices = add_workspace(&t.db, &keys, alice).await;
    let bobs = add_workspace(&t.db, &keys, bob).await;

    let mut tx = t.db.as_user(bob).await.unwrap();
    // Bob sees his own workspace, key and membership, and nothing of Alice's, even with no
    // WHERE clause at all.
    assert_eq!(count(&mut tx, "SELECT count(*) FROM workspaces").await, 1);
    assert_eq!(
        count(&mut tx, "SELECT count(*) FROM workspace_keys").await,
        1
    );
    assert_eq!(
        count(&mut tx, "SELECT count(*) FROM workspace_members").await,
        1
    );
    let theirs: Option<uuid::Uuid> = sqlx::query_scalar("SELECT id FROM workspaces WHERE id = $1")
        .bind(alices.as_uuid())
        .fetch_optional(&mut *tx)
        .await
        .unwrap();
    assert_eq!(theirs, None);
    // The key cache holds Alice's key, but it's only handed out when the row is visible.
    assert!(matches!(
        keys.version(&mut tx, alices, 1).await,
        Err(KeyError::NoKey(_))
    ));
    keys.version(&mut tx, bobs, 1).await.unwrap();
    tx.rollback().await.unwrap();

    // Bob can't make himself a member of Alice's workspace, give it a key, or create a
    // workspace in her name.
    for (sql, a, b) in [
        (
            "INSERT INTO workspace_members (workspace_id, user_id, role) VALUES ($1, $2, 'owner')",
            alices.as_uuid(),
            bob.as_uuid(),
        ),
        (
            "INSERT INTO workspaces (id, created_by) VALUES ($1, $2)",
            WorkspaceId::new().as_uuid(),
            alice.as_uuid(),
        ),
    ] {
        let mut tx = t.db.as_user(bob).await.unwrap();
        let err = sqlx::query(sql)
            .bind(a)
            .bind(b)
            .execute(&mut *tx)
            .await
            .unwrap_err();
        assert!(is_rls_violation(&err), "{sql}: {err}");
    }
    let mut tx = t.db.as_user(bob).await.unwrap();
    let err = sqlx::query(
        "INSERT INTO workspace_keys (workspace_id, version, master_key_id, wrapped)
         VALUES ($1, 2, 'x', '\\x00')",
    )
    .bind(alices.as_uuid())
    .execute(&mut *tx)
    .await
    .unwrap_err();
    assert!(is_rls_violation(&err), "{err}");
    tx.rollback().await.unwrap();

    // Updates and deletes of Alice's rows find nothing to change.
    let mut tx = t.db.as_user(bob).await.unwrap();
    let deleted = sqlx::query("DELETE FROM workspaces WHERE id = $1")
        .bind(alices.as_uuid())
        .execute(&mut *tx)
        .await
        .unwrap();
    assert_eq!(deleted.rows_affected(), 0);
    tx.commit().await.unwrap();

    // With no user set, workspace data is invisible; the system sees everything.
    let mut tx = t.db.anonymous().await.unwrap();
    assert_eq!(count(&mut tx, "SELECT count(*) FROM workspaces").await, 0);
    assert_eq!(
        count(&mut tx, "SELECT count(*) FROM workspace_keys").await,
        0
    );
    tx.rollback().await.unwrap();
    let mut tx = t.db.as_system().await.unwrap();
    assert_eq!(count(&mut tx, "SELECT count(*) FROM workspaces").await, 2);
    tx.rollback().await.unwrap();

    // The user setting is per transaction: it doesn't leak into the next one on the connection.
    let mut conn = t.db.pool().acquire().await.unwrap();
    for _ in 0..3 {
        let mut tx = t.db.as_user(alice).await.unwrap();
        assert_eq!(count(&mut tx, "SELECT count(*) FROM workspaces").await, 1);
        tx.commit().await.unwrap();
        assert_eq!(count(&mut conn, "SELECT count(*) FROM workspaces").await, 0);
    }
}

#[tokio::test]
async fn only_owners_manage_members_and_keys() {
    let Some(t) = common::fresh_db().await else {
        return;
    };
    let keys = local_keyring(&LocalMasterKeys::generate_entry().unwrap());
    let alice = add_user(&t.db, "alice@example.com").await;
    let bob = add_user(&t.db, "bob@example.com").await;
    let ws = add_workspace(&t.db, &keys, alice).await;

    let mut tx = t.db.as_user(alice).await.unwrap();
    sqlx::query(
        "INSERT INTO workspace_members (workspace_id, user_id, role) VALUES ($1, $2, 'editor')",
    )
    .bind(ws.as_uuid())
    .bind(bob.as_uuid())
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();

    // Bob, an editor, sees the workspace and reads its key, but can't add a key version or
    // promote himself.
    let mut tx = t.db.as_user(bob).await.unwrap();
    assert_eq!(count(&mut tx, "SELECT count(*) FROM workspaces").await, 1);
    keys.current(&mut tx, ws).await.unwrap();
    let err = keys.create(&mut tx, ws).await.unwrap_err();
    assert!(
        matches!(&err, KeyError::Db(e) if is_rls_violation(e)),
        "{err}"
    );
    tx.rollback().await.unwrap();
    let mut tx = t.db.as_user(bob).await.unwrap();
    let promoted = sqlx::query(
        "UPDATE workspace_members SET role = 'owner' WHERE workspace_id = $1 AND user_id = $2",
    )
    .bind(ws.as_uuid())
    .bind(bob.as_uuid())
    .execute(&mut *tx)
    .await
    .unwrap();
    assert_eq!(promoted.rows_affected(), 0);
    tx.rollback().await.unwrap();

    // Alice can make the next key version.
    let mut tx = t.db.as_user(alice).await.unwrap();
    assert_eq!(keys.create(&mut tx, ws).await.unwrap().version(), 2);
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn workspace_keys_survive_a_restart_and_a_master_key_rotation() {
    let Some(t) = common::fresh_db().await else {
        return;
    };
    let old = LocalMasterKeys::generate_entry().unwrap();
    let alice = add_user(&t.db, "alice@example.com").await;
    let ws = add_workspace(&t.db, &local_keyring(&old), alice).await;

    let sealed = {
        let keys = local_keyring(&old);
        let mut tx = t.db.as_user(alice).await.unwrap();
        let key = keys.current(&mut tx, ws).await.unwrap();
        key.encrypt(b"ctx", b"note").unwrap()
    };

    // A new process (empty cache) with a new master key first and the old one kept.
    let new = LocalMasterKeys::generate_entry().unwrap();
    let keys = local_keyring(&format!("{new} {old}"));
    let mut tx = t.db.as_user(alice).await.unwrap();
    let key = keys.version(&mut tx, ws, 1).await.unwrap();
    assert_eq!(key.decrypt(b"ctx", &sealed).unwrap(), b"note");
    // New key versions are wrapped by the new master key.
    keys.create(&mut tx, ws).await.unwrap();
    let ids: Vec<String> =
        sqlx::query_scalar("SELECT master_key_id FROM workspace_keys ORDER BY version")
            .fetch_all(&mut *tx)
            .await
            .unwrap();
    let id_of = |entry: &str| entry.split_once(':').unwrap().0.to_owned();
    assert_eq!(ids, [id_of(&old), id_of(&new)]);
    tx.commit().await.unwrap();

    // Without the old master key, the old version can't be opened.
    let keys = local_keyring(&new);
    let mut tx = t.db.as_user(alice).await.unwrap();
    assert!(keys.version(&mut tx, ws, 1).await.is_err());
}

#[tokio::test]
async fn account_recovery_issues_one_hashed_link_at_a_time() {
    let Some(t) = common::fresh_db().await else {
        return;
    };
    let ttl = time::Duration::minutes(30);
    let (user, first) = account::create(&t.db, "Carol@Example.com", "Carol", ttl)
        .await
        .unwrap();
    assert!(matches!(
        account::create(&t.db, "carol@example.com", "", ttl).await,
        Err(AccountError::Exists(_))
    ));
    assert!(matches!(
        account::create(&t.db, "not-an-email", "", ttl).await,
        Err(AccountError::BadEmail(_))
    ));
    assert!(matches!(
        account::recover(&t.db, "nobody@example.com", ttl).await,
        Err(AccountError::NoSuchAccount(_))
    ));

    let (again, second) = account::recover(&t.db, "CAROL@example.com", ttl)
        .await
        .unwrap();
    assert_eq!(again, user);
    let rows: Vec<Vec<u8>> = sqlx::query_scalar("SELECT token_hash FROM recovery_tokens")
        .fetch_all(t.db.pool())
        .await
        .unwrap();
    // Only the newest link is live, and only its hash is stored.
    assert_eq!(rows, [hash_token(&second).to_vec()]);
    assert_ne!(first, second);
    let verified: bool = sqlx::query_scalar("SELECT email_verified FROM users WHERE id = $1")
        .bind(user.as_uuid())
        .fetch_one(t.db.pool())
        .await
        .unwrap();
    assert!(!verified);

    let link = account::recovery_link(&url::Url::parse("https://n.example.com/").unwrap(), &second);
    assert_eq!(link.path(), "/auth/recover");
    assert_eq!(link.query(), None);
    assert_eq!(link.fragment(), Some(format!("token={second}").as_str()));
}
