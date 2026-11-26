// SPDX-License-Identifier: AGPL-3.0-or-later

//! Test helpers: a fresh database per test, a config without a file.

#![allow(dead_code, unreachable_pub, clippy::unwrap_used)]

use drusite_server::config::Config;
use drusite_server::crypto::LocalMasterKeys;
use drusite_server::db::Db;
use sqlx::postgres::PgPoolOptions;
use url::Url;

/// The role tests connect as: an ordinary role, so row-level security applies to it.
const APP_ROLE: &str = "drusite_test";

/// A database made for one test, owned by an ordinary role, migrated.
pub struct TestDb {
    pub db: Db,
    /// The same database through the admin (superuser) connection.
    pub admin: Db,
    pub app_url: String,
}

/// `DRUSITE_TEST_DATABASE_URL` is a superuser URL for a PostgreSQL server with pgvector. Without
/// it the database tests are skipped, unless `DRUSITE_REQUIRE_DB_TESTS` is set (as in CI).
pub async fn fresh_db() -> Option<TestDb> {
    let Ok(admin_url) = std::env::var("DRUSITE_TEST_DATABASE_URL") else {
        assert!(
            std::env::var_os("DRUSITE_REQUIRE_DB_TESTS").is_none(),
            "DRUSITE_REQUIRE_DB_TESTS is set but DRUSITE_TEST_DATABASE_URL isn't"
        );
        eprintln!("skipped: set DRUSITE_TEST_DATABASE_URL to run the database tests");
        return None;
    };
    let admin = PgPoolOptions::new()
        .max_connections(2)
        .connect(&admin_url)
        .await
        .expect("connect as admin");
    // Tests run in parallel; two may race to create the role.
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "DO $$ BEGIN
             CREATE ROLE {APP_ROLE} LOGIN PASSWORD '{APP_ROLE}' NOSUPERUSER NOBYPASSRLS;
         EXCEPTION WHEN duplicate_object OR unique_violation THEN NULL;
         END $$"
    )))
    .execute(&admin)
    .await
    .expect("create the test role");
    let name = format!("drusite_test_{}", uuid::Uuid::now_v7().simple());
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE DATABASE {name} OWNER {APP_ROLE}"
    )))
    .execute(&admin)
    .await
    .expect("create the test database");
    admin.close().await;

    let mut url = Url::parse(&admin_url).unwrap();
    url.set_path(&format!("/{name}"));
    let admin_db_url = url.to_string();
    let admin = PgPoolOptions::new()
        .max_connections(2)
        .connect(&admin_db_url)
        .await
        .unwrap();
    // pgvector isn't trusted, so the administrator enables it, as a deployment's would.
    sqlx::query("CREATE EXTENSION vector")
        .execute(&admin)
        .await
        .unwrap();

    url.set_username(APP_ROLE).unwrap();
    url.set_password(Some(APP_ROLE)).unwrap();
    let app_url = url.to_string();
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&app_url)
        .await
        .expect("connect as the test role");
    let db = Db::from_pool(pool);
    db.preflight(false).await.expect("preflight");
    db.migrate().await.expect("migrate");
    Some(TestDb {
        db,
        admin: Db::from_pool(admin),
        app_url,
    })
}

/// A config with password login on, master keys in a temporary file, and the given overrides.
pub fn config(database_url: &str, overrides: &[(&str, &str)]) -> (Config, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let keys = dir.path().join("master-keys");
    std::fs::write(&keys, LocalMasterKeys::generate_entry().unwrap()).unwrap();
    let mut env: Vec<(String, String)> = [
        ("DRUSITE__PUBLIC_URL", "http://localhost:8080"),
        ("DRUSITE__DATABASE__URL", database_url),
        ("DRUSITE__STORAGE__BUCKET", "drusite"),
        ("DRUSITE__KEYS__SOURCE", "file"),
        ("DRUSITE__KEYS__PATH", keys.to_str().unwrap()),
    ]
    .iter()
    .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
    .collect();
    env.extend(
        overrides
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned())),
    );
    (Config::load(None, env).unwrap(), dir)
}
