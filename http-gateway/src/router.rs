use crate::handler::{Handler, Request, Response, StringId};
use either::Either;
use futures::FutureExt;
use hyper::{Method, StatusCode};
use std::any::TypeId;

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

                        RouterHandler::<U>::do_handle(route, req).await
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

                                    RouterHandler::<U>::do_handle(route, req).await
                                },
                            )
                            .await;
                    }

                    async fn route_if_recursive<I, F, U>(&mut self, if_: I, f: F)
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
                    async fn leaf_if<I, F, U>(&mut self, if_: I, f: F)
                    where
                        I: FnOnce(&Method) -> bool,
                        F: AsyncFnOnce(R, Request) -> U,
                        U: Response,
                    {
                        self.0
                            .execute_if(
                                |(_, req)| (if_)(&req.method),
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
                        let mut other_methods = Vec::new();
                        for method in [
                            Method::HEAD,
                            Method::GET,
                            Method::PUT,
                            Method::POST,
                            Method::DELETE,
                        ] {
                            struct FindOtherMethods(Method, bool);
                            impl<R> Router<R> for FindOtherMethods {
                                async fn leaf_if<I, F, U>(&mut self, if_: I, _f: F)
                                where
                                    I: FnOnce(&Method) -> bool,
                                    F: AsyncFnOnce(R, Request) -> U,
                                    U: Response,
                                {
                                    if (if_)(&self.0) {
                                        self.1 = true;
                                    }
                                }
                            }

                            let mut check_one = FindOtherMethods(method, false);
                            R::register(&mut check_one).await;
                            if check_one.1 {
                                other_methods.push(check_one.0);
                            }
                        }

                        match other_methods.is_empty() {
                            true => {
                                tracing::debug!(method=?req.method, route=std::any::type_name::<R>(), "Not matching route for leaf");
                                RouterResponse::e404()
                            }
                            false => {
                                tracing::debug!(allowed=?other_methods, method=?req.method, "Method not allowed");
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

    fn path_recursive<F, U, P>(&mut self, path: P, f: F) -> impl Future<Output = ()>
    where
        F: AsyncFnOnce(T, &mut Request) -> U,
        U: MakeRoute,
        P: Into<StringId>,
    {
        self.route_if_recursive(
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

    fn route_recursive<F, U>(&mut self, f: F) -> impl Future<Output = ()>
    where
        F: AsyncFnOnce(T, &mut Request, StringId) -> U,
        U: MakeRoute,
    {
        self.route_if_recursive(|_| true, f)
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

    fn route_if_recursive<I, F, U>(&mut self, if_: I, f: F) -> impl Future<Output = ()>
    where
        I: FnOnce(&StringId) -> bool,
        F: AsyncFnOnce(T, &mut Request, StringId) -> U,
        U: MakeRoute,
    {
        let _ = (if_, f);
        async move {}
    }

    fn leaf<F, U>(&mut self, method: Method, f: F) -> impl Future<Output = ()>
    where
        F: AsyncFnOnce(T, Request) -> U,
        U: Response,
    {
        self.leaf_if(move |req_method| req_method == method, f)
    }

    fn any_leaf<F, U>(&mut self, f: F) -> impl Future<Output = ()>
    where
        F: AsyncFnOnce(T, Request) -> U,
        U: Response,
    {
        self.leaf_if(|_| true, f)
    }

    fn leaf_if<I, F, U>(&mut self, if_: I, f: F) -> impl Future<Output = ()>
    where
        I: FnOnce(&Method) -> bool,
        F: AsyncFnOnce(T, Request) -> U,
        U: Response,
    {
        let _ = (if_, f);
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

            async fn route_if_recursive<I, F, U>(&mut self, if_: I, f: F)
            where
                I: FnOnce(&StringId) -> bool,
                F: AsyncFnOnce(T, &mut Request, StringId) -> U,
                U: MakeRoute,
            {
                self.0
                    .route_if_recursive(if_, async |self_, req, path| match self_ {
                        Some(self_) => Some(f(self_, req, path).await),
                        None => None,
                    })
                    .await
            }

            async fn leaf_if<I, F, U>(&mut self, if_: I, f: F)
            where
                I: FnOnce(&Method) -> bool,
                F: AsyncFnOnce(T, Request) -> U,
                U: Response,
            {
                self.0
                    .leaf_if(if_, async |self_, req| match self_ {
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
impl<T, E> MakeRoute for Result<T, E>
where
    T: MakeRoute,
    E: Response,
{
    fn register<R: Router<Self>>(router: &mut R) -> impl Future<Output = ()> {
        struct ResultRouter<'a, R, E>(&'a mut R, std::marker::PhantomData<E>);
        impl<T, E, R> Router<T> for ResultRouter<'_, R, E>
        where
            T: MakeRoute,
            E: Response,
            R: Router<Result<T, E>>,
        {
            async fn middleware<F, U>(&mut self, f: F)
            where
                F: AsyncFnOnce(T, &mut Request) -> U,
                U: MakeRoute,
            {
                self.0
                    .middleware(async |self_, req| match self_ {
                        Ok(self_) => Ok(f(self_, req).await),
                        Err(e) => Err(e),
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
                        Ok(self_) => Ok(f(self_, req, path).await),
                        Err(e) => Err(e),
                    })
                    .await
            }

            async fn route_if_recursive<I, F, U>(&mut self, if_: I, f: F)
            where
                I: FnOnce(&StringId) -> bool,
                F: AsyncFnOnce(T, &mut Request, StringId) -> U,
                U: MakeRoute,
            {
                self.0
                    .route_if_recursive(if_, async |self_, req, path| match self_ {
                        Ok(self_) => Ok(f(self_, req, path).await),
                        Err(e) => Err(e),
                    })
                    .await
            }

            async fn leaf_if<I, F, U>(&mut self, if_: I, f: F)
            where
                I: FnOnce(&Method) -> bool,
                F: AsyncFnOnce(T, Request) -> U,
                U: Response,
            {
                self.0
                    .leaf_if(if_, async |self_, req| match self_ {
                        Ok(self_) => Ok(f(self_, req).await),
                        Err(e) => Err(e),
                    })
                    .await
            }
        }

        async move {
            T::register(&mut ResultRouter(router, Default::default())).await;
        }
    }
}
impl MakeRoute for () {
    async fn register<R: Router<Self>>(_router: &mut R) {}
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
    type Body = serde_json::Value;

    fn into_body(self) -> Option<Self::Body> {
        self.boxed.into_json()
    }

    fn status_code(&self) -> StatusCode {
        self.boxed.status_code()
    }
}

struct EmptyResponse(StatusCode);
impl Response for EmptyResponse {
    type Body = serde_json::Value;

    fn into_body(self) -> Option<Self::Body> {
        None
    }

    fn status_code(&self) -> StatusCode {
        self.0
    }
}

trait BoxedResponse: 'static {
    fn into_json(self: Box<Self>) -> Option<serde_json::Value>;
    fn status_code(&self) -> StatusCode;
    fn type_id(&self) -> TypeId;
    fn type_name(&self) -> &'static str;
}
impl<R> BoxedResponse for R
where
    R: Response,
{
    fn into_json(self: Box<Self>) -> Option<serde_json::Value> {
        self.into_body()
            .map(|body| serde_json::to_value(body).unwrap())
    }

    fn status_code(&self) -> StatusCode {
        Response::status_code(self)
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

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::{handler::Json200, uri_subject::path_str_to_path};

    type Str200 = Json200<&'static str>;

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

        async fn get<T, R>(&self, root: R, path: &str) -> T
        where
            R: MakeRoute,
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
                .await
                .downcast::<T>()
                .clone()
        }
    }

    #[test]
    fn downcast_response() {
        let response = RouterResponse::new(Json200("Ola"));
        assert_eq!(response.downcast::<Json200<&'static str>>().0, "Ola");
    }

    #[test]
    fn error_response() {
        Given::start(async |given| {
            struct Root;
            impl MakeRoute for Root {
                async fn register<R: Router<Self>>(router: &mut R) {
                    router
                        .route(async |_, _, path| {
                            if path == "inexistent" {
                                Err(Json200("circuit breaker"))
                            } else {
                                Ok(Middleware)
                            }
                        })
                        .await;
                }
            }
            struct Middleware;
            impl MakeRoute for Middleware {
                async fn register<R: Router<Self>>(router: &mut R) {
                    router.middleware(async |_, _| JustRoute).await
                }
            }
            struct JustRoute;
            impl MakeRoute for JustRoute {
                async fn register<R: Router<Self>>(router: &mut R) {
                    router.route(async |_, _, _| Root2).await
                }
            }
            struct Root2;
            impl MakeRoute for Root2 {
                async fn register<R: Router<Self>>(router: &mut R) {
                    router.route_recursive(async |self_, _, _| self_).await;

                    router.any_leaf(async |_, _| Json200("done")).await;
                }
            }

            assert_eq!(
                given
                    .get::<Result<Str200, Str200>, _>(Root, "hello/thing/more")
                    .await
                    .ok()
                    .unwrap()
                    .0,
                "done"
            );

            assert_eq!(
                given
                    .get::<Result<Str200, Str200>, _>(Root, "hello/thing")
                    .await
                    .ok()
                    .unwrap()
                    .0,
                "done"
            );

            assert_eq!(
                given
                    .get::<Result<Str200, Str200>, _>(Root, "inexistent/more/more")
                    .await
                    .err()
                    .unwrap()
                    .0,
                "circuit breaker"
            );
        });
    }
}
