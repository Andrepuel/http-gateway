use http_gateway::{
    handler::{Authorization, Json200, StringId},
    router::{self, MakeRoute, RouterHandler},
};
use std::collections::HashMap;

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
