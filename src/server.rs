use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::num::NonZeroU16;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
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
pub(crate) const NEW_BUFFER: u8 = 4;
pub(crate) const CLOSE_BUFFER: u8 = 5;
pub(crate) const PREVIOUS_BUFFER: u8 = 6;
pub(crate) const NEXT_BUFFER: u8 = 7;
pub(crate) const OUTPUT: u8 = 1;
pub(crate) const CLOSE: u8 = 2;
const MAX_FRAME: usize = 1024 * 1024;

pub(crate) enum Outgoing {
    Bytes(Vec<u8>),
    Close,
}

struct Client {
    sender: SyncSender<Outgoing>,
    stream: UnixStream,
    rows: u16,
    cols: u16,
}

struct Buffer {
    writer: Box<dyn Write + Send>,
    master: Box<dyn MasterPty + Send>,
    parser: vt100::Parser,
    child: Box<dyn portable_pty::Child>,
    session_id: Option<libc::pid_t>,
}

struct SessionState {
    clients: HashMap<u64, Client>,
    buffers: HashMap<u64, Buffer>,
    order: Vec<u64>,
    active: u64,
    next_buffer: u64,
}

struct Session {
    name: String,
    cwd: PathBuf,
    state: Mutex<SessionState>,
    next_client: AtomicU64,
    shutdown: std::sync::atomic::AtomicBool,
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

    let cwd = match cwd {
        Some(cwd) => cwd,
        None => std::env::current_dir()?,
    };
    let (mut initial_buffer, reader) = spawn_buffer(command, &cwd, size)?;
    let pid = std::process::id();
    let start_time = process_start_time(pid)?;
    paths.write_pid(&name, pid, start_time)?;

    let listener = match UnixListener::bind(&socket_path) {
        Ok(listener) => listener,
        Err(error) => {
            paths.remove_pid(&name);
            terminate_child(initial_buffer.child.as_mut(), initial_buffer.session_id);
            return Err(error);
        }
    };
    fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;

    let session = Arc::new(Session {
        name: name.clone(),
        cwd,
        state: Mutex::new(SessionState {
            clients: HashMap::new(),
            buffers: HashMap::from([(1, initial_buffer)]),
            order: vec![1],
            active: 1,
            next_buffer: 2,
        }),
        next_client: AtomicU64::new(1),
        shutdown: std::sync::atomic::AtomicBool::new(false),
    });
    spawn_output_reader(reader, Arc::clone(&session), 1);

    loop {
        if session.shutdown.load(Ordering::Acquire) || reap_exited_buffers(&session)? {
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

    {
        let mut state = lock_state(&session);
        for buffer in state.buffers.values_mut() {
            terminate_child(buffer.child.as_mut(), buffer.session_id);
        }
        state.buffers.clear();
        state.order.clear();
        close_clients(&mut state);
    }
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

fn spawn_buffer(
    command: Vec<OsString>,
    cwd: &Path,
    size: PtySize,
) -> io::Result<(Buffer, Box<dyn Read + Send>)> {
    let pair = native_pty_system()
        .openpty(size)
        .map_err(io::Error::other)?;
    let mut command_builder = CommandBuilder::from_argv(command);
    command_builder.cwd(cwd);
    let child = pair
        .slave
        .spawn_command(command_builder)
        .map_err(io::Error::other)?;
    drop(pair.slave);
    let master = pair.master;
    let session_id = child.process_id().map(|pid| pid as libc::pid_t);
    let writer = master.take_writer().map_err(io::Error::other)?;
    let reader = master.try_clone_reader().map_err(io::Error::other)?;
    let parser = vt100::Parser::new(
        NonZeroU16::new(size.rows).unwrap_or(NonZeroU16::MIN),
        NonZeroU16::new(size.cols).unwrap_or(NonZeroU16::MIN),
        1000,
    );
    Ok((
        Buffer {
            writer,
            master,
            parser,
            child,
            session_id,
        },
        reader,
    ))
}
fn spawn_output_reader(mut reader: Box<dyn Read + Send>, session: Arc<Session>, buffer_id: u64) {
    thread::spawn(move || {
        let mut output = [0u8; 16 * 1024];
        loop {
            match reader.read(&mut output) {
                Ok(0) => break,
                Ok(length) => {
                    let mut state = lock_state(&session);
                    if let Some(buffer) = state.buffers.get_mut(&buffer_id) {
                        buffer.parser.process(&output[..length]);
                        if state.active == buffer_id {
                            broadcast(&mut state, Outgoing::Bytes(output[..length].to_vec()));
                        }
                    } else {
                        break;
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
    });
}

fn lock_state(session: &Session) -> std::sync::MutexGuard<'_, SessionState> {
    session
        .state
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
}

fn broadcast(state: &mut SessionState, message: Outgoing) {
    let mut disconnected = Vec::new();
    for (&id, client) in &state.clients {
        let result = match &message {
            Outgoing::Bytes(bytes) => client.sender.try_send(Outgoing::Bytes(bytes.clone())),
            Outgoing::Close => client.sender.try_send(Outgoing::Close),
        };
        match result {
            Ok(()) | Err(TrySendError::Disconnected(_)) => {}
            Err(TrySendError::Full(_)) => disconnected.push(id),
        }
    }
    let removed_clients = !disconnected.is_empty();
    for id in disconnected {
        if let Some(client) = state.clients.remove(&id) {
            let _ = client.stream.shutdown(std::net::Shutdown::Both);
        }
    }
    if removed_clients {
        resize_active(state);
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
        let mut state = lock_state(&session);
        state.clients.insert(
            id,
            Client {
                sender,
                stream: stream.try_clone()?,
                rows,
                cols,
            },
        );
        resize_active(&mut state);
        send_snapshot(&mut state);
    }
    thread::spawn(move || output_loop(output_stream, receiver));

    loop {
        let Some((kind, payload)) = read_frame(&mut reader)? else {
            break;
        };
        match kind {
            INPUT => {
                let mut state = lock_state(&session);
                let active = state.active;
                if let Some(buffer) = state.buffers.get_mut(&active) {
                    if buffer.writer.write_all(&payload).is_err() {
                        break;
                    }
                    let _ = buffer.writer.flush();
                }
            }
            RESIZE if payload.len() == 4 => {
                let mut state = lock_state(&session);
                if let Some(client) = state.clients.get_mut(&id) {
                    client.rows = u16::from_be_bytes([payload[0], payload[1]]).max(1);
                    client.cols = u16::from_be_bytes([payload[2], payload[3]]).max(1);
                }
                resize_active(&mut state);
            }
            NEW_BUFFER if payload.is_empty() => create_buffer(&session),
            CLOSE_BUFFER if payload.is_empty() => close_active_buffer(&session),
            PREVIOUS_BUFFER if payload.is_empty() => switch_buffer(&session, false),
            NEXT_BUFFER if payload.is_empty() => switch_buffer(&session, true),
            DETACH => break,
            _ => break,
        }
    }

    {
        let mut state = lock_state(&session);
        if let Some(client) = state.clients.remove(&id) {
            let _ = client.stream.shutdown(std::net::Shutdown::Both);
        }
        resize_active(&mut state);
    }
    Ok(())
}

fn create_buffer(session: &Arc<Session>) {
    let (id, size) = {
        let mut state = lock_state(session);
        let id = state.next_buffer;
        state.next_buffer = id.wrapping_add(1).max(1);
        let size = current_size(&state).unwrap_or_else(|| {
            state
                .buffers
                .get(&state.active)
                .and_then(|buffer| buffer.master.get_size().ok())
                .unwrap_or(PtySize {
                    rows: 24,
                    cols: 80,
                    pixel_width: 0,
                    pixel_height: 0,
                })
        });
        (id, size)
    };
    let shell = std::env::var_os("SHELL").unwrap_or_else(|| OsString::from("/bin/sh"));
    let (buffer, reader) = match spawn_buffer(vec![shell, OsString::from("-i")], &session.cwd, size)
    {
        Ok(buffer) => buffer,
        Err(_) => return,
    };
    {
        let mut state = lock_state(session);
        state.order.push(id);
        state.buffers.insert(id, buffer);
        state.active = id;
        resize_active(&mut state);
        send_snapshot(&mut state);
    }
    spawn_output_reader(reader, Arc::clone(session), id);
}

fn close_active_buffer(session: &Session) {
    let mut state = lock_state(session);
    if state.order.len() == 1 {
        let active = state.active;
        if let Some(mut buffer) = state.buffers.remove(&active) {
            terminate_child(buffer.child.as_mut(), buffer.session_id);
        }
        state.order.clear();
        close_clients(&mut state);
        session.shutdown.store(true, Ordering::Release);
        return;
    }
    let old_active = state.active;
    let index = state
        .order
        .iter()
        .position(|id| *id == old_active)
        .unwrap_or(0);
    state.order.remove(index);
    if let Some(mut buffer) = state.buffers.remove(&old_active) {
        terminate_child(buffer.child.as_mut(), buffer.session_id);
    }
    state.active = state.order[index.min(state.order.len() - 1)];
    resize_active(&mut state);
    send_snapshot(&mut state);
}

fn switch_buffer(session: &Session, next: bool) {
    let mut state = lock_state(session);
    if state.order.len() < 2 {
        return;
    }
    let index = state
        .order
        .iter()
        .position(|id| *id == state.active)
        .unwrap_or(0);
    let new_index = if next {
        (index + 1) % state.order.len()
    } else {
        (index + state.order.len() - 1) % state.order.len()
    };
    state.active = state.order[new_index];
    resize_active(&mut state);
    send_snapshot(&mut state);
}

fn send_snapshot(state: &mut SessionState) {
    let snapshot = state
        .buffers
        .get(&state.active)
        .map(|buffer| buffer.parser.screen().contents_formatted().into_bytes());
    if let Some(snapshot) = snapshot {
        for chunk in snapshot.chunks(MAX_FRAME) {
            broadcast(state, Outgoing::Bytes(chunk.to_vec()));
        }
    }
}

fn current_size(state: &SessionState) -> Option<PtySize> {
    state
        .clients
        .values()
        .fold(None::<(u16, u16)>, |size, client| {
            Some(match size {
                None => (client.rows, client.cols),
                Some((rows, cols)) => (rows.min(client.rows), cols.min(client.cols)),
            })
        })
        .map(|(rows, cols)| PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
}

fn resize_active(state: &mut SessionState) {
    let Some(size) = current_size(state) else {
        return;
    };
    let active = state.active;
    if let Some(buffer) = state.buffers.get_mut(&active) {
        let current = buffer.master.get_size().ok();
        if !current.is_some_and(|current| current.rows == size.rows && current.cols == size.cols) {
            let _ = buffer.master.resize(size);
        }
        buffer.parser.set_size(
            NonZeroU16::new(size.rows).unwrap_or(NonZeroU16::MIN),
            NonZeroU16::new(size.cols).unwrap_or(NonZeroU16::MIN),
        );
    }
}

fn reap_exited_buffers(session: &Session) -> io::Result<bool> {
    let mut state = lock_state(session);
    let mut exited = Vec::new();
    for (&id, buffer) in &mut state.buffers {
        if buffer.child.try_wait().map_err(io::Error::other)?.is_some() {
            exited.push(id);
        }
    }
    if exited.is_empty() {
        return Ok(false);
    }
    let active_index = state
        .order
        .iter()
        .position(|id| *id == state.active)
        .unwrap_or(0);
    let active_exited = exited.contains(&state.active);
    for id in exited {
        if let Some(mut buffer) = state.buffers.remove(&id) {
            terminate_child(buffer.child.as_mut(), buffer.session_id);
        }
        state.order.retain(|ordered_id| *ordered_id != id);
    }
    if state.order.is_empty() {
        close_clients(&mut state);
        session.shutdown.store(true, Ordering::Release);
        return Ok(true);
    }
    if active_exited {
        state.active = state.order[active_index.min(state.order.len() - 1)];
        resize_active(&mut state);
        send_snapshot(&mut state);
    }
    Ok(false)
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

fn close_clients(state: &mut SessionState) {
    for (_, client) in state.clients.drain() {
        let _ = client.sender.try_send(Outgoing::Close);
        let _ = client.stream.shutdown(std::net::Shutdown::Both);
    }
}

fn terminate_child(child: &mut dyn portable_pty::Child, session_id: Option<libc::pid_t>) {
    let groups = session_id
        .filter(|session| *session > 1)
        .and_then(|session| process_groups_in_session(session).ok());
    let mut killed = false;
    if let Some(groups) = groups {
        for group in groups {
            if group > 1 && group != unsafe { libc::getpgrp() } {
                killed |= unsafe { libc::kill(-group, libc::SIGKILL) == 0 };
            }
        }
    }
    if !killed {
        let _ = child.kill();
    }
    let _ = child.wait();
}

fn process_groups_in_session(session_id: libc::pid_t) -> io::Result<Vec<libc::pid_t>> {
    let mut groups = HashSet::new();
    for process in fs::read_dir("/proc")? {
        let process = match process {
            Ok(process) => process,
            Err(_) => continue,
        };
        let stat = match fs::read_to_string(process.path().join("stat")) {
            Ok(stat) => stat,
            Err(_) => continue,
        };
        let Some((_, fields)) = stat.rsplit_once(')') else {
            continue;
        };
        let mut fields = fields.split_whitespace();
        let _state = fields.next();
        let _parent = fields.next();
        let Some(group) = fields
            .next()
            .and_then(|value| value.parse::<libc::pid_t>().ok())
        else {
            continue;
        };
        let Some(session) = fields
            .next()
            .and_then(|value| value.parse::<libc::pid_t>().ok())
        else {
            continue;
        };
        if session == session_id {
            groups.insert(group);
        }
    }
    Ok(groups.into_iter().collect())
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
