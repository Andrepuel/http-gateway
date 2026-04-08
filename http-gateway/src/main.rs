use std::{collections::HashMap, io};

use http_gateway::{
    Never,
    handler::{Authorization, Json200, StringId},
    http_server,
    router::{self, MakeRoute, RouterHandler},
};
use tracing::Instrument;

fn main() {
    let r = dotenvy::dotenv();
    tracing_subscriber::fmt::init();
    if let Err(e) = r {
        tracing::warn!(%e, "Bad dotenv file");
        tracing::debug!(?e);
    }

    let r = tokio::runtime::LocalRuntime::new()
        .unwrap()
        .block_on(async_main());

    match r {
        Ok(_) => unreachable!(),
        Err(e) => {
            tracing::error!(%e, "Fatal error");
            tracing::debug!(?e);
        }
    }
}

async fn async_main() -> io::Result<Never> {
    let listen = std::env::var("LISTEN")
        .map_err(|e| io::Error::other(format!("Missing var LISTEN: {e}")))?;

    let listen = listen
        .parse()
        .map_err(|e| io::Error::other(format!("Bad LISTEN url ({listen:?}): {e}")))?;

    http_server(listen, RouterHandler::new(AuthRouter))
        .instrument(tracing::info_span!("server"))
        .await
}

#[derive(Clone, Copy)]
struct AuthRouter;
impl MakeRoute for AuthRouter {
    async fn register<R: router::Router<Self>>(router: &mut R) {
        router
            .middleware(async |_self_, req| {
                let auth = req
                    .headers
                    .remove(&"authorization".into())
                    .map(Authorization::from);

                Some(EchoRouter {
                    auth,
                    path: Default::default(),
                })
            })
            .await;
    }
}

#[derive(Clone)]
struct EchoRouter {
    auth: Option<Authorization>,
    path: Vec<StringId>,
}

impl MakeRoute for EchoRouter {
    async fn register<R: router::Router<Self>>(router: &mut R) {
        router
            .route_recursive(async |mut self_, _req, hop| {
                self_.path.push(hop);
                self_
            })
            .await;

        router
            .any_leaf(async |self_, req| {
                Json200(EchoResponse {
                    auth: self_.auth.map(|auth| (auth.scheme, auth.params)),
                    method: req.method.to_string(),
                    path: self_.path,
                    headers: req.headers,
                    query: req.query,
                    body: req.body,
                })
            })
            .await;
    }
}

#[derive(serde::Serialize)]
struct EchoResponse {
    pub auth: Option<(StringId, String)>,
    pub method: String,
    pub path: Vec<StringId>,
    pub headers: HashMap<StringId, String>,
    pub query: HashMap<StringId, String>,
    pub body: Option<serde_json::Value>,
}
