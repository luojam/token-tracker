use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    os::unix::fs::PermissionsExt,
    thread,
    time::{Duration, Instant},
};

use serde_json::json;
use token_tracker::ExportSnapshot;

use crate::support::TempTree;

const TOKEN: &str = "test-token-with-at-least-32-characters";

fn auth_file(tree: &TempTree) -> std::path::PathBuf {
    let path = tree.write("auth.token", format!("{TOKEN}\n"));
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    path
}

fn listener() -> TcpListener {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    listener
}

fn request(listener: &TcpListener) -> (TcpStream, Vec<u8>) {
    let deadline = Instant::now() + Duration::from_secs(15);
    let stream = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(Instant::now() < deadline, "upload did not arrive");
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("{error}"),
        }
    };
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut reader = BufReader::new(stream);
    let mut headers = String::new();
    loop {
        let mut line = String::new();
        assert_ne!(reader.read_line(&mut line).unwrap(), 0);
        if line == "\r\n" {
            break;
        }
        headers.push_str(&line);
    }
    let headers = headers.to_ascii_lowercase();
    assert!(
        headers.starts_with("post /snapshots http/1.1\r\n"),
        "{headers}"
    );
    assert!(headers.contains(&format!("authorization: bearer {TOKEN}\r\n")));
    assert!(headers.contains("content-type: application/json\r\n"));
    let length: usize = headers
        .lines()
        .find_map(|line| line.strip_prefix("content-length: "))
        .unwrap()
        .parse()
        .unwrap();
    let mut body = vec![0; length];
    reader.read_exact(&mut body).unwrap();
    (reader.into_inner(), body)
}

fn respond(mut stream: TcpStream, status: u16, body: &str) {
    write!(stream, "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nContent-Type: application/json\r\nLocation: /redirected\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
}

#[test]
fn upload_sends_retained_snapshot_and_bypasses_proxies_for_loopback_http() {
    let tree = TempTree::new();
    let source = tree.write(".pi/agent/sessions/history.jsonl", super::ALL_USAGE);
    super::successful_report(super::command(&tree.root).output().unwrap());
    fs::write(source, "invalid source must not be refreshed").unwrap();
    let auth = auth_file(&tree);
    let proxy = listener();
    let proxy_url = format!("http://{}", proxy.local_addr().unwrap());
    let listener = listener();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = thread::spawn(move || {
        let (stream, body) = request(&listener);
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
        let (mut stalled, _) = request(&listener);
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
            let (stream, _) = request(&listener);
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
    let output = super::command(&tree.root)
        .args(["upload", "http://127.0.0.1:3000", "--auth-file"])
        .arg(auth)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("chmod 600"));
    assert!(!tree.root.join(".local").exists());
}
