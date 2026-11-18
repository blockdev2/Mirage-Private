//! The WebRTC runtime with large UDP socket buffers.
//!
//! A 1080p60 stream of a busy game peaks at several hundred packets in a
//! burst (a keyframe alone spans over a hundred). Windows gives a UDP socket
//! a small receive buffer by default, so those bursts overflowed it and the
//! packets were lost inside the machine: a third of a game stream in tests,
//! seen as frozen and smeared video. WebRTC binds its sockets itself, but
//! wraps each one through the runtime, which is where the buffers grow.

use std::fmt;
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use webrtc::runtime::{
    default_runtime, AsyncInterval, AsyncTcpListener, AsyncTcpStream, AsyncUdpSocket, JoinHandle,
    Runtime,
};

/// Receive and send buffer asked for each socket. The system may grant less.
pub const SOCKET_BUFFER: usize = 8 * 1024 * 1024;

/// The default runtime with `SOCKET_BUFFER` on every UDP socket.
pub fn media_runtime() -> Option<Arc<dyn Runtime>> {
    default_runtime().map(|inner| Arc::new(BigBuffers { inner }) as Arc<dyn Runtime>)
}

/// Best effort: a refused size keeps the system default, as before.
fn enlarge(socket: &std::net::UdpSocket) {
    let sock = socket2::SockRef::from(socket);
    let _ = sock.set_recv_buffer_size(SOCKET_BUFFER);
    let _ = sock.set_send_buffer_size(SOCKET_BUFFER);
}

struct BigBuffers {
    inner: Arc<dyn Runtime>,
}

impl fmt::Debug for BigBuffers {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("BigBuffers")
    }
}

impl Runtime for BigBuffers {
    fn spawn(&self, future: Pin<Box<dyn Future<Output = ()> + Send>>) -> Box<dyn JoinHandle> {
        self.inner.spawn(future)
    }

    fn spawn_reactor(
        &self,
        reactor_pool_size: usize,
        future: Pin<Box<dyn Future<Output = ()> + Send>>,
    ) -> Box<dyn JoinHandle> {
        self.inner.spawn_reactor(reactor_pool_size, future)
    }

    fn wrap_udp_socket(&self, socket: std::net::UdpSocket) -> io::Result<Arc<dyn AsyncUdpSocket>> {
        enlarge(&socket);
        self.inner.wrap_udp_socket(socket)
    }

    fn wrap_tcp_listener(
        &self,
        listener: std::net::TcpListener,
    ) -> io::Result<Arc<dyn AsyncTcpListener>> {
        self.inner.wrap_tcp_listener(listener)
    }

    fn connect_tcp<'a>(
        &'a self,
        remote_addr: SocketAddr,
    ) -> Pin<Box<dyn Future<Output = io::Result<Arc<dyn AsyncTcpStream>>> + Send + 'a>> {
        self.inner.connect_tcp(remote_addr)
    }

    fn resolve_host<'a>(
        &'a self,
        host: &'a str,
    ) -> Pin<Box<dyn Future<Output = io::Result<Vec<SocketAddr>>> + Send + 'a>> {
        self.inner.resolve_host(host)
    }

    fn now(&self) -> std::time::Instant {
        self.inner.now()
    }

    fn sleep(&self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + 'static>> {
        self.inner.sleep(duration)
    }

    fn interval(&self, period: Duration) -> Box<dyn AsyncInterval> {
        self.inner.interval(period)
    }

    fn block_on(&self, future: Pin<Box<dyn Future<Output = ()> + '_>>) {
        self.inner.block_on(future)
    }

    fn yield_now(&self) -> Pin<Box<dyn Future<Output = ()> + Send + 'static>> {
        self.inner.yield_now()
    }

    fn name(&self) -> &'static str {
        self.inner.name()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sockets_get_large_buffers() {
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        enlarge(&socket);
        let sock = socket2::SockRef::from(&socket);
        assert_eq!(sock.recv_buffer_size().unwrap(), SOCKET_BUFFER);
        assert_eq!(sock.send_buffer_size().unwrap(), SOCKET_BUFFER);
    }
}
