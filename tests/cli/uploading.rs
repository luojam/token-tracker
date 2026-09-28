use std::{
    fs,
    io::{Read, Write},
    os::unix::fs::PermissionsExt,
    thread,
    time::Duration,
};

use serde_json::json;
use token_tracker::ExportSnapshot;

use super::server::{TOKEN, auth_file, listener, request, respond};
use crate::support::TempTree;

#[test]
fn upload_sends_retained_snapshot_and_bypasses_proxies_for_loopback_http() {
    let tree = TempTree::new();
    let source = tree.write(".pi/agent/sessions/history.jsonl", super::ALL_USAGE);
    super::successful_report(super::command(&tree.root).output().unwrap());
    fs::write(source, "invalid source must not be refreshed").unwrap();
    tree.write(
        ".config/token-tracker/config.toml",
        "server_url = 'invalid URL'\nauth_file = 'missing.token'\n",
    );
    let auth = auth_file(&tree);
    let proxy = listener();
    let proxy_url = format!("http://{}", proxy.local_addr().unwrap());
    let listener = listener();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = thread::spawn(move || {
        let (stream, body) = request(&listener, "post /snapshots");
        let snapshot: ExportSnapshot = serde_json::from_slice(&body).unwrap();
        assert_eq!(snapshot.events.len(), 4);
        respond(
            stream,
            200,
            &json!({
                "status": "published",
                "machine_id": snapshot.machine_id,
                "export_revision": snapshot.export_revision,
            })
            .to_string(),
        );
    });
    let output = super::command(&tree.root)
        .env("HTTP_PROXY", &proxy_url)
        .env("http_proxy", &proxy_url)
        .env("ALL_PROXY", &proxy_url)
        .env("all_proxy", &proxy_url)
        .env("NO_PROXY", "")
        .env("no_proxy", "")
        .env_remove("REQUEST_METHOD")
        .args(["upload", &url, "--auth-file"])
        .arg(auth)
        .output()
        .unwrap();
    server.join().unwrap();
    assert_eq!(
        proxy.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    assert_eq!(
        super::successful_report(output),
        "Snapshot revision 1 published.\n"
    );
}

#[test]
fn upload_deadline_covers_headers_and_streamed_body() {
    let tree = TempTree::new();
    let auth = auth_file(&tree);
    let listener = listener();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = thread::spawn(move || {
        let (mut stalled, _) = request(&listener, "post /snapshots");
        thread::sleep(Duration::from_secs(10));
        write!(
            stalled,
            "HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\n{{"
        )
        .unwrap();
        thread::sleep(Duration::from_secs(10));
        stalled.write_all(b" ").unwrap();
        stalled
            .set_read_timeout(Some(Duration::from_secs(15)))
            .unwrap();
        assert_eq!(stalled.read(&mut [0]).unwrap(), 0);
    });
    let output = super::command(&tree.root)
        .args(["upload", &url, "--auth-file"])
        .arg(auth)
        .output()
        .unwrap();
    server.join().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("network request failed"));
}

#[test]
fn upload_stops_on_rejections_redirects_invalid_acknowledgments_and_network_failures() {
    for (status, expected) in [
        (401, "HTTP 401"),
        (307, "HTTP 307"),
        (200, "did not acknowledge this snapshot"),
        (503, "HTTP 503"),
        (0, "network request failed"),
    ] {
        let tree = TempTree::new();
        let auth = auth_file(&tree);
        let listener = listener();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let (stream, _) = request(&listener, "post /snapshots");
            if status == 0 {
                drop(stream);
            } else {
                respond(
                    stream,
                    status,
                    &json!({
                        "status": "published", "machine_id": "wrong-machine",
                        "export_revision": 1, "error": TOKEN,
                    })
                    .to_string(),
                );
            }
            listener
        });
        let output = super::command(&tree.root)
            .args(["upload", &url, "--auth-file"])
            .arg(auth)
            .output()
            .unwrap();
        let listener = server.join().unwrap();
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let error = String::from_utf8(output.stderr).unwrap();
        assert!(error.contains(expected), "{error}");
        assert!(!error.contains(TOKEN));
    }
}

#[test]
fn invalid_upload_configuration_fails_before_opening_storage() {
    let tree = TempTree::new();
    let auth = auth_file(&tree);
    let output = super::command(&tree.root)
        .args(["upload", "http://example.com", "--auth-file"])
        .arg(&auth)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("must use HTTPS"));
    fs::set_permissions(&auth, fs::Permissions::from_mode(0o644)).unwrap();
    tree.write(
        ".config/token-tracker/config.toml",
        format!(
            "server_url = 'http://127.0.0.1:3000'\nauth_file = '{}'\n",
            auth.display()
        ),
    );
    let output = super::command(&tree.root).arg("upload").output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("chmod 600"));
    assert!(!tree.root.join(".local").exists());
}
