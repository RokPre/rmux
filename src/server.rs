use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::num::NonZeroU16;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::layout::{Axis, Direction, WindowLayout};

use crate::runtime::{SessionPaths, process_start_time};
use crate::{Request, Response};

pub(crate) const INPUT: u8 = 1;
pub(crate) const RESIZE: u8 = 2;
const DETACH: u8 = 3;
pub(crate) const NEW_BUFFER: u8 = 4;
pub(crate) const CLOSE_BUFFER: u8 = 5;
pub(crate) const PREVIOUS_BUFFER: u8 = 6;
pub(crate) const NEXT_BUFFER: u8 = 7;
pub(crate) const SPLIT_VERTICAL: u8 = 8;
pub(crate) const SPLIT_HORIZONTAL: u8 = 9;
pub(crate) const CLOSE_WINDOW: u8 = 10;
pub(crate) const FOCUS_WINDOW: u8 = 11;
pub(crate) const MOVE_WINDOW: u8 = 12;
pub(crate) const RESIZE_WINDOW: u8 = 13;
pub(crate) const REORDER_BUFFER_LEFT: u8 = 14;
pub(crate) const REORDER_BUFFER_RIGHT: u8 = 15;
pub(crate) const RENAME_BUFFER: u8 = 16;
pub(crate) const EQUALIZE_WINDOWS: u8 = 17;
pub(crate) const CLOSE: u8 = 2;
pub(crate) const WINDOW_STATE: u8 = 3;
pub(crate) const BUFFER_SNAPSHOT: u8 = 4;
pub(crate) const BUFFER_OUTPUT: u8 = 5;
const MAX_FRAME: usize = 1024 * 1024;

pub(crate) enum Outgoing {
    BufferSnapshot(Vec<u8>),
    BufferOutput(Vec<u8>),
    Layout(Vec<u8>),
    Refresh,
    Close,
}

struct Client {
    sender: SyncSender<Outgoing>,
    stream: UnixStream,
    rows: u16,
    cols: u16,
    resize_step: u8,
    snapshot_only: Arc<AtomicBool>,
    refresh_pending: Arc<AtomicBool>,
}

struct Buffer {
    writer: Box<dyn Write + Send>,
    master: Box<dyn MasterPty + Send>,
    parser: vt100::Parser<TerminalReplies>,
    child: Box<dyn portable_pty::Child>,
    session_id: Option<libc::pid_t>,
}

#[derive(Default)]
struct TerminalReplies {
    pending: Vec<u8>,
}

impl vt100::Callbacks for TerminalReplies {
    fn unhandled_csi(
        &mut self,
        screen: &mut vt100::Screen,
        i1: Option<u8>,
        i2: Option<u8>,
        params: &[&[u16]],
        command: char,
    ) {
        if i1.is_none() && i2.is_none() && command == 'n' && params == [&[6][..]] {
            let (row, col) = screen.cursor_position();
            write!(
                &mut self.pending,
                "\x1b[{};{}R",
                u32::from(row) + 1,
                u32::from(col) + 1
            )
            .expect("writing to a Vec cannot fail");
        }
    }
}

struct SessionState {
    clients: HashMap<u64, Client>,
    buffers: HashMap<u64, Buffer>,
    buffer_names: HashMap<u64, String>,
    order: Vec<u64>,
    layout: WindowLayout,
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
            buffer_names: HashMap::from([(1, "1".to_owned())]),
            order: vec![1],
            layout: WindowLayout::new(1),
            next_buffer: 2,
        }),
        next_client: AtomicU64::new(1),
        shutdown: std::sync::atomic::AtomicBool::new(false),
    });
    crate::debug_log(format_args!("server started session={name}"));
    spawn_output_reader(reader, Arc::clone(&session), 1);

    loop {
        if session.shutdown.load(Ordering::Acquire) || reap_exited_buffers(&session)? {
            break;
        }

        match listener.accept() {
            Ok((stream, _)) => {
                let session = Arc::clone(&session);
                thread::spawn(move || {
                    if let Err(error) = handle_client(stream, session) {
                        crate::debug_log(format_args!("client handler error: {error}"));
                    }
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
    let parser = vt100::Parser::new_with_callbacks(
        NonZeroU16::new(size.rows).unwrap_or(NonZeroU16::MIN),
        NonZeroU16::new(size.cols).unwrap_or(NonZeroU16::MIN),
        1000,
        TerminalReplies::default(),
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
                        let replies = std::mem::take(&mut buffer.parser.callbacks_mut().pending);
                        if !replies.is_empty() && buffer.writer.write_all(&replies).is_err() {
                            break;
                        }
                        if state.layout.is_buffer_visible(buffer_id) {
                            let mut payload = Vec::with_capacity(8 + length);
                            payload.extend_from_slice(&buffer_id.to_be_bytes());
                            payload.extend_from_slice(&output[..length]);
                            broadcast(&mut state, Outgoing::BufferOutput(payload));
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
fn active_buffer(state: &SessionState) -> Option<u64> {
    state.layout.focused_buffer()
}

fn broadcast(state: &mut SessionState, message: Outgoing) {
    let mut disconnected = Vec::new();
    for (&id, client) in &state.clients {
        if client.snapshot_only.load(Ordering::Acquire) {
            if matches!(message, Outgoing::Close) {
                if let Err(TrySendError::Disconnected(_)) =
                    client.sender.try_send(Outgoing::Close)
                {
                    disconnected.push(id);
                }
            } else if !client.refresh_pending.swap(true, Ordering::AcqRel) {
                match client.sender.try_send(Outgoing::Refresh) {
                    Ok(()) | Err(TrySendError::Full(_)) => {}
                    Err(TrySendError::Disconnected(_)) => disconnected.push(id),
                }
            }
            continue;
        }
        let result = match &message {
            Outgoing::BufferSnapshot(bytes) => client
                .sender
                .try_send(Outgoing::BufferSnapshot(bytes.clone())),
            Outgoing::BufferOutput(bytes) => client
                .sender
                .try_send(Outgoing::BufferOutput(bytes.clone())),
            Outgoing::Layout(bytes) => client.sender.try_send(Outgoing::Layout(bytes.clone())),
            Outgoing::Refresh => client.sender.try_send(Outgoing::Refresh),
            Outgoing::Close => client.sender.try_send(Outgoing::Close),
        };
        match result {
            Ok(()) | Err(TrySendError::Disconnected(_)) => {}
            Err(TrySendError::Full(_)) => {
                client.snapshot_only.store(true, Ordering::Release);
                client.refresh_pending.store(false, Ordering::Release);
                crate::debug_log(format_args!(
                    "client switched to snapshot-only client={id}"
                ));
            }
        }
    }
    for id in disconnected {
        state.clients.remove(&id);
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
    crate::debug_log(format_args!("client attached session={} client={id} size={cols}x{rows}", session.name));
    let (sender, receiver) = mpsc::sync_channel(64);
    let snapshot_only = Arc::new(AtomicBool::new(false));
    let refresh_pending = Arc::new(AtomicBool::new(false));
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
                resize_step: request.resize_step.clamp(1, 50),
                snapshot_only: Arc::clone(&snapshot_only),
                refresh_pending: Arc::clone(&refresh_pending),
            },
        );
        resize_visible_buffers(&mut state);
        send_layout_to_client(&mut state, id);
        send_visible_buffer_snapshots(&mut state);
    }
    let output_session = Arc::clone(&session);
    thread::spawn(move || {
        output_loop(
            output_stream,
            receiver,
            output_session,
            id,
            snapshot_only,
            refresh_pending,
        )
    });

    loop {
        let (kind, payload) = match read_frame(&mut reader) {
            Ok(Some(frame)) => frame,
            Ok(None) => break,
            Err(error) => {
                crate::debug_log(format_args!("client read error client={id}: {error}"));
                break;
            }
        };
        match kind {
            INPUT => {
                let mut state = lock_state(&session);
                if let Some(active) = active_buffer(&state) {
                    if let Some(buffer) = state.buffers.get_mut(&active) {
                        if buffer.writer.write_all(&payload).is_err() {
                            break;
                        }
                        let _ = buffer.writer.flush();
                    }
                }
            }
            RESIZE if payload.len() == 4 => {
                let mut state = lock_state(&session);
                if let Some(client) = state.clients.get_mut(&id) {
                    client.rows = u16::from_be_bytes([payload[0], payload[1]]).max(1);
                    client.cols = u16::from_be_bytes([payload[2], payload[3]]).max(1);
                    crate::debug_log(format_args!("client resize client={id} size={}x{}", client.cols, client.rows));
                }
                resize_visible_buffers(&mut state);
                send_visible_buffer_snapshots(&mut state);
            }
            NEW_BUFFER if payload.is_empty() => create_buffer(&session),
            CLOSE_BUFFER if payload.is_empty() => close_active_buffer(&session),
            PREVIOUS_BUFFER if payload.is_empty() => switch_buffer(&session, false),
            NEXT_BUFFER if payload.is_empty() => switch_buffer(&session, true),
            SPLIT_VERTICAL if payload.is_empty() => split_focused(&session, Axis::Vertical),
            SPLIT_HORIZONTAL if payload.is_empty() => split_focused(&session, Axis::Horizontal),
            CLOSE_WINDOW if payload.is_empty() => close_focused_window(&session),
            FOCUS_WINDOW if payload.len() == 1 => focus_window(&session, payload[0]),
            MOVE_WINDOW if payload.len() == 1 => move_focused_window(&session, payload[0]),
            RESIZE_WINDOW if payload.len() == 1 => {
                let step = lock_state(&session)
                    .clients
                    .get(&id)
                    .map_or(5, |client| client.resize_step);
                resize_focused_window(&session, payload[0], step);
            }
            REORDER_BUFFER_LEFT if payload.is_empty() => reorder_buffer(&session, false),
            REORDER_BUFFER_RIGHT if payload.is_empty() => reorder_buffer(&session, true),
            EQUALIZE_WINDOWS if payload.is_empty() => equalize_windows(&session),
            RENAME_BUFFER => rename_buffer(&session, &payload),
            DETACH => break,
            _ => break,
        }
    }

    {
        crate::debug_log(format_args!("client detach session={} client={id}", session.name));
        let mut state = lock_state(&session);
        if let Some(client) = state.clients.remove(&id) {
            let _ = client.stream.shutdown(std::net::Shutdown::Both);
        }
        resize_visible_buffers(&mut state);
        send_visible_buffer_snapshots(&mut state);
    }
    Ok(())
}

fn create_buffer(session: &Arc<Session>) {
    let (id, size) = {
        let mut state = lock_state(session);
        let id = state.next_buffer;
        state.next_buffer = id.wrapping_add(1).max(1);
        let size = active_buffer(&state)
            .and_then(|active| state.buffers.get(&active))
            .and_then(|buffer| buffer.master.get_size().ok())
            .unwrap_or(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            });
        (id, size)
    };
    let shell = std::env::var_os("SHELL").unwrap_or_else(|| OsString::from("/bin/sh"));
    let (mut buffer, reader) =
        match spawn_buffer(vec![shell, OsString::from("-i")], &session.cwd, size) {
            Ok(buffer) => buffer,
            Err(_) => return,
        };
    {
        let mut state = lock_state(session);
        if session.shutdown.load(Ordering::Acquire) {
            terminate_child(buffer.child.as_mut(), buffer.session_id);
            return;
        }
        state.order.push(id);
        state.buffers.insert(id, buffer);
        state.buffer_names.insert(id, id.to_string());
        state.layout.set_focused_buffer(id);
        resize_visible_buffers(&mut state);
        send_layout(&mut state);
        send_visible_buffer_snapshots(&mut state);
    }
    spawn_output_reader(reader, Arc::clone(session), id);
}

fn close_active_buffer(session: &Session) {
    let mut state = lock_state(session);
    let Some(closing) = state.layout.focused_buffer() else {
        return;
    };
    let Some(index) = state.order.iter().position(|id| *id == closing) else {
        return;
    };
    if state.order.len() == 1 {
        if let Some(mut buffer) = state.buffers.remove(&closing) {
            terminate_child(buffer.child.as_mut(), buffer.session_id);
        }
        state.order.clear();
        close_clients(&mut state);
        session.shutdown.store(true, Ordering::Release);
        return;
    }
    let replacement = state.order[(index + 1) % state.order.len()];
    state.order.remove(index);
    state.layout.replace_buffer(closing, replacement);
    if let Some(mut buffer) = state.buffers.remove(&closing) {
        terminate_child(buffer.child.as_mut(), buffer.session_id);
    }
    state.buffer_names.remove(&closing);
    resize_visible_buffers(&mut state);
    send_layout(&mut state);
    send_visible_buffer_snapshots(&mut state);
}

fn switch_buffer(session: &Session, next: bool) {
    let mut state = lock_state(session);
    if state.order.len() < 2 {
        return;
    }
    let Some(active) = state.layout.focused_buffer() else {
        return;
    };
    let index = state.order.iter().position(|id| *id == active).unwrap_or(0);
    let new_index = if next {
        (index + 1) % state.order.len()
    } else {
        (index + state.order.len() - 1) % state.order.len()
    };
    let buffer = state.order[new_index];
    state.layout.set_focused_buffer(buffer);
    resize_visible_buffers(&mut state);
    send_layout(&mut state);
    send_visible_buffer_snapshots(&mut state);
}

fn reorder_buffer(session: &Session, right: bool) {
    let mut state = lock_state(session);
    let Some(active) = state.layout.focused_buffer() else {
        return;
    };
    let Some(index) = state.order.iter().position(|id| *id == active) else {
        return;
    };
    let destination = if right {
        index + 1
    } else {
        index.saturating_sub(1)
    };
    if destination >= state.order.len() || destination == index {
        return;
    }
    state.order.swap(index, destination);
    send_layout(&mut state);
}

fn split_focused(session: &Session, axis: Axis) {
    let mut state = lock_state(session);
    if state.layout.split_focused(axis) {
        resize_visible_buffers(&mut state);
        send_layout(&mut state);
        send_visible_buffer_snapshots(&mut state);
    }
}

fn close_focused_window(session: &Session) {
    let mut state = lock_state(session);
    if state.layout.close_focused_window() {
        resize_visible_buffers(&mut state);
        send_layout(&mut state);
        send_visible_buffer_snapshots(&mut state);
    }
}

fn focus_window(session: &Session, encoded_direction: u8) {
    let Some(direction) = Direction::from_byte(encoded_direction) else {
        return;
    };
    let mut state = lock_state(session);
    if state.layout.focus_direction(direction) {
        send_layout(&mut state);
    }
}

fn move_focused_window(session: &Session, encoded_direction: u8) {
    let Some(direction) = Direction::from_byte(encoded_direction) else {
        return;
    };
    let mut state = lock_state(session);
    if state.layout.move_focused_window(direction) {
        resize_visible_buffers(&mut state);
        send_layout(&mut state);
        send_visible_buffer_snapshots(&mut state);
    }
}

fn resize_focused_window(session: &Session, encoded_direction: u8, step: u8) {
    let Some(direction) = Direction::from_byte(encoded_direction) else {
        return;
    };
    let mut state = lock_state(session);
    if state.layout.resize_focused_window(direction, step) {
        resize_visible_buffers(&mut state);
        send_layout(&mut state);
        send_visible_buffer_snapshots(&mut state);
    }
}

fn equalize_windows(session: &Session) {
    let mut state = lock_state(session);
    if state.layout.equalize() {
        resize_visible_buffers(&mut state);
        send_layout(&mut state);
        send_visible_buffer_snapshots(&mut state);
    }
}

fn send_layout(state: &mut SessionState) {
    let snapshot = state.layout.snapshot(&state.order, &state.buffer_names);
    broadcast(state, Outgoing::Layout(snapshot));
}
fn send_layout_to_client(state: &mut SessionState, id: u64) {
    let snapshot = state.layout.snapshot(&state.order, &state.buffer_names);
    if let Some(client) = state.clients.get(&id) {
        let _ = client.sender.try_send(Outgoing::Layout(snapshot));
    }
}
fn rename_buffer(session: &Session, payload: &[u8]) {
    if payload.len() < 8 {
        return;
    }
    let target = u64::from_be_bytes(payload[..8].try_into().expect("eight-byte buffer ID"));
    let Ok(name) = std::str::from_utf8(&payload[8..]) else {
        return;
    };
    if name.chars().any(char::is_control) {
        return;
    }

    let mut state = lock_state(session);
    if !state.buffers.contains_key(&target) {
        return;
    }
    let name = if name.is_empty() {
        target.to_string()
    } else {
        name.to_owned()
    };
    state.buffer_names.insert(target, name);
    send_layout(&mut state);
}
fn send_visible_buffer_snapshots(state: &mut SessionState) {
    let snapshots: Vec<_> = state
        .layout
        .visible_buffer_ids()
        .into_iter()
        .filter_map(|id| {
            state.buffers.get(&id).map(|buffer| {
                let screen = buffer.parser.screen();
                let (rows, cols) = screen.size();
                let mut contents = screen.contents_formatted().into_bytes();
                if screen.bracketed_paste() {
                    contents.extend_from_slice(b"\x1b[?2004h");
                }
                (id, rows.get(), cols.get(), contents)
            })
        })
        .collect();

    for (id, rows, cols, contents) in snapshots {
        let mut offset = 0;
        let mut first = true;
        loop {
            let end = (offset + MAX_FRAME - 12).min(contents.len());
            let mut payload = Vec::with_capacity(12 + end - offset);
            payload.extend_from_slice(&id.to_be_bytes());
            payload.extend_from_slice(&rows.to_be_bytes());
            payload.extend_from_slice(&cols.to_be_bytes());
            payload.extend_from_slice(&contents[offset..end]);
            let message = if first {
                Outgoing::BufferSnapshot(payload)
            } else {
                Outgoing::BufferOutput(payload)
            };
            broadcast(state, message);
            if end == contents.len() {
                break;
            }
            offset = end;
            first = false;
        }
    }
}

fn resize_visible_buffers(state: &mut SessionState) {
    let mut sizes = HashMap::<u64, (u16, u16)>::new();
    for client in state.clients.values() {
        for window in state.layout.geometry(client.cols, client.rows).windows {
            let rows = window.rect.height.max(1);
            let cols = window.rect.width.max(1);
            let size = sizes.entry(window.buffer).or_insert((rows, cols));
            size.0 = size.0.min(rows);
            size.1 = size.1.min(cols);
        }
    }

    for (id, (rows, cols)) in sizes {
        let Some(buffer) = state.buffers.get_mut(&id) else {
            continue;
        };
        let pty_size = PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        };
        let pty_matches = buffer
            .master
            .get_size()
            .is_ok_and(|size| size.rows == rows && size.cols == cols);
        let (screen_rows, screen_cols) = buffer.parser.screen().size();
        let screen_matches = screen_rows.get() == rows && screen_cols.get() == cols;
        if pty_matches && screen_matches {
            continue;
        }
        if !pty_matches && buffer.master.resize(pty_size).is_err() {
            continue;
        }
        if !screen_matches {
            buffer.parser.set_size(
                NonZeroU16::new(rows).expect("pane rows are at least one"),
                NonZeroU16::new(cols).expect("pane columns are at least one"),
            );
        }
    }
}

fn reap_exited_buffers(session: &Session) -> io::Result<bool> {
    let mut state = lock_state(session);
    let mut exited = Vec::new();
    for (&id, buffer) in &mut state.buffers {
        if let Some(status) = buffer.child.try_wait().map_err(io::Error::other)? {
            crate::debug_log(format_args!("buffer exited session={} buffer={id} status={status:?}", session.name));
            exited.push(id);
        }
    }
    if exited.is_empty() {
        return Ok(false);
    }

    let old_order = state.order.clone();
    let exited_set: HashSet<_> = exited.iter().copied().collect();
    for id in &exited {
        if let Some(mut buffer) = state.buffers.remove(id) {
            terminate_child(buffer.child.as_mut(), buffer.session_id);
        }
        state.buffer_names.remove(id);
    }
    state.order.retain(|id| !exited_set.contains(id));
    if state.order.is_empty() {
        close_clients(&mut state);
        session.shutdown.store(true, Ordering::Release);
        return Ok(true);
    }

    for id in exited {
        let Some(index) = old_order.iter().position(|old_id| *old_id == id) else {
            continue;
        };
        let replacement = (1..old_order.len())
            .map(|offset| old_order[(index + offset) % old_order.len()])
            .find(|candidate| !exited_set.contains(candidate));
        if let Some(replacement) = replacement {
            state.layout.replace_buffer(id, replacement);
        }
    }
    resize_visible_buffers(&mut state);
    send_layout(&mut state);
    send_visible_buffer_snapshots(&mut state);
    Ok(false)
}

fn output_loop(
    mut stream: UnixStream,
    receiver: mpsc::Receiver<Outgoing>,
    session: Arc<Session>,
    id: u64,
    snapshot_only: Arc<AtomicBool>,
    refresh_pending: Arc<AtomicBool>,
) {
    loop {
        let message = match receiver.recv() {
            Ok(message) => message,
            Err(_) => break,
        };
        if matches!(message, Outgoing::Close) {
            let _ = write_frame(&mut stream, CLOSE, &[]);
            break;
        }
        if snapshot_only.load(Ordering::Acquire) {
            if write_recovery_snapshot(
                &mut stream,
                &receiver,
                &session,
                id,
                &refresh_pending,
            )
            .is_err()
            {
                break;
            }
            continue;
        }
        if write_message(&mut stream, message).is_err() {
            break;
        }
        if snapshot_only.load(Ordering::Acquire)
            && write_recovery_snapshot(
                &mut stream,
                &receiver,
                &session,
                id,
                &refresh_pending,
            )
            .is_err()
        {
            break;
        }
    }
    let _ = stream.shutdown(std::net::Shutdown::Both);
}

fn write_message(stream: &mut UnixStream, message: Outgoing) -> io::Result<()> {
    match message {
        Outgoing::BufferSnapshot(bytes) => write_frame(stream, BUFFER_SNAPSHOT, &bytes),
        Outgoing::BufferOutput(bytes) => write_frame(stream, BUFFER_OUTPUT, &bytes),
        Outgoing::Layout(snapshot) => write_frame(stream, WINDOW_STATE, &snapshot),
        Outgoing::Refresh => Ok(()),
        Outgoing::Close => write_frame(stream, CLOSE, &[]),
    }
}

fn write_recovery_snapshot(
    stream: &mut UnixStream,
    receiver: &mpsc::Receiver<Outgoing>,
    session: &Session,
    id: u64,
    refresh_pending: &AtomicBool,
) -> io::Result<()> {
    loop {
        let snapshots = {
            let state = lock_state(session);
            while receiver.try_recv().is_ok() {}
            refresh_pending.store(false, Ordering::Release);
            recovery_snapshot(&state)
        };
        for message in snapshots {
            write_message(stream, message)?;
        }

        match receiver.try_recv() {
            Ok(Outgoing::Close) => return write_frame(stream, CLOSE, &[]),
            Ok(_) => {
                refresh_pending.store(false, Ordering::Release);
            }
            Err(mpsc::TryRecvError::Empty) => {
                if refresh_pending.load(Ordering::Acquire) {
                    continue;
                }
                return Ok(());
            }
            Err(mpsc::TryRecvError::Disconnected) => return Ok(()),
        }
        if !lock_state(session).clients.contains_key(&id) {
            return Ok(());
        }
    }
}

fn recovery_snapshot(state: &SessionState) -> Vec<Outgoing> {
    let mut messages = vec![Outgoing::Layout(
        state.layout.snapshot(&state.order, &state.buffer_names),
    )];
    let mut seen = HashSet::new();
    for id in state.layout.visible_buffer_ids() {
        if !seen.insert(id) {
            continue;
        }
        let Some(buffer) = state.buffers.get(&id) else {
            continue;
        };
        let screen = buffer.parser.screen();
        let (rows, cols) = screen.size();
        let mut contents = screen.state_formatted().into_bytes();
        if screen.bracketed_paste() {
            contents.extend_from_slice(b"\x1b[?2004h");
        }
        let mut offset = 0;
        let mut first = true;
        loop {
            let end = (offset + MAX_FRAME - 12).min(contents.len());
            let mut payload = Vec::with_capacity(12 + end - offset);
            payload.extend_from_slice(&id.to_be_bytes());
            payload.extend_from_slice(&rows.get().to_be_bytes());
            payload.extend_from_slice(&cols.get().to_be_bytes());
            payload.extend_from_slice(&contents[offset..end]);
            messages.push(if first {
                Outgoing::BufferSnapshot(payload)
            } else {
                Outgoing::BufferOutput(payload)
            });
            if end == contents.len() {
                break;
            }
            offset = end;
            first = false;
        }
    }
    messages
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
        supports_resize_step: true,
    }
}

fn error_response(error: &str) -> Response {
    Response {
        ok: false,
        error: error.into(),
        name: String::new(),
        pid: 0,
        start_time: 0,
        supports_resize_step: true,
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
