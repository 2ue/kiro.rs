use chrono::Utc;
use serde_json::Value;

use super::KiroProvider;
use crate::http_client::{
    response_text_with_limit_and_body_timeout, send_with_response_header_timeout,
};
use crate::kiro::endpoint::configured_upstream_url;
use crate::kiro::machine_id;
use crate::kiro::model::credentials::KiroCredentials;
use crate::kiro::protocol::{
    KIRO_BUILDER_ID_PLACEHOLDER_ARN, KIRO_SOCIAL_PROFILE_ARN, is_social_credentials,
};

/// 创建结果由管理层保存到独立 Key 表，不能写回来源账号认证字段。
#[derive(Clone)]
pub struct KiroApiKeyCreationResult {
    pub api_key: String,
    pub label: String,
    pub created_at: i64,
    pub region: String,
    pub key_id: String,
    pub key_prefix: String,
    pub profile_arn: String,
}

fn profile_candidates(credentials: &KiroCredentials) -> Vec<String> {
    let mut candidates = Vec::with_capacity(2);
    if let Some(arn) = credentials
        .profile_arn
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        candidates.push(arn.to_string());
    }
    let fallback = if is_social_credentials(credentials) {
        KIRO_SOCIAL_PROFILE_ARN
    } else {
        KIRO_BUILDER_ID_PLACEHOLDER_ARN
    };
    if !candidates.iter().any(|arn| arn == fallback) {
        candidates.push(fallback.to_string());
    }
    candidates
}

fn parse_created_key(
    body: &str,
    label: &str,
    region: &str,
) -> anyhow::Result<KiroApiKeyCreationResult> {
    // 不把反序列化错误的原始内容放进管理错误，响应可能包含明文 Key。
    let payload: Value = serde_json::from_str(body).map_err(|_| {
        anyhow::anyhow!("上游返回的创建结果不是有效 JSON；请检查官方密钥列表后再决定是否重试")
    })?;
    let object = payload
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("上游返回的创建结果不是 JSON 对象"))?;
    let api_key = ["rawKey", "apiKey", "apiKeyValue", "secretKey", "value"]
        .iter()
        .filter_map(|field| object.get(*field).and_then(Value::as_str))
        .find(|value| value.starts_with("ksk_") && value.len() > 4)
        .ok_or_else(|| {
            anyhow::anyhow!("上游已响应成功，但没有完整 rawKey；请检查官方密钥列表后再决定是否重试")
        })?;
    if object
        .get("keyPrefix")
        .and_then(Value::as_str)
        .is_some_and(|prefix| api_key.len() <= prefix.len())
    {
        anyhow::bail!("上游只返回了 Key 前缀，没有完整明文");
    }
    let created_at = object
        .get("createdAt")
        .and_then(|value| {
            value
                .as_f64()
                .or_else(|| value.as_str()?.parse::<f64>().ok())
        })
        .filter(|value| value.is_finite() && *value > 0.0)
        .map(|value| if value > 1e12 { value } else { value * 1000.0 })
        .map(|value| value.round() as i64)
        .unwrap_or_else(|| Utc::now().timestamp_millis());
    Ok(KiroApiKeyCreationResult {
        api_key: api_key.to_string(),
        label: object
            .get("label")
            .and_then(Value::as_str)
            .unwrap_or(label)
            .to_string(),
        created_at,
        region: region.to_string(),
        key_id: object
            .get("keyId")
            .or_else(|| object.get("id"))
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_default()
            .to_string(),
        key_prefix: object
            .get("keyPrefix")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| api_key.chars().take(12).collect()),
        profile_arn: String::new(),
    })
}

impl KiroProvider {
    pub async fn create_kiro_api_key_for_credential(
        &self,
        credential_id: u64,
        label: &str,
    ) -> anyhow::Result<KiroApiKeyCreationResult> {
        let label = label.trim();
        if label.is_empty() || label.chars().count() > 64 {
            anyhow::bail!("密钥名称须为 1–64 个字符");
        }
        let (body, region, profile_arn) = self
            .invoke_kiro_key_control(
                credential_id,
                "CreateApiKey",
                serde_json::json!({"label": label}),
                None,
            )
            .await?;
        let mut result = parse_created_key(&body, label, &region)?;
        result.profile_arn = profile_arn;
        Ok(result)
    }

    pub async fn delete_kiro_api_key_for_credential(
        &self,
        credential_id: u64,
        key_id: &str,
        region: &str,
        profile_arn: &str,
    ) -> anyhow::Result<()> {
        if key_id.trim().is_empty() {
            anyhow::bail!("缺少官方 Key ID，不能撤销");
        }
        self.invoke_kiro_key_control(
            credential_id,
            "DeleteApiKey",
            serde_json::json!({"keyId": key_id}),
            Some((region, profile_arn)),
        )
        .await?;
        Ok(())
    }

    async fn invoke_kiro_key_control(
        &self,
        credential_id: u64,
        operation: &str,
        extra: Value,
        stored: Option<(&str, &str)>,
    ) -> anyhow::Result<(String, String, String)> {
        let mut config = self
            .token_manager
            .runtime_config_snapshot()
            .as_ref()
            .clone();
        config.snapshot_kiro_version_for_request();
        let version = config.effective_kiro_version();
        let mut ctx = self
            .token_manager
            .acquire_context_for_credential_with_version(credential_id, Some(&version))
            .await?;
        if ctx.credentials.is_api_key_credential() {
            anyhow::bail!("该账号本身已经是 Kiro API Key");
        }
        let machine_id = machine_id::generate_from_credentials(&ctx.credentials, &config);
        self.ensure_profile_arn_for_context(&mut ctx, &config, &machine_id, None)
            .await;
        let inferred_region = if ctx
            .credentials
            .effective_api_region(&config)
            .starts_with("eu-")
        {
            "eu-central-1"
        } else {
            "us-east-1"
        };
        let region = stored.map(|(region, _)| region).unwrap_or(inferred_region);
        if !matches!(region, "us-east-1" | "eu-central-1") {
            anyhow::bail!("无效的官方 Key 服务区域");
        }
        let host = format!("management.{region}.kiro.dev");
        let endpoint =
            configured_upstream_url(&config, "").unwrap_or_else(|| format!("https://{host}/"));
        let mut candidates = profile_candidates(&ctx.credentials);
        if let Some((_, profile)) = stored.filter(|(_, profile)| !profile.is_empty()) {
            candidates.retain(|candidate| candidate != profile);
            candidates.insert(0, profile.to_string());
        }
        let mut refreshed = false;
        for (index, profile_arn) in candidates.iter().enumerate() {
            loop {
                let mut body = extra.clone();
                body["profileArn"] = Value::String(profile_arn.clone());
                let request = self.client_for(&ctx.credentials)?.post(&endpoint)
                    .header("accept", "*/*")
                    .header("content-type", "application/json")
                    .header("authorization", format!("Bearer {}", ctx.token))
                    .header("x-amz-target", format!("KiroControlPlaneBearerService.{operation}"))
                    .header("user-agent", format!(
                        "aws-sdk-rust/1.3.15 ua/2.1 api/codewhispererruntime/0.1.16551 os/{} lang/rust/1.92.0 md/appVersion-{} app/AmazonQ-For-CLI",
                        config.system_version, version,
                    ))
                    .header("x-amz-user-agent", format!(
                        "aws-sdk-rust/1.3.15 ua/2.1 api/codewhispererruntime/0.1.16551 os/{} lang/rust/1.92.0 m/F,C app/AmazonQ-For-CLI",
                        config.system_version,
                    ))
                    .header("amz-sdk-invocation-id", uuid::Uuid::new_v4().to_string())
                    .header("amz-sdk-request", "attempt=1; max=1")
                    .header("host", &host)
                    .json(&body);
                // CreateApiKey 没有幂等键；网络或响应读取失败不能自动重复创建。
                let response = send_with_response_header_timeout(
                    request,
                    config.kiro_upstream_response_timeout_secs,
                )
                .await
                .map_err(|_| {
                    anyhow::anyhow!("官方 Key 操作请求失败或超时；未自动重试，请刷新记录核对结果")
                })?;
                let status = response.status();
                let body = response_text_with_limit_and_body_timeout(
                    response,
                    config.kiro_upstream_response_timeout_secs,
                    1024 * 1024,
                )
                .await
                .map_err(|_| {
                    anyhow::anyhow!("读取官方 Key 操作结果失败；未自动重试，请刷新记录核对结果")
                })?;
                let already_deleted = operation == "DeleteApiKey"
                    && (status == reqwest::StatusCode::NOT_FOUND
                        || (status == reqwest::StatusCode::BAD_REQUEST
                            && serde_json::from_str::<Value>(&body)
                                .ok()
                                .and_then(|v| {
                                    v.get("__type").and_then(Value::as_str).map(str::to_string)
                                })
                                .is_some_and(|kind| kind.ends_with("ResourceNotFoundException"))));
                if status.is_success() || already_deleted {
                    return Ok((body, region.to_string(), profile_arn.clone()));
                }
                if status == reqwest::StatusCode::UNAUTHORIZED
                    && !refreshed
                    && ctx
                        .credentials
                        .refresh_token
                        .as_deref()
                        .is_some_and(|s| !s.trim().is_empty())
                {
                    refreshed = true;
                    self.token_manager
                        .force_refresh_token_for(credential_id)
                        .await?;
                    ctx = self
                        .token_manager
                        .acquire_context_for_credential_with_version(credential_id, Some(&version))
                        .await?;
                    if ctx.credentials.is_api_key_credential() {
                        anyhow::bail!("账号认证方式已变更，请重新选择账号");
                    }
                    continue;
                }
                let profile_rejected = status == reqwest::StatusCode::FORBIDDEN
                    || (status == reqwest::StatusCode::BAD_REQUEST
                        && body.to_lowercase().contains("profilearn"));
                if profile_rejected && index + 1 < candidates.len() {
                    break;
                }
                anyhow::bail!("官方 Key 操作失败（HTTP {}）", status.as_u16());
            }
        }
        anyhow::bail!("所有 Kiro Profile ARN 均被上游拒绝")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        extract::State,
        http::{HeaderMap, StatusCode},
        response::{IntoResponse, Response},
        routing::post,
    };
    use std::sync::{Arc, Mutex};

    #[test]
    fn official_kiro_key_parser_requires_full_raw_key_and_normalizes_timestamps() {
        for created_at in [
            serde_json::json!(1.78685640426e9),
            serde_json::json!("1786856404.26"),
            serde_json::json!(1786856404260i64),
        ] {
            let body = serde_json::json!({ "rawKey": "ksk_complete_test_value", "keyPrefix": "ksk_short", "createdAt": created_at }).to_string();
            let parsed = parse_created_key(&body, "label", "eu-central-1").unwrap();
            assert_eq!(parsed.api_key, "ksk_complete_test_value");
            assert_eq!(parsed.created_at, 1786856404260);
            assert_eq!(parsed.region, "eu-central-1");
        }
        for body in [
            r#"{"keyPrefix":"ksk_short"}"#,
            r#"{"rawKey":"ksk_short","keyPrefix":"ksk_short"}"#,
            "[]",
            "null",
            r#"{"rawKey":"ksk_secret_never_in_error", "#,
        ] {
            let error = parse_created_key(body, "label", "us-east-1")
                .err()
                .unwrap()
                .to_string();
            assert!(!error.contains("ksk_secret_never_in_error"));
        }
    }

    #[test]
    fn official_kiro_key_profile_candidates_follow_credential_identity() {
        let social = KiroCredentials {
            provider: Some("Google".to_string()),
            ..Default::default()
        };
        assert_eq!(profile_candidates(&social), vec![KIRO_SOCIAL_PROFILE_ARN]);
        let stored = KiroCredentials {
            profile_arn: Some("stored-arn".to_string()),
            auth_method: Some("idc".to_string()),
            ..Default::default()
        };
        assert_eq!(
            profile_candidates(&stored),
            vec!["stored-arn", KIRO_BUILDER_ID_PLACEHOLDER_ARN]
        );
        let blank = KiroCredentials {
            profile_arn: Some("  ".to_string()),
            ..Default::default()
        };
        assert_eq!(
            profile_candidates(&blank),
            vec![KIRO_BUILDER_ID_PLACEHOLDER_ARN]
        );
    }

    #[derive(Clone, Default)]
    struct FakeState {
        calls: Arc<Mutex<Vec<(String, String, Value)>>>,
        refreshes: Arc<std::sync::atomic::AtomicUsize>,
    }

    async fn create(
        State(state): State<FakeState>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> Response {
        let operation = headers["x-amz-target"].to_str().unwrap();
        assert!(matches!(
            operation,
            "KiroControlPlaneBearerService.CreateApiKey"
                | "KiroControlPlaneBearerService.DeleteApiKey"
        ));
        assert_eq!(headers["amz-sdk-request"], "attempt=1; max=1");
        let authorization = headers["authorization"].to_str().unwrap().to_string();
        state.calls.lock().unwrap().push((
            headers["host"].to_str().unwrap().to_string(),
            authorization.clone(),
            body.clone(),
        ));
        if operation.ends_with("DeleteApiKey") {
            assert_eq!(body["keyId"], "official-key-id");
            return StatusCode::NOT_FOUND.into_response();
        }
        if body["label"] == "failure" {
            return (
                StatusCode::TOO_MANY_REQUESTS,
                Json(serde_json::json!({"rawKey":"ksk_secret_never_in_error"})),
            )
                .into_response();
        }
        if authorization == "Bearer old-token" {
            return StatusCode::UNAUTHORIZED.into_response();
        }
        if body["profileArn"] == "stale-profile" {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"message":"Invalid profileArn"})),
            )
                .into_response();
        }
        Json(serde_json::json!({"keyId":"official-key-id", "rawKey":"ksk_complete_test_value", "keyPrefix":"ksk_short", "createdAt":1786856404.26})).into_response()
    }

    async fn refresh(State(state): State<FakeState>) -> Json<Value> {
        state
            .refreshes
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Json(
            serde_json::json!({"access_token":"new-token", "refresh_token":"r".repeat(150), "expires_in":3600}),
        )
    }

    #[tokio::test]
    async fn official_kiro_key_http_retries_only_rejections_and_preserves_scheduler_state() {
        use crate::kiro::token_manager::MultiTokenManager;
        use crate::model::config::Config;
        let state = FakeState::default();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new()
            .route("/", post(create))
            .route("/token", post(refresh))
            .with_state(state.clone());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut config = Config::default();
        config.kiro_upstream_base_url = Some(url.clone());
        config.kiro_upstream_response_timeout_secs = 2;
        let credential = KiroCredentials {
            id: Some(1),
            auth_method: Some("external_idp".to_string()),
            provider: Some("Enterprise".to_string()),
            access_token: Some("old-token".to_string()),
            refresh_token: Some("r".repeat(150)),
            expires_at: Some((Utc::now() + chrono::Duration::hours(1)).to_rfc3339()),
            token_endpoint: Some(format!("{url}/token")),
            client_id: Some("client".to_string()),
            profile_arn: Some("stale-profile".to_string()),
            api_region: Some("eu-west-1".to_string()),
            ..Default::default()
        };
        let key_credential = KiroCredentials {
            id: Some(2),
            auth_method: Some("api_key".to_string()),
            kiro_api_key: Some("ksk_existing".to_string()),
            ..Default::default()
        };
        let manager = Arc::new(
            MultiTokenManager::new(config, vec![credential, key_credential], None, None, false)
                .unwrap(),
        );
        let mut endpoints: std::collections::HashMap<
            String,
            Arc<dyn crate::kiro::endpoint::KiroEndpoint>,
        > = std::collections::HashMap::new();
        endpoints.insert(
            "ide".to_string(),
            Arc::new(crate::kiro::endpoint::IdeEndpoint),
        );
        let provider =
            KiroProvider::with_proxy(manager.clone(), None, endpoints, "ide".to_string());
        let before = manager.snapshot();
        let result = provider
            .create_kiro_api_key_for_credential(1, "created")
            .await
            .unwrap();
        assert_eq!(result.region, "eu-central-1");
        assert_eq!(result.api_key, "ksk_complete_test_value");
        {
            let calls = state.calls.lock().unwrap();
            assert_eq!(calls.len(), 3);
            assert_eq!(calls[0].1, "Bearer old-token");
            assert_eq!(calls[1].1, "Bearer new-token");
            assert_eq!(calls[2].2["profileArn"], KIRO_BUILDER_ID_PLACEHOLDER_ARN);
            assert!(
                calls
                    .iter()
                    .all(|call| call.0 == "management.eu-central-1.kiro.dev")
            );
        }
        assert_eq!(state.refreshes.load(std::sync::atomic::Ordering::SeqCst), 1);
        let error = provider
            .create_kiro_api_key_for_credential(1, "failure")
            .await
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("429"));
        assert!(!error.contains("ksk_secret_never_in_error"));
        assert_eq!(
            state.calls.lock().unwrap().len(),
            4,
            "429 must not retry or try another profile"
        );
        assert!(
            provider
                .create_kiro_api_key_for_credential(2, "skipped")
                .await
                .is_err()
        );
        assert_eq!(
            state.calls.lock().unwrap().len(),
            4,
            "API key account must not call upstream"
        );
        assert_eq!(result.key_id, "official-key-id");
        assert_eq!(result.profile_arn, KIRO_BUILDER_ID_PLACEHOLDER_ARN);
        provider
            .delete_kiro_api_key_for_credential(
                1,
                &result.key_id,
                &result.region,
                &result.profile_arn,
            )
            .await
            .unwrap();
        assert_eq!(state.calls.lock().unwrap().len(), 5);
        assert!(
            provider
                .delete_kiro_api_key_for_credential(
                    2,
                    &result.key_id,
                    &result.region,
                    &result.profile_arn
                )
                .await
                .is_err()
        );
        assert_eq!(state.calls.lock().unwrap().len(), 5);
        let after = manager.snapshot();
        assert_eq!(
            before.entries[0].scheduler_selection_count,
            after.entries[0].scheduler_selection_count
        );
        assert_eq!(
            before.entries[0].failure_count,
            after.entries[0].failure_count
        );
        assert_eq!(before.entries[0].disabled, after.entries[0].disabled);
        assert_eq!(
            before.global_in_flight_requests,
            after.global_in_flight_requests
        );
        assert!(manager.export_credentials().iter().all(|credential| {
            credential.kiro_api_key.as_deref() != Some("ksk_complete_test_value")
        }));
        server.abort();
    }
}
