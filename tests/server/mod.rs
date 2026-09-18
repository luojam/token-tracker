use std::{fs, io, os::unix::fs::PermissionsExt};

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
    response::Response,
};
use serde_json::{Value, json};
use token_tracker::{
    ExportSink, ExportSnapshot, PublishError, PublishOutcome, SqliteExportSink,
    server::{ServerConfig, router},
};
use tower::ServiceExt;

use crate::support::TempTree;

const TOKEN: &str = "test-token-with-at-least-32-characters";

fn sqlite_router(tree: &TempTree, limit: usize) -> Router {
    let sink = SqliteExportSink::open(tree.root.join("snapshots.db")).unwrap();
    router(TOKEN.into(), sink, limit).unwrap()
}

fn auth_file(tree: &TempTree) {
    let path = tree.write("auth.token", format!("{TOKEN}\n"));
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

fn snapshot() -> Value {
    serde_json::from_str(include_str!("../fixtures/export-example.json")).unwrap()
}

async fn upload(app: &Router, snapshot: &Value) -> Response {
    app.clone()
        .oneshot(
            Request::post("/snapshots")
                .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(snapshot.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn body(response: Response) -> Value {
    serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap()
}

#[test]
fn startup_requires_a_private_valid_auth_file() {
    let tree = TempTree::new();
    let config = ServerConfig {
        auth_file: tree.root.join("auth.token"),
        database_path: tree.root.join("snapshots.db"),
        max_upload_bytes: 4096,
    };
    assert!(config.read_token().is_err());
    auth_file(&tree);
    assert_eq!(config.read_token().unwrap(), TOKEN);
    fs::set_permissions(&config.auth_file, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(config.read_token().is_err());
    auth_file(&tree);
    tree.write("auth.token", "");
    assert!(config.read_token().is_err());
}

#[test]
fn router_rejects_invalid_authentication_tokens() {
    let sink = SqliteExportSink::open(":memory:").unwrap();
    assert!(router(format!("{TOKEN}\n"), sink, 4096).is_err());
}

#[tokio::test]
async fn authentication_precedes_body_processing_and_health_stays_public() {
    let tree = TempTree::new();
    let app = sqlite_router(&tree, 16);
    for credentials in [None, Some("Bearer wrong-token")] {
        let mut request = Request::post("/snapshots");
        if let Some(credentials) = credentials {
            request = request.header(header::AUTHORIZATION, credentials);
        }
        let response = app
            .clone()
            .oneshot(request.body(Body::from("x".repeat(32))).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(response.headers()[header::WWW_AUTHENTICATE], "Bearer");
    }
    let response = app
        .oneshot(Request::get("/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn uploads_validate_independently_of_storage_and_hide_storage_errors() {
    struct FailingSink;

    impl ExportSink for FailingSink {
        type Error = io::Error;

        fn publish(
            &mut self,
            _: &ExportSnapshot,
        ) -> Result<PublishOutcome, PublishError<Self::Error>> {
            Err(PublishError::Destination(io::Error::other(
                "private storage detail",
            )))
        }
    }

    let app = router(TOKEN.into(), FailingSink, 4096).unwrap();
    let mut snapshot = snapshot();
    snapshot["events"][0]["tokens"]["input"] = json!(u64::MAX);
    let response = upload(&app, &snapshot).await;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body(response).await, json!({"error": "storage_error"}));

    snapshot["format_version"] = json!(0);
    assert_eq!(
        upload(&app, &snapshot).await.status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
}

#[tokio::test]
async fn uploads_persist_across_restarts_and_keep_revision_rules() {
    let tree = TempTree::new();
    let first = snapshot();
    let app = sqlite_router(&tree, 4096);
    let response = upload(&app, &first).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        body(response).await,
        json!({
            "status": "published", "machine_id": "machine-example", "export_revision": 1,
        })
    );
    drop(app);

    let app = sqlite_router(&tree, 4096);
    assert_eq!(
        body(upload(&app, &first).await).await["status"],
        "already_published"
    );
    let mut newer = first.clone();
    newer["export_revision"] = json!(2);
    assert_eq!(upload(&app, &newer).await.status(), StatusCode::OK);
    let response = upload(&app, &first).await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(
        body(response).await,
        json!({"error": "stale_revision", "published_revision": 2})
    );
    let mut conflicting = newer.clone();
    conflicting["machine_name"] = json!("Changed");
    let response = upload(&app, &conflicting).await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(body(response).await["error"], "revision_conflict");
}

#[tokio::test]
async fn invalid_snapshots_and_oversized_bodies_do_not_replace_stored_data() {
    let tree = TempTree::new();
    let first = snapshot();
    let mut invalid = first.clone();
    invalid["export_revision"] = json!(2);
    invalid["events"][0]["tokens"]["input"] = json!(u64::MAX);
    let app = sqlite_router(&tree, invalid.to_string().len());
    assert_eq!(upload(&app, &first).await.status(), StatusCode::OK);
    assert_eq!(
        upload(&app, &invalid).await.status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let mut oversized = first.clone();
    oversized["machine_name"] = json!("x".repeat(4096));
    assert_eq!(
        upload(&app, &oversized).await.status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );

    assert_eq!(
        body(upload(&app, &first).await).await["status"],
        "already_published"
    );
}

#[tokio::test]
async fn upload_requires_json_with_the_snapshot_structure() {
    let tree = TempTree::new();
    let app = sqlite_router(&tree, 4096);
    for (content_type, payload, status) in [
        ("text/plain", "{}", StatusCode::UNSUPPORTED_MEDIA_TYPE),
        ("application/json", "{", StatusCode::BAD_REQUEST),
        ("application/json", "{}", StatusCode::UNPROCESSABLE_ENTITY),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::post("/snapshots")
                    .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                    .header(header::CONTENT_TYPE, content_type)
                    .body(Body::from(payload))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), status);
    }
}
