#![allow(async_fn_in_trait)]

use crate::{
    handler::{FullBody, Json, Request, Response, StringId},
    router::{MakeRoute, Router, RouterDerived, Then},
};

impl<T, S, B> RouterAuthExt<T, B> for S where S: Router<T, B> + RouterDerived<T, B> {}
/// Authentication helpers layered on top of [`Router`], available on the
/// `router` passed to [`MakeRoute::register`].
pub trait RouterAuthExt<T, B>: Router<T, B> + RouterDerived<T, B> {
    /// Middleware that authenticates a request carrying an `Authorization`
    /// header.
    ///
    /// When the header is present it is **removed from the request** before
    /// `f` runs, so the raw credentials do not leak into the nodes deeper in
    /// the routing tree. The header value is then split into its scheme and
    /// credentials and `f` is invoked with:
    ///
    /// 1. the node (`Self`),
    /// 2. the authorization scheme as a [`StringId`] (e.g. `Bearer`),
    /// 3. the credentials that follow the scheme (e.g. the bearer token), and
    /// 4. a mutable reference to the rest of the [`Request`].
    ///
    /// `f` returns `Ok(node)` to continue routing into that node once the
    /// caller is authenticated, or `Err(e)` to reject the request — any error
    /// convertible into [`AuthError`] becomes a `401`/`500` response. A header
    /// that is not of the form `<scheme> <credentials>` is rejected with
    /// [`AuthError::MalformedAuthorizationHeader`] before `f` is reached.
    ///
    /// If the request has **no** `Authorization` header this middleware does
    /// not match, leaving later declarations at the node free to handle the
    /// unauthenticated case.
    async fn authorization<F, U, E>(&mut self, f: F)
    where
        F: AsyncFnOnce(T, StringId, &str, &mut Request<B>) -> Result<U, E>,
        U: MakeRoute<B>,
        AuthError: From<E>,
    {
        self.middleware_map(
            |self_, req| match req.headers.remove(&StringId::from("authorization")) {
                Some(auth) => Then::Then((self_, auth)),
                None => Then::Else(self_),
            },
            async |(self_, auth), req| {
                let Some((scheme, credentials)) = auth.split_once(' ') else {
                    tracing::warn!(?auth, "Malformed Authorization header value");
                    return Err(AuthError::MalformedAuthorizationHeader);
                };

                let scheme = StringId::new(scheme);

                f(self_, scheme, credentials, req).await.map_err(|e| {
                    let e = AuthError::from(e);
                    tracing::debug!(?e, "Error authenticating user");
                    e
                })
            },
        )
        .await;
    }
}

/// The error surface of [`authorization`](RouterAuthExt::authorization),
/// usable as a [`Response`]: the credential-related variants map to `401
/// Unauthorized` and [`Error`](AuthError::Error) to `500`, each rendered as a
/// JSON `{ "error": ... }` body.
#[derive(thiserror::Error, Debug)]
pub enum AuthError {
    #[error("Authorization header value is not in the format <scheme> <credentials>")]
    MalformedAuthorizationHeader,
    #[error("The scheme {0} is not supported")]
    UnsupportedScheme(StringId),
    #[error("Invalid credentials")]
    InvalidCredentials,
    #[error("Internal server error")]
    Error(#[from] std::io::Error),
}
impl Response for AuthError {
    type Body = FullBody;

    fn status_code(&self) -> hyper::StatusCode {
        match self {
            AuthError::MalformedAuthorizationHeader
            | AuthError::UnsupportedScheme(_)
            | AuthError::InvalidCredentials => hyper::StatusCode::UNAUTHORIZED,
            AuthError::Error(_) => hyper::StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn into_body(self) -> Self::Body {
        let code = self.status_code();

        Json(
            AuthErrorBody {
                error: self.to_string(),
            },
            code,
        )
        .into_body()
    }
}

#[derive(serde::Serialize)]
struct AuthErrorBody {
    error: String,
}
