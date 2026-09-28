use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    os::unix::fs::PermissionsExt,
    thread,
    time::{Duration, Instant},
};

use crate::support::TempTree;

pub(super) const TOKEN: &str = "test-token-with-at-least-32-characters";

pub(super) fn auth_file(tree: &TempTree) -> std::path::PathBuf {
    let path = tree.write("auth.token", format!("{TOKEN}\n"));
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    path
}

pub(super) fn listener() -> TcpListener {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    listener
}

pub(super) fn request(listener: &TcpListener, target: &str) -> (TcpStream, Vec<u8>) {
    let deadline = Instant::now() + Duration::from_secs(15);
    let stream = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(Instant::now() < deadline, "request did not arrive");
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
        headers.starts_with(&format!("{target} http/1.1\r\n")),
        "{headers}"
    );
    assert!(headers.contains(&format!("authorization: bearer {TOKEN}\r\n")));
    if target.starts_with("post ") {
        assert!(headers.contains("content-type: application/json\r\n"));
    }
    let length: usize = headers
        .lines()
        .find_map(|line| line.strip_prefix("content-length: "))
        .unwrap_or("0")
        .parse()
        .unwrap();
    let mut body = vec![0; length];
    reader.read_exact(&mut body).unwrap();
    (reader.into_inner(), body)
}

pub(super) fn respond(mut stream: TcpStream, status: u16, body: &str) {
    write!(stream, "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nContent-Type: application/json\r\nLocation: /redirected\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
}

#[test]
fn summary_fetches_authenticated_totals_without_local_state() {
    let tree = TempTree::new();
    tree.write(".config/token-tracker/config.toml", "invalid config");
    tree.write(".local", "storage cannot be created here");
    let auth = auth_file(&tree);
    let proxy = listener();
    let proxy_url = format!("http://{}", proxy.local_addr().unwrap());
    let listener = listener();
    let url = format!("http://{}/tracker/", listener.local_addr().unwrap());
    let server = thread::spawn(move || {
        let (stream, body) = request(&listener, "get /tracker/summary");
        assert!(body.is_empty());
        respond(
            stream,
            200,
            r#"{
            "total_cost_usd": "123456789.123456789012",
            "tokens": {"total": 1234, "input": 1000, "output": 200, "cache_read": 30, "cache_write": 4}
        }"#,
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
        .args(["summary", "--server", &url, "--auth-file"])
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
        "Total tokens: 1,234\nInput tokens: 1,000\nOutput tokens: 200\nCache-read tokens: 30\nCache-write tokens: 4\nTotal cost: $123456789.123456789012\n"
    );
}

#[test]
fn summary_reports_failures_without_exposing_response_bodies_or_retrying() {
    for (status, body, expected) in [
        (401, TOKEN.to_owned(), "HTTP 401"),
        (307, TOKEN.to_owned(), "HTTP 307"),
        (200, "{}".into(), "invalid server response"),
        (200, r#"{"total_cost_usd":"1","tokens":{"total":9,"input":1,"output":0,"cache_read":0,"cache_write":0}}"#.into(), "invalid server response"),
        (200, r#"{"total_cost_usd":"-1","tokens":{"total":0,"input":0,"output":0,"cache_read":0,"cache_write":0}}"#.into(), "invalid server response"),
        (200, "x".repeat(4097), "invalid server response"),
        (0, String::new(), "network request failed"),
    ] {
        let tree = TempTree::new();
        let auth = auth_file(&tree);
        let listener = listener();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let (stream, _) = request(&listener, "get /summary");
            if status != 0 {
                respond(stream, status, &body);
            }
            listener
        });
        let output = super::command(&tree.root)
            .args(["summary", "--server", &url, "--auth-file"])
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
        assert!(!tree.root.join(".local").exists());
    }
}
