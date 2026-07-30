use std::net::{SocketAddr, ToSocketAddrs};

/// Resolve a hostname to IP addresses with Happy Eyeballs ordering:
/// IPv6 addresses first (tried immediately), then IPv4 addresses (tried
/// after a short delay by the HTTP client).  Within each family, the
/// address order from the system resolver is preserved.
///
/// Uses the blocking DNS resolver (glibc `getaddrinfo`).  This is
/// intended to be called once during download task construction, not
/// inside a hot loop.
pub fn resolve_host(host: &str, port: u16) -> Vec<SocketAddr> {
    let addr_str = if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };

    match addr_str.to_socket_addrs() {
        Ok(addrs) => {
            let mut v6: Vec<SocketAddr> = Vec::new();
            let mut v4: Vec<SocketAddr> = Vec::new();
            for addr in addrs {
                match addr {
                    SocketAddr::V6(_) => v6.push(addr),
                    SocketAddr::V4(_) => v4.push(addr),
                }
            }
            v6.extend(v4);
            v6
        }
        Err(_) => vec![],
    }
}
