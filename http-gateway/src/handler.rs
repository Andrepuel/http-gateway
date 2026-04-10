use std::{
    borrow::Cow,
    collections::{HashMap, VecDeque},
    hash::Hash,
    io,
    ops::Deref,
    pin::Pin,
    rc::Rc,
    task::{Context, Poll},
};

use either::Either;
use hyper::{Method, StatusCode};
use tokio::io::AsyncRead;

#[derive(Debug)]
pub struct Request {
    pub method: Method,
    pub path: VecDeque<StringId>,
    pub headers: HashMap<StringId, String>,
    pub query: HashMap<StringId, String>,
    pub body: Option<serde_json::Value>,
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

pub trait Handler {
    type Response: Response;

    fn handle(&self, req: Request) -> impl Future<Output = Self::Response>;
}
impl<H: Handler> Handler for Rc<H> {
    type Response = H::Response;

    fn handle(&self, req: Request) -> impl Future<Output = Self::Response> {
        H::handle(self, req)
    }
}

pub trait Response: 'static {
    type Body: ResponseBody;

    fn status_code(&self) -> StatusCode;
    fn into_body(self) -> Self::Body;
    fn extra_headers(&self) -> HashMap<StringId, String> {
        Default::default()
    }
}
impl Response for io::Error {
    type Body = NoBody;

    fn into_body(self) -> Self::Body {
        NoBody
    }

    fn status_code(&self) -> StatusCode {
        StatusCode::INTERNAL_SERVER_ERROR
    }
}
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

pub trait ResponseBody: AsyncRead {
    fn content_type(&self) -> Cow<'static, str>;
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

pub struct HttpResponse<B>(pub B, pub StatusCode);
impl<B: ResponseBody + 'static> HttpResponse<B> {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Json<T>(pub T, pub StatusCode);
impl<T: serde::Serialize + 'static> Json<T> {
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

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Json201<T>(pub T, String);
impl<T: serde::Serialize + 'static> Response for Json201<T> {
    type Body = FullBody;

    fn status_code(&self) -> StatusCode {
        StatusCode::CREATED
    }

    fn into_body(self) -> Self::Body {
        let code = self.status_code();
        Json(self.0, code).into_body()
    }

    fn extra_headers(&self) -> HashMap<StringId, String> {
        [(StringId::from("Location"), self.1.clone())]
            .into_iter()
            .collect()
    }
}

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

#[derive(Clone)]
pub enum StringId {
    Same(String),
    OriginalAndId(String, String),
    Static(&'static str),
    StaticAndId(&'static str, String),
}
impl StringId {
    pub fn new(id: &str) -> Self {
        Self::from(id.to_string())
    }

    pub fn id(&self) -> &str {
        match self {
            StringId::Same(id) => id,
            StringId::OriginalAndId(_, id) => id,
            StringId::Static(id) => id,
            StringId::StaticAndId(_, id) => id,
        }
    }
}
impl From<String> for StringId {
    fn from(value: String) -> Self {
        if value.chars().all(char::is_lowercase) {
            return Self::Same(value);
        }

        let id = value.to_lowercase();
        Self::OriginalAndId(value, id)
    }
}
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
