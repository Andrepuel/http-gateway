use crate::handler::{Handler, Request, Response, StringId};
use either::Either;
use futures::FutureExt;
use hyper::{Method, StatusCode};

pub struct RouterHandler<R> {
    root: R,
}
impl<R> RouterHandler<R>
where
    R: MakeRoute + Clone,
{
    pub fn new(root: R) -> Self {
        Self { root }
    }
}
impl<R> RouterHandler<R>
where
    R: MakeRoute,
{
    async fn do_handle(root: R, req: Request) -> RouterResponse {
        struct FindMiddleware<R>(RouterState<(R, Request)>);
        impl<R> Router<R> for FindMiddleware<R> {
            async fn middleware<F, U>(&mut self, f: F)
            where
                F: AsyncFnOnce(R, &mut Request) -> U,
                U: MakeRoute,
            {
                self.0
                    .execute(async |(root, mut req)| {
                        let route = f(root, &mut req).await;

                        RouterHandler::<U>::do_handle(route, req)
                            .boxed_local()
                            .await
                    })
                    .await;
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
                struct FindRoute<R>(RouterState<(StringId, R, Request)>);
                impl<R> Router<R> for FindRoute<R> {
                    async fn route_if<I, F, U>(&mut self, if_: I, f: F)
                    where
                        I: FnOnce(&StringId) -> bool,
                        F: AsyncFnOnce(R, &mut Request, StringId) -> U,
                        U: MakeRoute,
                    {
                        self.0
                            .execute_if(
                                |(req_path, _, _)| if_(req_path),
                                async |(path, root, mut req)| {
                                    let route = f(root, &mut req, path).await;

                                    RouterHandler::<U>::do_handle(route, req)
                                        .boxed_local()
                                        .await
                                },
                            )
                            .await;
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
                struct MakeRouteLeaf<R>(RouterState<(R, Request)>);
                impl<R> Router<R> for MakeRouteLeaf<R> {
                    async fn any_leaf<F, U>(&mut self, f: F)
                    where
                        F: AsyncFnOnce(R, Request) -> U,
                        U: Response,
                    {
                        self.0
                            .execute(async |(root, req)| RouterResponse::new(f(root, req).await))
                            .await;
                    }

                    async fn leaf<F, U>(&mut self, method: Method, f: F)
                    where
                        F: AsyncFnOnce(R, Request) -> U,
                        U: Response,
                    {
                        self.0
                            .execute_if(
                                |(_, req)| req.method == method,
                                async |(root, req)| RouterResponse::new(f(root, req).await),
                            )
                            .await;
                    }
                }
                let mut call = MakeRouteLeaf((root, req).into());
                R::register(&mut call).await;

                match call.0.take() {
                    Either::Right(r) => r,
                    Either::Left((_root, req)) => {
                        struct FindOtherMethods<R>(Vec<Method>, std::marker::PhantomData<R>);
                        impl<R> Router<R> for FindOtherMethods<R> {
                            async fn leaf<F, U>(&mut self, method: Method, _f: F)
                            where
                                F: AsyncFnOnce(R, Request) -> U,
                                U: Response,
                            {
                                self.0.push(method);
                            }
                        }
                        let mut other_method =
                            FindOtherMethods(Default::default(), Default::default());
                        R::register(&mut other_method).await;

                        match other_method.0.is_empty() {
                            true => {
                                tracing::debug!(method=?req.method, "Not matching route for leaf");
                                RouterResponse::e404()
                            }
                            false => {
                                tracing::debug!(allowed=?other_method.0, method=?req.method, "Method not allowed");
                                RouterResponse::e405()
                            }
                        }
                    }
                }
            }
        }
    }
}
impl<R> Handler for RouterHandler<R>
where
    R: MakeRoute + Clone,
{
    type Response = RouterResponse;

    fn handle(&self, req: Request) -> impl Future<Output = Self::Response> {
        tracing::debug!(path=?req.path);
        Self::do_handle(self.root.clone(), req)
    }
}

pub trait Router<T> {
    fn middleware<F, U>(&mut self, f: F) -> impl Future<Output = ()>
    where
        F: AsyncFnOnce(T, &mut Request) -> U,
        U: MakeRoute,
    {
        let _ = f;
        async move {}
    }

    fn path<F, U, P>(&mut self, path: P, f: F) -> impl Future<Output = ()>
    where
        F: AsyncFnOnce(T, &mut Request) -> U,
        U: MakeRoute,
        P: Into<StringId>,
    {
        self.route_if(
            |req_path| req_path == &path.into(),
            async |a1, a2, _a3| f(a1, a2).await,
        )
    }

    fn route<F, U>(&mut self, f: F) -> impl Future<Output = ()>
    where
        F: AsyncFnOnce(T, &mut Request, StringId) -> U,
        U: MakeRoute,
    {
        self.route_if(|_| true, f)
    }

    fn route_if<I, F, U>(&mut self, if_: I, f: F) -> impl Future<Output = ()>
    where
        I: FnOnce(&StringId) -> bool,
        F: AsyncFnOnce(T, &mut Request, StringId) -> U,
        U: MakeRoute,
    {
        let _ = (if_, f);
        async move {}
    }

    fn any_leaf<F, U>(&mut self, f: F) -> impl Future<Output = ()>
    where
        F: AsyncFnOnce(T, Request) -> U,
        U: Response,
    {
        let _ = f;
        async move {}
    }

    fn leaf<F, U>(&mut self, method: Method, f: F) -> impl Future<Output = ()>
    where
        F: AsyncFnOnce(T, Request) -> U,
        U: Response,
    {
        let _ = (method, f);
        async move {}
    }

    fn get<F, U>(&mut self, f: F) -> impl Future<Output = ()>
    where
        F: AsyncFnOnce(T, Request) -> U,
        U: Response,
    {
        self.leaf(Method::GET, f)
    }

    fn put<F, U>(&mut self, f: F) -> impl Future<Output = ()>
    where
        F: AsyncFnOnce(T, Request) -> U,
        U: Response,
    {
        self.leaf(Method::PUT, f)
    }

    fn post<F, U>(&mut self, f: F) -> impl Future<Output = ()>
    where
        F: AsyncFnOnce(T, Request) -> U,
        U: Response,
    {
        self.leaf(Method::POST, f)
    }

    fn delete<F, U>(&mut self, f: F) -> impl Future<Output = ()>
    where
        F: AsyncFnOnce(T, Request) -> U,
        U: Response,
    {
        self.leaf(Method::DELETE, f)
    }
}

pub trait MakeRoute: Sized + 'static {
    fn register<R: Router<Self>>(router: &mut R) -> impl Future<Output = ()>;
}
impl<T> MakeRoute for Option<T>
where
    T: MakeRoute,
{
    fn register<R: Router<Self>>(router: &mut R) -> impl Future<Output = ()> {
        struct OptRouter<'a, R>(&'a mut R);
        impl<R, T> Router<T> for OptRouter<'_, R>
        where
            R: Router<Option<T>>,
        {
            async fn middleware<F, U>(&mut self, f: F)
            where
                F: AsyncFnOnce(T, &mut Request) -> U,
                U: MakeRoute,
            {
                self.0
                    .middleware(async |self_, req| match self_ {
                        Some(self_) => Some(f(self_, req).await),
                        None => None,
                    })
                    .await;
            }

            async fn route_if<I, F, U>(&mut self, if_: I, f: F)
            where
                I: FnOnce(&StringId) -> bool,
                F: AsyncFnOnce(T, &mut Request, StringId) -> U,
                U: MakeRoute,
            {
                self.0
                    .route_if(if_, async |self_, req, path| match self_ {
                        Some(self_) => Some(f(self_, req, path).await),
                        None => None,
                    })
                    .await;
            }

            async fn any_leaf<F, U>(&mut self, f: F)
            where
                F: AsyncFnOnce(T, Request) -> U,
                U: Response,
            {
                self.0
                    .any_leaf(async |self_, req| match self_ {
                        Some(self_) => Some(f(self_, req).await),
                        None => None,
                    })
                    .await;
            }

            async fn leaf<F, U>(&mut self, method: Method, f: F)
            where
                F: AsyncFnOnce(T, Request) -> U,
                U: Response,
            {
                self.0
                    .leaf(method, async |self_, req| match self_ {
                        Some(self_) => Some(f(self_, req).await),
                        None => None,
                    })
                    .await;
            }
        }
        async move {
            T::register(&mut OptRouter(router)).await;
        }
    }
}

pub struct RouterResponse {
    status: StatusCode,
    body: Option<serde_json::Value>,
}
impl RouterResponse {
    pub fn new<R: Response>(response: R) -> Self {
        Self {
            status: response.status_code(),
            body: response
                .into_body()
                .map(|body| serde_json::to_value(body).unwrap()),
        }
    }

    pub fn e404() -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            body: None,
        }
    }

    pub fn e405() -> Self {
        Self {
            status: StatusCode::METHOD_NOT_ALLOWED,
            body: None,
        }
    }
}
impl Response for RouterResponse {
    type Body = serde_json::Value;

    fn into_body(self) -> Option<Self::Body> {
        self.body
    }

    fn status_code(&self) -> StatusCode {
        self.status
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
    async fn execute<F>(&mut self, f: F)
    where
        F: AsyncFnOnce(T) -> U,
    {
        match std::mem::replace(self, RouterState::Empty) {
            RouterState::Init(init) => {
                *self = RouterState::Response(f(init).await);
            }
            other => *self = other,
        }
    }

    async fn execute_if<IF, F>(&mut self, if_: IF, f: F)
    where
        IF: FnOnce(&mut T) -> bool,
        F: AsyncFnOnce(T) -> U,
    {
        let execute = match self {
            RouterState::Init(init) => if_(init),
            _ => false,
        };

        if execute {
            self.execute(f).await
        }
    }

    fn take(self) -> Either<T, U> {
        match self {
            RouterState::Init(init) => Either::Left(init),
            RouterState::Response(response) => Either::Right(response),
            RouterState::Empty => unreachable!("Empty never is not accessible from outside"),
        }
    }
}
