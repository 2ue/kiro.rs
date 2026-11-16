use std::{io::Write, path::Path};

use anyhow::Context;
use chrono::Utc;
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, postgres::PgPoolOptions};

use super::{ExportedFrom, LegacyPackage};

const LEGACY_TABLES: &[&str] = &[
    "external_upstream_pools",
    "runtime_config",
    "route_scenarios",
    "model_pricing",
    "model_capabilities",
];

pub async fn export_legacy(url: &str, out: &Path, redact_secrets: bool) -> anyhow::Result<()> {
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(url)
        .await
        .context("连接旧 PostgreSQL 失败")?;
    let package = read_package(&pool, redact_secrets).await?;
    pool.close().await;
    let bytes = serde_json::to_vec_pretty(&package)?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(out)
        .context("创建导出文件失败（不覆盖已有文件）")?;
    file.write_all(&bytes)?;
    file.write_all(b"\n")?;
    println!(
        "已导出 {} 个账号、{} 个路由场景、{} 个模型价格、{} 个手工模型到 {}{}",
        package.accounts.len(),
        package.route_scenarios.len(),
        package.model_pricing.len(),
        package.model_capabilities.len(),
        out.display(),
        if redact_secrets {
            "（密钥已脱敏，不能 --apply）"
        } else {
            ""
        }
    );
    Ok(())
}

pub(super) async fn read_package(
    pool: &PgPool,
    redact_secrets: bool,
) -> anyhow::Result<LegacyPackage> {
    let mut tx = pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *tx)
        .await?;
    let columns: Vec<Value> = sqlx::query_scalar(
        "SELECT to_jsonb(c) FROM information_schema.columns c \
         WHERE table_schema = current_schema() AND table_name = ANY($1) \
         ORDER BY table_name, ordinal_position",
    )
    .bind(LEGACY_TABLES)
    .fetch_all(&mut *tx)
    .await?;
    for table in LEGACY_TABLES {
        anyhow::ensure!(
            columns.iter().any(|c| c["table_name"] == *table),
            "旧库缺少表 {table}"
        );
    }
    // Schema 名称由部署决定，不属于列定义；剔除后指纹可跨临时 schema 重现。
    let mut columns = columns;
    for column in &mut columns {
        if let Some(column) = column.as_object_mut() {
            column.remove("table_catalog");
            column.remove("table_schema");
        }
    }
    let fingerprint = hex::encode(Sha256::digest(serde_json::to_vec(&columns)?));
    let accounts = sqlx::query_scalar::<_, Value>(
        "SELECT to_jsonb(t) FROM external_upstream_pools t WHERE deleted_at IS NULL ORDER BY name, id",
    ).fetch_all(&mut *tx).await?;
    let runtime_config =
        sqlx::query_scalar::<_, Value>("SELECT config FROM runtime_config WHERE id = 'default'")
            .fetch_optional(&mut *tx)
            .await?
            .context("旧库缺少 runtime_config 的 default 配置")?;
    let route_scenarios = sqlx::query_scalar::<_, Value>(
        "SELECT jsonb_build_object('name', name, 'description', description, 'content', content) \
         FROM route_scenarios ORDER BY name, id",
    )
    .fetch_all(&mut *tx)
    .await?;
    let model_pricing =
        sqlx::query_scalar::<_, Value>("SELECT to_jsonb(t) FROM model_pricing t ORDER BY model")
            .fetch_all(&mut *tx)
            .await?;
    let model_capabilities = sqlx::query_scalar::<_, Value>(
        "SELECT to_jsonb(t) FROM model_capabilities t WHERE source = 'manual' ORDER BY model",
    )
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    let mut package = LegacyPackage {
        format_version: 1,
        exported_at: Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        exported_from: ExportedFrom {
            kind: "kiro-rs-postgres".into(),
            schema_fingerprint: fingerprint,
            secrets_redacted: false,
        },
        accounts,
        runtime_config,
        route_scenarios,
        model_pricing,
        model_capabilities,
    };
    if redact_secrets {
        redact_package(&mut package);
    }
    Ok(package)
}

pub(super) fn redact_package(package: &mut LegacyPackage) {
    package.exported_from.secrets_redacted = true;
    for account in &mut package.accounts {
        if let Some(key) = account.get_mut("api_key") {
            *key = Value::String("<redacted>".into());
        }
    }
    fn hide(value: &mut Value) {
        match value {
            Value::String(text) => *text = "<redacted>".into(),
            Value::Array(items) => {
                for item in items {
                    hide(item);
                }
            }
            Value::Object(fields) => {
                for value in fields.values_mut() {
                    hide(value);
                }
            }
            _ => {}
        }
    }
    fn redact(value: &mut Value) {
        match value {
            Value::Object(fields) => {
                for (key, value) in fields {
                    let key = key.to_ascii_lowercase();
                    if key.contains("key") || key.contains("password") || key == "url" {
                        hide(value);
                    } else {
                        redact(value);
                    }
                }
            }
            Value::Array(items) => {
                for item in items {
                    redact(item);
                }
            }
            _ => {}
        }
    }
    redact(&mut package.runtime_config);
}
