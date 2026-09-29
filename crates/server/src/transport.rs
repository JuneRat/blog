//! Own connection tasks so draining has a real deadline. Dropping axum::serve
//! alone would leave its spawned connection tasks running.
use axum::{Router, body::Body, extract::ConnectInfo};
use hyper_util::rt::{TokioIo, TokioTimer};
use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::{TcpListener, TcpStream},
    sync::watch,
    task::JoinSet,
    time::{Instant, Sleep},
};
use tower::ServiceExt;

#[derive(Clone, Copy, Debug)]
pub struct HttpLimits {
    pub requests: interfaces::http_limits::RequestTimeouts,
    pub headers: Duration,
    pub io_idle: Duration,
    pub connection_age: Duration,
    pub shutdown: Duration,
    pub max_connections: usize,
}
impl Default for HttpLimits {
    fn default() -> Self {
        Self {
            requests: Default::default(),
            headers: Duration::from_secs(10),
            io_idle: Duration::from_secs(30),
            connection_age: Duration::from_secs(300),
            shutdown: Duration::from_secs(25),
            max_connections: 1024,
        }
    }
}

pub async fn serve(
    listener: TcpListener,
    app: Router,
    mut shutdown: watch::Receiver<bool>,
    limits: HttpLimits,
) -> Result<(), String> {
    let app = app.layer(axum::Extension(limits.requests));
    let mut tasks = JoinSet::new();
    let result = loop {
        tokio::select! {
            biased;
            _ = async { let _ = shutdown.wait_for(|closed| *closed).await; } => break Ok(()),
            Some(result) = tasks.join_next(), if !tasks.is_empty() => {
                if let Err(error) = result { tracing::warn!(%error, "HTTP 连接任务退出"); }
            }
            accepted = listener.accept(), if tasks.len() < limits.max_connections => {
                let (socket, peer) = match accepted { Ok(pair) => pair, Err(error) => break Err(format!("HTTP accept：{error}")) };
                let app = app.clone();
                let mut stopping = shutdown.clone();
                tasks.spawn(async move {
                    let service = hyper::service::service_fn(move |mut req: hyper::Request<hyper::body::Incoming>| {
                        req.extensions_mut().insert(ConnectInfo(peer));
                        app.clone().oneshot(req.map(Body::new))
                    });
                    let io = TokioIo::new(IdleIo::new(socket, limits.io_idle));
                    let mut builder = hyper::server::conn::http1::Builder::new();
                    builder.timer(TokioTimer::new()).header_read_timeout(limits.headers);
                    let connection = builder.serve_connection(io, service).with_upgrades();
                    tokio::pin!(connection);
                    let result = tokio::select! {
                        result = &mut connection => Some(result),
                        _ = tokio::time::sleep(limits.connection_age) => None,
                        _ = async { let _ = stopping.wait_for(|closed| *closed).await; } => {
                            connection.as_mut().graceful_shutdown();
                            // The owning JoinSet aborts this task if draining exceeds its budget.
                            Some(connection.await)
                        }
                    };
                    if let Some(Err(error)) = result { tracing::debug!(%error, "HTTP 连接关闭"); }
                });
            }
        }
    };
    drop(listener);
    // Reserve part of the shared shutdown budget for database close.
    let reserve = limits.shutdown / 5;
    if tokio::time::timeout(limits.shutdown - reserve, async {
        while tasks.join_next().await.is_some() {}
    })
    .await
    .is_err()
    {
        tracing::warn!(connections = tasks.len(), "HTTP 排空超时，取消剩余连接");
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
    }
    result
}

/// Bound stalled socket reads/writes, including response backpressure. Header
/// deadlines and maximum connection age additionally bound slow continuous drip.
struct IdleIo {
    socket: TcpStream,
    idle: Duration,
    read: Option<Pin<Box<Sleep>>>,
    write: Option<Pin<Box<Sleep>>>,
}
impl IdleIo {
    fn new(socket: TcpStream, idle: Duration) -> Self {
        Self {
            socket,
            idle,
            read: None,
            write: None,
        }
    }
    fn pending(
        timer: &mut Option<Pin<Box<Sleep>>>,
        idle: Duration,
        cx: &mut Context<'_>,
    ) -> Poll<io::Result<()>> {
        use std::future::Future;
        let timer =
            timer.get_or_insert_with(|| Box::pin(tokio::time::sleep_until(Instant::now() + idle)));
        if timer.as_mut().poll(cx).is_ready() {
            Poll::Ready(Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "HTTP I/O idle timeout",
            )))
        } else {
            Poll::Pending
        }
    }
}
impl AsyncRead for IdleIo {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        match Pin::new(&mut this.socket).poll_read(cx, buf) {
            Poll::Ready(value) => {
                this.read = None;
                Poll::Ready(value)
            }
            Poll::Pending => Self::pending(&mut this.read, this.idle, cx),
        }
    }
}
impl AsyncWrite for IdleIo {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        match Pin::new(&mut this.socket).poll_write(cx, buf) {
            Poll::Ready(value) => {
                this.write = None;
                Poll::Ready(value)
            }
            Poll::Pending => Self::pending(&mut this.write, this.idle, cx).map(|r| r.map(|_| 0)),
        }
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        match Pin::new(&mut this.socket).poll_flush(cx) {
            Poll::Ready(value) => {
                this.write = None;
                Poll::Ready(value)
            }
            Poll::Pending => Self::pending(&mut this.write, this.idle, cx),
        }
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        match Pin::new(&mut this.socket).poll_shutdown(cx) {
            Poll::Ready(value) => {
                this.write = None;
                Poll::Ready(value)
            }
            Poll::Pending => Self::pending(&mut this.write, this.idle, cx),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    async fn start(
        app: Router,
        limits: HttpLimits,
    ) -> (
        std::net::SocketAddr,
        watch::Sender<bool>,
        tokio::task::JoinHandle<Result<(), String>>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (shutdown, receiver) = watch::channel(false);
        let task = tokio::spawn(serve(listener, app, receiver, limits));
        (address, shutdown, task)
    }
    async fn exchange(address: std::net::SocketAddr, bytes: &[u8]) -> String {
        let mut socket = TcpStream::connect(address).await.unwrap();
        socket.write_all(bytes).await.unwrap();
        let mut response = Vec::new();
        tokio::time::timeout(Duration::from_secs(2), socket.read_to_end(&mut response))
            .await
            .unwrap()
            .unwrap();
        String::from_utf8(response).unwrap()
    }
    #[tokio::test]
    async fn headers_and_stalled_bodies_are_bounded() {
        let app = Router::new()
            .route(
                "/body",
                axum::routing::post(|_: axum::body::Bytes| async { "ok" }),
            )
            .layer(axum::middleware::from_fn(
                interfaces::http_support::request_context,
            ));
        let limits = HttpLimits {
            headers: Duration::from_millis(50),
            requests: interfaces::http_limits::RequestTimeouts {
                request: Duration::from_millis(80),
                upload: Duration::from_millis(160),
            },
            ..Default::default()
        };
        let (address, shutdown, task) = start(app, limits).await;
        let header = exchange(address, b"GET / HTTP/1.1\r\nHost: localhost\r\nX-Stalled: ").await;
        assert!(!header.contains("200 OK"));
        let body = exchange(address, b"POST /body HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Length: 20\r\n\r\nx").await;
        assert!(body.contains("408 Request Timeout"), "{body}");
        assert!(body.contains("x-request-id") && body.contains("request_timeout"));
        shutdown.send_replace(true);
        task.await.unwrap().unwrap();
    }
    #[tokio::test]
    async fn uploads_have_their_own_request_budget() {
        async fn slow() -> &'static str {
            tokio::time::sleep(Duration::from_millis(80)).await;
            "ok"
        }
        let app = Router::new()
            .route("/ordinary", axum::routing::post(slow))
            .route("/api/admin/v1/media", axum::routing::post(slow))
            .layer(axum::middleware::from_fn(
                interfaces::http_support::request_context,
            ));
        let limits = HttpLimits {
            requests: interfaces::http_limits::RequestTimeouts {
                request: Duration::from_millis(30),
                upload: Duration::from_millis(200),
            },
            ..Default::default()
        };
        let (address, shutdown, task) = start(app, limits).await;
        for (path, code) in [
            ("/ordinary", "408 Request Timeout"),
            ("/api/admin/v1/media", "200 OK"),
        ] {
            let response = exchange(address, format!("POST {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Length: 0\r\n\r\n").as_bytes()).await;
            assert!(response.contains(code), "{response}");
        }
        shutdown.send_replace(true);
        task.await.unwrap().unwrap();
    }
    #[tokio::test]
    async fn shutdown_actually_cancels_hanging_handlers() {
        struct Guard(Arc<AtomicBool>);
        impl Drop for Guard {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let dropped = Arc::new(AtomicBool::new(false));
        let started = Arc::new(tokio::sync::Notify::new());
        let (flag, signal) = (dropped.clone(), started.clone());
        let app = Router::new().route(
            "/",
            axum::routing::get(move || {
                let (flag, signal) = (flag.clone(), signal.clone());
                async move {
                    let _guard = Guard(flag);
                    signal.notify_one();
                    std::future::pending::<String>().await
                }
            }),
        );
        let (address, shutdown, task) = start(
            app,
            HttpLimits {
                shutdown: Duration::from_millis(150),
                ..Default::default()
            },
        )
        .await;
        let mut socket = TcpStream::connect(address).await.unwrap();
        socket
            .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), started.notified())
            .await
            .unwrap();
        shutdown.send_replace(true);
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(dropped.load(Ordering::SeqCst));
        assert!(TcpStream::connect(address).await.is_err());
    }
}
