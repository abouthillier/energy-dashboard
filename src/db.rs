use std::str::FromStr;

use anyhow::Context;
use sqlx::{
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
    SqlitePool,
};

pub async fn connect(database_url: &str) -> anyhow::Result<SqlitePool> {
    if let Some(path) = database_url.strip_prefix("sqlite:") {
        if let Some(parent) = std::path::Path::new(path).parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("creating sqlite parent directory {}", parent.display()))?;
        }
    }

    let options = SqliteConnectOptions::from_str(database_url)
        .with_context(|| format!("parsing sqlite url {database_url}"))?
        .create_if_missing(true);

    SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await
        .with_context(|| format!("connecting to {database_url}"))
}

pub async fn migrate(pool: &SqlitePool) -> anyhow::Result<()> {
    sqlx::migrate!("./migrations").run(pool).await?;
    Ok(())
}
