use std::io::{BufRead, BufReader, Write};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

/// Bumped whenever the wire format changes in an incompatible way.
pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    Ping,
    Status,
    Toggle,
    SetEnabled { enabled: bool },
    Reload,
    CaptureKey,
    Stop,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope {
    pub version: u32,
    pub request: Request,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Response {
    #[serde(default)]
    pub version: u32,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<StatusInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

impl Response {
    pub fn ok() -> Self {
        Self {
            version: PROTOCOL_VERSION,
            ok: true,
            ..Self::default()
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            ok: false,
            error: Some(message.into()),
            ..Self::default()
        }
    }

    pub fn with_status(status: StatusInfo) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            ok: true,
            status: Some(status),
            ..Self::default()
        }
    }

    pub fn with_key(key: Option<String>) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            ok: true,
            key,
            ..Self::default()
        }
    }
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusInfo {
    pub version: String,
    pub enabled: bool,
    pub elevated: bool,
    pub sticky: bool,
    pub toggle: Option<String>,
    pub groups: Vec<[String; 2]>,
    pub tray: bool,
    pub config_path: String,
}

pub fn read_request(reader: &mut impl BufRead) -> Result<Option<Envelope>> {
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Ok(None);
    }
    Ok(Some(
        serde_json::from_str(&line).context("invalid request from the client")?,
    ))
}

pub fn write_response(writer: &mut impl Write, response: &Response) -> Result<()> {
    let mut line = serde_json::to_string(response)?;
    line.push('\n');
    writer.write_all(line.as_bytes())?;
    writer.flush()?;
    Ok(())
}

const CLIENT_TIMEOUT: Duration = Duration::from_secs(10);

pub struct Client {
    reader: BufReader<transport::Stream>,
}

impl Client {
    pub fn connect(endpoint: Option<&str>) -> Result<Self> {
        Self::connect_with_timeout(endpoint, CLIENT_TIMEOUT)
    }

    pub fn connect_with_timeout(endpoint: Option<&str>, timeout: Duration) -> Result<Self> {
        let mut stream = transport::connect(endpoint)
            .context("failed to connect to the patpans daemon (is it running?)")?;
        stream
            .set_read_timeout(Some(timeout))
            .context("failed to arm the IPC read timeout")?;
        stream
            .set_write_timeout(Some(timeout))
            .context("failed to arm the IPC write timeout")?;
        Ok(Self {
            reader: BufReader::new(stream),
        })
    }

    pub fn request(&mut self, request: &Request) -> Result<Response> {
        let envelope = Envelope {
            version: PROTOCOL_VERSION,
            request: request.clone(),
        };
        let mut line = serde_json::to_string(&envelope)?;
        line.push('\n');
        self.reader.get_mut().write_all(line.as_bytes())?;
        self.reader.get_mut().flush()?;
        let mut response = String::new();
        if self.reader.read_line(&mut response)? == 0 {
            bail!("the patpans daemon closed the connection");
        }
        let response: Response =
            serde_json::from_str(&response).context("invalid response from the daemon")?;
        if response.version != PROTOCOL_VERSION {
            bail!(
                "protocol version mismatch: the daemon speaks v{} and this client speaks v{PROTOCOL_VERSION}",
                response.version
            );
        }
        Ok(response)
    }
}

#[cfg(target_os = "linux")]
pub mod transport {
    use std::fs;
    use std::io::{self, Read, Write};
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use nix::unistd::Uid;

    fn resolve(explicit: Option<&str>) -> (PathBuf, Option<PathBuf>) {
        if let Some(path) = explicit {
            return (PathBuf::from(path), None);
        }
        if let Ok(path) = std::env::var("PAT_PANS_SOCKET") {
            return (PathBuf::from(path), None);
        }
        let uid = Uid::effective().as_raw();
        if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") {
            let dir = PathBuf::from(dir);
            return (dir.join("patpans.sock"), None);
        }
        let runtime = PathBuf::from(format!("/run/user/{uid}"));
        if runtime.is_dir() {
            return (runtime.join("patpans.sock"), None);
        }
        let fallback = std::env::temp_dir().join(format!("patpans-{uid}"));
        (fallback.join("patpans.sock"), Some(fallback))
    }

    fn private_dir(path: &Path) -> io::Result<()> {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt};

        match fs::symlink_metadata(path) {
            Ok(meta) => {
                if !meta.file_type().is_dir() || meta.uid() != Uid::effective().as_raw() {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "the patpans socket directory is not safe to use",
                    ));
                }
                Ok(())
            }
            Err(_) => fs::DirBuilder::new().mode(0o700).create(path),
        }
    }

    pub struct Listener {
        listener: UnixListener,
        path: PathBuf,
    }

    pub struct Stream(UnixStream);

    pub fn listen(explicit: Option<&str>) -> io::Result<Listener> {
        let (path, managed_dir) = resolve(explicit);
        if let Some(dir) = managed_dir {
            private_dir(&dir)?;
        }
        if path.exists() {
            if UnixStream::connect(&path).is_ok() {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    "another patpans daemon is already running",
                ));
            }
            let _ = fs::remove_file(&path);
        }
        let listener = UnixListener::bind(&path)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        Ok(Listener { listener, path })
    }

    impl Listener {
        pub fn accept(&self) -> io::Result<Stream> {
            self.listener.accept().map(|(stream, _)| Stream(stream))
        }

        /// Connects to this listener to unblock a pending `accept`.
        pub fn wake(&self) -> io::Result<()> {
            UnixStream::connect(&self.path).map(drop)
        }
    }

    impl Drop for Listener {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.path);
        }
    }

    pub fn connect(explicit: Option<&str>) -> io::Result<Stream> {
        UnixStream::connect(resolve(explicit).0).map(Stream)
    }

    impl Stream {
        pub fn set_read_timeout(&mut self, timeout: Option<Duration>) -> io::Result<()> {
            self.0.set_read_timeout(timeout)
        }

        pub fn set_write_timeout(&mut self, timeout: Option<Duration>) -> io::Result<()> {
            self.0.set_write_timeout(timeout)
        }
    }

    impl Read for Stream {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.0.read(buf)
        }
    }

    impl Write for Stream {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.write(buf)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.0.flush()
        }
    }
}

#[cfg(windows)]
pub mod transport {
    use std::io::{self, Read, Write};
    use std::thread;
    use std::time::{Duration, Instant};

    use interprocess::local_socket::{
        GenericNamespaced, ListenerOptions, Stream as IpcStream, prelude::*,
    };

    const POLL_INTERVAL: Duration = Duration::from_millis(5);

    fn endpoint(explicit: Option<&str>) -> String {
        explicit.map_or_else(
            || std::env::var("PAT_PANS_PIPE").unwrap_or_else(|_| "patpans".to_string()),
            str::to_owned,
        )
    }

    pub struct Listener {
        listener: interprocess::local_socket::Listener,
        name: String,
    }

    /// Named pipes do not support I/O timeouts, so the stream is nonblocking
    /// underneath and `Read`/`Write` retry with a deadline instead.
    pub struct Stream {
        inner: IpcStream,
        timeout: Option<Duration>,
    }

    pub fn listen(explicit: Option<&str>) -> io::Result<Listener> {
        let name = endpoint(explicit);
        let listener = ListenerOptions::new()
            .name(name.as_str().to_ns_name::<GenericNamespaced>()?)
            .create_sync()?;
        Ok(Listener { listener, name })
    }

    impl Listener {
        pub fn accept(&self) -> io::Result<Stream> {
            self.listener.accept().map(Stream::new)
        }

        /// Connects to this listener to unblock a pending `accept`.
        pub fn wake(&self) -> io::Result<()> {
            let name = self.name.as_str().to_ns_name::<GenericNamespaced>()?;
            IpcStream::connect(name).map(drop)
        }
    }

    pub fn connect(explicit: Option<&str>) -> io::Result<Stream> {
        let name = endpoint(explicit);
        let name = name.as_str().to_ns_name::<GenericNamespaced>()?;
        IpcStream::connect(name).map(Stream::new)
    }

    impl Stream {
        fn new(inner: IpcStream) -> Self {
            let _ = inner.set_nonblocking(true);
            Self {
                inner,
                timeout: None,
            }
        }

        pub fn set_read_timeout(&mut self, timeout: Option<Duration>) -> io::Result<()> {
            self.timeout = timeout;
            Ok(())
        }

        pub fn set_write_timeout(&mut self, timeout: Option<Duration>) -> io::Result<()> {
            self.timeout = timeout;
            Ok(())
        }

        fn deadline(&self) -> Option<Instant> {
            self.timeout
                .and_then(|timeout| Instant::now().checked_add(timeout))
        }
    }

    fn wait(deadline: Option<Instant>, what: &str) -> io::Result<()> {
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("the patpans IPC {what} timed out"),
            ));
        }
        thread::sleep(POLL_INTERVAL);
        Ok(())
    }

    impl Read for Stream {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let deadline = self.deadline();
            loop {
                match self.inner.read(buf) {
                    Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                        wait(deadline, "read")?;
                    }
                    other => return other,
                }
            }
        }
    }

    impl Write for Stream {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let deadline = self.deadline();
            loop {
                match self.inner.write(buf) {
                    Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                        wait(deadline, "write")?;
                    }
                    Ok(0) if !buf.is_empty() => {
                        wait(deadline, "write")?;
                    }
                    other => return other,
                }
            }
        }

        fn flush(&mut self) -> io::Result<()> {
            self.inner.flush()
        }
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
pub mod transport {
    use std::io::{self, Read, Write};
    use std::time::Duration;

    fn unsupported() -> io::Error {
        io::Error::new(
            io::ErrorKind::Unsupported,
            "the patpans daemon IPC is supported on Linux and Windows only",
        )
    }

    pub struct Listener;

    pub struct Stream;

    pub fn listen(_explicit: Option<&str>) -> io::Result<Listener> {
        Err(unsupported())
    }

    pub fn connect(_explicit: Option<&str>) -> io::Result<Stream> {
        Err(unsupported())
    }

    impl Listener {
        pub fn accept(&self) -> io::Result<Stream> {
            Err(unsupported())
        }

        pub fn wake(&self) -> io::Result<()> {
            Err(unsupported())
        }
    }

    impl Stream {
        pub fn set_read_timeout(&mut self, _timeout: Option<Duration>) -> io::Result<()> {
            Err(unsupported())
        }

        pub fn set_write_timeout(&mut self, _timeout: Option<Duration>) -> io::Result<()> {
            Err(unsupported())
        }
    }

    impl Read for Stream {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            Err(unsupported())
        }
    }

    impl Write for Stream {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(unsupported())
        }

        fn flush(&mut self) -> io::Result<()> {
            Err(unsupported())
        }
    }
}
