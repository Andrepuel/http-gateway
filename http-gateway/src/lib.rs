#![recursion_limit = "512"]
//! A small, tree-structured router for building REST APIs.
//!
//! An API is described as a tree of [`MakeRoute`](router::MakeRoute) nodes. Each
//! node implements [`register`](router::MakeRoute::register), where it declares —
//! on the [`Router`](router::Router) it is given — the middleware, routes, and
//! leaves available at that point in the URL path. The router walks the tree one
//! path segment at a time, evaluating declarations in three phases (middleware,
//! then routes while segments remain, then leaves once the path is exhausted),
//! taking the first match in each phase. See [`MakeRoute`](router::MakeRoute) for
//! the full model.
//!
//! You rarely call the four [`Router`](router::Router) primitives directly.
//! Instead you use the ergonomic helpers on the extension traits, all of which
//! are in scope automatically on the `router` value:
//!
//! - [`RouterDerived`](router::RouterDerived) — paths, path parameters, and
//!   method leaves ([`path`](router::RouterDerived::path),
//!   [`get`](router::RouterDerived::get), [`get_route`](router::RouterDerived::get_route), …).
//! - [`RouterAuthExt`](router::auth::RouterAuthExt) — `Authorization` header
//!   handling.
//! - [`RouterExt`](router::ext::RouterExt) — JSON body deserialization,
//!   attribute endpoints, and transactional middleware.
//!
//! <div class="warning">This documentation was generated with the help of
//! AI-assisted tools.</div>
//!
//! # Quick start
//!
//! Define a root node, mount sub-resources with
//! [`path`](router::RouterDerived::path), answer requests with method leaves,
//! and serve it with [`http_server_main`]:
//!
//! ```ignore
//! use http_gateway::{
//!     handler::Json,
//!     http_server_main,
//!     router::{MakeRoute, Router, RouterDerived, RouterHandler},
//! };
//!
//! #[derive(Clone)]
//! struct Api;
//! impl MakeRoute for Api {
//!     async fn register<R: Router<Self>>(router: &mut R) {
//!         // GET /health
//!         router.get_path("health", async |_, _| Json::j200("ok")).await;
//!         // mount everything under /users
//!         router.path("users", async |_, _| Users).await;
//!     }
//! }
//!
//! struct Users;
//! impl MakeRoute for Users {
//!     async fn register<R: Router<Self>>(router: &mut R) {
//!         // GET /users
//!         router.get(async |_, _| Json::j200(["alice", "bob"])).await;
//!         // GET /users/{id}
//!         router
//!             .get_route(async |_, _, id| Json::j200(id.id().to_string()))
//!             .await;
//!     }
//! }
//!
//! fn main() {
//!     // `LISTEN=http://0.0.0.0:8080` is read from the environment.
//!     http_server_main(async || RouterHandler::new(Api));
//! }
//! ```
//!
//! A handler returns any [`Response`]; [`Json`](handler::Json)
//! and [`Json201`](handler::Json201) cover the common JSON cases.
//!
//! # Common helpers
//!
//! All of the following live on [`RouterDerived`](router::RouterDerived):
//!
//! - **Mounting** — [`path("name", …)`](router::RouterDerived::path) descends
//!   into the returned node when the next segment equals `"name"`;
//!   [`route(…)`](router::RouterDerived::route) matches *any* next segment and
//!   passes it to the closure, the way to capture a path parameter such as an id.
//! - **Method leaves** (the path must be fully consumed) —
//!   [`get`](router::RouterDerived::get), [`post`](router::RouterDerived::post),
//!   [`put`](router::RouterDerived::put),
//!   [`delete`](router::RouterDerived::delete), or
//!   [`any_leaf`](router::RouterDerived::any_leaf) for any method.
//! - **Segment + method in one call** —
//!   [`get_path`](router::RouterDerived::get_path) and friends match a fixed
//!   trailing segment (`POST /users/login`);
//!   [`get_route`](router::RouterDerived::get_route) and friends match a single
//!   trailing parameter (`GET /users/{id}`). Both require the segment to be the
//!   last one, so unmatched requests fall through to sibling declarations rather
//!   than 404-ing early.
//! - **Conditionals** — [`route_if`](router::RouterDerived::route_if),
//!   [`leaf_if`](router::RouterDerived::leaf_if), and
//!   [`middleware_if`](router::RouterDerived::middleware_if) gate a declaration
//!   on the node and request.
//! - **Recursion** — when a node routes into *its own type* (nested, tree-shaped
//!   resources), use [`path_recursive`](router::RouterDerived::path_recursive) /
//!   [`route_recursive`](router::RouterDerived::route_recursive).
//!
//! A route closure returns a [`MakeRoute`](router::MakeRoute), and the standard
//! library types implement it so you can branch or bail out inline:
//! `Result<T, E>` turns an `Err` into a response (short-circuit),
//! `Option<T>` turns `None` into `404`, and
//! [`Either<L, R>`](either::Either) lets one site yield two different node types.
//!
//! ```ignore
//! router
//!     .route(async |_, _, id| match load_user(&id).await {
//!         Some(user) => Ok(UserRoutes(user)), // descend into the resource
//!         None => Err(Json(Error::not_found(), StatusCode::NOT_FOUND)),
//!     })
//!     .await;
//! ```
//!
//! # Authentication
//!
//! [`authorization`](router::auth::RouterAuthExt::authorization) is middleware
//! that fires when the request carries an `Authorization` header. The header is
//! **removed from the request** before your closure runs (so credentials do not
//! leak into deeper nodes), then you receive the node, the scheme as a
//! [`StringId`] (e.g. `Bearer`), the credentials, and the
//! request. Return `Ok(node)` to continue routing as the authenticated caller,
//! or any error that converts into [`AuthError`](router::auth::AuthError) to
//! reject with `401`/`500`:
//!
//! ```ignore
//! use http_gateway::router::auth::{AuthError, RouterAuthExt};
//!
//! impl MakeRoute for Protected {
//!     async fn register<R: Router<Self>>(router: &mut R) {
//!         router
//!             .authorization(async |_, scheme, token, _req| {
//!                 if scheme == "Bearer" && verify(token).await {
//!                     Ok(Inner)
//!                 } else {
//!                     Err(AuthError::InvalidCredentials)
//!                 }
//!             })
//!             .await;
//!     }
//! }
//! ```
//!
//! Requests without an `Authorization` header skip this middleware, leaving
//! later declarations free to serve the unauthenticated case.
//!
//! # Handling the request body
//!
//! [`RouterExt`](router::ext::RouterExt) reads and JSON-deserializes the body
//! for you. The terminal helpers
//! [`post_body`](router::ext::RouterExt::post_body) and
//! [`put_body`](router::ext::RouterExt::put_body) parse the payload into a typed
//! value and hand it to the leaf alongside the request; a malformed body becomes
//! `400 Bad Request`:
//!
//! ```ignore
//! use http_gateway::{handler::Json201, router::ext::RouterExt};
//!
//! #[derive(serde::Deserialize)]
//! struct NewUser { name: String }
//!
//! impl MakeRoute for Users {
//!     async fn register<R: Router<Self>>(router: &mut R) {
//!         // POST /users  with a JSON body
//!         router
//!             .post_body(async |_, body: NewUser, _req| {
//!                 Json201(create_user(body.name).await)
//!             })
//!             .await;
//!     }
//! }
//! ```
//!
//! For non-terminal nodes, [`deserialize`](router::ext::RouterExt::deserialize)
//! is the same parsing as middleware, while
//! [`setter`](router::ext::RouterExt::setter) and
//! [`attribute`](router::ext::RouterExt::attribute) build a `PUT`-to-write
//! (and, for `attribute`, `GET`-to-read) endpoint from plain getter/setter
//! closures.
//!
//! # Transactions
//!
//! [`transaction`](router::ext::RouterExt::transaction) wraps the subtree below
//! a node in a unit of work whose outcome follows the response: it **commits on
//! a successful (2xx) response and rolls back on anything else**. You provide
//! four steps — `begin` derives a handle from the node, `route` produces the
//! child to descend into (borrowing the handle), and `commit`/`rollback` finish
//! the work:
//!
//! ```ignore
//! use http_gateway::router::ext::RouterExt;
//!
//! impl MakeRoute for Orders {
//!     async fn register<R: Router<Self>>(router: &mut R) {
//!         router
//!             .transaction(
//!                 async |db: &mut Db| db.begin().await,       // begin -> Result<Tx, E>
//!                 async |_self, tx| OrderRoutes(tx),          // route, using the Tx
//!                 async |tx| tx.commit().await,               // commit -> Result<(), E>
//!                 async |tx| tx.rollback().await,             // rollback
//!             )
//!             .await;
//!     }
//! }
//! ```
//!
//! Because the decision is driven purely by the status code, a handler controls
//! the transaction simply by the [`Response`] it returns —
//! returning an error response anywhere downstream rolls the whole thing back.

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

/// Process entry point that boots and runs the HTTP server, blocking until it
/// exits.
///
/// It performs the full startup sequence:
///
/// 1. loads environment variables from a `.env` file if present (a malformed
///    file is logged and ignored),
/// 2. initializes the [`tracing`] logging subsystem,
/// 3. starts a single-threaded async runtime, and
/// 4. reads the `LISTEN` environment variable — a URL such as
///    `http://0.0.0.0:8080` — and serves on its host and port.
///
/// `handler` is an async factory invoked once inside the runtime to build the
/// [`Handler`] (typically a
/// [`RouterHandler`](crate::router::RouterHandler)) that serves every request.
/// The server only returns on a fatal error, which is logged; a missing or
/// malformed `LISTEN` is such an error.
pub fn http_server_main<F, H>(handler: F)
where
    F: AsyncFnOnce() -> H,
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

            http_server(listen, handler().await)
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

/// Binds a TCP socket on the `listen` address and serves HTTP from it,
/// dispatching every request to `handler`.
///
/// The host and port are taken from the `listen` [`Url`]. Each accepted
/// connection is served on its own local task over HTTP/1, and each request is
/// tagged with a request id (from a `req-id` header, or a freshly generated
/// UUID) for tracing. The shared `handler` produces the [`Response`] for every
/// request. This loops forever, so it only returns by way of the [`Err`] case;
/// the [`Never`] success type signals it never completes normally.
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

/// An uninhabited type marking a value that can never be produced, used as the
/// `Ok` type of [`http_server`] to express that it only returns on error.
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
