use std::path::PathBuf;

#[cfg(windows)]
fn program_data_dir() -> PathBuf {
    std::env::var("PROGRAMDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(r"C:\ProgramData"))
}

#[cfg(unix)]
pub type DaemonListener = tokio::net::UnixListener;
#[cfg(windows)]
pub type DaemonListener = tokio::net::TcpListener;

#[cfg(unix)]
pub type DaemonStream = tokio::net::UnixStream;
#[cfg(windows)]
pub type DaemonStream = tokio::net::TcpStream;

#[cfg(unix)]
pub type DaemonWriteHalf = tokio::net::unix::OwnedWriteHalf;
#[cfg(windows)]
pub type DaemonWriteHalf = tokio::net::tcp::OwnedWriteHalf;

pub fn default_addr() -> String {
    if let Ok(addr) = std::env::var("RXD_SOCKET") {
        return addr;
    }
    #[cfg(unix)]
    {
        if let Ok(dir) = std::env::var("RUNTIME_DIRECTORY") {
            return PathBuf::from(dir)
                .join("zing.sock")
                .to_string_lossy()
                .to_string();
        }
        if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
            return PathBuf::from(dir)
                .join("zing.sock")
                .to_string_lossy()
                .to_string();
        }
        "/tmp/zing.sock".to_string()
    }
    #[cfg(windows)]
    {
        let port_file = program_data_dir().join("zing").join("daemon.port");
        std::fs::read_to_string(&port_file)
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|_| "127.0.0.1:0".to_string())
    }
}

pub async fn bind(addr: &str) -> std::io::Result<DaemonListener> {
    #[cfg(unix)]
    {
        let _ = tokio::fs::remove_file(addr).await;
        let listener = tokio::net::UnixListener::bind(addr)?;
        Ok(listener)
    }
    #[cfg(windows)]
    {
        let socket_addr: std::net::SocketAddr = addr
            .parse()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
        let listener = tokio::net::TcpListener::bind(socket_addr).await?;
        let actual = listener.local_addr()?;
        let port_dir = program_data_dir().join("zing");
        let _ = std::fs::create_dir_all(&port_dir);
        let port_file = port_dir.join("daemon.port");
        let _ = std::fs::write(&port_file, format!("127.0.0.1:{}", actual.port()));
        Ok(listener)
    }
}

pub fn auth_file(addr: &str) -> PathBuf {
    #[cfg(unix)]
    {
        let mut p = PathBuf::from(addr).into_os_string();
        p.push(".auth");
        PathBuf::from(p)
    }
    #[cfg(windows)]
    {
        let _ = &addr;
        program_data_dir().join("zing").join("auth.token")
    }
}

pub async fn connect(addr: &str) -> std::io::Result<DaemonStream> {
    #[cfg(unix)]
    {
        let stream = tokio::net::UnixStream::connect(addr).await?;
        Ok(stream)
    }
    #[cfg(windows)]
    {
        let socket_addr: std::net::SocketAddr = addr
            .parse()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
        let stream = tokio::net::TcpStream::connect(socket_addr).await?;
        Ok(stream)
    }
}
