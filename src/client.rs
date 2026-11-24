use std::process::{Command, Stdio};
use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::Request;
use crate::input::{InputAction, InputDecoder};
use crate::render::{BUFFER_OUTPUT, BUFFER_SNAPSHOT, Renderer, VIM_STATE};
use crate::runtime::{SessionPaths, read_response, write_request};
use crate::server::{
    CLOSE, HISTORY_SNAPSHOT, HISTORY_VIEW, INPUT, RENAME_BUFFER, RESIZE, VIM_FLUSH, VIM_KEY,
    WINDOW_STATE, YANK, read_frame, write_frame,
};

const PROMPT_ESCAPE_TIMEOUT: Duration = Duration::from_millis(40);
const PROMPT_KEYS: &[&[u8]] = &[b"\x1b\x7f", b"\x1b\x08", b"\x1b[1;5D", b"\x1b[1;5C"];

struct RawMode;
struct TerminalScreen;

impl RawMode {
    fn enable() -> io::Result<Self> {
        crossterm::terminal::enable_raw_mode()?;
        Ok(Self)
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

impl TerminalScreen {
    fn enter() -> io::Result<Self> {
        crossterm::execute!(
            io::stdout(),
            crossterm::terminal::EnterAlternateScreen,
            crossterm::cursor::Hide
        )?;
        Ok(Self)
    }
}

impl Drop for TerminalScreen {
    fn drop(&mut self) {
        let _ = crossterm::execute!(
            io::stdout(),
            crossterm::event::DisableBracketedPaste,
            crossterm::style::SetAttribute(crossterm::style::Attribute::Reset),
            crossterm::cursor::Show,
            crossterm::terminal::LeaveAlternateScreen
        );
    }
}

struct RenamePrompt {
    buffer: u64,
    name: String,
    cursor: usize,
    pending_escape: Vec<u8>,
    escape_started: Option<Instant>,
    pending_utf8: Vec<u8>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PromptResult {
    Editing,
    Commit,
    Cancel,
}

impl RenamePrompt {
    fn new(buffer: u64, name: String) -> Self {
        let cursor = name.len();
        Self {
            buffer,
            name,
            cursor,
            pending_escape: Vec::new(),
            escape_started: None,
            pending_utf8: Vec::new(),
        }
    }

    fn display(&self) -> (String, usize) {
        let prefix = format!("Rename {}: ", self.buffer);
        let cursor = prefix.len() + self.cursor;
        (prefix + &self.name, cursor)
    }

    fn feed(&mut self, bytes: &[u8]) -> (PromptResult, usize) {
        for (index, &byte) in bytes.iter().enumerate() {
            if !self.pending_escape.is_empty() {
                self.pending_escape.push(byte);
                if PROMPT_KEYS.contains(&self.pending_escape.as_slice()) {
                    match self.pending_escape.as_slice() {
                        b"\x1b\x7f" | b"\x1b\x08" => self.delete_previous_word(),
                        b"\x1b[1;5D" => self.cursor = self.previous_word(),
                        b"\x1b[1;5C" => self.cursor = self.next_word(),
                        _ => unreachable!(),
                    }
                    self.pending_escape.clear();
                    self.escape_started = None;
                } else if !PROMPT_KEYS.iter().any(|key| key.starts_with(&self.pending_escape)) {
                    return (PromptResult::Cancel, bytes.len());
                }
                continue;
            }
            match byte {
                b'\r' | b'\n' => return (PromptResult::Commit, index + 1),
                b'\x1b' => {
                    self.pending_utf8.clear();
                    self.pending_escape.push(byte);
                    self.escape_started = Some(Instant::now());
                }
                b'\x03' => return (PromptResult::Cancel, bytes.len()),
                b'\x08' | b'\x7f' => {
                    self.pending_utf8.clear();
                    if let Some((start, _)) = self.name[..self.cursor].char_indices().next_back() {
                        self.name.drain(start..self.cursor);
                        self.cursor = start;
                    }
                }
                b'\x17' => {
                    self.pending_utf8.clear();
                    self.delete_previous_word();
                }
                b'\x15' => {
                    self.pending_utf8.clear();
                    self.name.clear();
                    self.cursor = 0;
                }
                byte if byte.is_ascii_control() => self.pending_utf8.clear(),
                byte if byte.is_ascii() => {
                    self.name.insert(self.cursor, byte as char);
                    self.cursor += 1;
                }
                byte => self.push_utf8_byte(byte),
            }
        }
        (PromptResult::Editing, bytes.len())
    }

    fn previous_word(&self) -> usize {
        self.name[..self.cursor]
            .trim_end_matches(char::is_whitespace)
            .trim_end_matches(|character: char| !character.is_whitespace())
            .len()
    }

    fn delete_previous_word(&mut self) {
        let start = self.previous_word();
        self.name.drain(start..self.cursor);
        self.cursor = start;
    }

    fn next_word(&self) -> usize {
        let suffix = &self.name[self.cursor..];
        let word_end = suffix.find(char::is_whitespace).unwrap_or(suffix.len());
        let rest = &suffix[word_end..];
        self.cursor + word_end + rest.len() - rest.trim_start_matches(char::is_whitespace).len()
    }

    fn push_utf8_byte(&mut self, byte: u8) {
        self.pending_utf8.push(byte);
        match std::str::from_utf8(&self.pending_utf8) {
            Ok(character) => {
                if !character.chars().any(char::is_control) {
                    self.name.insert_str(self.cursor, character);
                    self.cursor += character.len();
                }
                self.pending_utf8.clear();
            }
            Err(error) if error.error_len().is_none() => {}
            Err(_) => self.pending_utf8.clear(),
        }
    }
}

pub(crate) fn attach(
    paths: &SessionPaths,
    name: &str,
    config: crate::config::Config,
) -> io::Result<()> {
    let (cols, rows) = terminal_size();
    let mut writer = UnixStream::connect(paths.socket_path(name))?;
    writer.set_read_timeout(Some(Duration::from_secs(30)))?;
    writer.set_write_timeout(Some(Duration::from_secs(5)))?;
    write_request(
        &mut writer,
        &Request {
            op: "attach".into(),
            name: name.into(),
            rows: rows.max(1),
            cols: cols.max(1),
            resize_step: config.resize.step_percent,
        },
    )?;
    let mut reader = std::io::BufReader::new(writer.try_clone()?);
    let response = read_response(&mut reader)?;
    if !response.ok {
        return Err(io::Error::other(response.error));
    }
    ensure_resize_support(config.resize.step_percent, response.supports_resize_step)?;

    let _raw_mode = RawMode::enable()?;
    let _terminal_screen = TerminalScreen::enter()?;
    crate::debug_log(format_args!("client connected session={name} size={cols}x{rows}"));
    writer.set_read_timeout(None)?;
    writer.set_write_timeout(None)?;
    let interrupted = Arc::new(AtomicBool::new(false));
    for signal in [
        signal_hook::consts::SIGTERM,
        signal_hook::consts::SIGHUP,
        signal_hook::consts::SIGINT,
    ] {
        signal_hook::flag::register(signal, Arc::clone(&interrupted))?;
    }

    let stdin = io::stdin();
    let stdin_fd = stdin.as_raw_fd();
    let server_fd = reader.get_ref().as_raw_fd();
    let stdout = io::stdout();
    let mut output = stdout.lock();
    let mut input = stdin.lock();
    let mut last_size = (cols, rows);
    let mut last_resize_check = Instant::now();
    let mut input_buffer = [0u8; 16 * 1024];
    let mut renderer = Renderer::with_config(config.clone());
    let mut input_decoder = InputDecoder::configured(&config);
    let mut rename_prompt = None;
    let mut vim_escape_started = None;

    loop {
        if interrupted.load(Ordering::Acquire) {
            crate::debug_log(format_args!("client interrupted session={name}"));
            let _ = write_frame(&mut writer, 3, &[]);
            break;
        }
        if rename_prompt.as_ref().is_some_and(|prompt: &RenamePrompt| {
            prompt.escape_started.is_some_and(|started| started.elapsed() >= PROMPT_ESCAPE_TIMEOUT)
        }) {
            rename_prompt = None;
            renderer.set_prompt(None);
            renderer.draw(&mut output, last_size.0, last_size.1)?;
        }
        if let Some(action) = input_decoder.expire(Instant::now()) {
            let keep_escape_timeout =
                matches!(&action, InputAction::Bytes(bytes) if bytes.ends_with(&[0x1b]));
            handle_input_action(
                &mut writer,
                &mut input_decoder,
                &mut renderer,
                &mut rename_prompt,
                &mut output,
                last_size,
                action,
            )?;
            if !keep_escape_timeout {
                vim_escape_started = None;
            }
        }
        if vim_escape_started
            .is_some_and(|started: Instant| started.elapsed() >= PROMPT_ESCAPE_TIMEOUT)
        {
            write_frame(&mut writer, VIM_FLUSH, &[])?;
            vim_escape_started = None;
        }
        let now = Instant::now();
        let mut timeout = input_decoder.remaining_timeout(now).unwrap_or(Duration::from_millis(100));
        if let Some(started) = rename_prompt.as_ref().and_then(|prompt| prompt.escape_started) {
            timeout = timeout.min(PROMPT_ESCAPE_TIMEOUT.saturating_sub(now.duration_since(started)));
        }
        if let Some(started) = vim_escape_started {
            timeout = timeout.min(PROMPT_ESCAPE_TIMEOUT.saturating_sub(now.duration_since(started)));
        }
        let server_data_buffered = !reader.buffer().is_empty();
        let timeout = if server_data_buffered { 0 } else { timeout.as_millis().clamp(1, 100) as i32 };
        let mut descriptors = [
            libc::pollfd {
                fd: stdin_fd,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: server_fd,
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let ready =
            unsafe { libc::poll(descriptors.as_mut_ptr(), descriptors.len() as _, timeout) };
        if ready < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }

        if descriptors[0].revents & libc::POLLIN != 0 {
            let length = input.read(&mut input_buffer)?;
            if length == 0 {
                crate::debug_log(format_args!("client terminal input closed session={name}"));
                let _ = write_frame(&mut writer, 3, &[]);
                break;
            }
            send_input(
                &mut writer,
                &mut input_decoder,
                &mut renderer,
                &mut rename_prompt,
                &mut output,
                last_size,
                &input_buffer[..length],
            )?;
            let raw_input = &input_buffer[..length];
            vim_escape_started =
                ends_with_unpaired_escape(raw_input, vim_escape_started.is_some()).then(Instant::now);
        }
        if descriptors[1].revents & libc::POLLIN != 0 || !reader.buffer().is_empty() {
            match read_frame(&mut reader)? {
                Some((BUFFER_SNAPSHOT, bytes)) => {
                    renderer.apply_frame(BUFFER_SNAPSHOT, &bytes)?;
                    renderer.draw(&mut output, last_size.0, last_size.1)?;
                }
                Some((BUFFER_OUTPUT, bytes)) => {
                    renderer.apply_frame(BUFFER_OUTPUT, &bytes)?;
                    renderer.draw(&mut output, last_size.0, last_size.1)?;
                }
                Some((HISTORY_SNAPSHOT, bytes)) => {
                    renderer.apply_frame(HISTORY_SNAPSHOT, &bytes)?;
                    renderer.draw(&mut output, last_size.0, last_size.1)?;
                }
                Some((YANK, bytes)) => report_yank(&bytes, &mut output, last_size),
                Some((VIM_STATE, bytes)) => {
                    renderer.apply_frame(VIM_STATE, &bytes)?;
                    renderer.draw(&mut output, last_size.0, last_size.1)?;
                }
                Some((WINDOW_STATE, bytes)) => {
                    let snapshot = serde_json::from_slice(&bytes)
                        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
                    renderer.set_layout(snapshot);
                    if rename_prompt
                        .as_ref()
                        .is_some_and(|prompt: &RenamePrompt| !renderer.has_buffer(prompt.buffer))
                    {
                        rename_prompt = None;
                        renderer.set_prompt(None);
                    }
                    renderer.draw(&mut output, last_size.0, last_size.1)?;
                }
                Some((CLOSE, _)) => {
                    crate::debug_log(format_args!("client received close session={name}"));
                    break;
                }
                None => {
                    crate::debug_log(format_args!("client server connection closed session={name}"));
                    break;
                }
                Some(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "invalid server frame",
                    ));
                }
            }
        }
        if descriptors[1].revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
            crate::debug_log(format_args!("client server poll closed session={name} revents={}", descriptors[1].revents));
            break;
        }

        if last_resize_check.elapsed() >= Duration::from_millis(200) {
            last_resize_check = Instant::now();
            let size = terminal_size();
            if size != last_size {
                crate::debug_log(format_args!("client terminal resize session={name} size={}x{}", size.0, size.1));
                let mut payload = Vec::with_capacity(4);
                payload.extend_from_slice(&size.1.max(1).to_be_bytes());
                payload.extend_from_slice(&size.0.max(1).to_be_bytes());
                write_frame(&mut writer, RESIZE, &payload)?;
                last_size = size;
                renderer.draw(&mut output, last_size.0, last_size.1)?;
            }
        }
    }
    Ok(())
}

fn ensure_resize_support(step: u8, supported: bool) -> io::Result<()> {
    if step != 5 && !supported {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "session server does not support per-client resize_step; set resize.step_percent to 5 to attach",
        ));
    }
    Ok(())
}

fn ends_with_unpaired_escape(bytes: &[u8], previous_pending: bool) -> bool {
    let trailing = bytes.iter().rev().take_while(|&&byte| byte == 0x1b).count();
    let previous = if previous_pending && bytes.iter().all(|&byte| byte == 0x1b) {
        1
    } else {
        0
    };
    (trailing + previous) % 2 == 1
}

fn send_input(
    server: &mut UnixStream,
    decoder: &mut InputDecoder,
    renderer: &mut Renderer,
    rename_prompt: &mut Option<RenamePrompt>,
    output: &mut impl io::Write,
    size: (u16, u16),
    bytes: &[u8],
) -> io::Result<()> {
    if rename_prompt.is_some() {
        return handle_prompt_input(
            server,
            decoder,
            renderer,
            rename_prompt,
            output,
            size,
            bytes,
        );
    }
    let context = renderer.input_context();
    if !decoder.should_decode_with_context(bytes, context) {
        return write_frame(server, INPUT, bytes);
    }
    let actions = decoder.feed_with_context(bytes, context);
    for action in actions {
        handle_input_action(
            server,
            decoder,
            renderer,
            rename_prompt,
            output,
            size,
            action,
        )?;
    }
    Ok(())
}

fn handle_input_action(
    server: &mut UnixStream,
    decoder: &mut InputDecoder,
    renderer: &mut Renderer,
    rename_prompt: &mut Option<RenamePrompt>,
    output: &mut impl io::Write,
    size: (u16, u16),
    action: InputAction,
) -> io::Result<()> {
    if rename_prompt.is_some() {
        return match action {
            InputAction::Bytes(bytes) => handle_prompt_input(
                server,
                decoder,
                renderer,
                rename_prompt,
                output,
                size,
                &bytes,
            ),
            InputAction::Command(_) => Ok(()),
        };
    }
    match action {
        InputAction::Bytes(bytes) => write_frame(server, INPUT, &bytes),
        InputAction::Command(command)
            if command.kind == RENAME_BUFFER && command.payload.is_empty() =>
        {
            if let Some((buffer, name)) = renderer.active_buffer() {
                decoder.reset();
                let prompt = RenamePrompt::new(buffer, name);
                renderer.set_prompt(Some(prompt.display()));
                *rename_prompt = Some(prompt);
                renderer.draw(output, size.0, size.1)?;
            }
            Ok(())
        }
        InputAction::Command(command) => {
            write_frame(server, command.kind, command.payload)?;
            if command.kind == VIM_KEY && command.payload == b"\x1b" {
                write_frame(server, VIM_FLUSH, &[])?;
            }
            Ok(())
        }
    }
}
#[cfg(test)]
mod handshake_tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn response_and_initial_frame_in_one_write_are_both_read() {
        let (mut sender, receiver) = UnixStream::pair().unwrap();
        let mut payload = b"{\"ok\":true,\"name\":\"test\",\"supports_resize_step\":true}\n".to_vec();
        payload.extend_from_slice(&[BUFFER_SNAPSHOT, 0, 0, 0, 12]);
        payload.extend_from_slice(&1u64.to_be_bytes());
        payload.extend_from_slice(&24u16.to_be_bytes());
        payload.extend_from_slice(&80u16.to_be_bytes());
        sender.write_all(&payload).unwrap();
        drop(sender);

        let mut reader = std::io::BufReader::new(receiver);
        let response = read_response(&mut reader).unwrap();
        assert!(response.ok);
        let (kind, frame) = read_frame(&mut reader).unwrap().unwrap();
        assert_eq!(kind, BUFFER_SNAPSHOT);
        assert_eq!(&frame[..8], &1u64.to_be_bytes());
        assert_eq!(&frame[8..10], &24u16.to_be_bytes());
        assert_eq!(&frame[10..12], &80u16.to_be_bytes());
        assert!(read_frame(&mut reader).unwrap().is_none());
    }
}

fn handle_prompt_input(
    server: &mut UnixStream,
    decoder: &mut InputDecoder,
    renderer: &mut Renderer,
    rename_prompt: &mut Option<RenamePrompt>,
    output: &mut impl io::Write,
    size: (u16, u16),
    bytes: &[u8],
) -> io::Result<()> {
    let Some(prompt) = rename_prompt.as_mut() else {
        return send_input(
            server,
            decoder,
            renderer,
            rename_prompt,
            output,
            size,
            bytes,
        );
    };
    let (result, consumed) = prompt.feed(bytes);
    match result {
        PromptResult::Editing => {
            renderer.set_prompt(Some(prompt.display()));
            renderer.draw(output, size.0, size.1)?;
        }
        PromptResult::Commit => {
            let buffer = prompt.buffer;
            let name = prompt.name.as_bytes().to_vec();
            decoder.reset();
            *rename_prompt = None;
            renderer.set_prompt(None);
            let mut payload = Vec::with_capacity(8 + name.len());
            payload.extend_from_slice(&buffer.to_be_bytes());
            payload.extend_from_slice(&name);
            write_frame(server, RENAME_BUFFER, &payload)?;
            renderer.draw(output, size.0, size.1)?;
            if consumed < bytes.len() {
                send_input(
                    server,
                    decoder,
                    renderer,
                    rename_prompt,
                    output,
                    size,
                    &bytes[consumed..],
                )?;
            }
        }
        PromptResult::Cancel => {
            decoder.reset();
            *rename_prompt = None;
            renderer.set_prompt(None);
            renderer.draw(output, size.0, size.1)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{LayoutSnapshot, WindowNode};

    fn renderer_with_buffer() -> Renderer {
        let mut renderer = Renderer::default();
        renderer.set_layout(LayoutSnapshot {
            tree: WindowNode::Window { id: 1, buffer: 7 },
            focused_window: 1,
            active_buffer: Some(7),
            buffers: vec![7],
            buffer_names: std::collections::HashMap::from([(7, "7".into())]),
        });
        renderer
    }

    fn send(
        server: &mut UnixStream,
        decoder: &mut InputDecoder,
        renderer: &mut Renderer,
        prompt: &mut Option<RenamePrompt>,
        output: &mut Vec<u8>,
        bytes: &[u8],
    ) {
        send_input(server, decoder, renderer, prompt, output, (20, 3), bytes).unwrap();
    }


    #[test]
    fn semantic_normal_escape_flushes_vim_escape_immediately() {
        let (mut client, mut peer) = UnixStream::pair().unwrap();
        let mut decoder = InputDecoder::default();
        let mut renderer = renderer_with_buffer();
        let mut prompt = None;
        let mut output = Vec::new();

        handle_input_action(
            &mut client,
            &mut decoder,
            &mut renderer,
            &mut prompt,
            &mut output,
            (20, 3),
            InputAction::Command(crate::input::InputCommand {
                kind: VIM_KEY,
                payload: b"\x1b",
            }),
        )
        .unwrap();

        assert_eq!(crate::server::read_frame(&mut peer).unwrap(), Some((VIM_KEY, b"\x1b".to_vec())));
        assert_eq!(crate::server::read_frame(&mut peer).unwrap(), Some((VIM_FLUSH, Vec::new())));
    }
    #[test]
    fn rename_edits_words_at_unicode_cursor_boundaries() {
        let mut prompt = RenamePrompt::new(7, "one 雪  three".into());
        assert!(matches!(prompt.feed(b"\x1b[1;5D").0, PromptResult::Editing));
        assert_eq!(&prompt.name[prompt.cursor..], "three");
        assert!(matches!(prompt.feed(b"\x1b[").0, PromptResult::Editing));
        assert!(matches!(prompt.feed(b"1;5D").0, PromptResult::Editing));
        assert_eq!(&prompt.name[prompt.cursor..], "雪  three");
        assert!(matches!(prompt.feed("山".as_bytes()).0, PromptResult::Editing));
        assert_eq!(prompt.name, "one 山雪  three");
        assert!(matches!(prompt.feed(b"\x1b[1;5C").0, PromptResult::Editing));
        assert_eq!(&prompt.name[prompt.cursor..], "three");
        assert!(matches!(prompt.feed(b"\x1b").0, PromptResult::Editing));
        assert!(matches!(prompt.feed(b"\x7f").0, PromptResult::Editing));
        assert_eq!(prompt.name, "one three");
        assert_eq!(prompt.cursor, 4);
        assert!(matches!(prompt.feed(b"\x17").0, PromptResult::Editing));
        assert_eq!(prompt.name, "three");
        assert_eq!(prompt.cursor, 0);
        assert!(matches!(prompt.feed(b"\x1b\x7f").0, PromptResult::Editing));
        assert_eq!(prompt.name, "three");
    }

    #[test]
    fn ctrl_r_commits_the_name_without_forwarding_prompt_text() {
        let (mut client, mut peer) = UnixStream::pair().unwrap();
        let mut decoder = InputDecoder::default();
        let mut renderer = renderer_with_buffer();
        let mut prompt = None;
        let mut output = Vec::new();

        send(
            &mut client,
            &mut decoder,
            &mut renderer,
            &mut prompt,
            &mut output,
            b"\x12",
        );
        send(
            &mut client,
            &mut decoder,
            &mut renderer,
            &mut prompt,
            &mut output,
            b"\x15renamed \xe9",
        );
        send(
            &mut client,
            &mut decoder,
            &mut renderer,
            &mut prompt,
            &mut output,
            b"\x9b\xaa\r",
        );

        let mut expected = 7u64.to_be_bytes().to_vec();
        expected.extend_from_slice("renamed 雪".as_bytes());
        assert_eq!(
            crate::server::read_frame(&mut peer).unwrap(),
            Some((RENAME_BUFFER, expected))
        );
        assert!(prompt.is_none());
    }

    #[test]
    fn ctrl_u_resets_the_buffer_label_to_its_numeric_id() {
        let (mut client, mut peer) = UnixStream::pair().unwrap();
        let mut decoder = InputDecoder::default();
        let mut renderer = renderer_with_buffer();
        let mut prompt = None;
        let mut output = Vec::new();

        send(
            &mut client,
            &mut decoder,
            &mut renderer,
            &mut prompt,
            &mut output,
            b"\x12",
        );
        send(
            &mut client,
            &mut decoder,
            &mut renderer,
            &mut prompt,
            &mut output,
            b"\x15\r",
        );

        assert_eq!(
            crate::server::read_frame(&mut peer).unwrap(),
            Some((RENAME_BUFFER, 7u64.to_be_bytes().to_vec()))
        );
    }

    #[test]
    fn escape_cancels_the_prompt_without_sending_text_to_the_shell() {
        let (mut client, mut peer) = UnixStream::pair().unwrap();
        let mut decoder = InputDecoder::default();
        let mut renderer = renderer_with_buffer();
        let mut prompt = None;
        let mut output = Vec::new();

        send(
            &mut client,
            &mut decoder,
            &mut renderer,
            &mut prompt,
            &mut output,
            b"\x12",
        );
        send(
            &mut client,
            &mut decoder,
            &mut renderer,
            &mut prompt,
            &mut output,
            b"not-a-command\x1bx",
        );

        peer.set_nonblocking(true).unwrap();
        let mut byte = [0];
        assert_eq!(
            peer.read(&mut byte).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        assert!(prompt.is_none());
    }
    #[test]
    fn history_view_request_encodes_buffer_and_scrollback_offset() {
        let (mut client, mut peer) = UnixStream::pair().unwrap();
        set_history_view(&mut client, 42, 17).unwrap();
        let mut expected = 42u64.to_be_bytes().to_vec();
        expected.extend_from_slice(&17u32.to_be_bytes());
        assert_eq!(
            crate::server::read_frame(&mut peer).unwrap(),
            Some((HISTORY_VIEW, expected))
        );
    }

    #[test]
    fn resize_step_requires_server_capability_only_when_nondefault() {
        assert!(ensure_resize_support(5, false).is_ok());
        assert!(ensure_resize_support(12, true).is_ok());
        assert_eq!(
            ensure_resize_support(12, false).unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
    }

    #[test]
    fn escape_timeout_ignores_complete_alt_escape_sequences() {
        assert!(ends_with_unpaired_escape(b"\x1b", false));
        assert!(!ends_with_unpaired_escape(b"\x1b\x1b", false));
        assert!(!ends_with_unpaired_escape(b"\x1b", true));
        assert!(ends_with_unpaired_escape(b"\x1b\x1b", true));
    }
}

pub(crate) fn set_history_view(
    server: &mut UnixStream,
    buffer: u64,
    scrollback_rows: u32,
) -> io::Result<()> {
    let mut payload = Vec::with_capacity(12);
    payload.extend_from_slice(&buffer.to_be_bytes());
    payload.extend_from_slice(&scrollback_rows.to_be_bytes());
    write_frame(server, HISTORY_VIEW, &payload)
}

pub(crate) fn terminal_size() -> (u16, u16) {
    match crossterm::terminal::size() {
        Ok((cols, rows)) if cols > 0 && rows > 0 => (cols, rows),
        _ => (80, 24),
    }
}
fn set_clipboard(bytes: &[u8]) -> io::Result<()> {
    for selection in ["clipboard", "primary"] {
        let mut child = Command::new("xclip")
            .args(["-selection", selection])
            .stdin(Stdio::piped())
            .spawn()?;
        let Some(mut stdin) = child.stdin.take() else {
            return Err(io::Error::other("xclip stdin unavailable"));
        };
        std::io::Write::write_all(&mut stdin, bytes)?;
        drop(stdin);
        let status = child.wait()?;
        if !status.success() {
            return Err(io::Error::other(format!(
                "xclip {selection} exited with {status}"
            )));
        }
    }
    Ok(())
}
fn report_yank(bytes: &[u8], output: &mut impl io::Write, size: (u16, u16)) {
    if let Err(error) = set_clipboard(bytes) {
        let _ = report_yank_error(error, output, size);
    }
}

fn report_yank_error(
    error: io::Error,
    output: &mut impl io::Write,
    size: (u16, u16),
) -> io::Result<()> {
    let reason = error
        .to_string()
        .chars()
        .map(|character| if character.is_control() { ' ' } else { character })
        .collect::<String>();
    let message = format!("rmux: clipboard yank failed (xclip): {reason}");
    crossterm::queue!(
        output,
        crossterm::cursor::MoveTo(0, size.1.saturating_sub(1)),
        crossterm::terminal::Clear(crossterm::terminal::ClearType::CurrentLine),
        crossterm::style::Print(message)
    )?;
    output.flush()
}

#[cfg(test)]
mod clipboard_tests {
    use super::*;

    #[test]
    fn clipboard_errors_are_reported_in_terminal_status() {
        let mut output = Vec::new();
        report_yank_error(
            io::Error::new(io::ErrorKind::NotFound, "xclip unavailable\n"),
            &mut output,
            (80, 24),
        )
        .unwrap();
        let text = String::from_utf8_lossy(&output);
        assert!(text.contains("rmux: clipboard yank failed (xclip): xclip unavailable "));
        assert!(!text.contains('\n'));
    }
}

