use reqwest::{
    StatusCode,
    blocking::{Client, Response},
    header,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    io::{Read, Write},
    time::Duration,
};
use url::Url;

use super::{MAX_SNAPSHOT_BYTES, RemoteError, Result, invalid, valid_machine_id};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObjectMetadata {
    pub key: String,
    pub etag: String,
    pub size: u64,
    pub uploaded: String,
}

impl ObjectMetadata {
    pub fn machine_id(&self) -> Result<&str> {
        self.key
            .strip_prefix("machines/")
            .and_then(|key| key.strip_suffix(".json"))
            .filter(|id| valid_machine_id(id))
            .ok_or_else(|| invalid("invalid remote snapshot key"))
    }

    pub(crate) fn validate(&self) -> Result<()> {
        self.machine_id()?;
        if self.size > MAX_SNAPSHOT_BYTES
            || self.etag.is_empty()
            || self.etag.contains(['\r', '\n'])
            || chrono::DateTime::parse_from_rfc3339(&self.uploaded).is_err()
        {
            return Err(invalid("invalid remote object metadata"));
        }

        Ok(())
    }
}

pub trait RemoteStore {
    fn list(&self) -> Result<Vec<ObjectMetadata>>;
    fn download(&self, key: &str, destination: &mut dyn Write) -> Result<String>;
    fn upload(&self, machine_id: &str, previous_etag: Option<&str>, body: Vec<u8>) -> Result<()>;
}

pub struct HttpRemote {
    client: Client,
    base: Url,
    token: header::HeaderValue,
}

impl HttpRemote {
    pub fn new(endpoint: &str, token: &str) -> Result<Self> {
        let mut base = Url::parse(endpoint).map_err(|_| invalid("invalid remote URL"))?;
        let loopback = matches!(base.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
        if !(base.scheme() == "https" || base.scheme() == "http" && loopback)
            || base.host_str().is_none()
            || !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
            || base.path() != "/"
        {
            return Err(invalid(
                "remote URL must be an HTTPS origin (HTTP is allowed on loopback)",
            ));
        }
        base.set_path("/");

        if token.trim().is_empty() {
            return Err(invalid("remote token is empty"));
        }
        let mut token = header::HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| invalid("invalid remote token"))?;
        token.set_sensitive(true);

        let client = Client::builder()
            .timeout(Duration::from_secs(120))
            .connect_timeout(Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;

        Ok(Self {
            client,
            base,
            token,
        })
    }

    pub fn endpoint(&self) -> &str {
        self.base.as_str()
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::blocking::RequestBuilder {
        self.client
            .request(method, self.base.join(path).expect("validated remote path"))
            .header(header::AUTHORIZATION, self.token.clone())
    }
}

fn successful(response: Response) -> Result<Response> {
    if !response.status().is_success() {
        return Err(invalid(format!(
            "remote returned HTTP {}",
            response.status()
        )));
    }

    Ok(response)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Listing {
    objects: Vec<ObjectMetadata>,
    cursor: Option<String>,
}

impl RemoteStore for HttpRemote {
    fn list(&self) -> Result<Vec<ObjectMetadata>> {
        let mut objects = Vec::new();
        let mut cursor = None;
        let mut cursors = BTreeSet::new();
        let mut keys = BTreeSet::new();

        loop {
            let mut request = self.request(reqwest::Method::GET, "machines");
            if let Some(cursor) = &cursor {
                request = request.query(&[("cursor", cursor)]);
            }

            let response = successful(request.send()?)?;
            let mut body = Vec::new();
            response
                .take(MAX_SNAPSHOT_BYTES + 1)
                .read_to_end(&mut body)?;
            if body.len() as u64 > MAX_SNAPSHOT_BYTES {
                return Err(invalid("remote listing too large"));
            }

            let page: Listing = serde_json::from_slice(&body)?;
            for object in page.objects {
                object.validate()?;
                if !keys.insert(object.key.clone()) {
                    return Err(invalid("duplicate remote object"));
                }
                objects.push(object);
            }

            cursor = page.cursor;
            let Some(value) = &cursor else {
                return Ok(objects);
            };
            if value.is_empty() || !cursors.insert(value.clone()) {
                return Err(invalid("invalid remote pagination cursor"));
            }
        }
    }

    fn download(&self, key: &str, destination: &mut dyn Write) -> Result<String> {
        let id = key
            .strip_prefix("machines/")
            .and_then(|key| key.strip_suffix(".json"));
        if !id.is_some_and(valid_machine_id) {
            return Err(invalid("invalid download key"));
        }

        let response = successful(self.request(reqwest::Method::GET, key).send()?)?;
        let etag = response
            .headers()
            .get(header::ETAG)
            .and_then(|value| value.to_str().ok())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| invalid("download has no ETag"))?
            .to_owned();
        if response
            .content_length()
            .is_some_and(|size| size > MAX_SNAPSHOT_BYTES)
        {
            return Err(invalid("remote snapshot too large"));
        }

        let size = std::io::copy(&mut response.take(MAX_SNAPSHOT_BYTES + 1), destination)?;
        if size > MAX_SNAPSHOT_BYTES {
            return Err(invalid("remote snapshot too large"));
        }

        Ok(etag)
    }

    fn upload(&self, machine_id: &str, previous_etag: Option<&str>, body: Vec<u8>) -> Result<()> {
        if !valid_machine_id(machine_id) || body.len() as u64 > MAX_SNAPSHOT_BYTES {
            return Err(invalid("invalid machine ID or snapshot too large"));
        }

        let request = self
            .request(reqwest::Method::PUT, &format!("machines/{machine_id}.json"))
            .header(header::CONTENT_TYPE, "application/json");
        let request = match previous_etag {
            Some(etag) => request.header(header::IF_MATCH, etag),
            None => request.header(header::IF_NONE_MATCH, "*"),
        };

        let response = request.body(body).send()?;
        if response.status() == StatusCode::PRECONDITION_FAILED {
            return Err(RemoteError::Conflict);
        }
        successful(response)?;

        Ok(())
    }
}
