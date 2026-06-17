pub mod auth;
pub mod ext;

use crate::handler::{Empty404, Handler, NoBody, Request, Response, ResponseBody, StringId};
use either::Either;
use futures::FutureExt;
use hyper::{Method, StatusCode, body::Incoming};
use std::{any::TypeId, collections::HashMap, pin::Pin};

/// The [`Handler`] that drives a routing tree.
///
/// Wraps a root [`MakeRoute`] node `R` and, for each request, walks the tree
/// from that root to produce a [`RouterResponse`]. This is the primary
/// [`Handler`] implementation; construct one with [`new`](RouterHandler::new)
/// and hand it to the server.
pub struct RouterHandler<B, R> {
    root: R,
    body_type: std::marker::PhantomData<fn(B)>,
}
impl<B, R> RouterHandler<B, R>
where
    R: MakeRoute<B> + Clone,
{
    /// Build a handler that routes every request starting from `root`.
    ///
    /// `root` is cloned per request, so it holds the shared state from which a
    /// request's routing begins.
    pub fn new(root: R) -> Self {
        Self {
            root,
            body_type: Default::default(),
        }
    }
}
impl<B, R> RouterHandler<B, R>
where
    R: MakeRoute<B>,
{
    async fn do_handle(root: R, req: Request<B>) -> RouterResponse {
        struct FindMiddleware<B, R>(RouterState<(R, Request<B>)>);
        impl<B, R> Router<R, B> for FindMiddleware<B, R> {
            async fn middleware_mut_map<'a, I, T2, F, U, P>(&mut self, if_: I, f: F, post: P)
            where
                T2: 'a,
                I: FnOnce(R, &mut Request<B>) -> Then<T2, R>,
                F: AsyncFnOnce(&'a mut T2, &mut Request<B>) -> U,
                U: MakeRoute<B> + 'a,
                P: AsyncFnOnce(T2, RouterResponse) -> RouterResponse,
            {
                self.0
                    .execute_map(
                        |(root, mut req)| match if_(root, &mut req) {
                            Then::Then(t2) => Then::Then((t2, req)),
                            Then::Else(root) => Then::Else((root, req)),
                        },
                        async |(mut root, mut req)| {
                            let response = {
                                let root = unsafe {
                                    std::mem::transmute::<&mut T2, &'a mut T2>(&mut root)
                                };
                                let route = f(root, &mut req).await;
                                RouterHandler::<B, U>::do_handle(route, req).await
                            };
                            post(root, response).await
                        },
                    )
                    .await;
            }

            async fn route_map<I, T2, F, U>(&mut self, _if_: I, _f: F)
            where
                I: FnOnce(R, &Request<B>, &StringId) -> Then<T2, R>,
                F: AsyncFnOnce(T2, &mut Request<B>, StringId) -> U,
                U: MakeRoute<B>,
            {
            }

            async fn route_map_recursive<I, T2, F, U>(&mut self, _if_: I, _f: F)
            where
                I: FnOnce(R, &StringId) -> Then<T2, R>,
                F: AsyncFnOnce(T2, &mut Request<B>, StringId) -> U,
                U: MakeRoute<B>,
            {
            }

            async fn leaf_map<I, T2, F, U>(&mut self, _if_: I, _f: F)
            where
                I: FnOnce(R, &Method) -> Then<T2, R>,
                F: AsyncFnOnce(T2, Request<B>) -> U,
                U: Response,
            {
            }
        }
        let mut middleware = FindMiddleware((root, req).into());
        R::register(&mut middleware).await;
        let (root, mut req) = match middleware.0.take() {
            Either::Left(root_req) => root_req,
            Either::Right(result) => return result,
        };

        let next = req.path.pop_front();

        match next {
            Some(path) => {
                struct FindRoute<B, R>(RouterState<(StringId, R, Request<B>)>);
                impl<B, R> Router<R, B> for FindRoute<B, R> {
                    async fn route_map<I, R2, F, U>(&mut self, if_: I, f: F)
                    where
                        I: FnOnce(R, &Request<B>, &StringId) -> Then<R2, R>,
                        F: AsyncFnOnce(R2, &mut Request<B>, StringId) -> U,
                        U: MakeRoute<B>,
                    {
                        self.0
                            .execute_map(
                                |(req_path, root, req)| match if_(root, &req, &req_path) {
                                    Then::Then(root2) => Then::Then((req_path, root2, req)),
                                    Then::Else(root) => Then::Else((req_path, root, req)),
                                },
                                async |(path, root, mut req)| {
                                    let route = f(root, &mut req, path).await;

                                    RouterHandler::<B, U>::do_handle(route, req).await
                                },
                            )
                            .await;
                    }

                    async fn route_map_recursive<I, R2, F, U>(&mut self, if_: I, f: F)
                    where
                        I: FnOnce(R, &StringId) -> Then<R2, R>,
                        F: AsyncFnOnce(R2, &mut Request<B>, StringId) -> U,
                        U: MakeRoute<B>,
                    {
                        self.0
                            .execute_map(
                                |(req_path, root, req)| match if_(root, &req_path) {
                                    Then::Then(root2) => Then::Then((req_path, root2, req)),
                                    Then::Else(root) => Then::Else((req_path, root, req)),
                                },
                                async |(path, root, mut req)| {
                                    let route = f(root, &mut req, path).await;

                                    RouterHandler::<B, U>::do_handle(route, req)
                                        .boxed_local()
                                        .await
                                },
                            )
                            .await;
                    }

                    async fn middleware_mut_map<'a, I, T2, F, U, P>(
                        &mut self,
                        _if_: I,
                        _f: F,
                        _post: P,
                    ) where
                        T2: 'a,
                        I: FnOnce(R, &mut Request<B>) -> Then<T2, R>,
                        F: AsyncFnOnce(&'a mut T2, &mut Request<B>) -> U,
                        U: MakeRoute<B> + 'a,
                        P: AsyncFnOnce(T2, RouterResponse) -> RouterResponse,
                    {
                    }

                    async fn leaf_map<I, T2, F, U>(&mut self, _if_: I, _f: F)
                    where
                        I: FnOnce(R, &Method) -> Then<T2, R>,
                        F: AsyncFnOnce(T2, Request<B>) -> U,
                        U: Response,
                    {
                    }
                }

                let mut find_route = FindRoute((path, root, req).into());
                R::register(&mut find_route).await;
                match find_route.0.take() {
                    Either::Left((hop, _, req)) => {
                        tracing::debug!(?hop, method=?req.method, path=?req.path, "Could not find route");
                        RouterResponse::e404()
                    }
                    Either::Right(response) => response,
                }
            }
            None => {
                struct MakeRouteLeaf<B, R>(RouterState<(R, Request<B>)>);
                impl<B, R> Router<R, B> for MakeRouteLeaf<B, R> {
                    async fn leaf_map<I, R2, F, U>(&mut self, if_: I, f: F)
                    where
                        I: FnOnce(R, &Method) -> Then<R2, R>,
                        F: AsyncFnOnce(R2, Request<B>) -> U,
                        U: Response,
                    {
                        self.0
                            .execute_map(
                                |(root, req)| match if_(root, &req.method) {
                                    Then::Then(root2) => Then::Then((root2, req)),
                                    Then::Else(root) => Then::Else((root, req)),
                                },
                                async |(root, req)| RouterResponse::new(f(root, req).await),
                            )
                            .await;
                    }

                    async fn middleware_mut_map<'a, I, T2, F, U, P>(
                        &mut self,
                        _if_: I,
                        _f: F,
                        _post: P,
                    ) where
                        T2: 'a,
                        I: FnOnce(R, &mut Request<B>) -> Then<T2, R>,
                        F: AsyncFnOnce(&'a mut T2, &mut Request<B>) -> U,
                        U: MakeRoute<B> + 'a,
                        P: AsyncFnOnce(T2, RouterResponse) -> RouterResponse,
                    {
                    }

                    async fn route_map<I, T2, F, U>(&mut self, _if_: I, _f: F)
                    where
                        I: FnOnce(R, &Request<B>, &StringId) -> Then<T2, R>,
                        F: AsyncFnOnce(T2, &mut Request<B>, StringId) -> U,
                        U: MakeRoute<B>,
                    {
                    }

                    async fn route_map_recursive<I, T2, F, U>(&mut self, _if_: I, _f: F)
                    where
                        I: FnOnce(R, &StringId) -> Then<T2, R>,
                        F: AsyncFnOnce(T2, &mut Request<B>, StringId) -> U,
                        U: MakeRoute<B>,
                    {
                    }
                }
                let mut call = MakeRouteLeaf((root, req).into());
                R::register(&mut call).await;

                match call.0.take() {
                    Either::Right(r) => r,
                    Either::Left((root, req)) => {
                        let mut other_method = None;
                        let mut root = Some(root);

                        for method in [
                            Method::HEAD,
                            Method::GET,
                            Method::PUT,
                            Method::POST,
                            Method::DELETE,
                        ] {
                            struct FindOtherMethods<R>(Method, bool, Option<R>);
                            impl<B, R> Router<R, B> for FindOtherMethods<R> {
                                async fn leaf_map<I, R2, F, U>(&mut self, if_: I, _f: F)
                                where
                                    I: FnOnce(R, &Method) -> Then<R2, R>,
                                    F: AsyncFnOnce(R2, Request<B>) -> U,
                                    U: Response,
                                {
                                    if let Some(root) = self.2.take() {
                                        match if_(root, &self.0) {
                                            Then::Then(_) => self.1 = true,
                                            Then::Else(root) => self.2 = Some(root),
                                        }
                                    }
                                }

                                async fn middleware_mut_map<'a, I, T2, F, U, P>(
                                    &mut self,
                                    _if_: I,
                                    _f: F,
                                    _post: P,
                                ) where
                                    T2: 'a,
                                    I: FnOnce(R, &mut Request<B>) -> Then<T2, R>,
                                    F: AsyncFnOnce(&'a mut T2, &mut Request<B>) -> U,
                                    U: MakeRoute<B> + 'a,
                                    P: AsyncFnOnce(T2, RouterResponse) -> RouterResponse,
                                {
                                }

                                async fn route_map<I, T2, F, U>(&mut self, _if_: I, _f: F)
                                where
                                    I: FnOnce(R, &Request<B>, &StringId) -> Then<T2, R>,
                                    F: AsyncFnOnce(T2, &mut Request<B>, StringId) -> U,
                                    U: MakeRoute<B>,
                                {
                                }

                                async fn route_map_recursive<I, T2, F, U>(&mut self, _if_: I, _f: F)
                                where
                                    I: FnOnce(R, &StringId) -> Then<T2, R>,
                                    F: AsyncFnOnce(T2, &mut Request<B>, StringId) -> U,
                                    U: MakeRoute<B>,
                                {
                                }
                            }

                            let mut check_one = FindOtherMethods(method, false, root);
                            R::register(&mut check_one).await;
                            if check_one.1 {
                                other_method = Some(check_one.0);
                                break;
                            }
                            root = check_one.2;
                        }

                        match other_method {
                            None => {
                                tracing::debug!(method=?req.method, route=std::any::type_name::<R>(), "Not matching route for leaf");
                                RouterResponse::e404()
                            }
                            Some(other_method) => {
                                tracing::debug!(allowed=?other_method, method=?req.method, "Method not allowed");
                                RouterResponse::e405()
                            }
                        }
                    }
                }
            }
        }
    }
}
impl<B, R> Handler<B> for RouterHandler<B, R>
where
    R: MakeRoute<B> + Clone,
{
    type Response = RouterResponse;

    fn handle(&self, req: Request<B>) -> impl Future<Output = Self::Response> {
        tracing::debug!(path=?req.path);
        Self::do_handle(self.root.clone(), req)
    }
}

/// The declaration surface handed to [`MakeRoute::register`].
///
/// `T` is the node's own type (the `Self` of the [`MakeRoute`] being
/// registered) and `B` is the request body type. A `Router` collects the
/// node's middleware, routes, and leaves; the framework then evaluates them in
/// phase order (see [`MakeRoute`]) to resolve a request.
///
/// This trait holds only the four **primitive** declarations. Each takes a
/// matcher `if_` that receives the node by value and returns a [`Then`] —
/// `Then(t2)` to claim the request (optionally transforming the node into some
/// `T2`) or `Else(t)` to decline and pass the node on to the next declaration.
/// You normally do not call these directly; reach for the higher-level helpers
/// in [`RouterDerived`] ([`path`](RouterDerived::path),
/// [`get`](RouterDerived::get), [`middleware`](RouterDerived::middleware), …),
/// which are defined in terms of these four.
pub trait Router<T, B = Incoming> {
    /// Declare middleware: code that runs at this node *before* a path segment
    /// is consumed (phase 1).
    ///
    /// This is the most general middleware primitive. When `if_` returns
    /// `Then(t2)` the middleware claims the request:
    ///
    /// - `f` is invoked with a mutable borrow of the transformed state `t2`
    ///   and the [`Request`] (which it may rewrite), and returns the child
    ///   [`MakeRoute`] the router descends into to produce a response.
    /// - `post` is then invoked with the owned state and that
    ///   [`RouterResponse`], giving the middleware a chance to inspect or
    ///   replace the response on the way back out.
    ///
    /// The first matching middleware wins; later declarations at this node are
    /// skipped.
    fn middleware_mut_map<'a, I, T2, F, U, P>(
        &mut self,
        if_: I,
        f: F,
        post: P,
    ) -> impl Future<Output = ()>
    where
        T2: 'a,
        I: FnOnce(T, &mut Request<B>) -> Then<T2, T>,
        F: AsyncFnOnce(&'a mut T2, &mut Request<B>) -> U,
        U: MakeRoute<B> + 'a,
        P: AsyncFnOnce(T2, RouterResponse) -> RouterResponse;

    /// Declare a route: a child reached by consuming the next path segment
    /// (phase 2).
    ///
    /// `if_` sees the node, the [`Request`], and the upcoming segment
    /// ([`StringId`]) and decides whether to match. On `Then(t2)`, `f` receives
    /// the transformed state, the request, and the now-consumed segment, and
    /// returns the child [`MakeRoute`] to descend into. The first matching
    /// route wins; if none match, the router replies `404`.
    fn route_map<I, T2, F, U>(&mut self, if_: I, f: F) -> impl Future<Output = ()>
    where
        I: FnOnce(T, &Request<B>, &StringId) -> Then<T2, T>,
        F: AsyncFnOnce(T2, &mut Request<B>, StringId) -> U,
        U: MakeRoute<B>;

    /// Like [`route_map`](Router::route_map), but for a node that routes into
    /// *its own type*.
    ///
    /// The child future is boxed, erasing the recursion so the node type stays
    /// finite (an unboxed self-recursive route would be an infinitely-sized
    /// type). Because of that erasure the matcher `if_` is given only the node
    /// and the segment — not the [`Request`]; the request is still available to
    /// `f`. Use this for tree-shaped resources such as nested directories.
    fn route_map_recursive<I, T2, F, U>(&mut self, if_: I, f: F) -> impl Future<Output = ()>
    where
        I: FnOnce(T, &StringId) -> Then<T2, T>,
        F: AsyncFnOnce(T2, &mut Request<B>, StringId) -> U,
        U: MakeRoute<B>;

    /// Declare a leaf: a terminal handler reached when the path is exhausted
    /// (phase 3).
    ///
    /// `if_` selects on the request [`Method`]; on `Then(t2)`, `f` consumes the
    /// state and the owned [`Request`] and returns the final [`Response`]. The
    /// first matching leaf wins. If the path is exhausted but no leaf matches
    /// the method, the router replies `405` when another method would have
    /// matched, otherwise `404`.
    fn leaf_map<I, T2, F, U>(&mut self, if_: I, f: F) -> impl Future<Output = ()>
    where
        I: FnOnce(T, &Method) -> Then<T2, T>,
        F: AsyncFnOnce(T2, Request<B>) -> U,
        U: Response;
}

impl<T, B, R: Router<T, B>> RouterDerived<T, B> for R {}
/// Ergonomic helpers built on the four [`Router`] primitives.
///
/// This trait is blanket-implemented for every [`Router`], so its methods are
/// available on the `router` passed to [`MakeRoute::register`]. The helpers
/// fall into the three phases described on [`MakeRoute`]:
///
/// - **Middleware** — [`middleware`](Self::middleware),
///   [`middleware_if`](Self::middleware_if), [`middleware_map`](Self::middleware_map),
///   [`middleware_mut`](Self::middleware_mut), [`middleware_mut_if`](Self::middleware_mut_if).
/// - **Routes** (consume a segment, descend into a child) —
///   [`path`](Self::path), [`route`](Self::route), [`route_if`](Self::route_if),
///   and their [`path_recursive`](Self::path_recursive) /
///   [`route_recursive`](Self::route_recursive) counterparts.
/// - **Leaves** (terminal, by method) — [`get`](Self::get), [`put`](Self::put),
///   [`post`](Self::post), [`delete`](Self::delete), [`leaf`](Self::leaf),
///   [`any_leaf`](Self::any_leaf); the `*_path` and `*_route` families combine a
///   segment match with a terminal handler in one call.
pub trait RouterDerived<T, B>: Router<T, B> {
    /// Attach middleware that always runs at this node, transforming it into
    /// the [`MakeRoute`] returned by `f`. See [`middleware_mut_map`](Router::middleware_mut_map)
    /// for the underlying primitive.
    fn middleware<F, U>(&mut self, f: F) -> impl Future<Output = ()>
    where
        F: AsyncFnOnce(T, &mut Request<B>) -> U,
        U: MakeRoute<B>,
    {
        self.middleware_if(|_, _| true, f)
    }

    /// Attach middleware that runs only when `if_` returns `true` for the node
    /// and [`Request`].
    fn middleware_if<I, F, U>(&mut self, if_: I, f: F) -> impl Future<Output = ()>
    where
        I: FnOnce(&T, &Request<B>) -> bool,
        F: AsyncFnOnce(T, &mut Request<B>) -> U,
        U: MakeRoute<B>,
    {
        async move {
            self.middleware_mut_map(
                |self_, req| match if_(&self_, req) {
                    true => Then::Then(Some(self_)),
                    false => Then::Else(self_),
                },
                async |self_, req| f(self_.take().unwrap(), req).await,
                async |_self_, res| res,
            )
            .await
        }
    }

    /// Attach middleware whose match test may transform the node into a
    /// different type `T2` via [`Then`], without a post-processing step.
    fn middleware_map<I, T2, F, U>(&mut self, if_: I, f: F) -> impl Future<Output = ()>
    where
        I: FnOnce(T, &mut Request<B>) -> Then<T2, T>,
        F: AsyncFnOnce(T2, &mut Request<B>) -> U,
        U: MakeRoute<B>,
    {
        async move {
            self.middleware_mut_map(
                |self_, req| if_(self_, req).map(Some),
                async |self_, req| {
                    f(self_.take().unwrap(), req).await
                },
                async |_, res| res,
            ).await
        }
    }

    /// Attach always-running middleware that borrows the node mutably for the
    /// duration of the request and runs `post` on the [`RouterResponse`] on the
    /// way back out — e.g. to set a header or log the outcome.
    fn middleware_mut<'a, F, U, P>(&mut self, f: F, post: P) -> impl Future<Output = ()>
    where
        T: 'a,
        F: AsyncFnOnce(&'a mut T, &mut Request<B>) -> U,
        U: MakeRoute<B> + 'a,
        P: AsyncFnOnce(T, RouterResponse) -> RouterResponse,
    {
        self.middleware_mut_if(|_, _| true, f, post)
    }

    /// Like [`middleware_mut`](Self::middleware_mut), but runs only when `if_`
    /// returns `true` for the node and [`Request`].
    fn middleware_mut_if<'a, I, F, U, P>(
        &mut self,
        if_: I,
        f: F,
        post: P,
    ) -> impl Future<Output = ()>
    where
        T: 'a,
        I: FnOnce(&T, &Request<B>) -> bool,
        F: AsyncFnOnce(&'a mut T, &mut Request<B>) -> U,
        U: MakeRoute<B> + 'a,
        P: AsyncFnOnce(T, RouterResponse) -> RouterResponse,
    {
        self.middleware_mut_map(
            |self_, req| match if_(&self_, req) {
                true => Then::Then(self_),
                false => Then::Else(self_),
            },
            f,
            post,
        )
    }

    /// Declare a route taken when `if_` returns `true` for the node, the
    /// [`Request`], and the next path segment. `f` receives the consumed
    /// segment and returns the child [`MakeRoute`].
    fn route_if<I, F, U>(&mut self, if_: I, f: F) -> impl Future<Output = ()>
    where
        I: FnOnce(&T, &Request<B>, &StringId) -> bool,
        F: AsyncFnOnce(T, &mut Request<B>, StringId) -> U,
        U: MakeRoute<B>,
    {
        self.route_map(
            |self_, req, path| match if_(&self_, req, path) {
                true => Then::Then(self_),
                false => Then::Else(self_),
            },
            f,
        )
    }

    /// [`route_if`](Self::route_if) for a node that routes into its own type;
    /// see [`route_map_recursive`](Router::route_map_recursive). The condition
    /// sees only the node and the segment, not the [`Request`].
    fn route_if_recursive<I, F, U>(&mut self, if_: I, f: F) -> impl Future<Output = ()>
    where
        I: FnOnce(&T, &StringId) -> bool,
        F: AsyncFnOnce(T, &mut Request<B>, StringId) -> U,
        U: MakeRoute<B>,
    {
        self.route_map_recursive(
            |self_, path| match if_(&self_, path) {
                true => Then::Then(self_),
                false => Then::Else(self_),
            },
            f,
        )
    }

    /// Declare a leaf taken when `if_` returns `true` for the node and the
    /// request [`Method`]. Reached only when the path is exhausted.
    fn leaf_if<I, F, U>(&mut self, if_: I, f: F) -> impl Future<Output = ()>
    where
        I: FnOnce(&T, &Method) -> bool,
        F: AsyncFnOnce(T, Request<B>) -> U,
        U: Response,
    {
        self.leaf_map(
            |self_, method| match if_(&self_, method) {
                true => Then::Then(self_),
                false => Then::Else(self_),
            },
            f,
        )
    }

    /// Route into a child when the next path segment equals `path`. The common
    /// way to mount a fixed sub-resource (e.g. `"users"`).
    fn path<F, U, P>(&mut self, path: P, f: F) -> impl Future<Output = ()>
    where
        F: AsyncFnOnce(T, &mut Request<B>) -> U,
        U: MakeRoute<B>,
        P: Into<StringId>,
    {
        self.route_if(
            |_, _, req_path| req_path == &path.into(),
            async |a1, a2, _a3| f(a1, a2).await,
        )
    }

    /// [`path`](Self::path) for a node that routes into its own type; see
    /// [`route_map_recursive`](Router::route_map_recursive).
    fn path_recursive<F, U, P>(&mut self, path: P, f: F) -> impl Future<Output = ()>
    where
        F: AsyncFnOnce(T, &mut Request<B>) -> U,
        U: MakeRoute<B>,
        P: Into<StringId>,
    {
        self.route_if_recursive(
            |_, req_path| req_path == &path.into(),
            async |a1, a2, _a3| f(a1, a2).await,
        )
    }

    /// Route into a child for *any* next path segment, passing the consumed
    /// segment to `f`. Use this to capture a path parameter (e.g. an id).
    fn route<F, U>(&mut self, f: F) -> impl Future<Output = ()>
    where
        F: AsyncFnOnce(T, &mut Request<B>, StringId) -> U,
        U: MakeRoute<B>,
    {
        self.route_if(|_, _, _| true, f)
    }

    /// [`route`](Self::route) for a node that routes into its own type; see
    /// [`route_map_recursive`](Router::route_map_recursive).
    fn route_recursive<F, U>(&mut self, f: F) -> impl Future<Output = ()>
    where
        F: AsyncFnOnce(T, &mut Request<B>, StringId) -> U,
        U: MakeRoute<B>,
    {
        self.route_if_recursive(|_, _| true, f)
    }

    /// Declare a terminal leaf matched on `method`, reached when the path is
    /// exhausted.
    fn leaf<F, U>(&mut self, method: Method, f: F) -> impl Future<Output = ()>
    where
        F: AsyncFnOnce(T, Request<B>) -> U,
        U: Response,
    {
        self.leaf_if(move |_, req_method| req_method == method, f)
    }

    /// Declare a terminal leaf matched for any method.
    fn any_leaf<F, U>(&mut self, f: F) -> impl Future<Output = ()>
    where
        F: AsyncFnOnce(T, Request<B>) -> U,
        U: Response,
    {
        self.leaf_if(|_, _| true, f)
    }

    /// [`leaf`](Self::leaf) fixed to [`Method::GET`].
    fn get<F, U>(&mut self, f: F) -> impl Future<Output = ()>
    where
        F: AsyncFnOnce(T, Request<B>) -> U,
        U: Response,
    {
        self.leaf(Method::GET, f)
    }

    /// [`leaf`](Self::leaf) fixed to [`Method::PUT`].
    fn put<F, U>(&mut self, f: F) -> impl Future<Output = ()>
    where
        F: AsyncFnOnce(T, Request<B>) -> U,
        U: Response,
    {
        self.leaf(Method::PUT, f)
    }

    /// [`leaf`](Self::leaf) fixed to [`Method::POST`].
    fn post<F, U>(&mut self, f: F) -> impl Future<Output = ()>
    where
        F: AsyncFnOnce(T, Request<B>) -> U,
        U: Response,
    {
        self.leaf(Method::POST, f)
    }

    /// [`leaf`](Self::leaf) fixed to [`Method::DELETE`].
    fn delete<F, U>(&mut self, f: F) -> impl Future<Output = ()>
    where
        F: AsyncFnOnce(T, Request<B>) -> U,
        U: Response,
    {
        self.leaf(Method::DELETE, f)
    }

    /// Terminal handler for a request with *exactly one* segment remaining and
    /// the given `method`.
    ///
    /// Unlike [`leaf`](Self::leaf) (which fires only once the path is fully
    /// consumed), this matches a single trailing segment and hands it to `f`,
    /// treating the result as terminal. Use it for endpoints addressed by a
    /// trailing id, e.g. `GET /users/{id}`.
    ///
    /// The match requires the segment to be the last one. If further segments
    /// remain the route declines, so sibling declarations at this node still
    /// get a chance to match rather than the request short-circuiting to `404`.
    fn leaf_route<F, U>(&mut self, method: Method, f: F) -> impl Future<Output = ()>
    where
        F: AsyncFnOnce(T, &mut Request<B>, StringId) -> U,
        U: Response,
    {
        self.route_if(
            move |_, req, _| req.method == method && req.path.is_empty(),
            async |self_, req, path| LeafRoute(f(self_, req, path).await),
        )
    }

    /// [`leaf_route`](Self::leaf_route) fixed to [`Method::GET`].
    fn get_route<F, U>(&mut self, f: F) -> impl Future<Output = ()>
    where
        F: AsyncFnOnce(T, &mut Request<B>, StringId) -> U,
        U: Response,
    {
        self.leaf_route(Method::GET, f)
    }

    /// [`leaf_route`](Self::leaf_route) fixed to [`Method::PUT`].
    fn put_route<F, U>(&mut self, f: F) -> impl Future<Output = ()>
    where
        F: AsyncFnOnce(T, &mut Request<B>, StringId) -> U,
        U: Response,
    {
        self.leaf_route(Method::PUT, f)
    }

    /// [`leaf_route`](Self::leaf_route) fixed to [`Method::POST`].
    fn post_route<F, U>(&mut self, f: F) -> impl Future<Output = ()>
    where
        F: AsyncFnOnce(T, &mut Request<B>, StringId) -> U,
        U: Response,
    {
        self.leaf_route(Method::POST, f)
    }

    /// [`leaf_route`](Self::leaf_route) fixed to [`Method::DELETE`].
    fn delete_route<F, U>(&mut self, f: F) -> impl Future<Output = ()>
    where
        F: AsyncFnOnce(T, &mut Request<B>, StringId) -> U,
        U: Response,
    {
        self.leaf_route(Method::DELETE, f)
    }

    /// Terminal handler for a request whose final segment equals `path` and
    /// whose method is `method`.
    ///
    /// Combines a fixed-segment match with a terminal leaf in one call, e.g.
    /// `POST /users/login`.
    fn leaf_path<P, F, U>(&mut self, path: P, method: Method, f: F) -> impl Future<Output = ()>
    where
        P: Into<StringId>,
        F: AsyncFnOnce(T, &mut Request<B>) -> U,
        U: Response,
    {
        self.route_if(
            move |_, req, req_path| {
                req.method == method && req_path == &path.into() && req.path.is_empty()
            },
            async |self_, req, _| LeafRoute(f(self_, req).await),
        )
    }

    /// [`leaf_path`](Self::leaf_path) fixed to [`Method::GET`].
    fn get_path<P, F, U>(&mut self, path: P, f: F) -> impl Future<Output = ()>
    where
        P: Into<StringId>,
        F: AsyncFnOnce(T, &mut Request<B>) -> U,
        U: Response,
    {
        self.leaf_path(path, Method::GET, f)
    }

    /// [`leaf_path`](Self::leaf_path) fixed to [`Method::PUT`].
    fn put_path<P, F, U>(&mut self, path: P, f: F) -> impl Future<Output = ()>
    where
        P: Into<StringId>,
        F: AsyncFnOnce(T, &mut Request<B>) -> U,
        U: Response,
    {
        self.leaf_path(path, Method::PUT, f)
    }

    /// [`leaf_path`](Self::leaf_path) fixed to [`Method::POST`].
    fn post_path<P, F, U>(&mut self, path: P, f: F) -> impl Future<Output = ()>
    where
        P: Into<StringId>,
        F: AsyncFnOnce(T, &mut Request<B>) -> U,
        U: Response,
    {
        self.leaf_path(path, Method::POST, f)
    }

    /// [`leaf_path`](Self::leaf_path) fixed to [`Method::DELETE`].
    fn delete_path<P, F, U>(&mut self, path: P, f: F) -> impl Future<Output = ()>
    where
        P: Into<StringId>,
        F: AsyncFnOnce(T, &mut Request<B>) -> U,
        U: Response,
    {
        self.leaf_path(path, Method::DELETE, f)
    }
}

/// A node in the routing tree.
///
/// This is the trait you implement to describe how a request is routed. Each
/// implementor represents a single **node**: a point in the URL path at which
/// the router decides what to do next. From a node you may attach middleware,
/// descend into child nodes by consuming a path segment, or terminate the
/// request by producing a [`Response`].
///
/// # The routing model
///
/// A request carries its path as a queue of segments ([`Request::path`]). The
/// router walks the tree one segment at a time. At every node it calls
/// [`register`](MakeRoute::register), giving you a [`Router`] on which you
/// declare what that node offers. Declarations are evaluated in **three
/// phases**, and within a phase **in declaration order, first match wins**:
///
/// 1. **Middleware** — runs before any segment is consumed. The first
///    middleware whose condition matches takes over the request: it may
///    inspect or rewrite the [`Request`], swap the node for a different
///    [`MakeRoute`], and post-process the eventual [`RouterResponse`]. See
///    [`middleware`](RouterDerived::middleware) and friends.
/// 2. **Routes** — consulted only when the path still has at least one
///    segment. The matching route consumes that segment and yields a child
///    [`MakeRoute`] that the router descends into. See
///    [`path`](RouterDerived::path) and [`route`](RouterDerived::route). Use
///    the `*_recursive` variants when a node routes into *its own type* (e.g. a
///    directory whose children are also directories): the recursion is erased
///    behind a boxed future, which would otherwise be an infinitely-sized type
///    that fails to compile.
/// 3. **Leaves** — consulted only when the path is exhausted. A leaf is the
///    terminal of a route: it is selected by HTTP [`Method`] and produces the
///    final [`Response`]. See [`get`](RouterDerived::get),
///    [`post`](RouterDerived::post), [`any_leaf`](RouterDerived::any_leaf),
///    etc.
///
/// If no route matches a remaining segment the router replies `404`. If the
/// path is exhausted but no leaf matches the request method, the router
/// replies `405` when some *other* method would have matched, and `404`
/// otherwise.
///
/// # Composition
///
/// `MakeRoute` is implemented for several standard types so that a node can be
/// produced fallibly or conditionally and still plug into the tree:
///
/// - [`Result<T, E>`] — `Ok(node)` routes into `node`; `Err(e)` short-circuits
///   the request with `e` (where `E: Response`). This lets a node bail out with
///   an error response from anywhere.
/// - [`Option<T>`] — `Some(node)` routes into `node`; `None` replies `404`.
/// - [`Either<T1, T2>`] — registers both alternatives; the active variant
///   drives routing. Useful when two branches produce different node types.
/// - `()` — an empty node that matches nothing.
/// - [`ShortCircuit<T>`] — replies with `T` for any remaining path and method.
pub trait MakeRoute<B = Incoming>: Sized {
    /// Declare this node's middleware, routes, and leaves on `router`.
    ///
    /// The router calls this once per phase as it resolves a request (see the
    /// [trait documentation](MakeRoute) for the phase order), so `register`
    /// must be deterministic: it should make the same set of declarations on
    /// every call rather than depend on external state. The `Self` value for
    /// the node is threaded to you through the [`Router`] callbacks, not
    /// through `&self` — `register` is an associated function.
    fn register<R: Router<Self, B>>(router: &mut R) -> impl Future<Output = ()>;
}
/// `None` replies `404`; `Some(node)` routes into `node`.
impl<B, T> MakeRoute<B> for Option<T>
where
    T: MakeRoute<B>,
{
    fn register<R: Router<Self, B>>(router: &mut R) -> impl Future<Output = ()> {
        struct OptRouter<'a, R>(&'a mut R);
        impl<B, R, T> Router<T, B> for OptRouter<'_, R>
        where
            R: Router<Option<T>, B>,
        {
            async fn middleware_mut_map<'a, I, T2, F, U, P>(&mut self, if_: I, f: F, post: P)
            where
                T2: 'a,
                I: FnOnce(T, &mut Request<B>) -> Then<T2, T>,
                F: AsyncFnOnce(&'a mut T2, &mut Request<B>) -> U,
                U: MakeRoute<B> + 'a,
                P: AsyncFnOnce(T2, RouterResponse) -> RouterResponse,
            {
                self.0
                    .middleware_mut_map(
                        |self_, req| match self_ {
                            Some(self_) => match if_(self_, req) {
                                Then::Then(self2) => Then::Then(self2),
                                Then::Else(self_) => Then::Else(Some(self_)),
                            },
                            None => Then::Else(None),
                        },
                        f,
                        post,
                    )
                    .await;
            }

            async fn route_map<I, T2, F, U>(&mut self, if_: I, f: F)
            where
                I: FnOnce(T, &Request<B>, &StringId) -> Then<T2, T>,
                F: AsyncFnOnce(T2, &mut Request<B>, StringId) -> U,
                U: MakeRoute<B>,
            {
                self.0
                    .route_map(
                        |self_, req, path| match self_ {
                            Some(self_) => match if_(self_, req, path) {
                                Then::Then(self2) => Then::Then(self2),
                                Then::Else(self_) => Then::Else(Some(self_)),
                            },
                            None => Then::Else(None),
                        },
                        f,
                    )
                    .await;
            }

            async fn route_map_recursive<I, T2, F, U>(&mut self, if_: I, f: F)
            where
                I: FnOnce(T, &StringId) -> Then<T2, T>,
                F: AsyncFnOnce(T2, &mut Request<B>, StringId) -> U,
                U: MakeRoute<B>,
            {
                self.0
                    .route_map_recursive(
                        |self_, path| match self_ {
                            Some(self_) => match if_(self_, path) {
                                Then::Then(self2) => Then::Then(self2),
                                Then::Else(self_) => Then::Else(Some(self_)),
                            },
                            None => Then::Else(None),
                        },
                        f,
                    )
                    .await
            }

            async fn leaf_map<I, T2, F, U>(&mut self, if_: I, f: F)
            where
                I: FnOnce(T, &Method) -> Then<T2, T>,
                F: AsyncFnOnce(T2, Request<B>) -> U,
                U: Response,
            {
                self.0
                    .leaf_map(
                        |self_, method| match self_ {
                            Some(self_) => match if_(self_, method) {
                                Then::Then(self2) => Then::Then(self2),
                                Then::Else(self_) => Then::Else(Some(self_)),
                            },
                            None => Then::Else(None),
                        },
                        f,
                    )
                    .await;
            }
        }

        async move {
            router
                .middleware_if(
                    |self_, _| self_.is_none(),
                    async |self_, _| match self_ {
                        Some(_) => unreachable!(),
                        None => ShortCircuit(Empty404),
                    },
                )
                .await;
            T::register(&mut OptRouter(router)).await;
        }
    }
}

/// The outcome of a match test, carrying ownership of the node either way.
///
/// Routing tests take the node by value because a match needs to transform it
/// (often into a different type) while a miss needs to hand it back unchanged
/// so the next declaration can try. `Then(u)` means "matched — here is the
/// transformed state `u`"; `Else(t)` means "did not match — here is the
/// original node `t`".
pub enum Then<U, T> {
    /// The test matched, yielding the transformed state.
    Then(U),
    /// The test did not match, returning the node unchanged.
    Else(T),
}
impl<U, T> Then<U, T> {
    pub fn map<U2, F>(self, f: F) -> Then<U2, T>
    where
        F: FnOnce(U) -> U2,
    {
        match self {
            Then::Then(then) => Then::Then(f(then)),
            Then::Else(else_) => Then::Else(else_),
        }
    }

    pub fn and_then<U2, F>(self, f: F) -> Then<U2, T>
    where
        F: FnOnce(U) -> Then<U2, T>,
    {
        match self {
            Then::Then(then) => f(then),
            Then::Else(else_) => Then::Else(else_),
        }
    }

    pub fn map_else<T2, F>(self, f: F) -> Then<U, T2>
    where
        F: FnOnce(T) -> T2,
    {
        match self {
            Then::Then(then) => Then::Then(then),
            Then::Else(else_) => Then::Else(f(else_)),
        }
    }
}

/// `Err(e)` short-circuits with `e` as the response; `Ok(node)` routes into
/// `node`.
impl<B, T, E> MakeRoute<B> for Result<T, E>
where
    T: MakeRoute<B>,
    E: Response,
{
    fn register<R: Router<Self, B>>(router: &mut R) -> impl Future<Output = ()> {
        struct ResultRouter<'a, R, E>(&'a mut R, std::marker::PhantomData<E>);
        impl<B, T, E, R> Router<T, B> for ResultRouter<'_, R, E>
        where
            T: MakeRoute<B>,
            E: Response,
            R: Router<Result<T, E>, B>,
        {
            async fn middleware_mut_map<'a, I, T2, F, U, P>(&mut self, if_: I, f: F, post: P)
            where
                T2: 'a,
                I: FnOnce(T, &mut Request<B>) -> Then<T2, T>,
                F: AsyncFnOnce(&'a mut T2, &mut Request<B>) -> U,
                U: MakeRoute<B> + 'a,
                P: AsyncFnOnce(T2, RouterResponse) -> RouterResponse,
            {
                self.0
                    .middleware_mut_map(
                        |self_, req| match self_ {
                            Ok(self_) => match if_(self_, req) {
                                Then::Then(self2) => Then::Then(self2),
                                Then::Else(self_) => Then::Else(Ok(self_)),
                            },
                            self_ @ Err(_) => Then::Else(self_),
                        },
                        f,
                        post,
                    )
                    .await;
            }

            async fn route_map<I, T2, F, U>(&mut self, if_: I, f: F)
            where
                I: FnOnce(T, &Request<B>, &StringId) -> Then<T2, T>,
                F: AsyncFnOnce(T2, &mut Request<B>, StringId) -> U,
                U: MakeRoute<B>,
            {
                self.0
                    .route_map(
                        |self_, req, path| match self_ {
                            Ok(self_) => match if_(self_, req, path) {
                                Then::Then(self2) => Then::Then(self2),
                                Then::Else(self_) => Then::Else(Ok(self_)),
                            },
                            self_ @ Err(_) => Then::Else(self_),
                        },
                        f,
                    )
                    .await
            }

            async fn route_map_recursive<I, T2, F, U>(&mut self, if_: I, f: F)
            where
                I: FnOnce(T, &StringId) -> Then<T2, T>,
                F: AsyncFnOnce(T2, &mut Request<B>, StringId) -> U,
                U: MakeRoute<B>,
            {
                self.0
                    .route_map_recursive(
                        |self_, path| match self_ {
                            Ok(self_) => match if_(self_, path) {
                                Then::Then(self2) => Then::Then(self2),
                                Then::Else(self_) => Then::Else(Ok(self_)),
                            },
                            self_ @ Err(_) => Then::Else(self_),
                        },
                        f,
                    )
                    .await
            }

            async fn leaf_map<I, T2, F, U>(&mut self, if_: I, f: F)
            where
                I: FnOnce(T, &Method) -> Then<T2, T>,
                F: AsyncFnOnce(T2, Request<B>) -> U,
                U: Response,
            {
                self.0
                    .leaf_map(
                        |self_, method| match self_ {
                            Ok(self_) => match if_(self_, method) {
                                Then::Then(self2) => Then::Then(self2),
                                Then::Else(self_) => Then::Else(Ok(self_)),
                            },
                            self_ @ Err(_) => Then::Else(self_),
                        },
                        f,
                    )
                    .await
            }
        }

        async move {
            router
                .middleware_if(
                    |self_, _| self_.is_err(),
                    async |self_, _| match self_ {
                        Ok(_) => unreachable!(),
                        Err(e) => ShortCircuit(e),
                    },
                )
                .await;
            T::register(&mut ResultRouter(router, Default::default())).await;
        }
    }
}
/// Registers both alternatives; the variant the node currently holds drives
/// routing. Lets a single declaration site produce two different node types.
impl<B, T1, T2> MakeRoute<B> for Either<T1, T2>
where
    T1: MakeRoute<B>,
    T2: MakeRoute<B>,
{
    fn register<R: Router<Self, B>>(router: &mut R) -> impl Future<Output = ()> {
        struct EitherRouter<'a, R, O, E>(&'a mut R, std::marker::PhantomData<(E, O)>);

        trait OpenEither<E, T> {
            fn open(self_: E) -> Then<T, E>;
            fn close(t: T) -> E;
        }
        struct OpenLeft;
        impl<T1, T2> OpenEither<Either<T1, T2>, T1> for OpenLeft {
            fn open(self_: Either<T1, T2>) -> Then<T1, Either<T1, T2>> {
                match self_ {
                    Either::Left(t1) => Then::Then(t1),
                    e @ Either::Right(_) => Then::Else(e),
                }
            }

            fn close(t: T1) -> Either<T1, T2> {
                Either::Left(t)
            }
        }
        struct OpenRight;
        impl<T1, T2> OpenEither<Either<T1, T2>, T2> for OpenRight {
            fn open(self_: Either<T1, T2>) -> Then<T2, Either<T1, T2>> {
                match self_ {
                    e @ Either::Left(_) => Then::Else(e),
                    Either::Right(t2) => Then::Then(t2),
                }
            }

            fn close(t: T2) -> Either<T1, T2> {
                Either::Right(t)
            }
        }

        impl<B, T, E, R, O> Router<T, B> for EitherRouter<'_, R, O, E>
        where
            R: Router<E, B>,
            O: OpenEither<E, T>,
        {
            async fn middleware_mut_map<'a, I, T2, F, U, P>(&mut self, if_: I, f: F, post: P)
            where
                T2: 'a,
                I: FnOnce(T, &mut Request<B>) -> Then<T2, T>,
                F: AsyncFnOnce(&'a mut T2, &mut Request<B>) -> U,
                U: MakeRoute<B> + 'a,
                P: AsyncFnOnce(T2, RouterResponse) -> RouterResponse,
            {
                self.0
                    .middleware_mut_map(
                        |self_, req| {
                            O::open(self_).and_then(|self_| if_(self_, req).map_else(O::close))
                        },
                        f,
                        post,
                    )
                    .await
            }

            async fn route_map<I, T2, F, U>(&mut self, if_: I, f: F)
            where
                I: FnOnce(T, &Request<B>, &StringId) -> Then<T2, T>,
                F: AsyncFnOnce(T2, &mut Request<B>, StringId) -> U,
                U: MakeRoute<B>,
            {
                self.0
                    .route_map(
                        |self_, req, path| {
                            O::open(self_)
                                .and_then(|self_| if_(self_, req, path).map_else(O::close))
                        },
                        f,
                    )
                    .await
            }

            async fn route_map_recursive<I, T2, F, U>(&mut self, if_: I, f: F)
            where
                I: FnOnce(T, &StringId) -> Then<T2, T>,
                F: AsyncFnOnce(T2, &mut Request<B>, StringId) -> U,
                U: MakeRoute<B>,
            {
                self.0
                    .route_map_recursive(
                        |self_, path| {
                            O::open(self_).and_then(|self_| if_(self_, path).map_else(O::close))
                        },
                        f,
                    )
                    .await
            }

            async fn leaf_map<I, T2, F, U>(&mut self, if_: I, f: F)
            where
                I: FnOnce(T, &Method) -> Then<T2, T>,
                F: AsyncFnOnce(T2, Request<B>) -> U,
                U: Response,
            {
                self.0
                    .leaf_map(
                        |self_, method| {
                            O::open(self_).and_then(|self_| if_(self_, method).map_else(O::close))
                        },
                        f,
                    )
                    .await
            }
        }

        async move {
            T1::register(&mut EitherRouter::<_, OpenLeft, Either<T1, T2>>(
                router,
                Default::default(),
            ))
            .await;
            T2::register(&mut EitherRouter::<_, OpenRight, Either<T1, T2>>(
                router,
                Default::default(),
            ))
            .await;
        }
    }
}
/// An empty node: declares nothing, so it matches no route or leaf.
impl<B> MakeRoute<B> for () {
    async fn register<R: Router<Self, B>>(_router: &mut R) {}
}

/// A node that replies with `T` for any remaining path and any method.
///
/// Used to terminate routing unconditionally — e.g. to serve a fixed error or
/// to act as a catch-all. It is the mechanism behind the
/// [`Result`]/[`Option`] short-circuits.
pub struct ShortCircuit<T>(pub T);
impl<B, T> MakeRoute<B> for ShortCircuit<T>
where
    T: Response,
{
    async fn register<R: Router<Self, B>>(router: &mut R) {
        router.route_recursive(async |self_, _, _| self_).await;
        router.any_leaf(async |self_, _| self_.0).await;
    }
}

struct LeafRoute<T>(T);
impl<B, T> MakeRoute<B> for LeafRoute<T>
where
    T: Response,
{
    async fn register<R: Router<Self, B>>(router: &mut R) {
        router.any_leaf(async |self_, _| self_.0).await
    }
}

pub struct RouterResponse {
    boxed: Box<dyn BoxedResponse>,
}
impl RouterResponse {
    pub fn new<R: Response + 'static>(response: R) -> Self {
        Self {
            boxed: Box::new(response),
        }
    }

    pub fn e404() -> Self {
        Self::new(EmptyResponse(StatusCode::NOT_FOUND))
    }

    pub fn e405() -> Self {
        Self::new(EmptyResponse(StatusCode::METHOD_NOT_ALLOWED))
    }

    pub fn is_success(&self) -> bool {
        self.boxed.status_code().is_success()
    }

    #[track_caller]
    pub fn downcast<T>(&self) -> &T
    where
        T: 'static,
    {
        assert_eq!(
            self.boxed.type_id(),
            TypeId::of::<T>(),
            "Expected {:?} got {:?}",
            std::any::type_name::<T>(),
            self.boxed.type_name(),
        );

        unsafe { &*(self.boxed.as_ref() as *const dyn BoxedResponse as *const T) }
    }
}
impl Response for RouterResponse {
    type Body = Pin<Box<dyn ResponseBody>>;

    fn status_code(&self) -> StatusCode {
        self.boxed.status_code()
    }

    fn into_body(self) -> Self::Body {
        self.boxed.into_body()
    }

    fn extra_headers(&self) -> HashMap<StringId, String> {
        self.boxed.extra_headers()
    }
}

struct EmptyResponse(StatusCode);
impl Response for EmptyResponse {
    type Body = NoBody;

    fn status_code(&self) -> StatusCode {
        self.0
    }

    fn into_body(self) -> Self::Body {
        NoBody
    }
}

trait BoxedResponse: 'static {
    fn into_body(self: Box<Self>) -> Pin<Box<dyn ResponseBody>>;
    fn status_code(&self) -> StatusCode;
    fn extra_headers(&self) -> HashMap<StringId, String>;
    fn type_id(&self) -> TypeId;
    fn type_name(&self) -> &'static str;
}
impl<R> BoxedResponse for R
where
    R: Response,
{
    fn into_body(self: Box<Self>) -> Pin<Box<dyn ResponseBody>> {
        Box::pin(Response::into_body(*self))
    }

    fn status_code(&self) -> StatusCode {
        Response::status_code(self)
    }

    fn extra_headers(&self) -> HashMap<StringId, String> {
        Response::extra_headers(self)
    }

    fn type_id(&self) -> TypeId {
        TypeId::of::<Self>()
    }

    fn type_name(&self) -> &'static str {
        std::any::type_name::<Self>()
    }
}

enum RouterState<T, U = RouterResponse> {
    Init(T),
    Response(U),
    Empty,
}
impl<T, U> From<T> for RouterState<T, U> {
    fn from(value: T) -> Self {
        Self::Init(value)
    }
}
impl<T, U> RouterState<T, U> {
    async fn execute_map<IF, T2, F>(&mut self, if_: IF, f: F)
    where
        IF: FnOnce(T) -> Then<T2, T>,
        F: AsyncFnOnce(T2) -> U,
    {
        let new = match std::mem::replace(self, Self::Empty) {
            RouterState::Init(init) => match if_(init) {
                Then::Then(init2) => Self::Response(f(init2).await),
                Then::Else(init) => Self::Init(init),
            },
            other => other,
        };

        *self = new;
    }

    fn take(self) -> Either<T, U> {
        match self {
            RouterState::Init(init) => Either::Left(init),
            RouterState::Response(response) => Either::Right(response),
            RouterState::Empty => unreachable!("Empty never is not accessible from outside"),
        }
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::{handler::Json, uri_subject::path_str_to_path};

    struct Given;
    impl Given {
        fn start<F>(test: F)
        where
            F: AsyncFnOnce(Given),
        {
            tracing_subscriber::fmt::init();

            let rt = tokio::runtime::LocalRuntime::new().unwrap();
            rt.block_on(test(Given))
        }

        #[track_caller]
        fn get<T, R>(&self, root: R, path: &str) -> T
        where
            R: MakeRoute<()>,
            T: Response + Clone,
        {
            let req = Request {
                method: Method::GET,
                path: path_str_to_path(path),
                headers: Default::default(),
                query: Default::default(),
                body: Default::default(),
            };
            RouterHandler::do_handle(root, req)
                .now_or_never()
                .unwrap()
                .downcast::<T>()
                .clone()
        }
    }

    #[test]
    fn downcast_response() {
        let response = RouterResponse::new(Json::j200("Ola"));
        assert_eq!(response.downcast::<Json<&'static str>>().0, "Ola");
    }

    #[test]
    fn error_response() {
        Given::start(async |given| {
            struct Root;
            impl<B> MakeRoute<B> for Root {
                async fn register<R: Router<Self, B>>(router: &mut R) {
                    router
                        .route(async |_, _, path| {
                            if path == "inexistent" {
                                Err(Json::j200("circuit breaker"))
                            } else {
                                Ok(Middleware)
                            }
                        })
                        .await;
                }
            }
            struct Middleware;
            impl<B> MakeRoute<B> for Middleware {
                async fn register<R: Router<Self, B>>(router: &mut R) {
                    router.middleware(async |_, _| JustRoute).await
                }
            }
            struct JustRoute;
            impl<B> MakeRoute<B> for JustRoute {
                async fn register<R: Router<Self, B>>(router: &mut R) {
                    router.route(async |_, _, _| Root2).await
                }
            }
            struct Root2;
            impl<B> MakeRoute<B> for Root2 {
                async fn register<R: Router<Self, B>>(router: &mut R) {
                    router.route_recursive(async |self_, _, _| self_).await;

                    router.any_leaf(async |_, _| Json::j200("done")).await;
                }
            }

            assert_eq!(
                given.get::<Json<&str>, _>(Root, "hello/thing/more").0,
                "done"
            );
            assert_eq!(given.get::<Json<&str>, _>(Root, "hello/thing").0, "done");
            assert_eq!(
                given.get::<Json<&str>, _>(Root, "inexistent/more/more").0,
                "circuit breaker"
            );
        });
    }

    #[test]
    fn either_router() {
        Given::start(async |given| {
            struct Root;
            impl<B> MakeRoute<B> for Root {
                async fn register<R: Router<Self, B>>(router: &mut R) {
                    router
                        .route_if(
                            |_, _, path| path == "left" || path == "right",
                            async |_, _, path| {
                                if path == "left" {
                                    Either::Left(Mid(LeftRoute))
                                } else {
                                    Either::Right(Mid(RightRoute))
                                }
                            },
                        )
                        .await
                }
            }

            struct Mid<T>(T);
            impl<B, T> MakeRoute<B> for Mid<T>
            where
                T: MakeRoute<B>,
            {
                async fn register<R: Router<Self, B>>(router: &mut R) {
                    router.middleware(async |self_, _| self_.0).await;
                }
            }

            struct LeftRoute;
            impl<B> MakeRoute<B> for LeftRoute {
                async fn register<R: Router<Self, B>>(router: &mut R) {
                    router.route(async |_, _, _| LeafRoute("left")).await
                }
            }

            struct RightRoute;
            impl<B> MakeRoute<B> for RightRoute {
                async fn register<R: Router<Self, B>>(router: &mut R) {
                    router
                        .path_recursive("recursive", async |_, _| LeafRoute("recursive"))
                        .await;

                    router.path("bola", async |_, _| LeafRoute("right")).await;
                    router
                        .path("either_leaf", async |_, req| {
                            if req.headers.is_empty() {
                                Either::Right(EitherLeaf)
                            } else {
                                Either::Left(LeafRoute("never happens"))
                            }
                        })
                        .await;
                }
            }

            struct LeafRoute(&'static str);
            impl<B> MakeRoute<B> for LeafRoute {
                async fn register<R: Router<Self, B>>(router: &mut R) {
                    router.any_leaf(async |self_, _| Json::j200(self_.0)).await;
                }
            }

            struct EitherLeaf;
            impl<B> MakeRoute<B> for EitherLeaf {
                async fn register<R: Router<Self, B>>(router: &mut R) {
                    router.any_leaf(async |_, _| Json::j200("happened")).await
                }
            }

            assert_eq!(given.get::<Json<&str>, _>(Root, "left/bola").0, "left");
            assert_eq!(given.get::<Json<&str>, _>(Root, "right/bola").0, "right");
            assert_eq!(
                given.get::<Json<&str>, _>(Root, "right/recursive").0,
                "recursive"
            );
            assert_eq!(
                given.get::<Json<&str>, _>(Root, "right/either_leaf").0,
                "happened"
            );
        });
    }
}
