use bytes::{Bytes, BytesMut};
use either::Either;
use hyper::{
    Method, StatusCode,
    body::{Body, Incoming},
};
use std::{
    borrow::Cow,
    collections::{HashMap, VecDeque},
    convert::Infallible,
    future::poll_fn,
    hash::Hash,
    io,
    ops::Deref,
    pin::Pin,
    rc::Rc,
    task::{Context, Poll},
};
use tokio::io::AsyncRead;

#[derive(Debug)]
pub struct Request<B> {
    pub method: Method,
    pub path: VecDeque<StringId>,
    pub headers: HashMap<StringId, String>,
    pub query: HashMap<StringId, String>,
    pub body: B,
}
impl Request<Incoming> {
    pub async fn collect_body(&mut self) -> Result<BytesMut, hyper::Error> {
        let mut r = BytesMut::new();
        loop {
            let frame = match poll_fn(|cx| Pin::new(&mut self.body).poll_frame(cx)).await {
                Some(Ok(frame)) => frame,

                Some(Err(e)) => return Err(e),
                None => break,
            };

            if let Ok(data) = frame.into_data() {
                r.extend_from_slice(&data);
            }
        }

        Ok(r)
    }

    pub async fn next_chunk(&mut self) -> Result<Bytes, hyper::Error> {
        poll_fn(|cx| {
            loop {
                break match Pin::new(&mut self.body).poll_frame(cx) {
                    Poll::Ready(Some(Ok(frame))) => match frame.into_data() {
                        Ok(data) => Poll::Ready(Ok(data)),
                        Err(_) => continue,
                    },
                    Poll::Ready(Some(Err(e))) => Poll::Ready(Err(e)),
                    Poll::Ready(None) => Poll::Ready(Ok(Bytes::new())),
                    Poll::Pending => Poll::Pending,
                };
            }
        })
        .await
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Authorization {
    pub scheme: StringId,
    pub params: String,
}
impl From<String> for Authorization {
    fn from(value: String) -> Self {
        match value.trim().split_once(' ') {
            Some((scheme, params)) => Self {
                scheme: scheme.to_string().into(),
                params: params.to_string(),
            },
            None => Self {
                scheme: value.into(),
                params: Default::default(),
            },
        }
    }
}

/// Turns an HTTP [`Request`] into a [`Response`].
///
/// This is the generic entry point the server invokes for every incoming
/// request, parameterized over the body type `B`. Implementations are usually
/// composed rather than written by hand: the primary one is
/// [`RouterHandler`](crate::router::RouterHandler), which dispatches the request
/// through a routing tree of [`MakeRoute`](crate::router::MakeRoute) nodes.
pub trait Handler<B> {
    /// The response type this handler produces.
    type Response: Response;

    /// Handle a single request, resolving to the response.
    fn handle(&self, req: Request<B>) -> impl Future<Output = Self::Response>;
}
impl<B, H: Handler<B>> Handler<B> for Rc<H> {
    type Response = H::Response;

    fn handle(&self, req: Request<B>) -> impl Future<Output = Self::Response> {
        H::handle(self, req)
    }
}

/// A value that can be turned into an HTTP response.
///
/// A leaf handler returns some `Response`; the server reads its status, headers,
/// and streamed [`Body`](Response::Body) to build the wire response. Implement
/// it for your own response types, or use the provided ones — [`Json`],
/// [`Json201`], [`HttpResponse`], [`Empty404`] — and the blanket impls for
/// `Result`, `Option`, `Either`, and [`io::Error`].
///
/// The `'static` bound lets [`RouterResponse`](crate::router::RouterResponse)
/// erase the concrete type behind a box.
pub trait Response: 'static {
    /// The streamed response body. See [`ResponseBody`].
    type Body: ResponseBody;

    /// The HTTP status code to send.
    fn status_code(&self) -> StatusCode;
    /// Consume the response into its body stream.
    fn into_body(self) -> Self::Body;
    /// Headers to send in addition to the content type derived from the body.
    /// Defaults to none.
    fn extra_headers(&self) -> HashMap<StringId, String> {
        Default::default()
    }
}
/// Replies `500 Internal Server Error` with an empty body, logging the error.
impl Response for io::Error {
    type Body = NoBody;

    fn into_body(self) -> Self::Body {
        tracing::error!(e=%self, "Internal server error");
        tracing::debug!(e=?self);
        NoBody
    }

    fn status_code(&self) -> StatusCode {
        StatusCode::INTERNAL_SERVER_ERROR
    }
}
/// Responds with the `Ok` value on success or the `Err` value on failure; both
/// sides are themselves responses, so the status comes from whichever variant
/// is present.
impl<T, E> Response for Result<T, E>
where
    T: Response,
    E: Response,
{
    type Body = EitherBody<T::Body, E::Body>;

    fn status_code(&self) -> StatusCode {
        match self {
            Ok(self_) => self_.status_code(),
            Err(self_) => self_.status_code(),
        }
    }

    fn into_body(self) -> Self::Body {
        match self {
            Ok(self_) => Either::Left(self_.into_body()),
            Err(self_) => Either::Right(self_.into_body()),
        }
        .into()
    }

    fn extra_headers(&self) -> HashMap<StringId, String> {
        match self {
            Ok(self_) => self_.extra_headers(),
            Err(self_) => self_.extra_headers(),
        }
    }
}
/// Responds with whichever variant is present, letting a handler return one of
/// two different response types from the same branch.
impl<T1, T2> Response for Either<T1, T2>
where
    T1: Response,
    T2: Response,
{
    type Body = EitherBody<T1::Body, T2::Body>;

    fn status_code(&self) -> StatusCode {
        match self {
            Either::Left(self_) => self_.status_code(),
            Either::Right(self_) => self_.status_code(),
        }
    }

    fn into_body(self) -> Self::Body {
        match self {
            Either::Left(self_) => Either::Left(self_.into_body()),
            Either::Right(self_) => Either::Right(self_.into_body()),
        }
        .into()
    }

    fn extra_headers(&self) -> HashMap<StringId, String> {
        match self {
            Either::Left(self_) => self_.extra_headers(),
            Either::Right(self_) => self_.extra_headers(),
        }
    }
}
/// `Some` responds with the inner value; `None` replies `404 Not Found` with an
/// empty body.
impl<T> Response for Option<T>
where
    T: Response,
{
    type Body = EitherBody<T::Body, NoBody>;

    fn status_code(&self) -> StatusCode {
        match self {
            Some(self_) => self_.status_code(),
            None => StatusCode::NOT_FOUND,
        }
    }

    fn into_body(self) -> Self::Body {
        match self {
            Some(self_) => Either::Left(self_.into_body()),
            None => Either::Right(NoBody),
        }
        .into()
    }

    fn extra_headers(&self) -> HashMap<StringId, String> {
        self.as_ref()
            .map(Response::extra_headers)
            .unwrap_or_default()
    }
}
/// Never produced; lets a handler whose error half cannot occur still satisfy a
/// `Response` bound. Calling its methods panics.
impl Response for Infallible {
    type Body = NoBody;

    fn status_code(&self) -> StatusCode {
        unreachable!()
    }

    fn into_body(self) -> Self::Body {
        unreachable!()
    }
}

/// The body of a [`Response`]: an [`AsyncRead`] stream of bytes plus the
/// metadata needed to frame it.
///
/// The server reads from the stream until it ends, sending the bytes as the
/// response body. [`content_type`](ResponseBody::content_type) supplies the
/// `Content-Type` header (an empty string omits it) and
/// [`length`](ResponseBody::length) the `Content-Length` when known.
pub trait ResponseBody: AsyncRead {
    /// The MIME type for the `Content-Type` header; an empty string omits the
    /// header.
    fn content_type(&self) -> Cow<'static, str>;
    /// The body length in bytes if known ahead of time, else `None` for a
    /// streamed body of unknown size.
    fn length(&self) -> Option<u64>;
}
impl ResponseBody for Pin<Box<dyn ResponseBody>> {
    fn content_type(&self) -> Cow<'static, str> {
        ResponseBody::content_type(self.deref())
    }

    fn length(&self) -> Option<u64> {
        ResponseBody::length(self.deref())
    }
}

/// An empty [`ResponseBody`] — zero bytes and no content type.
pub struct NoBody;
impl AsyncRead for NoBody {
    fn poll_read(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        _buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}
impl ResponseBody for NoBody {
    fn content_type(&self) -> Cow<'static, str> {
        Cow::Borrowed("")
    }

    fn length(&self) -> Option<u64> {
        Some(0)
    }
}

/// A [`ResponseBody`] that is one of two body types, chosen at runtime — the
/// body counterpart to a `Response` for [`Either`], [`Result`], or [`Option`].
pub struct EitherBody<T1, T2>(Either<T1, T2>);
impl<T1, T2> From<Either<T1, T2>> for EitherBody<T1, T2> {
    fn from(value: Either<T1, T2>) -> Self {
        Self(value)
    }
}
impl<T1, T2> EitherBody<T1, T2> {
    fn project(self: Pin<&mut Self>) -> Either<Pin<&mut T1>, Pin<&mut T2>> {
        unsafe {
            self.get_unchecked_mut()
                .0
                .as_mut()
                .map_left(|p| Pin::new_unchecked(p))
                .map_right(|p| Pin::new_unchecked(p))
        }
    }
}
impl<T1, T2> AsyncRead for EitherBody<T1, T2>
where
    T1: AsyncRead,
    T2: AsyncRead,
{
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.project() {
            Either::Left(self_) => self_.poll_read(cx, buf),
            Either::Right(self_) => self_.poll_read(cx, buf),
        }
    }
}
impl<T1, T2> ResponseBody for EitherBody<T1, T2>
where
    T1: ResponseBody,
    T2: ResponseBody,
{
    fn content_type(&self) -> Cow<'static, str> {
        match &self.0 {
            Either::Left(self_) => self_.content_type(),
            Either::Right(self_) => self_.content_type(),
        }
    }

    fn length(&self) -> Option<u64> {
        match &self.0 {
            Either::Left(self_) => self_.length(),
            Either::Right(self_) => self_.length(),
        }
    }
}

/// A [`Response`] from an explicit [`ResponseBody`] and [`StatusCode`] — the
/// general-purpose way to return a raw or streamed body.
pub struct HttpResponse<B>(pub B, pub StatusCode);
impl<B: ResponseBody + 'static> HttpResponse<B> {
    /// Construct an `HttpResponse` with status `200 OK`.
    pub fn h200(b: B) -> Self {
        Self(b, StatusCode::OK)
    }
}
impl<B: ResponseBody + 'static> Response for HttpResponse<B> {
    type Body = B;

    fn status_code(&self) -> StatusCode {
        self.1
    }

    fn into_body(self) -> Self::Body {
        self.0
    }
}

/// A [`Response`] that serializes `T` to JSON with the given [`StatusCode`] and
/// `application/json` content type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Json<T>(pub T, pub StatusCode);
impl<T: serde::Serialize + 'static> Json<T> {
    /// Construct a `Json` response with status `200 OK`.
    pub fn j200(t: T) -> Self {
        Self(t, StatusCode::OK)
    }
}
impl<T: serde::Serialize + 'static> Response for Json<T> {
    type Body = FullBody;

    fn status_code(&self) -> StatusCode {
        self.1
    }

    fn into_body(self) -> Self::Body {
        FullBody(
            serde_json::to_string_pretty(&self.0)
                .expect("Serialization should never fail")
                .into_bytes()
                .into(),
            Cow::Borrowed("application/json"),
        )
    }
}

/// A `201 Created` [`Response`]: serializes `T` to JSON and sets a `Location`
/// header from the new resource's [`ResourceLocation`].
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Json201<T>(pub T);
impl<T: serde::Serialize + ResourceLocation + 'static> Response for Json201<T> {
    type Body = FullBody;

    fn status_code(&self) -> StatusCode {
        StatusCode::CREATED
    }

    fn into_body(self) -> Self::Body {
        let code = self.status_code();
        Json(self.0, code).into_body()
    }

    fn extra_headers(&self) -> HashMap<StringId, String> {
        [(StringId::from("Location"), self.0.location())]
            .into_iter()
            .collect()
    }
}

/// Supplies the URL of a created resource for the `Location` header of a
/// [`Json201`] response.
pub trait ResourceLocation {
    /// The full location, by default [`base`](ResourceLocation::base) followed
    /// by [`resource_id`](ResourceLocation::resource_id).
    fn location(&self) -> String {
        let mut out = Self::base().to_string();
        out.push_str(&self.resource_id());
        out
    }

    /// The base path under which resources of this type live (e.g. `/users/`).
    fn base() -> &'static str;
    /// This resource's identifier, appended to the base.
    fn resource_id(&self) -> Cow<'_, str>;
}

/// A [`Response`] that replies `404 Not Found` with an empty body.
pub struct Empty404;
impl Response for Empty404 {
    type Body = NoBody;

    fn status_code(&self) -> StatusCode {
        StatusCode::NOT_FOUND
    }

    fn into_body(self) -> Self::Body {
        NoBody
    }
}

/// A [`ResponseBody`] backed by fully-buffered bytes with a fixed content type
/// and known length. Backs [`Json`]/[`Json201`].
pub struct FullBody(bytes::Bytes, Cow<'static, str>);
impl AsyncRead for FullBody {
    fn poll_read(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let n = self.0.len().min(buf.remaining());
        buf.put_slice(&self.get_mut().0.split_to(n));
        Poll::Ready(Ok(()))
    }
}
impl ResponseBody for FullBody {
    fn content_type(&self) -> Cow<'static, str> {
        self.1.clone()
    }

    fn length(&self) -> Option<u64> {
        Some(self.0.len() as u64)
    }
}

/// A string that compares **case-insensitively** while preserving its original
/// casing.
///
/// Routing keys — path segments, header and query names — must match
/// regardless of case (`"Content-Type"` and `"content-type"` are the same key),
/// yet the original spelling is still worth keeping for display, logging, and
/// serialization. `StringId` carries both: the original text (via [`Deref`] to
/// [`str`], [`Display`](std::fmt::Display), and [`Serialize`](serde::Serialize))
/// and a lowercased identity (via [`id`](StringId::id)) used by [`PartialEq`],
/// [`Eq`], [`Hash`], and [`Ord`]. As a result it can key a [`HashMap`] while
/// staying case-insensitive.
///
/// It can be built from either an owned [`String`] or a `&'static str`, the
/// latter without allocating when the text is already lowercase. The four
/// variants encode those two axes — owned vs. static, and whether a separate
/// lowercased copy was needed:
///
/// ```
/// # use http_gateway::handler::StringId;
/// let from_static: StringId = "Authorization".into();
/// let from_owned: StringId = String::from("Authorization").into();
/// assert_eq!(from_static, from_owned);
/// assert_eq!(from_static, "authorization"); // case-insensitive comparison
/// ```
#[derive(Clone)]
pub enum StringId {
    /// An owned string that was already lowercase; the text is its own id.
    Same(String),
    /// An owned mixed-case string paired with its lowercased id.
    OriginalAndId(String, String),
    /// A `&'static str` that was already lowercase; the text is its own id.
    Static(&'static str),
    /// A `&'static str` with mixed case paired with its lowercased id.
    StaticAndId(&'static str, String),
}
impl StringId {
    /// Build a `StringId` from a borrowed string, copying it into an owned
    /// value. Prefer `StringId::from(&'static str)` when the text is a string
    /// literal to avoid the allocation.
    pub fn new(id: &str) -> Self {
        Self::from(id.to_string())
    }

    /// The lowercased identity used for comparison, hashing, and ordering. Use
    /// [`Deref`]/[`Display`](std::fmt::Display) instead when you want the
    /// original casing.
    pub fn id(&self) -> &str {
        match self {
            StringId::Same(id) => id,
            StringId::OriginalAndId(_, id) => id,
            StringId::Static(id) => id,
            StringId::StaticAndId(_, id) => id,
        }
    }
}
/// Takes ownership of `value`. If it is already lowercase no extra allocation
/// is made; otherwise a lowercased id is computed alongside it.
impl From<String> for StringId {
    fn from(value: String) -> Self {
        if value.chars().all(char::is_lowercase) {
            return Self::Same(value);
        }

        let id = value.to_lowercase();
        Self::OriginalAndId(value, id)
    }
}
/// Borrows a string literal without allocating when it is already lowercase;
/// only a mixed-case literal needs an owned lowercased id.
impl From<&'static str> for StringId {
    fn from(value: &'static str) -> Self {
        if value.chars().all(char::is_lowercase) {
            return Self::Static(value);
        }

        let id = value.to_lowercase();
        Self::StaticAndId(value, id)
    }
}
impl Deref for StringId {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        match self {
            StringId::Same(str) => str,
            StringId::OriginalAndId(str, _) => str,
            StringId::Static(str) => str,
            StringId::StaticAndId(str, _) => str,
        }
    }
}
impl PartialEq for StringId {
    fn eq(&self, other: &Self) -> bool {
        PartialEq::eq(self.id(), other.id())
    }
}
impl PartialEq<&str> for StringId {
    fn eq(&self, other: &&str) -> bool {
        if self.len() != other.len() {
            return false;
        }

        self.id()
            .chars()
            .zip(other.chars().flat_map(char::to_lowercase))
            .all(|(lhs, rhs)| lhs == rhs)
    }
}
impl PartialEq<&str> for &StringId {
    fn eq(&self, other: &&str) -> bool {
        StringId::eq(self, other)
    }
}
impl Eq for StringId {}
impl PartialOrd for StringId {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for StringId {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        Ord::cmp(self.id(), other.id())
    }
}
impl Hash for StringId {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        Hash::hash(self.id(), state)
    }
}
impl std::fmt::Display for StringId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self.deref(), f)
    }
}
impl std::fmt::Debug for StringId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self.deref(), f)
    }
}
impl serde::Serialize for StringId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serde::Serialize::serialize(self.deref(), serializer)
    }
}
impl<'de> serde::Deserialize<'de> for StringId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let string: String = serde::Deserialize::deserialize(deserializer)?;
        Ok(Self::from(string))
    }
}
