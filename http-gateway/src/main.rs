use http_gateway::{
    handler::{Authorization, Json, StringId},
    router::{self, MakeRoute, RouterHandler},
};
use std::{collections::HashMap, io};

fn main() {
    http_gateway::http_server_main(|| RouterHandler::new(AuthRouter));
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
            .any_leaf(async |self_, mut req| {
                let body = req.collect_body().await.map_err(io::Error::other)?;
                io::Result::Ok(Json::j200(EchoResponse {
                    auth: self_.auth.map(|auth| (auth.scheme, auth.params)),
                    method: req.method.to_string(),
                    path: self_.path,
                    headers: req.headers,
                    query: req.query,
                    body: serde_json::from_slice(&body).map_err(|e| e.to_string()),
                }))
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
    pub body: Result<serde_json::Value, String>,
}
