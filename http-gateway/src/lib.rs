#![recursion_limit = "512"]

pub mod handler;
pub mod router;
pub mod tokio_hyper;
pub mod uri_subject;

use crate::{
    handler::{EitherBody, Handler, NoBody, Request, Response, ResponseBody, StringId},
    tokio_hyper::TokioHyper,
};
use bytes::BytesMut;
use futures::pin_mut;
use hyper::{
    StatusCode,
    body::Incoming,
    header::{HeaderName, HeaderValue},
    service::service_fn,
};
use pin_project::pin_project;
use std::{
    collections::HashMap, future::poll_fn, io, pin::Pin, rc::Rc, str::FromStr, sync::Arc,
    task::Poll,
};
use tokio::{
    io::{AsyncRead, ReadBuf},
    net::TcpListener,
};
use tracing::Instrument;
use url::Url;
use uuid::Uuid;

pub use bytes;
pub use hyper;
pub use serde_json;

pub fn http_server_main<F, H>(handler: F)
where
    F: FnOnce() -> H,
    H: Handler<Incoming> + 'static,
{
    let r = dotenvy::dotenv();
    tracing_subscriber::fmt::init();
    if let Err(e) = r {
        tracing::warn!(%e, "Bad dotenv file");
        tracing::debug!(?e);
    }

    let r = tokio::runtime::LocalRuntime::new()
        .unwrap()
        .block_on(async move {
            let listen = std::env::var("LISTEN")
                .map_err(|e| io::Error::other(format!("Missing var LISTEN: {e}")))?;

            let listen = listen
                .parse()
                .map_err(|e| io::Error::other(format!("Bad LISTEN url ({listen:?}): {e}")))?;

            http_server(listen, handler())
                .instrument(tracing::info_span!("server"))
                .await
        });

    match r {
        Ok(_) => unreachable!(),
        Err(e) => {
            tracing::error!(%e, "Fatal error");
            tracing::debug!(?e);
        }
    }
}

pub async fn http_server<H>(listen: Url, handler: H) -> io::Result<Never>
where
    H: Handler<Incoming> + 'static,
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
) -> io::Result<hyper::Response<WriteBody<EitherBody<<H::Response as Response>::Body, NoBody>>>>
where
    H: Handler<Incoming>,
{
    let mut headers = HashMap::new();
    for (name, value) in req.headers() {
        let Ok(value) = value.to_str() else {
            continue;
        };

        if name.as_str().eq_ignore_ascii_case("req-id") {
            continue;
        }

        headers.insert(StringId::new(name.as_str()), value.to_string());
    }

    let subject = uri_subject::uri_to_path(req.uri().clone());
    tracing::debug!(?subject);
    let method = req.method().clone();
    let query = uri_subject::uri_to_query(req.uri());
    let body = req.into_body();

    let response = handler
        .handle(Request {
            method,
            path: subject,
            headers,
            query,
            body,
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
    let headers = response.extra_headers();
    let response = response.into_body();
    let content_type = response.content_type();

    let mut response = hyper::Response::new(WriteBody {
        write: response,
        buf: Default::default(),
    });
    *response.status_mut() = status;

    if !content_type.is_empty() {
        response.headers_mut().append(
            "content-type",
            HeaderValue::from_str(&content_type).map_err(io::Error::other)?,
        );
    }

    for (header, value) in headers {
        response.headers_mut().append(
            HeaderName::from_str(&header).map_err(io::Error::other)?,
            HeaderValue::from_str(&value).map_err(io::Error::other)?,
        );
    }

    Ok(response)
}

#[pin_project]
struct WriteBody<R> {
    #[pin]
    write: R,
    buf: BytesMut,
}
impl<R> hyper::body::Body for WriteBody<R>
where
    R: ResponseBody,
{
    type Data = BytesMut;
    type Error = io::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Option<Result<hyper::body::Frame<Self::Data>, Self::Error>>> {
        let self_ = self.project();
        if self_.buf.len() < 4096 {
            self_.buf.resize(8192, 0);
        }

        let mut buf = ReadBuf::new(self_.buf);
        match AsyncRead::poll_read(self_.write, cx, &mut buf) {
            Poll::Ready(Ok(())) => match buf.filled().len() {
                0 => Poll::Ready(None),
                n => Poll::Ready(Some(Ok(hyper::body::Frame::data(self_.buf.split_to(n))))),
            },
            Poll::Ready(Err(e)) => Poll::Ready(Some(Err(e))),
            Poll::Pending => Poll::Pending,
        }
    }

    fn size_hint(&self) -> hyper::body::SizeHint {
        match self.write.length() {
            Some(len) => hyper::body::SizeHint::with_exact(len),
            None => Default::default(),
        }
    }
}

pub enum Never {}

struct PanicResponse;
impl Response for PanicResponse {
    type Body = NoBody;

    fn status_code(&self) -> StatusCode {
        StatusCode::INTERNAL_SERVER_ERROR
    }

    fn into_body(self) -> Self::Body {
        NoBody
    }
}
