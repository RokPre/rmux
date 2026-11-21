use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use crate::{Request, Response};

pub(crate) struct SessionPaths {
    sessions: PathBuf,
    locks: PathBuf,
    pids: PathBuf,
}

pub(crate) struct SessionLock(File);

impl Drop for SessionLock {
    fn drop(&mut self) {
        unsafe { libc::flock(std::os::fd::AsRawFd::as_raw_fd(&self.0), libc::LOCK_UN) };
    }
}

impl SessionPaths {
    pub(crate) fn new() -> io::Result<Self> {
        let runtime = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(format!("/run/user/{}", unsafe { libc::geteuid() })));
        let metadata = fs::symlink_metadata(&runtime)?;
        if metadata.file_type().is_symlink()
            || !metadata.is_dir()
            || metadata.uid() != unsafe { libc::geteuid() }
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("runtime directory is unsafe: {}", runtime.display()),
            ));
        }

        let root = runtime.join("rmux-v2");
        let sessions = root.join("sessions");
        let locks = root.join("locks");
        let pids = root.join("pids");
        for path in [&root, &sessions, &locks, &pids] {
            create_private_dir(path)?;
        }
        Ok(Self {
            sessions,
            locks,
            pids,
        })
    }

    pub(crate) fn socket_path(&self, name: &str) -> PathBuf {
        self.sessions.join(format!("{name}.sock"))
    }

    pub(crate) fn pid_path(&self, name: &str) -> PathBuf {
        self.pids.join(name)
    }

    pub(crate) fn lock(&self, name: &str) -> io::Result<SessionLock> {
        let path = self.locks.join(name);
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)?;
        let result = unsafe { libc::flock(std::os::fd::AsRawFd::as_raw_fd(&file), libc::LOCK_EX) };
        if result == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(SessionLock(file))
    }

    pub(crate) fn probe(&self, name: &str) -> io::Result<bool> {
        let path = self.socket_path(name);
        let stream = match UnixStream::connect(&path) {
            Ok(stream) => stream,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound
                        | io::ErrorKind::ConnectionRefused
                        | io::ErrorKind::ConnectionReset
                        | io::ErrorKind::ConnectionAborted
                ) =>
            {
                return Ok(false);
            }
            Err(error) => return Err(error),
        };
        stream.set_read_timeout(Some(Duration::from_millis(300)))?;
        stream.set_write_timeout(Some(Duration::from_millis(300)))?;
        let mut reader = BufReader::new(stream);
        serde_json::to_writer(
            &mut reader.get_mut(),
            &Request {
                op: "ping".into(),
                name: name.into(),
                rows: 0,
                cols: 0,
                resize_step: 5,
            },
        )?;
        reader.get_mut().write_all(b"\n")?;
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => Ok(false),
            Ok(_) => match serde_json::from_str::<Response>(&line) {
                Ok(response) => Ok(response.ok && response.name == name),
                Err(_) => Ok(false),
            },
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::ConnectionReset
                        | io::ErrorKind::ConnectionAborted
                ) =>
            {
                Ok(false)
            }
            Err(error) => Err(io::Error::new(
                error.kind(),
                format!("reading session probe: {error}"),
            )),
        }
    }

    pub(crate) fn remove_stale(&self, name: &str) -> io::Result<()> {
        let socket = self.socket_path(name);
        match fs::symlink_metadata(&socket) {
            Ok(metadata) if metadata.file_type().is_socket() => fs::remove_file(socket)?,
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "session path is not a socket",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        remove_owned_file(&self.pid_path(name))
    }

    pub(crate) fn live_sessions(&self) -> io::Result<BTreeSet<String>> {
        let mut sessions = BTreeSet::new();
        for entry in fs::read_dir(&self.sessions)? {
            let entry = entry?;
            if !entry.file_type()?.is_socket() {
                continue;
            }
            let Some(filename) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Some(name) = filename.strip_suffix(".sock") else {
                continue;
            };
            if self.probe(name)? {
                sessions.insert(name.to_owned());
            }
        }
        Ok(sessions)
    }

    pub(crate) fn kill(&self, name: &str) -> io::Result<()> {
        let identity = {
            let socket = UnixStream::connect(self.socket_path(name))?;
            socket.set_read_timeout(Some(Duration::from_secs(1)))?;
            socket.set_write_timeout(Some(Duration::from_secs(1)))?;
            let mut stream = BufReader::new(socket);
            write_request(
                stream.get_mut(),
                &Request {
                    op: "identity".into(),
                    name: name.into(),
                    rows: 0,
                    cols: 0,
                    resize_step: 5,
                },
            )?;
            read_response(&mut stream).map_err(|error| {
                io::Error::new(error.kind(), format!("reading session identity: {error}"))
            })?
        };
        if !identity.ok || identity.name != name {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "session identity does not match",
            ));
        }
        let recorded = fs::read_to_string(self.pid_path(name))?;
        let mut fields = recorded.split_whitespace();
        let pid: u32 = fields
            .next()
            .and_then(|field| field.parse().ok())
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "invalid session identity")
            })?;
        let start: u64 = fields
            .next()
            .and_then(|field| field.parse().ok())
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "invalid session identity")
            })?;
        if pid != identity.pid || start != identity.start_time || process_start_time(pid)? != start
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "session identity no longer matches",
            ));
        }

        let socket = UnixStream::connect(self.socket_path(name))?;
        socket.set_read_timeout(Some(Duration::from_secs(1)))?;
        socket.set_write_timeout(Some(Duration::from_secs(1)))?;
        let mut stream = BufReader::new(socket);
        write_request(
            stream.get_mut(),
            &Request {
                op: "kill".into(),
                name: name.into(),
                rows: 0,
                cols: 0,
                resize_step: 5,
            },
        )?;
        let response = read_response(&mut stream).map_err(|error| {
            io::Error::new(error.kind(), format!("reading kill response: {error}"))
        })?;
        if !response.ok || response.pid != pid || response.start_time != start {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "session identity changed",
            ));
        }
        drop(stream);

        let deadline = Instant::now() + Duration::from_secs(5);
        while self.probe(name)? {
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "session did not stop cleanly",
                ));
            }
            thread::sleep(Duration::from_millis(10));
        }
        self.remove_stale(name)
    }

    pub(crate) fn write_pid(&self, name: &str, pid: u32, start: u64) -> io::Result<()> {
        let path = self.pid_path(name);
        let temporary = self
            .pids
            .join(format!(".{name}.{}.tmp", std::process::id()));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&temporary)?;
        writeln!(file, "{pid} {start}")?;
        file.sync_all()?;
        fs::rename(temporary, path)
    }

    pub(crate) fn remove_pid(&self, name: &str) {
        let _ = remove_owned_file(&self.pid_path(name));
    }

    pub(crate) fn remove_socket(&self, name: &str) {
        let _ = fs::remove_file(self.socket_path(name));
    }
}

pub(crate) fn process_start_time(pid: u32) -> io::Result<u64> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let fields = stat
        .rsplit_once(") ")
        .map(|(_, rest)| rest.split_whitespace().collect::<Vec<_>>())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid process stat"))?;
    fields
        .get(19)
        .and_then(|field| field.parse().ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing process start time"))
}

pub(crate) fn write_request(stream: &mut UnixStream, request: &Request) -> io::Result<()> {
    serde_json::to_writer(&mut *stream, request)?;
    stream.write_all(b"\n")
}

pub(crate) fn read_response(reader: &mut impl BufRead) -> io::Result<Response> {
    let mut line = String::new();
    reader.read_line(&mut line)?;
    if line.is_empty() || line.len() > 4096 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "invalid response",
        ));
    }
    serde_json::from_str(&line).map_err(io::Error::other)
}

fn create_private_dir(path: &Path) -> io::Result<()> {
    match fs::create_dir(path) {
        Ok(()) => fs::set_permissions(path, fs::Permissions::from_mode(0o700))?,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("unsafe session directory: {}", path.display()),
        ));
    }
    if metadata.permissions().mode() & 0o077 != 0 {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn remove_owned_file(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "unsafe session metadata file",
            ))
        }
        Ok(_) => fs::remove_file(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}
