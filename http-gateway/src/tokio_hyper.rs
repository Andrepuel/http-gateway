use std::{io, pin::Pin};
use tokio::io::{AsyncRead, AsyncWrite};

pub struct TokioHyper<S>(pub S);
impl<S> TokioHyper<S> {
    fn project(self: Pin<&mut Self>) -> Pin<&mut S> {
        unsafe { Pin::new_unchecked(&mut self.get_unchecked_mut().0) }
    }
}
impl<S: AsyncRead> hyper::rt::Read for TokioHyper<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        mut buf_hyper: hyper::rt::ReadBufCursor<'_>,
    ) -> std::task::Poll<Result<(), io::Error>> {
        let mut buf_tokio = tokio::io::ReadBuf::uninit(unsafe { buf_hyper.as_mut() });
        let poll = self.project().poll_read(cx, &mut buf_tokio);
        let n = buf_tokio.filled().len();
        unsafe {
            buf_hyper.advance(n);
        }

        poll
    }
}
impl<S: AsyncWrite> hyper::rt::Write for TokioHyper<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<Result<usize, io::Error>> {
        self.project().poll_write(cx, buf)
    }

    fn poll_flush(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), io::Error>> {
        self.project().poll_flush(cx)
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), io::Error>> {
        self.project().poll_shutdown(cx)
    }
}
