//! Kiro Web Portal CBOR operations. These calls are explicit and separate from REST usage.

use std::{fmt, io::Cursor};

use anyhow::Context;
use reqwest::{Client, Response, header};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::{
    http_client::{ProxyConfig, build_client},
    kiro::endpoint::configured_upstream_url,
    kiro::{machine_id, model::credentials::KiroCredentials},
    model::config::Config,
};

const PORTAL_OPERATION_BASE: &str = "https://app.kiro.dev/service/KiroWebPortalService/operation";
const PORTAL_SDK_VERSION: &str = "1.0.34";

#[derive(Debug)]
pub(crate) struct PortalHttpError {
    operation: String,
    status: reqwest::StatusCode,
    details: Option<String>,
}

impl fmt::Display for PortalHttpError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "portal {} failed with HTTP {}",
            self.operation, self.status
        )?;
        if let Some(details) = &self.details {
            write!(formatter, ": {details}")?;
        }
        Ok(())
    }
}

impl std::error::Error for PortalHttpError {}

pub(crate) fn is_auth_or_permission_error(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<PortalHttpError>()
        .is_some_and(|error| matches!(error.status.as_u16(), 401 | 403))
}

/// Identity returned by the portal `GetUserInfo` operation.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PortalUserInfo {
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub idp: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub feature_flags: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct UserInfoRequest {
    origin: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct UsageAndLimitsRequest {
    is_email_required: bool,
    origin: &'static str,
}

#[derive(Deserialize)]
struct PortalErrorBody {
    #[serde(rename = "__type")]
    error_type: Option<String>,
    message: Option<String>,
}

/// Reusable client for explicit Kiro Portal CBOR calls.
pub struct KiroPortalClient {
    client: Client,
    operation_base: String,
    x_amz_user_agent: String,
}

impl KiroPortalClient {
    /// Build a client using the account proxy, configured TLS backend, machine identity and
    /// active Kiro IDE version. Pass the account's actual IdP value to each operation; `None`
    /// uses the Portal convention `BuilderId`.
    pub fn for_credentials(
        credentials: &KiroCredentials,
        config: &Config,
        proxy: Option<&ProxyConfig>,
    ) -> anyhow::Result<Self> {
        let mut request_config = config.clone();
        request_config.snapshot_kiro_version_for_request();
        let machine_id = machine_id::generate_from_credentials(credentials, &request_config);
        let client = build_client(
            proxy,
            request_config.kiro_upstream_response_timeout_secs,
            request_config.tls_backend,
        )?;

        Ok(Self {
            client,
            operation_base: configured_upstream_url(&request_config, "")
                .map(|base| base.trim_end_matches('/').to_string())
                .unwrap_or_else(|| PORTAL_OPERATION_BASE.to_string()),
            x_amz_user_agent: format!(
                "aws-sdk-js/{PORTAL_SDK_VERSION} KiroIDE-{}-{}",
                request_config.effective_kiro_version(),
                machine_id
            ),
        })
    }

    /// Query account identity through portal `GetUserInfo`.
    pub async fn get_user_info(
        &self,
        access_token: &str,
        idp: Option<&str>,
    ) -> anyhow::Result<PortalUserInfo> {
        self.call(
            "GetUserInfo",
            access_token,
            idp,
            &UserInfoRequest { origin: "KIRO_IDE" },
        )
        .await
    }

    /// Query usage through portal `GetUserUsageAndLimits`.
    pub async fn get_user_usage_and_limits(
        &self,
        access_token: &str,
        idp: Option<&str>,
    ) -> anyhow::Result<crate::kiro::model::usage_limits::UsageLimitsResponse> {
        self.call(
            "GetUserUsageAndLimits",
            access_token,
            idp,
            &UsageAndLimitsRequest {
                is_email_required: true,
                origin: "KIRO_IDE",
            },
        )
        .await
    }

    async fn call<T: DeserializeOwned, B: Serialize>(
        &self,
        operation: &str,
        access_token: &str,
        idp: Option<&str>,
        body: &B,
    ) -> anyhow::Result<T> {
        let idp = idp
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("BuilderId");
        let mut encoded_body = Vec::new();
        ciborium::ser::into_writer(body, &mut encoded_body)
            .context("encode portal CBOR request")?;

        let response = self
            .client
            .post(format!("{}/{operation}", self.operation_base))
            .header(header::ACCEPT, "application/cbor")
            .header(header::CONTENT_TYPE, "application/cbor")
            .header("smithy-protocol", "rpc-v2-cbor")
            .header("amz-sdk-invocation-id", uuid::Uuid::new_v4().to_string())
            .header("amz-sdk-request", "attempt=1; max=1")
            .header("x-amz-user-agent", &self.x_amz_user_agent)
            .header(header::AUTHORIZATION, format!("Bearer {access_token}"))
            .header(
                header::COOKIE,
                format!("Idp={idp}; AccessToken={access_token}"),
            )
            .body(encoded_body)
            .send()
            .await
            .with_context(|| format!("send portal {operation} request"))?;

        decode_response(operation, response).await
    }
}

async fn decode_response<T: DeserializeOwned>(
    operation: &str,
    response: Response,
) -> anyhow::Result<T> {
    let status = response.status();
    let bytes = response.bytes().await?;
    if !status.is_success() {
        let details = ciborium::de::from_reader::<PortalErrorBody, _>(Cursor::new(&bytes))
            .ok()
            .map(|error| {
                let error_type = error
                    .error_type
                    .as_deref()
                    .and_then(|value| value.rsplit('#').next());
                [error_type, error.message.as_deref()]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(": ")
            })
            .filter(|details| !details.is_empty());
        return Err(PortalHttpError {
            operation: operation.to_string(),
            status,
            details,
        }
        .into());
    }

    // Portal 把整数值写进浮点字段（`currentOverages: 0`），日期包在 tag 1 里；
    // 先转成 JSON 值再反序列化，数字类型按 serde_json 的规则放宽。
    let value: ciborium::Value = ciborium::de::from_reader(Cursor::new(bytes))
        .with_context(|| format!("decode portal {operation} CBOR response"))?;
    serde_json::from_value(cbor_to_json(value))
        .with_context(|| format!("decode portal {operation} CBOR response"))
}

fn cbor_to_json(value: ciborium::Value) -> serde_json::Value {
    use ciborium::Value as Cbor;
    use serde_json::Value as Json;

    match value {
        Cbor::Integer(integer) => {
            let integer = i128::from(integer);
            i64::try_from(integer)
                .map(Json::from)
                .or_else(|_| u64::try_from(integer).map(Json::from))
                .unwrap_or_else(|_| Json::from(integer as f64))
        }
        Cbor::Float(float) => serde_json::Number::from_f64(float)
            .map(Json::Number)
            .unwrap_or(Json::Null),
        Cbor::Text(text) => Json::String(text),
        Cbor::Bool(flag) => Json::Bool(flag),
        Cbor::Null => Json::Null,
        Cbor::Tag(_, inner) => cbor_to_json(*inner),
        Cbor::Array(items) => Json::Array(items.into_iter().map(cbor_to_json).collect()),
        Cbor::Map(entries) => Json::Object(
            entries
                .into_iter()
                .filter_map(|(key, value)| match key {
                    Cbor::Text(key) => Some((key, cbor_to_json(value))),
                    _ => None,
                })
                .collect(),
        ),
        Cbor::Bytes(_) => Json::Null,
        _ => Json::Null,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::{
        Router,
        body::Bytes,
        extract::{Path, State},
        http::{HeaderMap, StatusCode},
        response::IntoResponse,
        routing::post,
    };
    use serde_json::{Value, json};
    use tokio::net::TcpListener;

    use super::*;

    #[derive(Clone, Default)]
    struct CapturedRequests(Arc<Mutex<Vec<(String, HeaderMap, Value)>>>);

    async fn portal_handler(
        State(captured): State<CapturedRequests>,
        Path(operation): Path<String>,
        headers: HeaderMap,
        body: Bytes,
    ) -> impl IntoResponse {
        let decoded: Value = ciborium::de::from_reader(Cursor::new(body)).unwrap();
        captured
            .0
            .lock()
            .unwrap()
            .push((operation.clone(), headers, decoded));

        let response = match operation.as_str() {
            "GetUserInfo" => json!({
                "email": "person@example.invalid",
                "userId": "user-1",
                "idp": "Google",
                "status": "ACTIVE",
                "featureFlags": ["flag-a"]
            }),
            "GetUserUsageAndLimits" => json!({
                "subscriptionInfo": {"subscriptionTitle": "KIRO FREE"},
                "usageBreakdownList": [{"currentUsage": 12, "usageLimit": 100}]
            }),
            _ => unreachable!(),
        };
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&response, &mut bytes).unwrap();
        (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/cbor")],
            bytes,
        )
    }

    #[tokio::test]
    async fn portal_usage_decodes_integer_floats_and_tagged_dates() {
        use ciborium::Value as Cbor;

        let text = |value: &str| Cbor::Text(value.to_string());
        let date = || Cbor::Tag(1, Box::new(Cbor::Float(1796083200.0)));
        // 与真实 GetUserUsageAndLimits 响应同形：浮点字段给整数，日期包在 tag 1 里。
        let response = Cbor::Map(vec![
            (text("nextDateReset"), date()),
            (
                text("subscriptionInfo"),
                Cbor::Map(vec![(text("subscriptionTitle"), text("KIRO FREE"))]),
            ),
            (
                text("usageBreakdownList"),
                Cbor::Array(vec![Cbor::Map(vec![
                    (text("currentOverages"), Cbor::Integer(0.into())),
                    (text("currentUsage"), Cbor::Integer(0.into())),
                    (text("currentUsageWithPrecision"), Cbor::Float(0.43)),
                    (text("nextDateReset"), date()),
                    (text("overageCap"), Cbor::Integer(10000.into())),
                    (text("overageRate"), Cbor::Float(0.04)),
                    (text("usageLimit"), Cbor::Integer(50.into())),
                    (text("usageLimitWithPrecision"), Cbor::Float(50.0)),
                    (text("bonuses"), Cbor::Array(vec![])),
                ])]),
            ),
        ]);
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&response, &mut bytes).unwrap();

        let app = Router::new().route(
            "/{operation}",
            post(move || {
                let bytes = bytes.clone();
                async move {
                    (
                        StatusCode::OK,
                        [(header::CONTENT_TYPE, "application/cbor")],
                        bytes,
                    )
                }
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let usage = test_client(format!("http://{address}"))
            .get_user_usage_and_limits("access-token", Some("Google"))
            .await
            .unwrap();
        server.abort();

        assert_eq!(usage.subscription_title(), Some("KIRO FREE"));
        assert_eq!(usage.next_date_reset, Some(1796083200.0));
        let breakdown = &usage.usage_breakdown_list[0];
        assert_eq!(breakdown.usage_limit, 50);
        assert_eq!(breakdown.current_overages, 0.0);
        assert_eq!(breakdown.overage_cap, 10000.0);
        assert_eq!(breakdown.current_usage_with_precision, 0.43);
    }

    fn test_client(base: String) -> KiroPortalClient {
        KiroPortalClient {
            client: Client::new(),
            operation_base: base,
            x_amz_user_agent: "aws-sdk-js/1.0.34 KiroIDE-1.4.121-machine".to_string(),
        }
    }

    #[tokio::test]
    async fn portal_operations_encode_expected_headers_and_cbor_bodies() {
        let captured = CapturedRequests::default();
        let app = Router::new()
            .route("/{operation}", post(portal_handler))
            .with_state(captured.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = test_client(format!("http://{address}"));

        let user = client
            .get_user_info("access-token", Some("Google"))
            .await
            .unwrap();
        assert_eq!(user.email.as_deref(), Some("person@example.invalid"));
        assert_eq!(user.idp.as_deref(), Some("Google"));
        assert_eq!(user.feature_flags, ["flag-a"]);

        let usage = client
            .get_user_usage_and_limits("access-token", None)
            .await
            .unwrap();
        assert_eq!(usage.subscription_title(), Some("KIRO FREE"));
        assert_eq!(usage.usage_breakdown_list[0].usage_limit, 100);

        server.abort();
        let requests = captured.0.lock().unwrap();
        assert_eq!(requests.len(), 2);

        let (operation, headers, body) = &requests[0];
        assert_eq!(operation, "GetUserInfo");
        assert_eq!(body, &json!({"origin": "KIRO_IDE"}));
        assert_eq!(headers[header::ACCEPT], "application/cbor");
        assert_eq!(headers[header::CONTENT_TYPE], "application/cbor");
        assert_eq!(headers["smithy-protocol"], "rpc-v2-cbor");
        assert_eq!(headers["amz-sdk-request"], "attempt=1; max=1");
        assert_eq!(
            headers["x-amz-user-agent"],
            "aws-sdk-js/1.0.34 KiroIDE-1.4.121-machine"
        );
        assert_eq!(headers[header::AUTHORIZATION], "Bearer access-token");
        assert_eq!(
            headers[header::COOKIE],
            "Idp=Google; AccessToken=access-token"
        );
        uuid::Uuid::parse_str(headers["amz-sdk-invocation-id"].to_str().unwrap()).unwrap();

        let (operation, _, body) = &requests[1];
        assert_eq!(operation, "GetUserUsageAndLimits");
        assert_eq!(
            body,
            &json!({"isEmailRequired": true, "origin": "KIRO_IDE"})
        );
    }
}
