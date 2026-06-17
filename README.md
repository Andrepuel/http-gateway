# http-gateway

A small, tree-structured HTTP router for building REST APIs in Rust.

An API is described as a tree of `MakeRoute` nodes; the router walks the URL path
one segment at a time, running middleware, routes, and method leaves. JSON
bodies, `Authorization` handling, and transactional middleware are provided by
extension traits.

## Documentation

The routing API is documented in depth at the crate root. The module-level guide
in [`http-gateway/src/lib.rs`](http-gateway/src/lib.rs) covers the routing model
and walks through quick start, common helpers, authentication, request bodies,
and transactions. Render it locally with:

```sh
cargo doc -p http-gateway --open
```

## Examples

- **`http-file-explorer`** — serves the filesystem as a browsable, editable
  tree, showcasing recursive routing for nested resources.
- **`http-todos`** — a small todos REST API.

## Running

A server reads its configuration from the environment (a `.env` file is loaded
if present) and needs a `LISTEN` URL:

```sh
LISTEN=http://0.0.0.0:8080 cargo run -p http-todos
```
