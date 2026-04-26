#![allow(async_fn_in_trait)]

use crate::{
    handler::{Json, Request, Response, StringId},
    router::{EmptyResponse, MakeRoute, Router},
};
use hyper::{Method, StatusCode, body::Incoming};

impl<T, S> RouterExt<T> for S where S: Router<T, Incoming> {}
pub trait RouterExt<T>: Router<T, Incoming> {
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

    async fn leaf_body<F, B, U>(&mut self, method: Method, f: F)
    where
        B: serde::de::DeserializeOwned,
        F: AsyncFnOnce(T, B, Request<Incoming>) -> U,
        U: Response,
    {
        self.deserialize_if::<B, _, _, _>(
            |_, req| req.method == method,
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

    async fn post_body<F, B, U>(&mut self, f: F)
    where
        B: serde::de::DeserializeOwned,
        F: AsyncFnOnce(T, B, Request<Incoming>) -> U,
        U: Response,
    {
        self.leaf_body::<F, B, U>(Method::POST, f).await
    }

    async fn put_body<F, B, U>(&mut self, f: F)
    where
        B: serde::de::DeserializeOwned,
        F: AsyncFnOnce(T, B, Request<Incoming>) -> U,
        U: Response,
    {
        self.leaf_body::<F, B, U>(Method::PUT, f).await
    }

    async fn setter<B, E, P, S>(&mut self, name: P, set: S)
    where
        B: serde::de::DeserializeOwned,
        E: Response,
        P: Into<StringId>,
        S: AsyncFnOnce(T, B) -> Result<(), E>,
    {
        let name = name.into();

        self.route_if(
            |_, req, path| req.method == Method::PUT && path == &name,
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
}

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
