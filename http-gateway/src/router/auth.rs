#![allow(async_fn_in_trait)]

use crate::{
    handler::{FullBody, Json, Request, Response, StringId},
    router::{MakeRoute, Router, RouterDerived, Then},
};

impl<T, S, B> RouterAuthExt<T, B> for S where S: Router<T, B> + RouterDerived<T, B> {}
pub trait RouterAuthExt<T, B>: Router<T, B> + RouterDerived<T, B> {
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
