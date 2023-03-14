#![warn(clippy::all, clippy::nursery, rust_2018_idioms)]

use reqwest::{IntoUrl, Method, RequestBuilder};
use serde::Serialize;

pub(crate) mod model;

#[derive(Debug, Default)]
pub struct Client {
    client: reqwest::Client,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("request: {0}")]
    Client(#[from] reqwest::Error),

    #[error("status code: {}", reqwest::Response::status(.0))]
    StatusCode(reqwest::Response),
}

const BASE_URL: &str = "http://ws.audioscrobbler.com/2.0/";

pub(crate) mod params {
    pub const ALBUM_SEARCH: &str = "album.search";
}

impl Client {
    async fn request<U, D>(&self, method: Method, url: U, data: D) -> Result<String, crate::Error>
    where
        U: IntoUrl + Send,
        D: Fn(RequestBuilder) -> RequestBuilder + Send,
    {
        // TODO: tracing

        let mut request = self.client.request(method, url);
        request = data(request);
        let response = request.send().await?;

        if response.status().is_success() {
            response.text().await.map_err(Into::into)
        } else {
            Err(crate::Error::StatusCode(response))
        }
    }

    #[inline]
    pub async fn get<U, T>(&self, url: U, query: &T) -> Result<String, crate::Error>
    where
        U: IntoUrl + Send,
        T: Serialize + Sync + ?Sized,
    {
        self.request(Method::GET, url, |req| req.query(query)).await
    }

    #[inline]
    async fn endpoint_get<U>(&self, url: U, query: &T)
}
