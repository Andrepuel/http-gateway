pub mod ext;

use crate::handler::{Empty404, Handler, NoBody, Request, Response, ResponseBody, StringId};
use either::Either;
use futures::FutureExt;
use hyper::{Method, StatusCode, body::Incoming};
use std::{any::TypeId, collections::HashMap, pin::Pin};

pub struct RouterHandler<B, R> {
    root: R,
    body_type: std::marker::PhantomData<fn(B)>,
}
impl<B, R> RouterHandler<B, R>
where
    R: MakeRoute<B> + Clone,
{
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
                I: FnOnce(R, &Request<B>) -> Then<T2, R>,
                F: AsyncFnOnce(&'a mut T2, &mut Request<B>) -> U,
                U: MakeRoute<B> + 'a,
                P: AsyncFnOnce(T2, RouterResponse) -> RouterResponse,
            {
                self.0
                    .execute_map(
                        |(root, req)| match if_(root, &req) {
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
                        I: FnOnce(R, &Request<B>) -> Then<T2, R>,
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
                        I: FnOnce(R, &Request<B>) -> Then<T2, R>,
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
                                    I: FnOnce(R, &Request<B>) -> Then<T2, R>,
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

pub trait Router<T, B = Incoming> {
    fn middleware_mut_map<'a, I, T2, F, U, P>(
        &mut self,
        if_: I,
        f: F,
        post: P,
    ) -> impl Future<Output = ()>
    where
        T2: 'a,
        I: FnOnce(T, &Request<B>) -> Then<T2, T>,
        F: AsyncFnOnce(&'a mut T2, &mut Request<B>) -> U,
        U: MakeRoute<B> + 'a,
        P: AsyncFnOnce(T2, RouterResponse) -> RouterResponse;

    fn route_map<I, T2, F, U>(&mut self, if_: I, f: F) -> impl Future<Output = ()>
    where
        I: FnOnce(T, &Request<B>, &StringId) -> Then<T2, T>,
        F: AsyncFnOnce(T2, &mut Request<B>, StringId) -> U,
        U: MakeRoute<B>;

    fn route_map_recursive<I, T2, F, U>(&mut self, if_: I, f: F) -> impl Future<Output = ()>
    where
        I: FnOnce(T, &StringId) -> Then<T2, T>,
        F: AsyncFnOnce(T2, &mut Request<B>, StringId) -> U,
        U: MakeRoute<B>;

    fn leaf_map<I, T2, F, U>(&mut self, if_: I, f: F) -> impl Future<Output = ()>
    where
        I: FnOnce(T, &Method) -> Then<T2, T>,
        F: AsyncFnOnce(T2, Request<B>) -> U,
        U: Response;
}

impl<T, B, R: Router<T, B>> RouterDerived<T, B> for R {}
pub trait RouterDerived<T, B>: Router<T, B> {
    fn middleware<F, U>(&mut self, f: F) -> impl Future<Output = ()>
    where
        F: AsyncFnOnce(T, &mut Request<B>) -> U,
        U: MakeRoute<B>,
    {
        self.middleware_if(|_, _| true, f)
    }

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

    fn middleware_mut<'a, F, U, P>(&mut self, f: F, post: P) -> impl Future<Output = ()>
    where
        T: 'a,
        F: AsyncFnOnce(&'a mut T, &mut Request<B>) -> U,
        U: MakeRoute<B> + 'a,
        P: AsyncFnOnce(T, RouterResponse) -> RouterResponse,
    {
        self.middleware_mut_if(|_, _| true, f, post)
    }

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

    fn route<F, U>(&mut self, f: F) -> impl Future<Output = ()>
    where
        F: AsyncFnOnce(T, &mut Request<B>, StringId) -> U,
        U: MakeRoute<B>,
    {
        self.route_if(|_, _, _| true, f)
    }

    fn route_recursive<F, U>(&mut self, f: F) -> impl Future<Output = ()>
    where
        F: AsyncFnOnce(T, &mut Request<B>, StringId) -> U,
        U: MakeRoute<B>,
    {
        self.route_if_recursive(|_, _| true, f)
    }

    fn leaf<F, U>(&mut self, method: Method, f: F) -> impl Future<Output = ()>
    where
        F: AsyncFnOnce(T, Request<B>) -> U,
        U: Response,
    {
        self.leaf_if(move |_, req_method| req_method == method, f)
    }

    fn any_leaf<F, U>(&mut self, f: F) -> impl Future<Output = ()>
    where
        F: AsyncFnOnce(T, Request<B>) -> U,
        U: Response,
    {
        self.leaf_if(|_, _| true, f)
    }

    fn get<F, U>(&mut self, f: F) -> impl Future<Output = ()>
    where
        F: AsyncFnOnce(T, Request<B>) -> U,
        U: Response,
    {
        self.leaf(Method::GET, f)
    }

    fn put<F, U>(&mut self, f: F) -> impl Future<Output = ()>
    where
        F: AsyncFnOnce(T, Request<B>) -> U,
        U: Response,
    {
        self.leaf(Method::PUT, f)
    }

    fn post<F, U>(&mut self, f: F) -> impl Future<Output = ()>
    where
        F: AsyncFnOnce(T, Request<B>) -> U,
        U: Response,
    {
        self.leaf(Method::POST, f)
    }

    fn delete<F, U>(&mut self, f: F) -> impl Future<Output = ()>
    where
        F: AsyncFnOnce(T, Request<B>) -> U,
        U: Response,
    {
        self.leaf(Method::DELETE, f)
    }

    fn leaf_route<F, U>(&mut self, method: Method, f: F) -> impl Future<Output = ()>
    where
        F: AsyncFnOnce(T, &mut Request<B>, StringId) -> U,
        U: Response,
    {
        self.route_if(
            move |_, req, _| req.method == method,
            async |self_, req, path| LeafRoute(f(self_, req, path).await),
        )
    }

    fn get_route<F, U>(&mut self, f: F) -> impl Future<Output = ()>
    where
        F: AsyncFnOnce(T, &mut Request<B>, StringId) -> U,
        U: Response,
    {
        self.leaf_route(Method::GET, f)
    }

    fn put_route<F, U>(&mut self, f: F) -> impl Future<Output = ()>
    where
        F: AsyncFnOnce(T, &mut Request<B>, StringId) -> U,
        U: Response,
    {
        self.leaf_route(Method::PUT, f)
    }

    fn post_route<F, U>(&mut self, f: F) -> impl Future<Output = ()>
    where
        F: AsyncFnOnce(T, &mut Request<B>, StringId) -> U,
        U: Response,
    {
        self.leaf_route(Method::POST, f)
    }

    fn delete_route<F, U>(&mut self, f: F) -> impl Future<Output = ()>
    where
        F: AsyncFnOnce(T, &mut Request<B>, StringId) -> U,
        U: Response,
    {
        self.leaf_route(Method::DELETE, f)
    }

    fn leaf_path<P, F, U>(&mut self, path: P, method: Method, f: F) -> impl Future<Output = ()>
    where
        P: Into<StringId>,
        F: AsyncFnOnce(T, &mut Request<B>) -> U,
        U: Response,
    {
        self.route_if(
            move |_, req, req_path| req.method == method && req_path == &path.into(),
            async |self_, req, _| LeafRoute(f(self_, req).await),
        )
    }

    fn get_path<P, F, U>(&mut self, path: P, f: F) -> impl Future<Output = ()>
    where
        P: Into<StringId>,
        F: AsyncFnOnce(T, &mut Request<B>) -> U,
        U: Response,
    {
        self.leaf_path(path, Method::GET, f)
    }

    fn put_path<P, F, U>(&mut self, path: P, f: F) -> impl Future<Output = ()>
    where
        P: Into<StringId>,
        F: AsyncFnOnce(T, &mut Request<B>) -> U,
        U: Response,
    {
        self.leaf_path(path, Method::PUT, f)
    }

    fn post_path<P, F, U>(&mut self, path: P, f: F) -> impl Future<Output = ()>
    where
        P: Into<StringId>,
        F: AsyncFnOnce(T, &mut Request<B>) -> U,
        U: Response,
    {
        self.leaf_path(path, Method::POST, f)
    }

    fn delete_path<P, F, U>(&mut self, path: P, f: F) -> impl Future<Output = ()>
    where
        P: Into<StringId>,
        F: AsyncFnOnce(T, &mut Request<B>) -> U,
        U: Response,
    {
        self.leaf_path(path, Method::DELETE, f)
    }
}

pub trait MakeRoute<B = Incoming>: Sized {
    fn register<R: Router<Self, B>>(router: &mut R) -> impl Future<Output = ()>;
}
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
                I: FnOnce(T, &Request<B>) -> Then<T2, T>,
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

pub enum Then<U, T> {
    Then(U),
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
                I: FnOnce(T, &Request<B>) -> Then<T2, T>,
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
                I: FnOnce(T, &Request<B>) -> Then<T2, T>,
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
impl<B> MakeRoute<B> for () {
    async fn register<R: Router<Self, B>>(_router: &mut R) {}
}

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
