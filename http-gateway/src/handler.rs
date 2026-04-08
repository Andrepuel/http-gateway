use std::{
    collections::{HashMap, VecDeque},
    hash::Hash,
    io,
    ops::Deref, rc::Rc,
};

use either::Either;
use hyper::{Method, StatusCode};

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
    type Body: serde::Serialize;

    fn into_body(self) -> Option<Self::Body>;
    fn status_code(&self) -> StatusCode;
}
impl Response for io::Error {
    type Body = serde_json::Value;

    fn into_body(self) -> Option<Self::Body> {
        tracing::error!(e=%self, "Server internal error");
        tracing::debug!(e=?self);
        None
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
    type Body = EitherSer<T::Body, E::Body>;

    fn into_body(self) -> Option<Self::Body> {
        match self {
            Ok(ok) => ok.into_body().map(Either::Left),
            Err(err) => err.into_body().map(Either::Right),
        }
        .map(Into::into)
    }

    fn status_code(&self) -> StatusCode {
        match self {
            Ok(self_) => self_.status_code(),
            Err(self_) => self_.status_code(),
        }
    }
}
impl<T1, T2> Response for Either<T1, T2>
where
    T1: Response,
    T2: Response,
{
    type Body = EitherSer<T1::Body, T2::Body>;

    fn into_body(self) -> Option<Self::Body> {
        match self {
            Either::Left(self_) => self_.into_body().map(Either::Left),
            Either::Right(self_) => self_.into_body().map(Either::Right),
        }
        .map(Into::into)
    }

    fn status_code(&self) -> StatusCode {
        match self {
            Either::Left(self_) => self_.status_code(),
            Either::Right(self_) => self_.status_code(),
        }
    }
}
impl<T> Response for Option<T>
where
    T: Response,
{
    type Body = T::Body;

    fn into_body(self) -> Option<Self::Body> {
        match self {
            Some(self_) => self_.into_body(),
            None => None,
        }
    }

    fn status_code(&self) -> StatusCode {
        match self {
            Some(self_) => self_.status_code(),
            None => StatusCode::NOT_FOUND,
        }
    }
}

pub struct EitherSer<T1, T2>(Either<T1, T2>);
impl<T1, T2> From<Either<T1, T2>> for EitherSer<T1, T2> {
    fn from(value: Either<T1, T2>) -> Self {
        Self(value)
    }
}
impl<T1, T2> serde::Serialize for EitherSer<T1, T2>
where
    T1: serde::Serialize,
    T2: serde::Serialize,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match &self.0 {
            Either::Left(self_) => serde::Serialize::serialize(self_, serializer),
            Either::Right(self_) => serde::Serialize::serialize(self_, serializer),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Json200<T>(pub T);
impl<T: serde::Serialize> From<T> for Json200<T> {
    fn from(value: T) -> Self {
        Self(value)
    }
}
impl<T: serde::Serialize + 'static> Response for Json200<T> {
    type Body = T;

    fn into_body(self) -> Option<Self::Body> {
        Some(self.0)
    }

    fn status_code(&self) -> StatusCode {
        StatusCode::OK
    }
}

pub struct Empty404;
impl Response for Empty404 {
    type Body = serde_json::Value;

    fn into_body(self) -> Option<Self::Body> {
        None
    }

    fn status_code(&self) -> StatusCode {
        StatusCode::NOT_FOUND
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
