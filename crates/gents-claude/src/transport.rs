//! The native HTTP/1.1 client that sends a rendered request byte for byte:
//! rustls with `http/1.1` ALPN, a keep-alive pool, gzip with Bun's encoders and
//! decoding of compressed responses.

mod body;
mod compress;
mod conn;
mod decode;
mod error;
mod http1;
mod idle;
mod proxy;

#[cfg(test)]
mod compress_tests;
#[cfg(test)]
mod http1_tests;
#[cfg(test)]
mod proxy_tests;
#[cfg(test)]
mod tls_tests;

pub use body::Body;
pub use compress::{gzip, CompressError};
pub use error::TransportError;
pub use http1::{Client, ClientOptions, Response, Roots, Target};
pub use proxy::{proxy_from_env, Proxy, ProxyError};
