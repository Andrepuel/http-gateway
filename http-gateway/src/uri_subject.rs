use hyper::Uri;
use std::{
    collections::{HashMap, VecDeque},
    ops::Deref,
};

use crate::handler::StringId;

pub fn uri_to_path(uri: Uri) -> VecDeque<StringId> {
    let Some(path_and_query) = uri.path_and_query() else {
        return Default::default();
    };

    path_and_query
        .path()
        .split('/')
        .filter_map(|path| urlencoding::decode(path).ok())
        .filter(|path| !str::is_empty(path.deref()))
        .map(|path| replace_special_chars(path.into_owned()))
        .map(StringId::from)
        .collect()
}

pub fn uri_to_query(uri: &Uri) -> HashMap<StringId, String> {
    let Some(path_and_query) = uri.path_and_query().and_then(|pnq| pnq.query()) else {
        return Default::default();
    };

    path_and_query
        .split('&')
        .map(|kv| match kv.split_once('=') {
            Some((k, v)) => (k, v),
            None => (kv, Default::default()),
        })
        .filter_map(|(k, v)| Some((urlencoding::decode(k).ok()?, urlencoding::decode(v).ok()?)))
        .map(|(k, v)| (StringId::from(k.into_owned()), v.to_string()))
        .collect()
}

fn replace_special_chars(mut path: String) -> String {
    for char in ['.', ' ', '*', '>'] {
        if path.contains(char) {
            path = path.replace(char, "_");
        }
    }

    path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_to_subject_cases() {
        for (uri, expected) in [
            ("/abc/def", vec!["abc", "def"]),
            (
                "/a-a/b%20b/c_c/d%2Ad/e%3Ee/f.f",
                vec!["a-a", "b_b", "c_c", "d_d", "e_e", "f_f"],
            ),
        ] {
            let uri = uri.parse().unwrap();
            let subject = uri_to_path(uri);
            assert_eq!(
                subject.iter().map(|x| x.deref()).collect::<Vec<_>>(),
                expected
            );
        }
    }

    #[test]
    fn uri_to_query_cases() {
        for (uri, expected) in [
            ("/abc?a=b&c=d", [("a", "b"), ("c", "d")].as_slice()),
            ("/abc?%20=%3E", &[(" ", ">")]),
        ] {
            let uri = uri.parse().unwrap();
            let query = uri_to_query(&uri);
            let expected = expected
                .iter()
                .map(|(k, v)| (k.to_string().into(), v.to_string()))
                .collect();

            assert_eq!(query, expected);
        }
    }
}
