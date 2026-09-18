use std::{
    fmt,
    io::{self, Read},
    path::Path,
    time::Duration,
};

use reqwest::{StatusCode, Url, blocking::Client, header, redirect};
use serde::Deserialize;
use token_tracker::ExportSnapshot;

pub(super) struct Uploader {
    client: Client,
    endpoint: Url,
    authorization: header::HeaderValue,
}

impl Uploader {
    pub fn new(server_url: &str, auth_file: &Path) -> Result<Self, UploadError> {
        let mut endpoint =
            Url::parse(server_url).map_err(|_| UploadError::Config("invalid server URL"))?;
        let loopback = endpoint.host_str().is_some_and(|host| {
            host == "localhost"
                || host
                    .trim_matches(['[', ']'])
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        });
        if endpoint.host_str().is_none()
            || !(endpoint.scheme() == "https" || endpoint.scheme() == "http" && loopback)
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
        {
            return Err(UploadError::Config(
                "server URL must use HTTPS (or loopback HTTP), without credentials, query, or fragment",
            ));
        }
        endpoint
            .path_segments_mut()
            .map_err(|_| UploadError::Config("invalid server URL"))?
            .pop_if_empty()
            .push("snapshots");
        let token = token_tracker::auth::read_token(auth_file).map_err(UploadError::Auth)?;
        let mut authorization = header::HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| UploadError::Config("invalid authentication token"))?;
        authorization.set_sensitive(true);
        let mut client = Client::builder()
            .redirect(redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(10));
        if loopback && endpoint.scheme() == "http" {
            client = client.no_proxy();
        }
        let client = client
            .build()
            .map_err(|_| UploadError::Config("could not initialize HTTP client"))?;
        Ok(Self {
            client,
            endpoint,
            authorization,
        })
    }

    pub fn upload(&self, snapshot: &ExportSnapshot) -> Result<&'static str, UploadError> {
        let payload = serde_json::to_vec(snapshot)
            .map_err(|_| UploadError::Config("could not serialize snapshot"))?;
        let response = self
            .client
            .post(self.endpoint.clone())
            .header(header::AUTHORIZATION, self.authorization.clone())
            .header(header::CONTENT_TYPE, "application/json")
            .body(payload)
            .timeout(Duration::from_secs(30))
            .send()
            .map_err(|_| UploadError::Transport)?;
        if response.status() != StatusCode::OK {
            return Err(UploadError::Http(response.status()));
        }
        let mut body = Vec::new();
        response
            .take(4097)
            .read_to_end(&mut body)
            .map_err(|_| UploadError::Transport)?;
        acknowledge(&body, snapshot)
    }
}

#[derive(Deserialize)]
struct Acknowledgment {
    status: String,
    machine_id: String,
    export_revision: u64,
}

fn acknowledge(body: &[u8], snapshot: &ExportSnapshot) -> Result<&'static str, UploadError> {
    if body.len() > 4096 {
        return Err(UploadError::Response);
    }
    let acknowledgment: Acknowledgment =
        serde_json::from_slice(body).map_err(|_| UploadError::Response)?;
    if acknowledgment.machine_id != snapshot.machine_id
        || acknowledgment.export_revision != snapshot.export_revision
    {
        return Err(UploadError::Response);
    }
    match acknowledgment.status.as_str() {
        "published" => Ok("published"),
        "already_published" => Ok("already published"),
        _ => Err(UploadError::Response),
    }
}

#[derive(Debug)]
pub(super) enum UploadError {
    Config(&'static str),
    Auth(io::Error),
    Http(StatusCode),
    Transport,
    Response,
}

impl fmt::Display for UploadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(message) => formatter.write_str(message),
            Self::Auth(error) => write!(formatter, "{error}"),
            Self::Http(status) => write!(formatter, "server returned HTTP {status}"),
            Self::Transport => formatter.write_str("network request failed; the server may have received the snapshot. Rerun to try again"),
            Self::Response => formatter.write_str("server did not acknowledge this snapshot; it may have received the upload. Rerun to try again"),
        }
    }
}
