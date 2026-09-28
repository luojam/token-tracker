use std::{
    fmt,
    io::{self, Read},
    path::Path,
    time::Duration,
};

use reqwest::{
    Method, StatusCode, Url,
    blocking::{Client, RequestBuilder},
    header, redirect,
};
use token_tracker::domain::ExportSummary;

pub(super) struct ServerClient {
    client: Client,
    endpoint: Url,
    authorization: header::HeaderValue,
}

impl ServerClient {
    pub fn new(server_url: &str, auth_file: &Path) -> Result<Self, ServerError> {
        let endpoint =
            Url::parse(server_url).map_err(|_| ServerError::Config("invalid server URL"))?;
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
            return Err(ServerError::Config(
                "server URL must use HTTPS (or loopback HTTP), without credentials, query, or fragment",
            ));
        }
        let token = token_tracker::auth::read_token(auth_file).map_err(ServerError::Auth)?;
        let mut authorization = header::HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| ServerError::Config("invalid authentication token"))?;
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
            .map_err(|_| ServerError::Config("could not initialize HTTP client"))?;
        Ok(Self {
            client,
            endpoint,
            authorization,
        })
    }

    pub fn request(&self, method: Method, resource: &str) -> RequestBuilder {
        let mut endpoint = self.endpoint.clone();
        endpoint
            .path_segments_mut()
            .expect("validated HTTP URL")
            .pop_if_empty()
            .push(resource);
        self.client
            .request(method, endpoint)
            .header(header::AUTHORIZATION, self.authorization.clone())
            .timeout(Duration::from_secs(30))
    }

    pub fn send(&self, request: RequestBuilder) -> Result<Vec<u8>, ServerError> {
        let response = request.send().map_err(|_| ServerError::Transport)?;
        if response.status() != StatusCode::OK {
            return Err(ServerError::Http(response.status()));
        }
        let mut body = Vec::new();
        response
            .take(4097)
            .read_to_end(&mut body)
            .map_err(|_| ServerError::Transport)?;
        if body.len() > 4096 {
            return Err(ServerError::Response);
        }
        Ok(body)
    }

    pub fn summary(&self) -> Result<ExportSummary, ServerError> {
        let body = self.send(self.request(Method::GET, "summary"))?;
        serde_json::from_slice(&body).map_err(|_| ServerError::Response)
    }
}

#[derive(Debug)]
pub(super) enum ServerError {
    Config(&'static str),
    Auth(io::Error),
    Http(StatusCode),
    Transport,
    Response,
}

impl fmt::Display for ServerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(message) => formatter.write_str(message),
            Self::Auth(error) => write!(formatter, "{error}"),
            Self::Http(status) => write!(formatter, "server returned HTTP {status}"),
            Self::Transport => formatter.write_str("network request failed"),
            Self::Response => formatter.write_str("invalid server response"),
        }
    }
}
