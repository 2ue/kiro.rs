use super::*;
use crate::model::config::Config;
use legacy_import::{prepare_import, validate_config};
use serde_json::{Value, json};

fn sample() -> LegacyPackage {
    serde_json::from_str(include_str!("fixtures/legacy-sample.json")).unwrap()
}

#[test]
fn migration_report_and_config_cover_all_mappings() {
    let package = sample();
    let plan = prepare_import(&package, &Config::default());
    let text = plan.report.render(false);
    for expected in [
        "dry-run",
        "name/model 自然键",
        "request_body_mode",
        "raw_model_mode",
        "preserve_path",
        "skip_non_stream_usage_projection",
        "auto_disabled_last_error",
        "revision",
        ".id = 101",
        "processed_mapping → passthrough_mapping",
        "该模式已删除",
        "[保留旧值]",
        "usage_projection_mode",
        "externalPoolRetryMaxAttempts",
        "externalDirectPolicyEnabled",
        "directExternal",
        "localPool",
        "fallbackOn",
        "externalPoolLocalRescue",
        "externalPoolMaxInputTokens",
        "externalPoolTransientFailureCooldownThreshold",
        "cachePolicy.pathOverrides",
        "reportedUsage.pathOverrides",
        "promptInjection.pathOverrides",
        "routeFeatures.pathOverrides",
        "definedCacheRoutes",
        "accounts.accountRouteMode",
        "accounts.accountRouteRules",
        "kiroRsTool",
        "prefixTool",
        "prefix_tool_plus",
        "cachePoint",
        "cacheTtl.local",
        "externalPool=false",
        "导入后会对账号生效",
        "missingMaxTokens",
        "reasoningCompat",
        "payloadGuardEnabled",
        "payloadTooLongHandling",
        "payloadShaping",
        "imageProcessing",
        "Kiro/旧顶层字段",
        "region",
        "credentialRpm",
        "apiKey",
        "apiKeys",
        "requestApiKeyPolicies",
        "adminApiKey 会被全量覆盖",
        "机器本地字段",
        "postgres",
        "redis",
        "observabilityRedis",
        "host",
        "port",
        "content.local",
        "content.localAccounts",
        "model_pricing",
        "model_capabilities",
        "listed=true",
        "并发总量：20",
        "请求体不再规范化",
        "maxQueuedRequests",
        "capacityMode",
        "dispatchMaxWaitSecs",
        "请确认哪些路由要开",
    ] {
        assert!(text.contains(expected), "报告缺少 {expected}\n{text}");
    }
    let config = plan
        .ensure_applicable()
        .unwrap_or_else(|e| panic!("{e}\n{text}"));
    validate_config(config).unwrap();
    assert!(
        config
            .route_features
            .defaults
            .payload_guard
            .unwrap()
            .enabled
    );
    assert_eq!(config.accounts.retry_max_attempts, 3);
    assert_eq!(config.api_key.as_deref(), Some("fixture-request-key"));
    assert!(config.postgres.url.is_none());
    assert_eq!(
        plan.accounts[0]["model_mapping_mode"],
        "passthrough_mapping"
    );
    assert_eq!(plan.accounts[0]["usage_projection_mode"], "pass_through");
    assert_eq!(plan.capabilities[0]["listed"], true);
    assert!(plan.scenarios[0]["content"].get("local").is_none());
    assert!(
        config
            .routes
            .routes
            .iter()
            .any(|r| r.prefix == "/dfcache/sample")
    );
}

#[test]
fn migration_mapping_is_deterministic_and_preserves_deployment() {
    let package = sample();
    let mut deployment = Config::default();
    deployment.host = "target-host".into();
    deployment.port = 9001;
    deployment.postgres.url = Some("postgres://target/config".into());
    let a = prepare_import(&package, &deployment);
    let b = prepare_import(&package, &deployment);
    assert_eq!(
        serde_json::to_value(a.ensure_applicable().unwrap()).unwrap(),
        serde_json::to_value(b.ensure_applicable().unwrap()).unwrap()
    );
    assert_eq!(a.accounts, b.accounts);
    assert_eq!(a.scenarios, b.scenarios);
    assert_eq!(a.report.render(false), b.report.render(false));
    assert_eq!(a.config.unwrap().host, "target-host");
}

#[test]
fn migration_redacted_and_invalid_packages_refuse_apply() {
    let mut package = sample();
    legacy_export::redact_package(&mut package);
    assert_eq!(package.accounts[0]["api_key"], "<redacted>");
    assert!(
        prepare_import(&package, &Config::default())
            .ensure_applicable()
            .unwrap_err()
            .to_string()
            .contains("脱敏包")
    );
    let mut package = sample();
    package.runtime_config["externalPools"]["externalPoolRetryMaxAttempts"] = json!(10001);
    assert!(
        prepare_import(&package, &Config::default())
            .ensure_applicable()
            .is_err()
    );
    let mut package = sample();
    package.runtime_config["routes"] = json!([{"prefix":"/bad/../route"}]);
    assert!(
        prepare_import(&package, &Config::default())
            .ensure_applicable()
            .is_err()
    );
    let mut package = sample();
    package.accounts.push(package.accounts[0].clone());
    assert!(
        prepare_import(&package, &Config::default())
            .ensure_applicable()
            .is_err()
    );
}

#[test]
fn migration_explicit_capacity_and_feature_off_are_preserved() {
    let mut package = sample();
    package.runtime_config["externalPools"]["externalPoolCapacityMode"] = json!("fail_fast");
    package.runtime_config["externalPools"]["externalPoolDispatchMaxWaitSecs"] = json!(5);
    package.runtime_config["externalPools"]["externalPoolMaxQueuedRequests"] = json!(12);
    package.runtime_config["payloadGuardEnabled"] = json!(false);
    package.runtime_config["payloadGuardExternalEnabled"] = json!(false);
    let plan = prepare_import(&package, &Config::default());
    let config = plan.ensure_applicable().unwrap();
    assert!(config.route_features.defaults.payload_guard.is_none());
    assert_eq!(config.accounts.dispatch_max_wait_secs, 5);
    assert!(plan.report.changed_defaults.is_empty());
}

#[tokio::test]
async fn migration_postgres_roundtrip_overwrite_idempotence_and_rollback() {
    let Ok(url) = std::env::var("CLAUDE_POOL_TEST_POSTGRES_URL") else {
        return;
    };
    if url.trim().is_empty() {
        return;
    }
    let mut config = Config::default();
    config.postgres.url = Some(url.clone());
    let target = crate::storage::postgres::PostgresStore::connect_test(&config)
        .await
        .unwrap();
    let schema = format!("migration_legacy_{}", uuid::Uuid::new_v4().simple());
    let source = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .unwrap();
    let result = async {
        sqlx::query(&format!("CREATE SCHEMA {schema}")).execute(&source).await?;
        sqlx::query(&format!("SET search_path TO {schema}")).execute(&source).await?;
        let sample = sample();
        // 手工旧 schema 包含新模型里已删除的列，用于证明导出完全不依赖新 Rust 类型。
        sqlx::raw_sql(r#"
            CREATE TABLE external_upstream_pools (
                id bigint, revision bigint, name text, base_url text, api_key text, auth_type text,
                header_profile text, append_beta_query boolean, header_overrides jsonb, wire_profile text,
                tls_profile text, enabled boolean, priority integer, max_concurrent_requests integer,
                usage_projection_mode text, stream_response_mode text, auto_disable_policy text,
                pre_output_stream_retry_mode text, normalize_model_version_dots boolean,
                model_mapping_mode text, model_mapping_require_match boolean, model_mapping_rules jsonb,
                supported_models jsonb, route_mode text, route_rules jsonb, notes text,
                request_body_mode text, raw_model_mode text, preserve_path boolean,
                skip_non_stream_usage_projection boolean, auto_disabled boolean, auto_disabled_reason text,
                auto_disabled_at timestamptz, auto_disabled_until timestamptz, auto_disabled_last_error text,
                created_at timestamptz, updated_at timestamptz, deleted_at timestamptz
            );
            CREATE TABLE runtime_config (id text, config jsonb);
            CREATE TABLE route_scenarios (id bigserial, name text, description text, content jsonb);
            CREATE TABLE model_pricing (
                model text,input_cost_per_token float8,output_cost_per_token float8,
                cache_creation_input_token_cost float8,cache_creation_1h_input_token_cost float8,
                cache_read_input_token_cost float8,source text,source_url text,last_synced_at text,last_error text,
                updated_at timestamptz
            );
            CREATE TABLE model_capabilities (
                model text,display_name text,description text,max_input_tokens integer,max_output_tokens integer,
                supports_prompt_caching boolean,supported_input_types text[],source text,last_synced_at text,
                last_error text,reasoning_cohort_enabled boolean,updated_at timestamptz
            );
        "#).execute(&source).await?;
        for account in &sample.accounts {
            sqlx::query("INSERT INTO external_upstream_pools SELECT * FROM jsonb_populate_record(NULL::external_upstream_pools,$1)")
                .bind(account).execute(&source).await?;
        }
        let mut deleted=sample.accounts[0].clone();
        deleted["id"]=json!(103);
        deleted["deleted_at"]=json!("2026-11-17T00:00:00Z");
        sqlx::query("INSERT INTO external_upstream_pools SELECT * FROM jsonb_populate_record(NULL::external_upstream_pools,$1)")
            .bind(deleted).execute(&source).await?;
        sqlx::query("INSERT INTO runtime_config VALUES ('default',$1)").bind(&sample.runtime_config).execute(&source).await?;
        for scenario in &sample.route_scenarios {
            sqlx::query("INSERT INTO route_scenarios (name,description,content) VALUES ($1,$2,$3)")
                .bind(scenario["name"].as_str()).bind(scenario["description"].as_str()).bind(&scenario["content"]).execute(&source).await?;
        }
        for row in &sample.model_pricing {
            sqlx::query("INSERT INTO model_pricing SELECT * FROM jsonb_populate_record(NULL::model_pricing,$1)").bind(row).execute(&source).await?;
        }
        for row in &sample.model_capabilities {
            sqlx::query("INSERT INTO model_capabilities SELECT * FROM jsonb_populate_record(NULL::model_capabilities,$1)").bind(row).execute(&source).await?;
        }
        sqlx::query("INSERT INTO model_capabilities (model,source) VALUES ('discovered','account_discovery')").execute(&source).await?;
        let package=legacy_export::read_package(&source,false).await?;
        assert_eq!(package.accounts.len(),2);
        let first = package.accounts.iter().find(|account| account["name"] == "旧账号甲").unwrap();
        assert_eq!(first["request_body_mode"],"normalized");
        assert_eq!(first["model_mapping_mode"],"processed_mapping");
        assert_eq!(package.runtime_config,sample.runtime_config);
        assert_eq!(package.model_capabilities.len(),1);
        assert_eq!(package.exported_from.schema_fingerprint.len(),64);
        let redacted=legacy_export::read_package(&source,true).await?;
        assert!(prepare_import(&redacted,&config).ensure_applicable().is_err());
        let plan=prepare_import(&package,&config);
        let imported=plan.ensure_applicable()?;
        let existing: crate::account::CreateAccountRequest=serde_json::from_value(json!({
            "name":"旧账号甲","baseUrl":"https://before.example","apiKey":"before-key"
        }))?;
        let existing=target.create_account_unmasked(existing).await?;
        let old_scenario=target.insert_route_scenario("旧场景",None,&json!({})).await?;
        target.insert_route_scenario("包外场景",None,&json!({})).await?;
        sqlx::query("INSERT INTO model_capabilities (model,display_name,source) VALUES ('keep-discovered','Keep','account_discovery'),('remove-manual','Remove','manual')").execute(target.pool()).await?;
        let apply=|| target.replace_configuration_transaction(imported,&plan.accounts,&plan.scenarios,&plan.pricing,&plan.capabilities);
        let v1=apply().await?;
        let rows=target.list_accounts(false).await?;
        assert_eq!(rows.len(),2);
        assert_eq!(rows.iter().find(|row|row.name=="旧账号甲").unwrap().id,existing.id);
        assert!(rows.iter().all(|row|!row.auto_disabled));
        let scenarios=target.list_route_scenarios().await?;
        assert_eq!(scenarios.len(),1);
        assert_eq!(scenarios[0].id,old_scenario.id);
        let persisted=target.load_runtime_config().await?.unwrap();
        validate_config(&persisted)?;
        assert_eq!(persisted.routes.routes.len(),imported.routes.routes.len());
        assert_eq!(persisted.accounts.retry_max_attempts,3);
        assert_eq!(persisted.api_key.as_deref(),Some("fixture-request-key"));
        let caps:Vec<(String,bool)>=sqlx::query_as("SELECT model,listed FROM model_capabilities ORDER BY model").fetch_all(target.pool()).await?;
        assert_eq!(caps,vec![("fixture-model".into(),true),("keep-discovered".into(),true)]);
        assert_eq!(target.load_model_pricing().await?.len(),1);
        let snapshot=|| async {
            sqlx::query_scalar::<_,Value>("SELECT to_jsonb(a)-'revision'-'updated_at'-'created_at' FROM accounts a ORDER BY name").fetch_all(target.pool()).await
        };
        let before=snapshot().await?;
        assert_eq!(apply().await?,v1+1);
        assert_eq!(snapshot().await?,before);
        let mut broken=plan.pricing.clone();
        broken[0]["source"]=Value::Null;
        assert!(target.replace_configuration_transaction(imported,&[],&[],&broken,&[]).await.is_err());
        assert_eq!(snapshot().await?,before);
        assert_eq!(target.list_route_scenarios().await?.len(),1);
        let stored_version:i64=sqlx::query_scalar("SELECT version FROM runtime_config WHERE id='default'").fetch_one(target.pool()).await?;
        assert_eq!(stored_version,v1+1);
        if let Ok(redis_url) = std::env::var("CLAUDE_POOL_TEST_REDIS_URL") {
            use futures::StreamExt;
            let mut deployment=config.clone();
            let target_schema:String=sqlx::query_scalar("SELECT current_schema()").fetch_one(target.pool()).await?;
            let mut target_url=url::Url::parse(&url)?;
            target_url.query_pairs_mut().append_pair("options",&format!("-csearch_path={target_schema}"));
            deployment.postgres.url=Some(target_url.to_string());
            deployment.redis.url=Some(redis_url);
            deployment.redis.key_prefix=format!("migration_test_{}",uuid::Uuid::new_v4().simple());
            let redis=crate::storage::redis_cache::RedisStore::connect(&deployment).await?;
            let mut subscriber=redis.subscribe_runtime_events().await?;
            let path=std::env::temp_dir().join(format!("migration-{}.json",uuid::Uuid::new_v4().simple()));
            std::fs::write(&path,serde_json::to_vec(&package)?)?;
            let imported=super::import_legacy(&path,true,&deployment).await;
            std::fs::remove_file(&path)?;
            imported?;
            let mut events=subscriber.on_message();
            let mut channels=std::collections::BTreeSet::new();
            for _ in 0..2 {
                let event=tokio::time::timeout(std::time::Duration::from_secs(3),events.next()).await?.unwrap();
                channels.insert(event.get_channel_name().to_string());
            }
            assert!(channels.contains(&redis.runtime_config_changed_channel()));
            assert!(channels.contains(&redis.account_data_changed_channel()));
            assert_eq!(target.list_accounts(false).await?.len(),2);
        }
        anyhow::Ok(())
    }.await;
    let drop_source = sqlx::query(&format!("DROP SCHEMA IF EXISTS {schema} CASCADE"))
        .execute(&source)
        .await;
    source.close().await;
    let drop_target = target.drop_test_schema().await;
    result.unwrap();
    drop_source.unwrap();
    drop_target.unwrap();
}

#[test]
fn migration_empty_redacted_package_and_malformed_models_are_rejected() {
    let mut package = sample();
    package.accounts.clear();
    package.runtime_config = json!({});
    legacy_export::redact_package(&mut package);
    assert!(
        prepare_import(&package, &Config::default())
            .ensure_applicable()
            .is_err()
    );
    let mut package = sample();
    package.model_pricing[0]["input_cost_per_token"] = json!(-1);
    assert!(
        prepare_import(&package, &Config::default())
            .ensure_applicable()
            .is_err()
    );
    let mut package = sample();
    package.model_capabilities[0]["supported_input_types"] = json!("TEXT");
    assert!(
        prepare_import(&package, &Config::default())
            .ensure_applicable()
            .is_err()
    );
}

#[test]
fn migration_real_legacy_names_and_cache_ttl_are_preserved() {
    let mut package = sample();
    package.runtime_config["externalPools"]["externalPoolsEnabled"] = json!(false);
    package.runtime_config["promptCacheMaxEntriesPerCredential"] = json!(32);
    let plan = prepare_import(&package, &Config::default());
    let config = plan.ensure_applicable().unwrap();
    assert!(!config.accounts.enabled);
    assert_eq!(config.prompt_cache_max_entries_per_account, 32);
    let value = serde_json::to_value(config).unwrap();
    assert_eq!(
        value["cachePolicy"]["prefixTool"]["cacheTtl"]["account"],
        "upstream_then_request_default_5m"
    );
}
