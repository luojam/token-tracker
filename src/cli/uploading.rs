use std::{fmt, path::Path};

use reqwest::{Method, header};
use serde::Deserialize;
use token_tracker::ExportSnapshot;

use super::server::{ServerClient, ServerError};

pub(super) struct Uploader {
    client: ServerClient,
}

impl Uploader {
    pub fn new(server_url: &str, auth_file: &Path) -> Result<Self, UploadError> {
        Ok(Self {
            client: ServerClient::new(server_url, auth_file).map_err(UploadError::Server)?,
        })
    }

    pub fn upload(&self, snapshot: &ExportSnapshot) -> Result<&'static str, UploadError> {
        let payload = serde_json::to_vec(snapshot).map_err(|_| {
            UploadError::Server(ServerError::Config("could not serialize snapshot"))
        })?;
        let request = self
            .client
            .request(Method::POST, "snapshots")
            .header(header::CONTENT_TYPE, "application/json")
            .body(payload);
        let body = self.client.send(request).map_err(UploadError::Server)?;
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
    Server(ServerError),
    Response,
}

impl fmt::Display for UploadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Server(ServerError::Transport) => formatter.write_str("network request failed; the server may have received the snapshot. Rerun to try again"),
            Self::Response | Self::Server(ServerError::Response) => formatter.write_str("server did not acknowledge this snapshot; it may have received the upload. Rerun to try again"),
            Self::Server(error) => write!(formatter, "{error}"),
        }
    }
}
