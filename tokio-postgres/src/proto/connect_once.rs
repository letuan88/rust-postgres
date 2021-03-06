use futures::{try_ready, Async, Future, Poll};
use futures_cpupool::{CpuFuture, CpuPool};
use lazy_static::lazy_static;
use state_machine_future::{transition, RentToOwn, StateMachineFuture};
use std::collections::HashMap;
use std::io;
use std::net::{SocketAddr, ToSocketAddrs};
#[cfg(unix)]
use std::path::Path;
use std::vec;
use tokio_tcp::TcpStream;
#[cfg(unix)]
use tokio_uds::UnixStream;

use crate::proto::{Client, Connection, HandshakeFuture};
use crate::{Error, Socket, TlsMode};

lazy_static! {
    static ref DNS_POOL: CpuPool = futures_cpupool::Builder::new()
        .name_prefix("postgres-dns-")
        .pool_size(2)
        .create();
}

#[derive(StateMachineFuture)]
pub enum ConnectOnce<T>
where
    T: TlsMode<Socket>,
{
    #[state_machine_future(start)]
    #[cfg_attr(unix, state_machine_future(transitions(ConnectingUnix, ResolvingDns)))]
    #[cfg_attr(not(unix), state_machine_future(transitions(ConnectingTcp)))]
    Start {
        host: String,
        port: u16,
        tls_mode: T,
        params: HashMap<String, String>,
    },
    #[cfg(unix)]
    #[state_machine_future(transitions(Handshaking))]
    ConnectingUnix {
        future: tokio_uds::ConnectFuture,
        tls_mode: T,
        params: HashMap<String, String>,
    },
    #[state_machine_future(transitions(ConnectingTcp))]
    ResolvingDns {
        future: CpuFuture<vec::IntoIter<SocketAddr>, io::Error>,
        tls_mode: T,
        params: HashMap<String, String>,
    },
    #[state_machine_future(transitions(Handshaking))]
    ConnectingTcp {
        future: tokio_tcp::ConnectFuture,
        addrs: vec::IntoIter<SocketAddr>,
        tls_mode: T,
        params: HashMap<String, String>,
    },
    #[state_machine_future(transitions(Finished))]
    Handshaking { future: HandshakeFuture<Socket, T> },
    #[state_machine_future(ready)]
    Finished((Client, Connection<T::Stream>)),
    #[state_machine_future(error)]
    Failed(Error),
}

impl<T> PollConnectOnce<T> for ConnectOnce<T>
where
    T: TlsMode<Socket>,
{
    fn poll_start<'a>(state: &'a mut RentToOwn<'a, Start<T>>) -> Poll<AfterStart<T>, Error> {
        let state = state.take();

        #[cfg(unix)]
        {
            if state.host.starts_with('/') {
                let path = Path::new(&state.host).join(format!(".s.PGSQL.{}", state.port));
                transition!(ConnectingUnix {
                    future: UnixStream::connect(path),
                    tls_mode: state.tls_mode,
                    params: state.params,
                })
            }
        }

        let host = state.host;
        let port = state.port;
        transition!(ResolvingDns {
            future: DNS_POOL.spawn_fn(move || (&*host, port).to_socket_addrs()),
            tls_mode: state.tls_mode,
            params: state.params,
        })
    }

    #[cfg(unix)]
    fn poll_connecting_unix<'a>(
        state: &'a mut RentToOwn<'a, ConnectingUnix<T>>,
    ) -> Poll<AfterConnectingUnix<T>, Error> {
        let stream = try_ready!(state.future.poll().map_err(Error::connect));
        let stream = Socket::new_unix(stream);
        let state = state.take();

        transition!(Handshaking {
            future: HandshakeFuture::new(stream, state.tls_mode, state.params)
        })
    }

    fn poll_resolving_dns<'a>(
        state: &'a mut RentToOwn<'a, ResolvingDns<T>>,
    ) -> Poll<AfterResolvingDns<T>, Error> {
        let mut addrs = try_ready!(state.future.poll().map_err(Error::connect));
        let state = state.take();

        let addr = match addrs.next() {
            Some(addr) => addr,
            None => {
                return Err(Error::connect(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "resolved 0 addresses",
                )))
            }
        };

        transition!(ConnectingTcp {
            future: TcpStream::connect(&addr),
            addrs,
            tls_mode: state.tls_mode,
            params: state.params,
        })
    }

    fn poll_connecting_tcp<'a>(
        state: &'a mut RentToOwn<'a, ConnectingTcp<T>>,
    ) -> Poll<AfterConnectingTcp<T>, Error> {
        let stream = loop {
            match state.future.poll() {
                Ok(Async::Ready(stream)) => break Socket::new_tcp(stream),
                Ok(Async::NotReady) => return Ok(Async::NotReady),
                Err(e) => {
                    let addr = match state.addrs.next() {
                        Some(addr) => addr,
                        None => return Err(Error::connect(e)),
                    };
                    state.future = TcpStream::connect(&addr);
                }
            }
        };
        let state = state.take();

        transition!(Handshaking {
            future: HandshakeFuture::new(stream, state.tls_mode, state.params),
        })
    }

    fn poll_handshaking<'a>(
        state: &'a mut RentToOwn<'a, Handshaking<T>>,
    ) -> Poll<AfterHandshaking<T>, Error> {
        let r = try_ready!(state.future.poll());

        transition!(Finished(r))
    }
}

impl<T> ConnectOnceFuture<T>
where
    T: TlsMode<Socket>,
{
    pub fn new(
        host: String,
        port: u16,
        tls_mode: T,
        params: HashMap<String, String>,
    ) -> ConnectOnceFuture<T> {
        ConnectOnce::start(host, port, tls_mode, params)
    }
}
