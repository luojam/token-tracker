mod config;

use std::sync::{Arc, Mutex};

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Request, State, rejection::JsonRejection},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde_json::json;
use subtle::ConstantTimeEq;

use crate::application::SummaryReadStore;
use crate::{ExportSink, ExportSnapshot, PublishError, PublishOutcome};

pub use config::ServerConfig;

pub fn router<S: ExportSink + SummaryReadStore + Send + 'static>(
    token: String,
    sink: S,
    max_upload_bytes: usize,
) -> Result<Router, Box<dyn std::error::Error>> {
    if !crate::auth::valid_token(&token) {
        return Err("authentication token must be 32-4096 ASCII bearer-token characters".into());
    }
    if max_upload_bytes == 0 {
        return Err("maximum upload size must be positive".into());
    }
    let state = Arc::new(Mutex::new(sink));
    Ok(Router::new()
        .route("/snapshots", post(upload::<S>))
        .route("/summary", get(summary::<S>))
        .layer(DefaultBodyLimit::max(max_upload_bytes))
        .route_layer(middleware::from_fn_with_state(
            Arc::<str>::from(token),
            authenticate,
        ))
        .route("/health", get(|| async { "ok\n" }))
        .with_state(state))
}

async fn authenticate(
    State(expected_token): State<Arc<str>>,
    request: Request,
    next: Next,
) -> Response {
    let mut headers = request.headers().get_all(header::AUTHORIZATION).iter();
    let token = headers
        .next()
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split_once(' '))
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("Bearer"))
        .map(|(_, token)| token.as_bytes());
    if headers.next().is_some()
        || !token.is_some_and(|token| bool::from(token.ct_eq(expected_token.as_bytes())))
    {
        let mut response = error(StatusCode::UNAUTHORIZED, "unauthorized");
        response
            .headers_mut()
            .insert(header::WWW_AUTHENTICATE, "Bearer".parse().unwrap());
        return response;
    }
    next.run(request).await
}

async fn upload<S: ExportSink + Send + 'static>(
    State(sink): State<Arc<Mutex<S>>>,
    payload: Result<Json<ExportSnapshot>, JsonRejection>,
) -> Response {
    let snapshot = match payload {
        Ok(Json(snapshot)) => snapshot,
        Err(rejection) => {
            let status = rejection.status();
            return error(
                status,
                match status {
                    StatusCode::PAYLOAD_TOO_LARGE => "upload_too_large",
                    StatusCode::UNSUPPORTED_MEDIA_TYPE => "expected_application_json",
                    StatusCode::UNPROCESSABLE_ENTITY => "invalid_snapshot",
                    _ => "invalid_json",
                },
            );
        }
    };
    let machine_id = snapshot.machine_id.clone();
    let export_revision = snapshot.export_revision;
    let result = tokio::task::spawn_blocking(move || {
        snapshot
            .validate()
            .map_err(|reason| PublishError::InvalidSnapshot {
                reason: reason.into(),
            })?;
        sink.lock()
            .expect("snapshot storage lock poisoned")
            .publish(&snapshot)
    })
    .await;
    match result {
        Ok(Ok(outcome)) => Json(json!({
            "status": match outcome {
                PublishOutcome::Published => "published",
                PublishOutcome::AlreadyPublished => "already_published",
            },
            "machine_id": machine_id,
            "export_revision": export_revision,
        }))
        .into_response(),
        Ok(Err(PublishError::InvalidSnapshot { .. })) => {
            error(StatusCode::UNPROCESSABLE_ENTITY, "invalid_snapshot")
        }
        Ok(Err(PublishError::StaleRevision {
            published_revision, ..
        })) => (
            StatusCode::CONFLICT,
            Json(json!({"error": "stale_revision", "published_revision": published_revision})),
        )
            .into_response(),
        Ok(Err(PublishError::RevisionConflict { revision })) => (
            StatusCode::CONFLICT,
            Json(json!({"error": "revision_conflict", "export_revision": revision})),
        )
            .into_response(),
        Ok(Err(PublishError::Destination(error))) => {
            eprintln!("snapshot storage failed: {error}");
            self::error(StatusCode::INTERNAL_SERVER_ERROR, "storage_error")
        }
        Err(error) => {
            eprintln!("snapshot task failed: {error}");
            self::error(StatusCode::INTERNAL_SERVER_ERROR, "internal_error")
        }
    }
}

async fn summary<S: SummaryReadStore + Send + 'static>(
    State(store): State<Arc<Mutex<S>>>,
) -> Response {
    let result = tokio::task::spawn_blocking(move || {
        store
            .lock()
            .expect("snapshot storage lock poisoned")
            .summary()
    })
    .await;
    match result {
        Ok(Ok(summary)) => Json(summary).into_response(),
        Ok(Err(error)) => {
            eprintln!("summary storage failed: {error}");
            self::error(StatusCode::INTERNAL_SERVER_ERROR, "storage_error")
        }
        Err(error) => {
            eprintln!("summary task failed: {error}");
            self::error(StatusCode::INTERNAL_SERVER_ERROR, "internal_error")
        }
    }
}

fn error(status: StatusCode, code: &'static str) -> Response {
    (status, Json(json!({ "error": code }))).into_response()
}
