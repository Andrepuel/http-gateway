#![allow(async_fn_in_trait)]

use crate::{
    handler::{Json, Request, Response, StringId},
    router::{EmptyResponse, MakeRoute, Router, RouterDerived, RouterResponse, Then},
};
use hyper::{Method, StatusCode, body::Incoming};

impl<T, S> RouterExt<T> for S where S: Router<T, Incoming> + RouterDerived<T, Incoming> {}
/// Higher-level, JSON-oriented routing helpers for real HTTP requests.
///
/// Where [`Router`] and [`RouterDerived`] are body-agnostic, these helpers are
/// fixed to the [`Incoming`] body so they can read and deserialize request
/// payloads. The trait is blanket-implemented for every such router, so its
/// methods are available on the `router` passed to [`MakeRoute::register`]. On
/// top of the routing primitives it adds JSON body deserialization
/// ([`deserialize`](Self::deserialize), [`leaf_body`](Self::leaf_body), …),
/// read/write attribute endpoints ([`setter`](Self::setter),
/// [`attribute`](Self::attribute)), and transactional middleware
/// ([`transaction`](Self::transaction)).
pub trait RouterExt<T>: Router<T, Incoming> + RouterDerived<T, Incoming> {
    /// Middleware that, when `if_` matches, reads the entire request body,
    /// deserializes it from JSON into `B`, and routes into the node returned by
    /// `f`.
    ///
    /// A transport error while collecting the body, or a JSON parse failure,
    /// short-circuits the request with `400 Bad Request` carrying the cause as
    /// a JSON `{ "error": ... }` body. Because it consumes the body, use it only
    /// for endpoints that expect a payload.
    async fn deserialize_if<B, I, F, U>(&mut self, if_: I, f: F)
    where
        B: serde::de::DeserializeOwned,
        I: FnOnce(&T, &Request<Incoming>) -> bool,
        F: AsyncFnOnce(T, B) -> U,
        U: MakeRoute<Incoming>,
    {
        self.middleware_if(if_, async |self_, req| {
            let bytes = req.collect_body().await.map_err(|e| {
                tracing::warn!(%e, "Collect body error");
                tracing::debug!(?e);
                SerdeError {
                    error: "Bad body transport".to_string(),
                }
            })?;

            let body = serde_json::from_slice(&bytes).map_err(|e| SerdeError {
                error: e.to_string(),
            })?;

            Result::<U, SerdeError>::Ok(f(self_, body).await)
        })
        .await
    }

    /// [`deserialize_if`](Self::deserialize_if) restricted to `POST` and `PUT`
    /// requests — the methods that conventionally carry a body.
    async fn deserialize<B, F, U>(&mut self, f: F)
    where
        B: serde::de::DeserializeOwned,
        F: AsyncFnOnce(T, B) -> U,
        U: MakeRoute<Incoming>,
    {
        self.deserialize_if(
            |_, req| req.method == Method::POST || req.method == Method::PUT,
            f,
        )
        .await
    }

    /// Terminal handler that deserializes the JSON request body into `B` and
    /// invokes `f(node, body, request)` to produce the [`Response`], for
    /// requests with the given `method`.
    ///
    /// This is the body-aware counterpart to [`leaf`](RouterDerived::leaf): the
    /// payload is parsed up front and passed to `f` alongside the still-readable
    /// [`Request`]. A malformed body yields `400 Bad Request`.
    async fn leaf_body<F, B, U>(&mut self, method: Method, f: F)
    where
        B: serde::de::DeserializeOwned,
        F: AsyncFnOnce(T, B, Request<Incoming>) -> U,
        U: Response,
    {
        self.deserialize_if::<B, _, _, _>(
            |_, req| req.method == method && req.path.is_empty(),
            async |self_, body| BodyRoute(self_, body, f),
        )
        .await;

        struct BodyRoute<T, B, F>(T, B, F);
        impl<T, B, F, U> MakeRoute<Incoming> for BodyRoute<T, B, F>
        where
            F: AsyncFnOnce(T, B, Request<Incoming>) -> U,
            U: Response,
        {
            async fn register<R: Router<Self, Incoming>>(router: &mut R) {
                router
                    .leaf_if(
                        |_, _| true,
                        async |body_route, req| {
                            (body_route.2)(body_route.0, body_route.1, req).await
                        },
                    )
                    .await;
            }
        }
    }

    /// [`leaf_body`](Self::leaf_body) fixed to [`Method::POST`].
    async fn post_body<F, B, U>(&mut self, f: F)
    where
        B: serde::de::DeserializeOwned,
        F: AsyncFnOnce(T, B, Request<Incoming>) -> U,
        U: Response,
    {
        self.leaf_body::<F, B, U>(Method::POST, f).await
    }

    /// [`leaf_body`](Self::leaf_body) fixed to [`Method::PUT`].
    async fn put_body<F, B, U>(&mut self, f: F)
    where
        B: serde::de::DeserializeOwned,
        F: AsyncFnOnce(T, B, Request<Incoming>) -> U,
        U: Response,
    {
        self.leaf_body::<F, B, U>(Method::PUT, f).await
    }

    /// Declares a `PUT` endpoint at the path segment `name` that deserializes
    /// the JSON body into `B` and hands it to `set`.
    ///
    /// `set` returning `Ok(())` replies `200 OK` with an empty body; `Err(e)`
    /// replies with `e`. This is the write half of an
    /// [`attribute`](Self::attribute).
    async fn setter<B, E, P, S>(&mut self, name: P, set: S)
    where
        B: serde::de::DeserializeOwned,
        E: Response,
        P: Into<StringId>,
        S: AsyncFnOnce(T, B) -> Result<(), E>,
    {
        let name = name.into();

        self.route_if(
            |_, req, path| req.method == Method::PUT && path == &name && req.path.is_empty(),
            async |self_, _, _| Body(self_, set, Default::default()),
        )
        .await;

        struct Body<T, S, B, E>(T, S, std::marker::PhantomData<(B, E)>);
        impl<T, S, B, E> MakeRoute<Incoming> for Body<T, S, B, E>
        where
            B: serde::de::DeserializeOwned,
            S: AsyncFnOnce(T, B) -> Result<(), E>,
            E: Response,
        {
            async fn register<R: Router<Self, Incoming>>(router: &mut R) {
                router
                    .deserialize::<B, _, _>(async |self_, body| {
                        Set(self_.0, self_.1, body, Default::default())
                    })
                    .await;
            }
        }
        struct Set<T, B, S, E>(T, S, B, std::marker::PhantomData<E>);
        impl<T, B, S, E> MakeRoute<Incoming> for Set<T, B, S, E>
        where
            B: serde::de::DeserializeOwned,
            S: AsyncFnOnce(T, B) -> Result<(), E>,
            E: Response,
        {
            async fn register<R: Router<Self, Incoming>>(router: &mut R) {
                router
                    .any_leaf(async |self_, _| match (self_.1)(self_.0, self_.2).await {
                        Ok(()) => Ok(EmptyResponse(StatusCode::OK)),
                        Err(e) => Err(e),
                    })
                    .await;
            }
        }
    }

    /// Declares a readable and writable attribute at the path segment `name`.
    ///
    /// `GET name` replies with `get(node)` serialized as JSON; `PUT name`
    /// deserializes the JSON body and applies it with `set` (see
    /// [`setter`](Self::setter)). Both halves share the same `name` and report
    /// failure via the [`Response`] error type `E`.
    async fn attribute<E, B, P, S, G>(&mut self, name: P, set: S, get: G)
    where
        E: Response,
        B: serde::Serialize + serde::de::DeserializeOwned + Clone + 'static,
        P: Into<StringId>,
        S: AsyncFnOnce(T, B) -> Result<(), E>,
        G: AsyncFnOnce(T) -> Result<B, E>,
    {
        let name = name.into();
        self.leaf_path(name.clone(), Method::GET, async |self_, _| {
            get(self_).await.map(Json::j200)
        })
        .await;

        self.setter(name, set).await;
    }

    /// Wraps the subtree below this node in a transaction whose outcome follows
    /// the response.
    ///
    /// The four callbacks form the transaction lifecycle:
    ///
    /// - `begin` derives a transaction handle `D` from the node; if it fails
    ///   the request short-circuits with the error and the rest is skipped.
    /// - `route` produces the child [`MakeRoute`] to descend into, borrowing
    ///   the handle mutably so the routed work can use the transaction.
    /// - `commit` runs once the downstream [`RouterResponse`] is successful
    ///   ([`is_success`](RouterResponse::is_success)); if the commit itself
    ///   fails its error replaces the response.
    /// - `rollback` runs instead whenever the response is not successful, and
    ///   the original response is preserved.
    ///
    /// In other words the transaction commits on a 2xx and rolls back on
    /// anything else, so handlers control the outcome simply by the status they
    /// return.
    async fn transaction<'a, B, F, U, C, R, D, E>(
        &mut self,
        begin: B,
        route: F,
        commit: C,
        rollback: R,
    ) where
        B: AsyncFnOnce(&mut T) -> Result<D, E>,
        F: AsyncFnOnce(T, &'a mut D) -> U + 'a,
        U: MakeRoute<Incoming> + 'a,
        C: AsyncFnOnce(D) -> Result<(), E> + 'a,
        R: AsyncFnOnce(D) + 'a,
        E: Response,
        T: 'a,
        D: 'a,
    {
        self.middleware(async move |mut self_, _| {
            let trans = begin(&mut self_).await;
            trans.map(|trans| Transaction {
                self_: Some(self_),
                route: Some(route),
                trans,
                commit,
                rollback,
                marker: Default::default(),
            })
        })
        .await;

        struct Transaction<'a, T, F, D, C, R> {
            self_: Option<T>,
            route: Option<F>,
            trans: D,
            commit: C,
            rollback: R,
            marker: std::marker::PhantomData<fn(&'a ())>,
        }
        impl<'a, T, F, U, D, C, R, E> MakeRoute<Incoming> for Transaction<'a, T, F, D, C, R>
        where
            F: AsyncFnOnce(T, &'a mut D) -> U + 'a,
            U: MakeRoute<Incoming> + 'a,
            C: AsyncFnOnce(D) -> Result<(), E> + 'a,
            R: AsyncFnOnce(D) + 'a,
            E: Response,
            T: 'a,
            D: 'a,
        {
            async fn register<Ro: Router<Self, Incoming>>(router: &mut Ro) {
                router
                    .middleware_mut_map(
                        |self_, _| Then::Then(self_),
                        async |self_, _| {
                            (self_.route.take().unwrap())(
                                self_.self_.take().unwrap(),
                                &mut self_.trans,
                            )
                            .await
                        },
                        async |self_, res| match res.is_success() {
                            true => match (self_.commit)(self_.trans).await {
                                Ok(()) => res,
                                Err(error) => RouterResponse::new(error),
                            },
                            false => {
                                (self_.rollback)(self_.trans).await;
                                res
                            }
                        },
                    )
                    .await
            }
        }
    }
}

/// `400 Bad Request` response produced when a request body cannot be read or
/// JSON-deserialized, rendered as a JSON `{ "error": ... }` body.
#[derive(serde::Serialize)]
struct SerdeError {
    error: String,
}
impl Response for SerdeError {
    type Body = <Json<SerdeError> as Response>::Body;

    fn status_code(&self) -> StatusCode {
        StatusCode::BAD_REQUEST
    }

    fn into_body(self) -> Self::Body {
        let code = self.status_code();
        Json(self, code).into_body()
    }
}
