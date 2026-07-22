use axum::{
    extract::{FromRequest, Request, rejection::BytesRejection},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use bytes::Bytes;

use crate::common::auth::RequestApiKeyIdentity;

use super::{
    envelope,
    request_admission::{RequestRejectionAttribution, RequestRejectionReason},
};

pub(crate) const MAX_MESSAGES_BODY_SIZE: usize = 50 * 1024 * 1024;
const BODY_LIMIT_MESSAGE: &str = "The request body exceeds the 50 MiB limit.";

/// Source-owned request body extraction for Anthropic Messages endpoints.
///
/// A `413` observed here can only be the `Bytes` buffering limit rejection.
/// Downstream handler or upstream `413` responses never pass through this code.
pub(crate) struct MessagesBody(pub(crate) Bytes, pub(crate) Option<String>);

impl<S> FromRequest<S> for MessagesBody
where
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        let attribution = request
            .extensions()
            .get::<RequestRejectionAttribution>()
            .cloned();
        let request_api_key_id = attribution
            .as_ref()
            .map(RequestRejectionAttribution::request_api_key_id)
            .or_else(|| {
                request
                    .extensions()
                    .get::<RequestApiKeyIdentity>()
                    .copied()
                    .map(RequestApiKeyIdentity::stable_id)
            });
        let uri = request.uri().clone();
        match Bytes::from_request(request, state).await {
            Ok(body) => Ok(Self(body, request_api_key_id)),
            Err(rejection) if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE => {
                let request_id = envelope::request_id();
                if let Some(attribution) = attribution.as_ref() {
                    attribution.record(
                        RequestRejectionReason::BodyTooLarge,
                        "body_extractor",
                        StatusCode::PAYLOAD_TOO_LARGE,
                        &request_id,
                        uri.path(),
                    );
                }
                Err(envelope::error_response_with_id(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "invalid_request_error",
                    BODY_LIMIT_MESSAGE,
                    &request_id,
                ))
            }
            Err(rejection) => {
                let request_id = envelope::request_id();
                let status = rejection.status();
                let mut response = bytes_rejection_response(rejection);
                envelope::insert_request_id_headers(response.headers_mut(), &request_id);
                if let Some(attribution) = attribution.as_ref() {
                    attribution.record(
                        RequestRejectionReason::BodyReadFailed,
                        "body_extractor",
                        status,
                        &request_id,
                        uri.path(),
                    );
                }
                Err(response)
            }
        }
    }
}

fn bytes_rejection_response(rejection: BytesRejection) -> Response {
    rejection.into_response()
}

#[cfg(test)]
mod tests {
    use std::{
        convert::Infallible,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use axum::{
        Router,
        extract::{DefaultBodyLimit, State},
        middleware,
        routing::post,
    };
    use futures::stream;
    use tower::ServiceExt;

    use crate::{
        anthropic::{
            request_admission::{
                RequestAdmissionController, RequestAdmissionMiddlewareState,
                request_admission_middleware,
            },
            usage::{UsageRecordQuery, UsageRecorder},
        },
        common::auth::RequestApiKeyStore,
        model::config::RequestAdmissionConfig,
    };

    use super::*;

    async fn body_len(MessagesBody(body, _): MessagesBody) -> String {
        body.len().to_string()
    }

    async fn counted_body(
        State(hits): State<Arc<AtomicUsize>>,
        MessagesBody(body, _): MessagesBody,
    ) -> String {
        hits.fetch_add(1, Ordering::SeqCst);
        body.len().to_string()
    }

    fn identified_request(
        path: &str,
        body: axum::body::Body,
        identity: RequestApiKeyIdentity,
    ) -> Request {
        let mut request = Request::builder()
            .method("POST")
            .uri(path)
            .body(body)
            .unwrap();
        request.extensions_mut().insert(identity);
        request
    }

    #[tokio::test]
    async fn exact_configured_boundary_is_accepted_and_next_byte_is_normalized() {
        const TEST_LIMIT: usize = 32;
        let app = Router::new()
            .route("/messages", post(body_len))
            .layer(DefaultBodyLimit::max(TEST_LIMIT));

        let accepted = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/messages")
                    .body(axum::body::Body::from(vec![b'x'; TEST_LIMIT]))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(accepted.status(), StatusCode::OK);

        let rejected = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/messages")
                    .body(axum::body::Body::from(vec![b'x'; TEST_LIMIT + 1]))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(rejected.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(
            rejected
                .headers()
                .get(axum::http::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("application/json")
        );
        assert!(rejected.headers().contains_key("request-id"));
        assert!(rejected.headers().contains_key("anthropic-request-id"));
    }

    #[tokio::test]
    async fn chunked_unknown_length_body_obeys_the_same_boundary_for_five_rounds() {
        const TEST_LIMIT: usize = 32;
        let app = Router::new()
            .route("/messages", post(body_len))
            .layer(DefaultBodyLimit::max(TEST_LIMIT));

        for _ in 0..5 {
            let exact_body = axum::body::Body::from_stream(stream::iter([
                Ok::<_, Infallible>(Bytes::from_static(&[b'x'; 16])),
                Ok::<_, Infallible>(Bytes::from_static(&[b'y'; 16])),
            ]));
            let accepted = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/messages")
                        .body(exact_body)
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(accepted.status(), StatusCode::OK);

            let oversized_body = axum::body::Body::from_stream(stream::iter([
                Ok::<_, Infallible>(Bytes::from_static(&[b'x'; 16])),
                Ok::<_, Infallible>(Bytes::from_static(&[b'y'; 17])),
            ]));
            let rejected = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/messages")
                        .body(oversized_body)
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(rejected.status(), StatusCode::PAYLOAD_TOO_LARGE);
            let body = axum::body::to_bytes(rejected.into_body(), 4096)
                .await
                .unwrap();
            let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(value["error"]["type"], "invalid_request_error");
        }
    }

    #[tokio::test]
    async fn body_limit_attribution_is_sampled_without_double_charging_rpm_for_five_rounds() {
        const TEST_LIMIT: usize = 32;
        for round in 0..5 {
            let key = format!("body-attribution-{round}");
            let store = RequestApiKeyStore::new([key.as_str()]);
            let identity = store.authenticate(&key).unwrap();
            let controller = Arc::new(RequestAdmissionController::new(RequestAdmissionConfig {
                rpm: 2,
                max_concurrent_requests: 1,
                max_queued_requests: 0,
                queue_timeout_ms: 0,
            }));
            let recorder = Arc::new(UsageRecorder::new(16));
            let hits = Arc::new(AtomicUsize::new(0));
            let app = Router::new()
                .route(
                    "/messages",
                    post(counted_body).layer(middleware::from_fn_with_state(
                        RequestAdmissionMiddlewareState::new(controller, recorder.clone()),
                        request_admission_middleware,
                    )),
                )
                .layer(DefaultBodyLimit::max(TEST_LIMIT))
                .with_state(hits.clone());

            let oversized = app
                .clone()
                .oneshot(identified_request(
                    "/messages",
                    axum::body::Body::from(vec![b'x'; TEST_LIMIT + 1]),
                    identity,
                ))
                .await
                .unwrap();
            assert_eq!(oversized.status(), StatusCode::PAYLOAD_TOO_LARGE);
            drop(oversized);
            assert_eq!(hits.load(Ordering::SeqCst), 0);

            let accepted = app
                .clone()
                .oneshot(identified_request(
                    "/messages",
                    axum::body::Body::from("ok"),
                    identity,
                ))
                .await
                .unwrap();
            assert_eq!(accepted.status(), StatusCode::OK);
            drop(accepted);
            assert_eq!(hits.load(Ordering::SeqCst), 1);

            let rpm_rejected = app
                .clone()
                .oneshot(identified_request(
                    "/messages",
                    axum::body::Body::from("not-dispatched"),
                    identity,
                ))
                .await
                .unwrap();
            assert_eq!(rpm_rejected.status(), StatusCode::TOO_MANY_REQUESTS);
            assert_eq!(hits.load(Ordering::SeqCst), 1);

            let records = recorder.query(UsageRecordQuery::default()).records;
            assert_eq!(records.len(), 2);
            let mut reasons = records
                .iter()
                .filter_map(|record| {
                    record
                        .error_metadata
                        .as_ref()
                        .and_then(|metadata| metadata["reason"].as_str())
                })
                .collect::<Vec<_>>();
            reasons.sort_unstable();
            assert_eq!(reasons, vec!["admission_rpm", "body_too_large"]);
            assert!(records.iter().all(|record| {
                record.total_input_tokens == 0
                    && record.output_tokens == 0
                    && record.request_api_key_id.as_deref() == Some(identity.stable_id().as_str())
            }));
        }
    }
}
