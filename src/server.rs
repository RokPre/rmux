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
use std::time::{Duration, Instant};

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
pub(crate) const HISTORY_VIEW: u8 = 18;
pub(crate) const CLOSE: u8 = 2;
pub(crate) const WINDOW_STATE: u8 = 3;
pub(crate) const BUFFER_SNAPSHOT: u8 = 4;
pub(crate) const BUFFER_OUTPUT: u8 = 5;
pub(crate) const HISTORY_SNAPSHOT: u8 = 6;
pub(crate) const VIM_STATE: u8 = 7;
pub(crate) const VIM_FLUSH: u8 = 19;
pub(crate) const YANK: u8 = 8;
pub(crate) const VIM_KEY: u8 = 20;
const MAX_FRAME: usize = 1024 * 1024;
const SCROLLBACK_LIMIT: usize = 2000;
const YANK_FLASH_DURATION: Duration = Duration::from_millis(400);

pub(crate) enum Outgoing {
    BufferSnapshot(Vec<u8>),
    BufferOutput(Vec<u8>),
    HistorySnapshot(Vec<u8>),
    VimState(Vec<u8>),
    Yank(Vec<u8>),
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
    pending_yanks: Vec<Vec<u8>>,
}

struct Buffer {
    writer: Box<dyn Write + Send>,
    master: Box<dyn MasterPty + Send>,
    parser: vt100::Parser<TerminalReplies>,
    child: Box<dyn portable_pty::Child>,
    session_id: Option<libc::pid_t>,
    child_identity: Option<crate::vim_control::ProcessIdentity>,
    vim: crate::vim::Vim,
    yank_flash_deadline: Option<Instant>,
    app_control: crate::vim_control::AppControl,
}

#[derive(Default)]
struct TerminalReplies {
    pending: Vec<u8>,
    app_requests: Vec<Vec<u8>>,
    dropped_primary_rows: usize,
    ris_escape_pending: bool,
    control_string: u8,
    control_string_escape_pending: bool,
}

impl TerminalReplies {
    fn observe_ris(&mut self, bytes: &[u8]) -> bool {
        let mut found = false;
        for &byte in bytes {
            if self.control_string != 0 {
                if self.control_string_escape_pending {
                    self.control_string_escape_pending = false;
                    if byte == b'\\' {
                        self.control_string = 0;
                        continue;
                    }
                    // vte ends a control string at ESC even when it is not
                    // followed by ST. Process the following byte as the next
                    // escape sequence instead of keeping stale string state.
                    self.control_string = 0;
                    self.ris_escape_pending = true;
                } else if byte == 0x1b {
                    self.control_string_escape_pending = true;
                    continue;
                } else if self.control_string == b']' && byte == 0x07 {
                    self.control_string = 0;
                    continue;
                } else {
                    continue;
                }
            }

            if self.ris_escape_pending {
                self.ris_escape_pending = false;
                if byte == b'c' {
                    found = true;
                    continue;
                }
                if matches!(byte, b']' | b'P' | b'_' | b'^' | b'X') {
                    self.control_string = byte;
                    continue;
                }
                if byte == 0x1b {
                    self.ris_escape_pending = true;
                }
                continue;
            }

            if byte == 0x1b {
                self.ris_escape_pending = true;
            }
        }
        found
    }
    fn record_primary_eviction(&mut self, alternate_screen: bool) {
        if !alternate_screen {
            self.dropped_primary_rows = self.dropped_primary_rows.saturating_add(1);
        }
    }
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

    fn unhandled_osc(&mut self, _: &mut vt100::Screen, params: &[&[u8]]) {
        if params.first().copied() == Some(&b"777"[..])
            && params.get(1).copied() == Some(&b"rmux-app"[..])
        {
            let mut payload = Vec::new();
            for (index, param) in params.iter().enumerate() {
                if index > 0 {
                    payload.push(b';');
                }
                payload.extend_from_slice(param);
            }
            self.app_requests.push(payload);
        }
    }

    fn on_scroll(&mut self, _contents: vt100::capture::RowContents<'_>, alternate_screen: bool) {
        self.record_primary_eviction(alternate_screen);
    }
}

#[derive(serde::Serialize)]
struct VimSnapshot {
    buffer: u64,
    mode: String,
    selection_mode: Option<String>,
    cursor_row: usize,
    cursor_col: u16,
    selection: Option<(usize, u16, usize, u16)>,
    yank_selection: Option<(usize, u16, usize, u16)>,
    yank_mode: Option<String>,
    matches: Vec<Vec<(usize, u16)>>,
    active_match: Option<usize>,
    prompt: Option<String>,
    app_control: bool,
    history_len: usize,
    scrollback: usize,
}

struct SessionState {
    clients: HashMap<u64, Client>,
    buffers: HashMap<u64, Buffer>,
    buffer_names: HashMap<u64, String>,
    order: Vec<u64>,
    layout: WindowLayout,
    history_offsets: HashMap<u64, usize>,
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
            history_offsets: HashMap::new(),
            next_buffer: 2,
        }),
        next_client: AtomicU64::new(1),
        shutdown: std::sync::atomic::AtomicBool::new(false),
    });
    crate::debug_log(format_args!("server started session={name}"));
    spawn_output_reader(reader, Arc::clone(&session), 1);

    let mut last_app_prune = Instant::now();
    loop {
        expire_yank_flashes(&session);
        if session.shutdown.load(Ordering::Acquire) || reap_exited_buffers(&session)? {
            break;
        }
        if last_app_prune.elapsed() >= Duration::from_secs(1) {
            last_app_prune = Instant::now();
            prune_app_controls(&session);
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
    let child_identity = session_id.and_then(|pid| {
        process_start_time(pid as u32)
            .ok()
            .map(|start_time| crate::vim_control::ProcessIdentity {
                pid: pid as u32,
                start_time,
            })
    });
    let writer = master.take_writer().map_err(io::Error::other)?;
    let reader = master.try_clone_reader().map_err(io::Error::other)?;
    let parser = vt100::Parser::new_with_callbacks(
        NonZeroU16::new(size.rows).unwrap_or(NonZeroU16::MIN),
        NonZeroU16::new(size.cols).unwrap_or(NonZeroU16::MIN),
        SCROLLBACK_LIMIT,
        TerminalReplies::default(),
    );
    Ok((
        Buffer {
            writer,
            master,
            parser,
            child,
            session_id,
            child_identity,
            vim: crate::vim::Vim::new(),
            yank_flash_deadline: None,
            app_control: crate::vim_control::AppControl::default(),
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
                        let old_alternate = buffer.parser.screen().alternate_screen();
                        let ris = buffer
                            .parser
                            .callbacks_mut()
                            .observe_ris(&output[..length]);
                        buffer.parser.process(&output[..length]);
                        let dropped_rows =
                            std::mem::take(&mut buffer.parser.callbacks_mut().dropped_primary_rows);
                        {
                            let screen = buffer.parser.screen_mut();
                            let alternate_changed = screen.alternate_screen() != old_alternate;
                            let dropped_rows = if screen.alternate_screen() { 0 } else { dropped_rows };
                            buffer.vim.history_changed(
                                screen,
                                if ris { None } else { Some(dropped_rows) },
                                alternate_changed || ris,
                            );
                        }
                        let app_control_changed = process_app_requests(buffer);
                        let replies = std::mem::take(&mut buffer.parser.callbacks_mut().pending);
                        if !replies.is_empty() && buffer.writer.write_all(&replies).is_err() {
                            break;
                        }
                        if state.layout.is_buffer_visible(buffer_id) {
                            let mut payload = Vec::with_capacity(8 + length);
                            payload.extend_from_slice(&buffer_id.to_be_bytes());
                            payload.extend_from_slice(&output[..length]);
                            broadcast(&mut state, Outgoing::BufferOutput(payload));
                            send_vim_snapshot(&mut state, buffer_id);
                            send_buffer_history_snapshots(&mut state, buffer_id);
                        } else if app_control_changed {
                            send_vim_snapshot(&mut state, buffer_id);
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
fn process_app_requests(buffer: &mut Buffer) -> bool {
    let was_enabled = buffer.app_control.is_enabled();
    let requests = std::mem::take(&mut buffer.parser.callbacks_mut().app_requests);
    for payload in requests {
        let Some(request) = crate::vim_control::parse_request(&payload) else {
            continue;
        };
        let Ok(start_time) = process_start_time(request.pid) else {
            continue;
        };
        let identity = crate::vim_control::ProcessIdentity {
            pid: request.pid,
            start_time,
        };
        if buffer_process_owns(buffer, identity) {
            buffer.app_control.apply(request, identity);
        }
    }
    let child_identity = buffer.child_identity;
    let session_id = buffer.session_id.map(|pid| pid as u32);
    buffer.app_control.prune_dead(|identity| {
        process_is_owned(identity, child_identity, session_id)
    });
    was_enabled != buffer.app_control.is_enabled()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ProcessStat {
    state: char,
    parent_pid: u32,
    session_id: u32,
    start_time: u64,
}

impl ProcessStat {
    fn is_alive(self) -> bool {
        !matches!(self.state, 'Z' | 'X')
    }
}

fn process_stat(pid: u32) -> Option<ProcessStat> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    parse_process_stat(&stat)
}

fn parse_process_stat(stat: &str) -> Option<ProcessStat> {
    let fields = stat.rsplit_once(") ")?.1.split_whitespace().collect::<Vec<_>>();
    Some(ProcessStat {
        state: fields.first()?.chars().next()?,
        parent_pid: fields.get(1)?.parse().ok()?,
        session_id: fields.get(3)?.parse().ok()?,
        start_time: fields.get(19)?.parse().ok()?,
    })
}




fn process_is_owned(
    identity: crate::vim_control::ProcessIdentity,
    child_identity: Option<crate::vim_control::ProcessIdentity>,
    session_id: Option<u32>,
) -> bool {
    process_is_owned_with(identity, child_identity, session_id, process_stat)
}

fn process_is_owned_with(
    identity: crate::vim_control::ProcessIdentity,
    child_identity: Option<crate::vim_control::ProcessIdentity>,
    session_id: Option<u32>,
    mut get_stat: impl FnMut(u32) -> Option<ProcessStat>,
) -> bool {
    let Some(stat) = get_stat(identity.pid) else {
        return false;
    };
    if !stat.is_alive() || stat.start_time != identity.start_time {
        return false;
    }

    let Some(child) = child_identity else {
        return false;
    };
    if !get_stat(child.pid)
        .is_some_and(|stat| stat.is_alive() && stat.start_time == child.start_time)
    {
        return false;
    }
    if session_id == Some(stat.session_id) {
        return true;
    }

    let mut pid = identity.pid;
    let mut visited = HashSet::new();
    while pid != 0 && visited.insert(pid) {
        if pid == child.pid {
            return get_stat(pid).is_some_and(|stat| stat.start_time == child.start_time);
        }
        let Some(stat) = get_stat(pid) else {
            return false;
        };
        pid = stat.parent_pid;
    }
    false
}

fn buffer_process_owns(buffer: &Buffer, identity: crate::vim_control::ProcessIdentity) -> bool {
    process_is_owned(
        identity,
        buffer.child_identity,
        buffer.session_id.map(|pid| pid as u32),
    )
}

fn is_insert_passthrough(
    was_insert: bool,
    is_insert: bool,
    effects: &[crate::vim::Effect],
) -> bool {
    was_insert
        && is_insert
        && effects
            .iter()
            .all(|effect| matches!(effect, crate::vim::Effect::Forward(_)))
}

fn flash_scrollback(flash: crate::vim::YankFlash, history_len: usize) -> usize {
    history_len.saturating_sub(flash.viewport_top)
}
fn send_vim_snapshot(state: &mut SessionState, id: u64) {
    let Some(buffer) = state.buffers.get_mut(&id) else {
        return;
    };
    let payload = vim_snapshot_payload(buffer, id);
    let scrollback = buffer.vim.yank_flash().map_or_else(
        || buffer.parser.screen().scrollback(),
        |flash| flash_scrollback(flash, buffer.parser.screen().scrollback_len()),
    );
    state.history_offsets.insert(id, scrollback);
    if let Some(payload) = payload {
        broadcast(state, Outgoing::VimState(payload));
    }
}

fn vim_snapshot_payload(buffer: &mut Buffer, id: u64) -> Option<Vec<u8>> {
    let flash = buffer.vim.yank_flash();
    let screen = buffer.parser.screen_mut();
    let cursor = buffer.vim.cursor(screen);
    let selection = buffer.vim.selection(screen).map(|range| {
        (range.start.row, range.start.col, range.end.row, range.end.col)
    });
    let history_len = screen.scrollback_len();
    let scrollback = flash.map_or(screen.scrollback(), |flash| flash_scrollback(flash, history_len));
    let (rows, _) = screen.size();
    let top = history_len.saturating_sub(scrollback);
    let bottom = top + usize::from(rows.get()) - 1;
    let matches = visible_matches(&buffer.vim, top, bottom);
    let mode = vim_mode_name(buffer.vim.mode());
    let yank_selection = flash.map(|flash| (
        flash.range.start.row, flash.range.start.col, flash.range.end.row, flash.range.end.col,
    ));
    let yank_mode = flash.map(|flash| match flash.kind {
        crate::vim::VisualKind::Character => "visual",
        crate::vim::VisualKind::Line => "visual-line",
        crate::vim::VisualKind::Block => "visual-block",
    }.to_owned());
    serde_json::to_vec(&VimSnapshot {
        buffer: id,
        mode: mode.to_owned(),
        selection_mode: buffer.vim.selection_mode().map(|mode| mode.to_owned()),
        cursor_row: cursor.row,
        cursor_col: cursor.col,
        selection,
        yank_selection,
        yank_mode,
        active_match: matches.iter().position(|positions| positions.first() == Some(&(cursor.row, cursor.col))),
        matches,
        prompt: (mode == "search").then(|| buffer.vim.prompt_text().to_owned()),
        app_control: buffer.app_control.is_enabled(),
        history_len,
        scrollback,
    })
    .ok()
}

fn vim_mode_name(mode: crate::vim::Mode) -> &'static str {
    match mode {
        crate::vim::Mode::Insert => "insert",
        crate::vim::Mode::Normal => "normal",
        crate::vim::Mode::Visual(crate::vim::VisualKind::Character) => "visual",
        crate::vim::Mode::Visual(crate::vim::VisualKind::Line) => "visual-line",
        crate::vim::Mode::Visual(crate::vim::VisualKind::Block) => "visual-block",
        crate::vim::Mode::Search => "search",
    }
}

fn visible_matches(vim: &crate::vim::Vim, top: usize, bottom: usize) -> Vec<Vec<(usize, u16)>> {
    const MAX_MATCH_POSITIONS: usize = 8192;
    let mut positions = 0;
    vim.search_matches().iter().filter_map(|matched| {
        if matched.is_empty() || matched.iter().any(|p| p.row < top || p.row > bottom) {
            return None;
        }
        if positions + matched.len() > MAX_MATCH_POSITIONS {
            return None;
        }
        positions += matched.len();
        Some(matched.iter().map(|p| (p.row, p.col)).collect())
    }).collect()
}

fn expire_yank_flashes(session: &Session) {
    let now = Instant::now();
    let mut state = lock_state(session);
    let expired: Vec<_> = state.buffers.iter_mut().filter_map(|(&id, buffer)| {
        (buffer.yank_flash_deadline.is_some_and(|deadline| deadline <= now)).then(|| {
            buffer.yank_flash_deadline = None;
            buffer.vim.clear_yank_flash();
            id
        })
    }).collect();
    for id in expired {
        send_vim_snapshot(&mut state, id);
        send_buffer_history_snapshots(&mut state, id);
    }
}
fn hold_yank(state: &mut SessionState, client_id: u64, bytes: Vec<u8>) {
    if let Some(client) = state.clients.get_mut(&client_id) {
        client.pending_yanks.push(bytes);
    }
}

fn enqueue_yank(state: &mut SessionState, client_id: u64, bytes: Vec<u8>) {
    let Some(client) = state.clients.get_mut(&client_id) else {
        return;
    };
    if client.snapshot_only.load(Ordering::Acquire) {
        client.pending_yanks.push(bytes);
        return;
    }
    if let Err(TrySendError::Full(Outgoing::Yank(bytes))) =
        client.sender.try_send(Outgoing::Yank(bytes))
    {
        client.pending_yanks.push(bytes);
        client.snapshot_only.store(true, Ordering::Release);
    }
}
fn flush_vim_pending(state: &mut SessionState, buffer_id: u64, client_id: u64) {
    let mut forwarded = Vec::new();
    let mut yanks = Vec::new();
    if let Some(buffer) = state.buffers.get_mut(&buffer_id) {
        for effect in buffer.vim.flush_pending(buffer.parser.screen_mut()) {
            match effect {
                crate::vim::Effect::Forward(bytes) => forwarded.extend(bytes),
                crate::vim::Effect::Yank(text) => yanks.push(text.into_bytes()),
                crate::vim::Effect::SearchPrompt(_) | crate::vim::Effect::SearchClosed => {}
            }
        }
        if !forwarded.is_empty() {
            let _ = buffer.writer.write_all(&forwarded);
            let _ = buffer.writer.flush();
        }
    }
    for bytes in yanks {
        enqueue_yank(state, client_id, bytes);
    }
    send_vim_snapshot(state, buffer_id);
    send_buffer_history_snapshots(state, buffer_id);
}
fn prune_app_controls(session: &Session) {
    let mut state = lock_state(session);
    let changed: Vec<_> = state
        .buffers
        .iter_mut()
        .filter_map(|(&id, buffer)| {
            let before = buffer.app_control.is_enabled();
            let child_identity = buffer.child_identity;
            let session_id = buffer.session_id.map(|pid| pid as u32);
            buffer
                .app_control
                .prune_dead(|identity| process_is_owned(identity, child_identity, session_id));
            (before != buffer.app_control.is_enabled()).then_some(id)
        })
        .collect();
    for id in changed {
        send_vim_snapshot(&mut state, id);
    }
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
            Outgoing::VimState(bytes) => client
                .sender
                .try_send(Outgoing::VimState(bytes.clone())),
            Outgoing::Yank(bytes) => client
                .sender
                .try_send(Outgoing::Yank(bytes.clone())),
            Outgoing::Layout(bytes) => client.sender.try_send(Outgoing::Layout(bytes.clone())),
            Outgoing::Refresh => client.sender.try_send(Outgoing::Refresh),
            Outgoing::HistorySnapshot(bytes) => client
                .sender
                .try_send(Outgoing::HistorySnapshot(bytes.clone())),
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
                pending_yanks: Vec::new(),
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
            INPUT | VIM_KEY => {
                let mut state = lock_state(&session);
                if let Some(active) = active_buffer(&state) {
                    let mut forwarded = Vec::new();
                    let mut yanks = Vec::new();
                    let mut publish_vim_state = true;
                    if let Some(buffer) = state.buffers.get_mut(&active) {
                        let had_yank_flash = buffer.vim.yank_flash().is_some();
                        buffer.vim.clear_yank_flash();
                        buffer.yank_flash_deadline = None;
                        if buffer.app_control.is_enabled() {
                            if kind == INPUT {
                                forwarded.extend_from_slice(&payload);
                            }
                        } else {
                            let was_insert = buffer.vim.mode() == crate::vim::Mode::Insert;
                            let effects = buffer
                                .vim
                                .handle_bytes(buffer.parser.screen_mut(), &payload);
                            buffer.yank_flash_deadline = buffer.vim.yank_flash()
                                .map(|_| Instant::now() + YANK_FLASH_DURATION);
                            publish_vim_state = had_yank_flash || !is_insert_passthrough(
                                was_insert,
                                buffer.vim.mode() == crate::vim::Mode::Insert,
                                &effects,
                            );
                            for effect in effects {
                                match effect {
                                    crate::vim::Effect::Forward(bytes) => forwarded.extend(bytes),
                                    crate::vim::Effect::Yank(text) => yanks.push(text.into_bytes()),
                                    crate::vim::Effect::SearchPrompt(_)
                                    | crate::vim::Effect::SearchClosed => {}
                                }
                            }
                        }
                        if !forwarded.is_empty() {
                            if buffer.writer.write_all(&forwarded).is_err() {
                                break;
                            }
                            let _ = buffer.writer.flush();
                        }
                    }
                    for bytes in yanks {
                        enqueue_yank(&mut state, id, bytes);
                    }
                    if publish_vim_state {
                        send_vim_snapshot(&mut state, active);
                        send_buffer_history_snapshots(&mut state, active);
                    }
                }
            }
            VIM_FLUSH if payload.is_empty() => {
                let mut state = lock_state(&session);
                if let Some(active) = active_buffer(&state) {
                    flush_vim_pending(&mut state, active, id);
                }
            }
            RESIZE if payload.len() == 4 => {
                let mut state = lock_state(&session);
                if let Some(client) = state.clients.get_mut(&id) {
                    client.rows = u16::from_be_bytes([payload[0], payload[1]]).max(1);
                    client.cols = u16::from_be_bytes([payload[2], payload[3]]).max(1);
                    crate::debug_log(format_args!("client resize client={id} size={}x{}", client.cols, client.rows));
                }
                for buffer in state.buffers.values_mut() {
                    buffer.vim.clear_yank_flash();
                    buffer.yank_flash_deadline = None;
                }
                resize_visible_buffers(&mut state);
                send_visible_buffer_snapshots(&mut state);
            }
            HISTORY_VIEW if payload.len() == 12 => {
                let buffer = u64::from_be_bytes(payload[..8].try_into().unwrap());
                let offset = u32::from_be_bytes(payload[8..12].try_into().unwrap()) as usize;
                let mut state = lock_state(&session);
                if !state.clients.contains_key(&id) {
                    continue;
                }
                if let Some(buffer) = state.buffers.get_mut(&buffer) {
                    buffer.vim.clear_yank_flash();
                    buffer.yank_flash_deadline = None;
                }
                state.history_offsets.insert(buffer, offset);
                send_vim_snapshot(&mut state, buffer);
                state.history_offsets.insert(buffer, offset);
                send_buffer_history_snapshots(&mut state, buffer);
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
        state.history_offsets.remove(&closing);
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
    state.history_offsets.remove(&closing);
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
            state.buffers.get_mut(&id).map(|buffer| {
                let screen = buffer.parser.screen_mut();
                let saved_scrollback = screen.scrollback();
                screen.set_scrollback(0);
                let (rows, cols) = screen.size();
                let mut contents = Vec::new();
                if screen.alternate_screen() {
                    contents.extend_from_slice(b"\x1b[?1049h");
                }
                contents.extend_from_slice(screen.contents_formatted().as_bytes());
                if screen.bracketed_paste() {
                    contents.extend_from_slice(b"\x1b[?2004h");
                }
                screen.set_scrollback(saved_scrollback);
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
    let ids: Vec<_> = state.layout.visible_buffer_ids().into_iter().collect();
    for id in ids {
        send_vim_snapshot(state, id);
        send_buffer_history_snapshots(state, id);
    }
}

fn send_client_history_snapshots(state: &mut SessionState) {
    let mut seen = HashSet::new();
    let buffers: Vec<_> = state
        .layout
        .visible_buffer_ids()
        .into_iter()
        .filter(|buffer| seen.insert(*buffer) && state.history_offsets.contains_key(buffer))
        .collect();
    for buffer in buffers {
        send_buffer_history_snapshots(state, buffer);
    }
}

fn send_buffer_history_snapshots(state: &mut SessionState, buffer_id: u64) {
    let Some(&requested_offset) = state.history_offsets.get(&buffer_id) else {
        return;
    };
    let Some(chunks) = history_snapshot(state, buffer_id, requested_offset) else {
        return;
    };
    for client in state.clients.values() {
        for payload in &chunks {
            if let Err(TrySendError::Full(_)) = client
                .sender
                .try_send(Outgoing::HistorySnapshot(payload.clone()))
            {
                client.snapshot_only.store(true, Ordering::Release);
                client.refresh_pending.store(false, Ordering::Release);
                break;
            }
        }
    }
}

fn resize_scrollback(
    old_scrollback: usize,
    old_top: usize,
    dropped_rows: usize,
    new_history_len: usize,
) -> usize {
    if old_scrollback == 0 {
        return 0;
    }
    new_history_len.saturating_sub(old_top.saturating_sub(dropped_rows))
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
            let screen = buffer.parser.screen_mut();
            let old_scrollback = screen.scrollback();
            let old_top = screen.scrollback_len().saturating_sub(old_scrollback);
            let was_alternate = screen.alternate_screen();
            buffer.parser.set_size(
                NonZeroU16::new(rows).expect("pane rows are at least one"),
                NonZeroU16::new(cols).expect("pane columns are at least one"),
            );
            let dropped_rows =
                std::mem::take(&mut buffer.parser.callbacks_mut().dropped_primary_rows);
            let screen = buffer.parser.screen_mut();
            let dropped_rows = if screen.alternate_screen() { 0 } else { dropped_rows };
            let new_scrollback = resize_scrollback(
                old_scrollback,
                old_top,
                dropped_rows,
                screen.scrollback_len(),
            );
            screen.set_scrollback(new_scrollback);
            let alternate_changed = was_alternate != screen.alternate_screen();
            buffer.vim.history_changed(screen, Some(dropped_rows), alternate_changed);
            buffer.vim.resized(screen);
            let offset = screen.scrollback();
            state.history_offsets.insert(id, offset);
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
        state.history_offsets.remove(id);
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
            if let Outgoing::Yank(bytes) = message {
                hold_yank(&mut lock_state(&session), id, bytes);
            }
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
        Outgoing::Yank(bytes) => write_frame(stream, YANK, &bytes),
        Outgoing::Layout(snapshot) => write_frame(stream, WINDOW_STATE, &snapshot),
        Outgoing::HistorySnapshot(bytes) => write_frame(stream, HISTORY_SNAPSHOT, &bytes),
        Outgoing::VimState(bytes) => write_frame(stream, VIM_STATE, &bytes),
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
            let mut state = lock_state(session);
            let mut queued_yanks = Vec::new();
            let mut closed = false;
            loop {
                match receiver.try_recv() {
                    Ok(Outgoing::Yank(bytes)) => queued_yanks.push(bytes),
                    Ok(Outgoing::Close) => {
                        closed = true;
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
            if closed {
                return write_frame(stream, CLOSE, &[]);
            }
            prepend_queued_yanks(&mut state, id, queued_yanks);
            refresh_pending.store(false, Ordering::Release);
            recovery_snapshot(&mut state, id)
        };
        for message in snapshots {
            write_message(stream, message)?;
        }

        match receiver.try_recv() {
            Ok(Outgoing::Yank(bytes)) => {
                hold_yank(&mut lock_state(session), id, bytes);
                refresh_pending.store(false, Ordering::Release);
            }
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

fn prepend_queued_yanks(state: &mut SessionState, client_id: u64, mut queued: Vec<Vec<u8>>) {
    if let Some(client) = state.clients.get_mut(&client_id) {
        queued.append(&mut client.pending_yanks);
        client.pending_yanks = queued;
    }
}

fn recovery_snapshot(state: &mut SessionState, client_id: u64) -> Vec<Outgoing> {
    let mut messages = vec![Outgoing::Layout(
        state.layout.snapshot(&state.order, &state.buffer_names),
    )];
    let pending_yanks = state
        .clients
        .get_mut(&client_id)
        .map(|client| std::mem::take(&mut client.pending_yanks))
        .unwrap_or_default();
    messages.extend(pending_yanks.into_iter().map(Outgoing::Yank));
    let mut seen = HashSet::new();
    for buffer_id in state.layout.visible_buffer_ids() {
        if !seen.insert(buffer_id) {
            continue;
        }
        let Some(buffer) = state.buffers.get_mut(&buffer_id) else {
            continue;
        };
        let screen = buffer.parser.screen_mut();
        let saved_scrollback = screen.scrollback();
        screen.set_scrollback(0);
        let (rows, cols) = screen.size();
        let mut contents = Vec::new();
        if screen.alternate_screen() {
            contents.extend_from_slice(b"\x1b[?1049h");
        }
        contents.extend_from_slice(screen.state_formatted().as_bytes());
        if screen.bracketed_paste() {
            contents.extend_from_slice(b"\x1b[?2004h");
        }
        screen.set_scrollback(saved_scrollback);
        let mut offset = 0;
        let mut first = true;
        loop {
            let end = (offset + MAX_FRAME - 12).min(contents.len());
            let mut payload = Vec::with_capacity(12 + end - offset);
            payload.extend_from_slice(&buffer_id.to_be_bytes());
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
        let vim_payload = state
            .buffers
            .get_mut(&buffer_id)
            .and_then(|buffer| vim_snapshot_payload(buffer, buffer_id));
        if let Some(payload) = vim_payload {
            messages.push(Outgoing::VimState(payload));
        }
        if let Some(&history_offset) = state.history_offsets.get(&buffer_id) {
            if let Some(chunks) = history_snapshot(state, buffer_id, history_offset) {
                messages.extend(chunks.into_iter().map(Outgoing::HistorySnapshot));
            }
        }
    }
    for buffer_id in hidden_buffer_ids(state.buffers.keys().copied(), &seen) {
        if let Some(buffer) = state.buffers.get_mut(&buffer_id) {
            if let Some(payload) = vim_snapshot_payload(buffer, buffer_id) {
                messages.push(Outgoing::VimState(payload));
            }
        }
    }
    messages
}

fn hidden_buffer_ids(buffer_ids: impl Iterator<Item = u64>, visible: &HashSet<u64>) -> Vec<u64> {
    buffer_ids.filter(|id| !visible.contains(id)).collect()
}


fn history_snapshot(
    state: &mut SessionState,
    buffer_id: u64,
    requested_offset: usize,
) -> Option<Vec<Vec<u8>>> {
    let buffer = state.buffers.get_mut(&buffer_id)?;
    let screen = buffer.parser.screen_mut();
    let previous_offset = screen.scrollback();
    screen.set_scrollback(requested_offset);
    let (rows, cols) = screen.size();
    let actual_offset = screen.scrollback() as u32;
    let contents = if actual_offset == 0 {
        Vec::new()
    } else {
        screen.contents_formatted().into_bytes()
    };
    screen.set_scrollback(previous_offset);
    Some(history_snapshot_chunks(
        buffer_id,
        rows.get(),
        cols.get(),
        actual_offset,
        &contents,
    ))
}

fn history_snapshot_chunks(
    buffer_id: u64,
    rows: u16,
    cols: u16,
    actual_offset: u32,
    contents: &[u8],
) -> Vec<Vec<u8>> {
    let mut chunks = Vec::new();
    let mut offset = 0;
    loop {
        let end = (offset + MAX_FRAME - 16).min(contents.len());
        let mut payload = Vec::with_capacity(16 + end - offset);
        payload.extend_from_slice(&buffer_id.to_be_bytes());
        payload.extend_from_slice(&rows.to_be_bytes());
        payload.extend_from_slice(&cols.to_be_bytes());
        let continuation = if offset == 0 { 0 } else { 1 << 31 };
        payload.extend_from_slice(&(actual_offset | continuation).to_be_bytes());
        payload.extend_from_slice(&contents[offset..end]);
        chunks.push(payload);
        if end == contents.len() {
            break;
        }
        offset = end;
    }
    chunks
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

#[cfg(test)]
mod history_tests {
    use super::*;
    #[test]
    fn insert_passthrough_does_not_publish_vim_state() {
        let forwarded = [crate::vim::Effect::Forward(b"ordinary input".to_vec())];
        assert!(is_insert_passthrough(true, true, &forwarded));
        assert!(is_insert_passthrough(true, true, &[]));
        assert!(!is_insert_passthrough(false, false, &forwarded));
        assert!(!is_insert_passthrough(true, false, &forwarded));
    }

    #[test]
    fn yank_flash_scrollback_tracks_history_growth_and_eviction() {
        let mut flash = crate::vim::YankFlash {
            range: crate::vim::CellRange {
                start: crate::vim::Position { row: 6, col: 0 },
                end: crate::vim::Position { row: 6, col: 2 },
            },
            kind: crate::vim::VisualKind::Character,
            viewport_top: 4,
        };
        assert_eq!(flash_scrollback(flash, 10), 6);
        assert_eq!(flash_scrollback(flash, 13), 9);
        flash.viewport_top = flash.viewport_top.saturating_sub(2);
        flash.range.start.row -= 2;
        flash.range.end.row -= 2;
        assert_eq!(flash_scrollback(flash, 11), 9);
    }

    #[test]
    fn insert_mode_transitions_and_search_updates_publish_vim_state() {
        let search = [crate::vim::Effect::SearchPrompt("query".into())];
        assert!(!is_insert_passthrough(true, false, &search));
        assert!(!is_insert_passthrough(false, false, &search));
    }


    #[test]
    fn server_scrollback_is_bounded_and_csi_three_j_clears_it() {
        let mut parser = vt100::Parser::new(
            NonZeroU16::new(2).unwrap(),
            NonZeroU16::new(8).unwrap(),
            2000,
        );
        assert_eq!(parser.screen().scrollback_capacity(), 2000);
        parser.process(b"one\r\ntwo\r\nthree\r\nfour\r\nfive");
        assert!(parser.screen().scrollback_len() > 0);
        parser.process(b"\x1b[3J\x1b[3J");
        assert_eq!(parser.screen().scrollback_len(), 0);
        assert_eq!(parser.screen().scrollback(), 0);
    }

    #[test]
    fn alternate_screen_round_trip_restores_primary_history() {
        let mut parser = vt100::Parser::new(
            NonZeroU16::new(2).unwrap(),
            NonZeroU16::new(8).unwrap(),
            2000,
        );
        parser.process(b"one\r\ntwo\r\nthree\r\n");
        let history_len = parser.screen().scrollback_len();
        assert!(history_len > 0);
        parser.process(b"\x1b[?1049h\x1b[?1049l");
        assert!(!parser.screen().alternate_screen());
        assert_eq!(parser.screen().scrollback_len(), history_len);
    }

    #[test]
    fn ris_reset_clears_terminal_and_is_detected_across_reads() {
        let mut parser = vt100::Parser::new_with_callbacks(
            NonZeroU16::new(2).unwrap(),
            NonZeroU16::new(8).unwrap(),
            20,
            TerminalReplies::default(),
        );
        parser.process(b"before\r\nscrollback");
        assert!(parser.screen().scrollback_len() > 0);
        assert!(!parser.callbacks_mut().observe_ris(b"\x1b"));
        assert!(parser.callbacks_mut().observe_ris(b"c"));
        parser.process(b"\x1bc");
        assert_eq!(parser.screen().scrollback_len(), 0);
        assert!(!parser.screen().alternate_screen());
        assert!(!parser
            .screen()
            .contents_formatted()
            .contains("before"));
    }

    #[test]
    fn ris_after_unterminated_osc_is_detected_but_st_after_osc_is_not() {
        let mut callbacks = TerminalReplies::default();
        assert!(!callbacks.observe_ris(b"\x1b]title"));
        assert!(callbacks.observe_ris(b"\x1bc"));

        let mut callbacks = TerminalReplies::default();
        assert!(!callbacks.observe_ris(b"\x1b]title\x1b\\"));
        assert!(!callbacks.observe_ris(b"\x07"));
    }

    #[test]
    fn alternate_screen_scrolls_do_not_count_as_primary_evictions() {
        let mut callbacks = TerminalReplies::default();
        callbacks.record_primary_eviction(true);
        assert_eq!(callbacks.dropped_primary_rows, 0);
        callbacks.record_primary_eviction(false);
        assert_eq!(callbacks.dropped_primary_rows, 1);
    }

    #[test]
    fn resize_keeps_scrolled_absolute_top_and_live_view_at_bottom() {
        assert_eq!(resize_scrollback(2, 5, 1, 6), 2);
        assert_eq!(resize_scrollback(0, 5, 0, 9), 0);
    }
    #[test]
    fn zombie_owner_is_pruned_even_when_its_session_matches() {
        let mut fields = vec!["0"; 20];
        fields[0] = "Z";
        fields[1] = "1";
        fields[3] = "42";
        fields[19] = "99";
        let owner_stat = parse_process_stat(&format!(
            "123 (nvim worker) {}",
            fields.join(" ")
        ))
        .unwrap();
        assert_eq!(owner_stat.state, 'Z');

        let child = ProcessStat {
            state: 'S',
            parent_pid: 1,
            session_id: 42,
            start_time: 10,
        };
        let owner = crate::vim_control::ProcessIdentity {
            pid: 123,
            start_time: 99,
        };
        let child_identity = crate::vim_control::ProcessIdentity {
            pid: 42,
            start_time: 10,
        };
        assert!(!process_is_owned_with(owner, Some(child_identity), Some(42), |pid| {
            match pid {
                123 => Some(owner_stat),
                42 => Some(child),
                _ => None,
            }
        }));
    }

    #[test]
    fn process_ownership_checks_start_time_and_descendant_chain() {
        let child = ProcessStat {
            state: 'S',
            parent_pid: 1,
            session_id: 42,
            start_time: 10,
        };
        let intermediate = ProcessStat {
            state: 'S',
            parent_pid: 42,
            session_id: 99,
            start_time: 30,
        };
        let owner_stat = ProcessStat {
            state: 'S',
            parent_pid: 43,
            session_id: 99,
            start_time: 40,
        };
        let owner = crate::vim_control::ProcessIdentity {
            pid: 44,
            start_time: 40,
        };
        let child_identity = crate::vim_control::ProcessIdentity {
            pid: 42,
            start_time: 10,
        };
        let mut get_stat = |pid| match pid {
            42 => Some(child),
            43 => Some(intermediate),
            44 => Some(owner_stat),
            _ => None,
        };
        assert!(process_is_owned_with(owner, Some(child_identity), Some(42), &mut get_stat));

        let reused_pid = crate::vim_control::ProcessIdentity {
            pid: 44,
            start_time: 41,
        };
        assert!(!process_is_owned_with(
            reused_pid,
            Some(child_identity),
            Some(42),
            get_stat
        ));
    }

    #[test]
    fn queued_yanks_precede_newer_pending_yanks_during_recovery() {
        let (stream, _peer) = UnixStream::pair().unwrap();
        let (sender, _receiver) = mpsc::sync_channel(1);
        let mut clients = HashMap::new();
        clients.insert(
            7,
            Client {
                sender,
                stream,
                rows: 24,
                cols: 80,
                resize_step: 5,
                snapshot_only: Arc::new(AtomicBool::new(true)),
                refresh_pending: Arc::new(AtomicBool::new(false)),
                pending_yanks: vec![b"newer".to_vec()],
            },
        );
        let mut state = SessionState {
            clients,
            buffers: HashMap::new(),
            buffer_names: HashMap::new(),
            order: Vec::new(),
            layout: WindowLayout::new(1),
            history_offsets: HashMap::new(),
            next_buffer: 2,
        };

        prepend_queued_yanks(
            &mut state,
            7,
            vec![b"older-1".to_vec(), b"older-2".to_vec()],
        );
        assert_eq!(
            state.clients[&7].pending_yanks,
            [b"older-1".to_vec(), b"older-2".to_vec(), b"newer".to_vec()]
        );
    }


    #[test]
    fn recovery_retains_a_yank_dequeued_before_snapshot() {
        let (stream, _peer) = UnixStream::pair().unwrap();
        let (sender, receiver) = mpsc::sync_channel(1);
        let client_id = 7;
        assert!(sender
            .try_send(Outgoing::Yank(b"copied".to_vec()))
            .is_ok());
        let Outgoing::Yank(bytes) = receiver.try_recv().unwrap() else {
            unreachable!();
        };
        let mut clients = HashMap::new();
        clients.insert(
            client_id,
            Client {
                sender,
                stream,
                rows: 24,
                cols: 80,
                resize_step: 5,
                snapshot_only: Arc::new(AtomicBool::new(true)),
                refresh_pending: Arc::new(AtomicBool::new(false)),
                pending_yanks: Vec::new(),
            },
        );
        let mut state = SessionState {
            clients,
            buffers: HashMap::new(),
            buffer_names: HashMap::new(),
            order: Vec::new(),
            layout: WindowLayout::new(1),
            history_offsets: HashMap::new(),
            next_buffer: 2,
        };

        hold_yank(&mut state, client_id, b"copied".to_vec());
        let messages = recovery_snapshot(&mut state, client_id);
        assert!(matches!(
            messages.get(1),
            Some(Outgoing::Yank(bytes)) if bytes.as_slice() == b"copied"
        ));
        assert!(state.clients[&client_id].pending_yanks.is_empty());
    }


    #[test]
    fn snapshot_recovery_includes_hidden_buffers_for_state_sync() {
        let visible = HashSet::from([1, 3]);
        assert_eq!(
            hidden_buffer_ids([1, 2, 3].into_iter(), &visible),
            vec![2]
        );
    }
    #[test]
    fn zero_offset_history_snapshot_contains_only_its_header() {
        let chunks = history_snapshot_chunks(9, 3, 12, 0, &[]);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].len(), 16);
        assert_eq!(&chunks[0][..8], &9_u64.to_be_bytes());
        assert_eq!(&chunks[0][8..10], &3_u16.to_be_bytes());
        assert_eq!(&chunks[0][10..12], &12_u16.to_be_bytes());
        assert_eq!(&chunks[0][12..16], &0_u32.to_be_bytes());
    }
}
