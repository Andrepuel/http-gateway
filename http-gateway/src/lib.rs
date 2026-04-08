#![recursion_limit = "512"]

pub mod handler;
pub mod router;
pub mod tokio_hyper;
pub mod uri_subject;

use crate::{
    handler::{Handler, Request, Response, StringId},
    tokio_hyper::TokioHyper,
};
use futures::pin_mut;
use http_body_util::{BodyExt, Full};
use hyper::{StatusCode, body::Incoming, header::HeaderValue, service::service_fn};
use std::{
    collections::{HashMap, VecDeque},
    future::poll_fn,
    io,
    rc::Rc,
    sync::Arc,
    task::Poll,
};
use tokio::net::TcpListener;
use tracing::Instrument;
use url::Url;
use uuid::Uuid;

pub async fn http_server<H>(listen: Url, handler: H) -> io::Result<Never>
where
    H: Handler + 'static,
{
    let listen = (
        listen
            .host_str()
            .ok_or_else(|| io::Error::other(format!("Bad hostname for url {listen:?}")))?,
        listen
            .port_or_known_default()
            .ok_or_else(|| io::Error::other(format!("Bad port number for url {listen:?}")))?,
    );

    let listener = TcpListener::bind(listen).await?;
    tracing::info!(?listen, "Listening");
    let handler = Rc::new(handler);

    loop {
        let (stream, addr) = listener.accept().await?;
        let stream = TokioHyper(stream);

        let handler = handler.clone();
        tokio::task::spawn_local(
            async move {
                tracing::info!("Received new connection");
                match hyper::server::conn::http1::Builder::new()
                    .serve_connection(
                        stream,
                        service_fn(|req| {
                            let req_id: Arc<str> = extract_req_id(&req).into();
                            service_http(req_id.clone(), handler.clone(), req)
                                .instrument(tracing::info_span!("req", %req_id))
                        }),
                    )
                    .await
                {
                    Ok(()) => {}
                    Err(e) => {
                        tracing::error!(%e, "error on connection");
                        tracing::debug!(?e);
                    }
                }
                tracing::info!("Serve connection done");
            }
            .instrument(tracing::info_span!("conn", ?addr)),
        );
    }
}

fn extract_req_id<T>(req: &hyper::Request<T>) -> String {
    req.headers()
        .iter()
        .find_map(|(name, value)| {
            let value = value.to_str().ok()?;
            if !name.as_str().eq_ignore_ascii_case("req-id") {
                return None;
            }

            Some(value.to_string())
        })
        .unwrap_or_else(|| Uuid::now_v7().to_string())
}

async fn service_http<H>(
    reqid: Arc<str>,
    handler: H,
    req: hyper::Request<Incoming>,
) -> io::Result<hyper::Response<FullBody>>
where
    H: Handler,
{
    let mut content_type = None;
    let mut headers = HashMap::new();
    for (name, value) in req.headers() {
        let Ok(value) = value.to_str() else {
            continue;
        };

        if name.as_str().eq_ignore_ascii_case("content-type") {
            tracing::debug!(content_type=?value);
            content_type = Some(value.to_ascii_lowercase());
        }

        if name.as_str().eq_ignore_ascii_case("req-id") {
            continue;
        }

        headers.insert(StringId::new(name.as_str()), value.to_string());
    }

    let content_type = content_type.unwrap_or_default();
    let subject = uri_subject::uri_to_path(req.uri().clone());
    tracing::debug!(?subject);
    let method = req.method().clone();
    let query = uri_subject::uri_to_query(req.uri());
    let body = req
        .into_body()
        .collect()
        .await
        .map_err(io::Error::other)?
        .to_bytes();
    let body = match content_type.as_str() {
        "application/json" => {
            let json = std::str::from_utf8(&body)
                .map_err(|e| {
                    tracing::warn!(%e, "bad UTF8 json body");
                    tracing::debug!(?e);
                })
                .and_then(|body| {
                    serde_json::from_str(body).map_err(|e| {
                        tracing::warn!(%e, "bad json body");
                        tracing::debug!(?e);
                    })
                });

            match json {
                Ok(json) => json,
                Err(()) => serde_json::Value::Null,
            }
        }
        _ => serde_json::Value::Null,
    };

    let response = handler
        .handle(Request {
            method,
            path: subject,
            headers,
            query,
            body: Some(body),
        })
        .instrument(tracing::info_span!("handler", ?reqid));
    pin_mut!(response);
    let response = poll_fn(|cx| {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| response.as_mut().poll(cx)))
        {
            Ok(Poll::Ready(ok)) => Poll::Ready(Ok(ok)),
            Ok(Poll::Pending) => Poll::Pending,
            Err(e) => Poll::Ready(Err(e)),
        }
    })
    .await;

    let response = response.map_err(|_| PanicResponse);
    let status = response.status_code();
    let (content_type, body) = match response.into_body() {
        Some(body) => {
            let body = serde_json::to_string_pretty(&body).unwrap().into_bytes();
            (Some("application/json"), body)
        }
        None => Default::default(),
    };

    let mut response = hyper::Response::new(Full::new(body.into()));
    *response.status_mut() = status;

    if let Some(content_type) = content_type {
        response.headers_mut().append(
            "content-type",
            HeaderValue::from_str(content_type).map_err(io::Error::other)?,
        );
    }

    Ok(response)
}

type FullBody = Full<VecDeque<u8>>;

pub enum Never {}

struct PanicResponse;
impl Response for PanicResponse {
    type Body = ();

    fn into_body(self) -> Option<Self::Body> {
        None
    }

    fn status_code(&self) -> StatusCode {
        StatusCode::INTERNAL_SERVER_ERROR
    }
}
