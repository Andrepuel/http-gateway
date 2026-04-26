use either::Either;
use http_gateway::{
    bytes::{Bytes, BytesMut},
    handler::{HttpResponse, Json, Json201, ResourceLocation, Response, ResponseBody},
    hyper::{Method, StatusCode},
    router::{MakeRoute, RouterHandler},
};
use std::{
    borrow::Cow,
    collections::VecDeque,
    io::{self, Read, Seek, Write},
    ops::Deref,
    path::PathBuf,
    task::Poll,
};
use tokio::sync::{mpsc, oneshot};

fn main() {
    let folder = std::path::absolute(std::env::current_dir().unwrap()).unwrap();
    http_gateway::http_server_main(|| RouterHandler::new(Folder(folder, PathBuf::from("/"))));
}

#[derive(Clone)]
struct Folder(PathBuf, PathBuf);
impl MakeRoute for Folder {
    async fn register<R: http_gateway::router::Router<Self>>(router: &mut R) {
        router
            .middleware_if(
                |_, req| req.method == Method::POST || req.method == Method::PUT,
                async |self_, _req| EditFile(self_.0, self_.1),
            )
            .await;

        router
            .route_recursive(async |self_, _, path| {
                let absolute = self_.0.join(path.deref());
                let relative = self_.1.join(path.deref());
                tracing::debug!(?absolute, ?relative);
                if absolute.exists() {
                    Some(Folder(absolute, relative))
                } else {
                    None
                }
            })
            .await;

        router
            .get(async |self_, _| {
                if self_.0.is_dir() {
                    let mut html = HtmlFile::new();
                    html.ul(|html| {
                        let entries =
                            self_.0.read_dir().unwrap().map(|entry| {
                                entry.unwrap().file_name().to_string_lossy().into_owned()
                            });
                        for entry in entries {
                            html.li(|html| {
                                let link = self_.1.join(&entry);
                                html.a(link.to_string_lossy().into_owned(), entry);
                            });
                        }
                    });
                    io::Result::Ok(Either::Left(html))
                } else {
                    let (emit_mime, on_mime) = oneshot::channel();
                    let (emit_read, on_read) = mpsc::channel(1);
                    let mut file = std::fs::File::open(self_.0)?;

                    let runtime = tokio::runtime::Handle::current();
                    tokio::task::spawn_blocking(move || {
                        runtime.block_on(async move {
                            let len = (|| {
                                let len = file.seek(io::SeekFrom::End(0))?;
                                file.seek(io::SeekFrom::Start(0))?;
                                io::Result::Ok(len)
                            })();

                            let len = match len {
                                Ok(len) => len,
                                Err(e) => {
                                    let _ = emit_mime.send(Err(e));
                                    return;
                                }
                            };

                            let mut buf = BytesMut::new();
                            buf.resize(8192, 0);

                            match file.read(&mut buf) {
                                Ok(n) => {
                                    let read = buf.split_to(n);
                                    let mime = infer::get(&read)
                                        .map(|t| t.mime_type())
                                        .unwrap_or_else(|| {
                                            if std::str::from_utf8(&read).is_ok() {
                                                "text/plain"
                                            } else {
                                                "application/octet-stream"
                                            }
                                        });
                                    let _ = emit_mime.send(Ok((mime, len)));
                                    let _ = emit_read.send(Ok(read)).await;
                                }
                                Err(e) => {
                                    let _ = emit_mime.send(Err(e));
                                }
                            }

                            loop {
                                if buf.len() < 4096 {
                                    buf.resize(8192, 0);
                                }

                                match file.read(&mut buf) {
                                    Ok(0) => {
                                        break;
                                    }
                                    Ok(n) => {
                                        if emit_read.send(Ok(buf.split_to(n))).await.is_err() {
                                            break;
                                        }
                                    }
                                    Err(e) => {
                                        let _ = emit_read.send(Err(e)).await;
                                        break;
                                    }
                                }
                            }
                        })
                    });

                    let (mime, len) = on_mime.await.unwrap()?;
                    tracing::debug!(?mime, ?len);
                    Ok(Either::Right(HttpResponse::h200(ReadFileBody(
                        on_read,
                        mime,
                        Default::default(),
                        len,
                    ))))
                }
            })
            .await;
    }
}

#[derive(Clone)]
struct EditFile(PathBuf, PathBuf);
impl MakeRoute for EditFile {
    async fn register<
        R: http_gateway::router::Router<Self, http_gateway::hyper::body::Incoming>,
    >(
        router: &mut R,
    ) {
        router
            .route_recursive(async |self_, _, path| {
                let absolute = self_.0.join(path.deref());
                let relative = self_.1.join(path.deref());
                tracing::debug!(?absolute, ?relative);
                Some(EditFile(absolute, relative))
            })
            .await;

        router
            .post(async |self_, mut req| {
                let mut i = 0;
                let (name, relative) = loop {
                    let newfile = format!("newfile{i}");
                    let name = self_.0.join(&newfile);
                    if name.exists() {
                        i += 1;
                        continue;
                    }
                    break (name, self_.1.join(newfile));
                };

                if let Some(dir) = name.parent() {
                    std::fs::create_dir_all(dir)?;
                }

                let mut file = std::fs::File::create(&name)?;
                loop {
                    let chunk = req.next_chunk().await.map_err(io::Error::other)?;
                    if chunk.is_empty() {
                        break;
                    }
                    file.write_all(&chunk)?;
                }

                io::Result::Ok(Json201(NewFile { path: relative }))
            })
            .await;

        router
            .put(async |self_, mut req| {
                if let Some(dir) = self_.0.parent() {
                    std::fs::create_dir_all(dir)?;
                }

                let mut file = std::fs::File::create(&self_.0)?;
                loop {
                    let chunk = req.next_chunk().await.map_err(io::Error::other)?;
                    if chunk.is_empty() {
                        break;
                    }
                    file.write_all(&chunk)?;
                }

                io::Result::Ok(Json::j200(()))
            })
            .await;
    }
}

#[derive(serde::Serialize)]
struct NewFile {
    path: PathBuf,
}
impl ResourceLocation for NewFile {
    fn base() -> &'static str {
        ""
    }

    fn resource_id(&self) -> Cow<'_, str> {
        self.path.to_string_lossy()
    }
}

struct HtmlFile(VecDeque<Bytes>);
impl HtmlFile {
    fn new() -> HtmlFile {
        HtmlFile(Default::default())
    }

    fn ul<F>(self: &mut HtmlFile, middle: F)
    where
        F: FnOnce(&mut HtmlFile),
    {
        self.0.push_back("<ul>".as_bytes().into());
        middle(self);
        self.0.push_back("</ul>".as_bytes().into());
    }

    fn li<F>(self: &mut HtmlFile, middle: F)
    where
        F: FnOnce(&mut HtmlFile),
    {
        self.0.push_back("<li>".as_bytes().into());
        middle(self);
        self.0.push_back("</li>".as_bytes().into());
    }

    fn a(self: &mut HtmlFile, link: String, text: String) {
        self.0
            .push_back(format!("<a href=\"{link}\">{text}</a>").into_bytes().into());
    }
}
impl Response for HtmlFile {
    type Body = Chunks;

    fn status_code(&self) -> StatusCode {
        StatusCode::OK
    }

    fn into_body(self) -> Self::Body {
        Chunks(self.0)
    }
}

struct Chunks(VecDeque<Bytes>);
impl ResponseBody for Chunks {
    fn content_type(&self) -> Cow<'static, str> {
        Cow::Borrowed("text/html")
    }

    fn length(&self) -> Option<u64> {
        Some(self.0.iter().map(|chunk| chunk.len() as u64).sum())
    }
}
impl tokio::io::AsyncRead for Chunks {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        while buf.remaining() > 0 {
            let Some(first) = self.0.front_mut() else {
                break;
            };

            if first.is_empty() {
                self.0.pop_front();
                continue;
            }

            let n = first.len().min(buf.remaining());
            buf.put_slice(&first.split_to(n));
        }

        Poll::Ready(Ok(()))
    }
}

struct ReadFileBody(
    mpsc::Receiver<io::Result<BytesMut>>,
    &'static str,
    BytesMut,
    u64,
);
impl ResponseBody for ReadFileBody {
    fn content_type(&self) -> Cow<'static, str> {
        Cow::Borrowed(self.1)
    }

    fn length(&self) -> Option<u64> {
        Some(self.3)
    }
}
impl tokio::io::AsyncRead for ReadFileBody {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let mut has_written = false;
        while buf.remaining() > 0 {
            if self.2.is_empty() {
                let p = self.0.poll_recv(cx);
                return match p {
                    Poll::Ready(Some(Ok(chunk))) => {
                        self.2 = chunk;
                        continue;
                    }
                    Poll::Ready(Some(Err(e))) => Poll::Ready(Err(e)),
                    Poll::Ready(None) => Poll::Ready(Ok(())),
                    Poll::Pending if has_written => Poll::Ready(Ok(())),
                    Poll::Pending => Poll::Pending,
                };
            }

            let n = self.2.len().min(buf.remaining());
            self.3 -= n as u64;
            buf.put_slice(&self.2.split_to(n));
            has_written = true;
        }

        Poll::Ready(Ok(()))
    }
}
