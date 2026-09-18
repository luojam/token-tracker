use std::{fs, os::unix::fs::PermissionsExt};

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
    response::Response,
};
use serde_json::{Value, json};
use token_tracker::server::{ServerConfig, router};
use tower::ServiceExt;

use crate::support::TempTree;

const TOKEN: &str = "test-token-with-at-least-32-characters";

fn config(tree: &TempTree, limit: usize) -> ServerConfig {
    ServerConfig {
        auth_file: tree.root.join("auth.token"),
        database_path: tree.root.join("snapshots.db"),
        max_upload_bytes: limit,
    }
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
    assert!(router(config(&tree, 4096)).is_err());
    auth_file(&tree);
    fs::set_permissions(
        tree.root.join("auth.token"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    assert!(router(config(&tree, 4096)).is_err());
    fs::set_permissions(
        tree.root.join("auth.token"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    tree.write("auth.token", "");
    assert!(router(config(&tree, 4096)).is_err());
    assert!(!tree.root.join("snapshots.db").exists());
    auth_file(&tree);
    assert!(router(config(&tree, 4096)).is_ok());
}

#[tokio::test]
async fn authentication_precedes_body_processing_and_health_stays_public() {
    let tree = TempTree::new();
    auth_file(&tree);
    let app = router(config(&tree, 16)).unwrap();
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
async fn uploads_persist_across_restarts_and_keep_revision_rules() {
    let tree = TempTree::new();
    auth_file(&tree);
    let first = snapshot();
    let app = router(config(&tree, 4096)).unwrap();
    let response = upload(&app, &first).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        body(response).await,
        json!({
            "status": "published", "machine_id": "machine-example", "export_revision": 1,
        })
    );
    drop(app);

    let app = router(config(&tree, 4096)).unwrap();
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
    auth_file(&tree);
    let first = snapshot();
    let mut invalid = first.clone();
    invalid["export_revision"] = json!(2);
    invalid["events"][0]["tokens"]["input"] = json!(u64::MAX);
    let app = router(config(&tree, invalid.to_string().len())).unwrap();
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
    auth_file(&tree);
    let app = router(config(&tree, 4096)).unwrap();
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
