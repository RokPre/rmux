use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::runtime::{SessionPaths, process_start_time};
use crate::{Request, Response};

pub(crate) const INPUT: u8 = 1;
pub(crate) const RESIZE: u8 = 2;
const DETACH: u8 = 3;
pub(crate) const OUTPUT: u8 = 1;
pub(crate) const CLOSE: u8 = 2;
const MAX_FRAME: usize = 1024 * 1024;

enum Outgoing {
    Bytes(Vec<u8>),
    Close,
}

struct Client {
    sender: SyncSender<Outgoing>,
    stream: UnixStream,
    rows: u16,
    cols: u16,
}

struct Session {
    name: String,
    clients: Mutex<HashMap<u64, Client>>,
    next_client: AtomicU64,
    writer: Mutex<Box<dyn Write + Send>>,
    master: Mutex<Box<dyn MasterPty + Send>>,
    shutdown: AtomicBool,
    pty_eof: AtomicBool,
}

pub(crate) fn run_server(args: &[std::ffi::OsString]) -> io::Result<()> {
    let (name, socket_path, size, cwd, command) = parse_server_args(args)?;
    let paths = SessionPaths::new()?;
    if socket_path != paths.socket_path(&name) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid session socket path",
        ));
    }

    let pty_system = native_pty_system();
    let pair = pty_system.openpty(size).map_err(io::Error::other)?;
    let mut command_builder = CommandBuilder::from_argv(command);
    if let Some(cwd) = cwd {
        command_builder.cwd(cwd);
    }
    let mut child = pair
        .slave
        .spawn_command(command_builder)
        .map_err(io::Error::other)?;
    drop(pair.slave);
    let master = pair.master;
    let process_group = master.process_group_leader();
    let writer = master.take_writer().map_err(io::Error::other)?;
    let reader = master.try_clone_reader().map_err(io::Error::other)?;

    let pid = std::process::id();
    let start_time = process_start_time(pid)?;
    paths.write_pid(&name, pid, start_time)?;

    let listener = match UnixListener::bind(&socket_path) {
        Ok(listener) => listener,
        Err(error) => {
            paths.remove_pid(&name);
            terminate_child(child.as_mut(), process_group);
            return Err(error);
        }
    };
    fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;

    let session = Arc::new(Session {
        name: name.clone(),
        clients: Mutex::new(HashMap::new()),
        next_client: AtomicU64::new(1),
        writer: Mutex::new(writer),
        master: Mutex::new(master),
        shutdown: AtomicBool::new(false),
        pty_eof: AtomicBool::new(false),
    });
    spawn_output_reader(reader, Arc::clone(&session));

    loop {
        if session.shutdown.load(Ordering::Acquire) || session.pty_eof.load(Ordering::Acquire) {
            terminate_child(child.as_mut(), process_group);
            break;
        }
        if child.try_wait()?.is_some() {
            break;
        }

        match listener.accept() {
            Ok((stream, _)) => {
                let session = Arc::clone(&session);
                thread::spawn(move || {
                    let _ = handle_client(stream, session);
                });
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => return Err(error),
        }
    }

    close_clients(&session);
    paths.remove_socket(&name);
    paths.remove_pid(&name);
    Ok(())
}

fn parse_server_args(
    args: &[OsString],
) -> io::Result<(String, PathBuf, PtySize, Option<PathBuf>, Vec<OsString>)> {
    if args.len() < 5 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid server arguments",
        ));
    }
    let name = args[0]
        .to_str()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid session name"))?
        .to_owned();
    let socket_path = PathBuf::from(&args[1]);
    let rows = parse_dimension(&args[2])?;
    let cols = parse_dimension(&args[3])?;
    let mut index = 4;
    let mut cwd = None;
    if args.get(index).is_some_and(|arg| arg == "--cwd") {
        cwd = Some(PathBuf::from(args.get(index + 1).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "missing working directory")
        })?));
        index += 2;
    }
    if args.get(index).is_none_or(|arg| arg != "--") || index + 1 >= args.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "missing terminal command",
        ));
    }
    Ok((
        name,
        socket_path,
        PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        },
        cwd,
        args[index + 1..].to_vec(),
    ))
}

fn parse_dimension(value: &std::ffi::OsStr) -> io::Result<u16> {
    value
        .to_str()
        .and_then(|value| value.parse().ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid terminal size"))
}

fn spawn_output_reader(mut reader: Box<dyn Read + Send>, session: Arc<Session>) {
    thread::spawn(move || {
        let mut buffer = [0u8; 16 * 1024];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(length) => broadcast(&session, Outgoing::Bytes(buffer[..length].to_vec())),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        session.pty_eof.store(true, Ordering::Release);
        broadcast(&session, Outgoing::Close);
    });
}

fn broadcast(session: &Session, message: Outgoing) {
    let mut clients = session
        .clients
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let mut disconnected = Vec::new();
    for (&id, client) in clients.iter() {
        let result = match &message {
            Outgoing::Bytes(bytes) => client.sender.try_send(Outgoing::Bytes(bytes.clone())),
            Outgoing::Close => client.sender.try_send(Outgoing::Close),
        };
        match result {
            Ok(()) | Err(TrySendError::Disconnected(_)) => {}
            Err(TrySendError::Full(_)) => disconnected.push(id),
        }
    }
    for id in disconnected {
        if let Some(client) = clients.remove(&id) {
            let _ = client.stream.shutdown(std::net::Shutdown::Both);
        }
    }
}

fn handle_client(stream: UnixStream, session: Arc<Session>) -> io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let request = read_request(&mut reader)?;
    match request.op.as_str() {
        "ping" | "identity" => {
            write_response(&stream, &identity_response(&session.name))?;
        }
        "kill" => {
            write_response(&stream, &identity_response(&session.name))?;
            reader
                .get_ref()
                .set_read_timeout(Some(Duration::from_secs(1)))?;
            let mut closing = [0];
            let _ = reader.read(&mut closing);
            session.shutdown.store(true, Ordering::Release);
        }
        "attach" => {
            if request.name != session.name {
                write_response(&stream, &error_response("session name does not match"))?;
                return Ok(());
            }
            reader.get_ref().set_read_timeout(None)?;
            attach_client(stream, reader, session, request)?;
        }
        _ => write_response(&stream, &error_response("unknown request"))?,
    }
    Ok(())
}

fn attach_client(
    stream: UnixStream,
    mut reader: BufReader<UnixStream>,
    session: Arc<Session>,
    request: Request,
) -> io::Result<()> {
    let rows = request.rows.max(1);
    let cols = request.cols.max(1);
    let id = session.next_client.fetch_add(1, Ordering::Relaxed);
    let (sender, receiver) = mpsc::sync_channel(64);
    let output_stream = stream.try_clone()?;
    write_response(&stream, &identity_response(&session.name))?;

    {
        let mut clients = session
            .clients
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        clients.insert(
            id,
            Client {
                sender,
                stream: stream.try_clone()?,
                rows,
                cols,
            },
        );
    }
    resize_for_clients(&session);
    thread::spawn(move || output_loop(output_stream, receiver));

    loop {
        let Some((kind, payload)) = read_frame(&mut reader)? else {
            break;
        };
        match kind {
            INPUT => {
                let mut writer = session
                    .writer
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner());
                if writer.write_all(&payload).is_err() {
                    break;
                }
                let _ = writer.flush();
            }
            RESIZE if payload.len() == 4 => {
                let rows = u16::from_be_bytes([payload[0], payload[1]]).max(1);
                let cols = u16::from_be_bytes([payload[2], payload[3]]).max(1);
                if let Some(client) = session
                    .clients
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner())
                    .get_mut(&id)
                {
                    client.rows = rows;
                    client.cols = cols;
                }
                resize_for_clients(&session);
            }
            DETACH => break,
            _ => break,
        }
    }

    if let Some(client) = session
        .clients
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .remove(&id)
    {
        let _ = client.stream.shutdown(std::net::Shutdown::Both);
    }
    resize_for_clients(&session);
    Ok(())
}

fn output_loop(mut stream: UnixStream, receiver: mpsc::Receiver<Outgoing>) {
    while let Ok(message) = receiver.recv() {
        match message {
            Outgoing::Bytes(bytes) => {
                if write_frame(&mut stream, OUTPUT, &bytes).is_err() {
                    break;
                }
            }
            Outgoing::Close => {
                let _ = write_frame(&mut stream, CLOSE, &[]);
                break;
            }
        }
    }
    let _ = stream.shutdown(std::net::Shutdown::Both);
}

fn resize_for_clients(session: &Session) {
    let size = {
        let clients = session
            .clients
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        clients.values().fold(None::<(u16, u16)>, |size, client| {
            Some(match size {
                None => (client.rows, client.cols),
                Some((rows, cols)) => (rows.min(client.rows), cols.min(client.cols)),
            })
        })
    };
    let Some((rows, cols)) = size else { return };
    let master = session
        .master
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let current = master.get_size().ok();
    if current.is_some_and(|current| current.rows == rows && current.cols == cols) {
        return;
    }
    let _ = master.resize(PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    });
}

fn close_clients(session: &Session) {
    let mut clients = session
        .clients
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    for (_, client) in clients.drain() {
        let _ = client.sender.try_send(Outgoing::Close);
        let _ = client.stream.shutdown(std::net::Shutdown::Both);
    }
}

fn terminate_child(child: &mut dyn portable_pty::Child, process_group: Option<libc::pid_t>) {
    let killed = if let Some(group) =
        process_group.filter(|group| *group > 1 && *group != unsafe { libc::getpgrp() })
    {
        unsafe { libc::kill(-group, libc::SIGKILL) == 0 }
    } else {
        false
    };
    if !killed {
        let _ = child.kill();
    }
    let _ = child.wait();
}

fn read_request(reader: &mut impl BufRead) -> io::Result<Request> {
    let mut line = String::new();
    reader.read_line(&mut line)?;
    if line.is_empty() || line.len() > 4096 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid request",
        ));
    }
    serde_json::from_str(&line).map_err(io::Error::other)
}

fn write_response(stream: &UnixStream, response: &Response) -> io::Result<()> {
    serde_json::to_writer(&mut &*stream, response)?;
    (&*stream).write_all(b"\n")
}

fn identity_response(name: &str) -> Response {
    Response {
        ok: true,
        error: String::new(),
        name: name.into(),
        pid: std::process::id(),
        start_time: process_start_time(std::process::id()).unwrap_or_default(),
    }
}

fn error_response(error: &str) -> Response {
    Response {
        ok: false,
        error: error.into(),
        name: String::new(),
        pid: 0,
        start_time: 0,
    }
}

pub(crate) fn write_frame(writer: &mut impl Write, kind: u8, payload: &[u8]) -> io::Result<()> {
    let length = u32::try_from(payload.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "frame is too large"))?;
    writer.write_all(&[kind])?;
    writer.write_all(&length.to_be_bytes())?;
    writer.write_all(payload)
}

pub(crate) fn read_frame(reader: &mut impl Read) -> io::Result<Option<(u8, Vec<u8>)>> {
    let mut header = [0; 5];
    match reader.read_exact(&mut header) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error),
    }
    let length = u32::from_be_bytes(header[1..5].try_into().unwrap()) as usize;
    if length > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame is too large",
        ));
    }
    let mut payload = vec![0; length];
    reader.read_exact(&mut payload)?;
    Ok(Some((header[0], payload)))
}
