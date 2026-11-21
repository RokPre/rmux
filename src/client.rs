use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::Request;
use crate::input::{InputAction, InputDecoder};
use crate::render::{BUFFER_OUTPUT, BUFFER_SNAPSHOT, Renderer};
use crate::runtime::{SessionPaths, read_response, write_request};
use crate::server::{CLOSE, INPUT, RENAME_BUFFER, RESIZE, WINDOW_STATE, read_frame, write_frame};

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
            crossterm::style::SetAttribute(crossterm::style::Attribute::Reset),
            crossterm::cursor::Show,
            crossterm::terminal::LeaveAlternateScreen
        );
    }
}

struct RenamePrompt {
    buffer: u64,
    name: String,
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
        Self {
            buffer,
            name,
            pending_utf8: Vec::new(),
        }
    }

    fn display(&self) -> String {
        format!("Rename {}: {}", self.buffer, self.name)
    }

    fn feed(&mut self, bytes: &[u8]) -> (PromptResult, usize) {
        for (index, &byte) in bytes.iter().enumerate() {
            match byte {
                b'\r' | b'\n' => return (PromptResult::Commit, index + 1),
                b'\x1b' | b'\x03' => return (PromptResult::Cancel, bytes.len()),
                b'\x08' | b'\x7f' => {
                    self.pending_utf8.clear();
                    self.name.pop();
                }
                b'\x15' => {
                    self.pending_utf8.clear();
                    self.name.clear();
                }
                byte if byte.is_ascii_control() => self.pending_utf8.clear(),
                byte if byte.is_ascii() => self.name.push(byte as char),
                byte => self.push_utf8_byte(byte),
            }
        }
        (PromptResult::Editing, bytes.len())
    }

    fn push_utf8_byte(&mut self, byte: u8) {
        self.pending_utf8.push(byte);
        match std::str::from_utf8(&self.pending_utf8) {
            Ok(character) => {
                if !character.chars().any(char::is_control) {
                    self.name.push_str(character);
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
    writer.set_read_timeout(None)?;
    writer.set_write_timeout(None)?;
    let mut server = reader.into_inner();
    drop(writer);
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
    let server_fd = server.as_raw_fd();
    let stdout = io::stdout();
    let mut output = stdout.lock();
    let mut input = stdin.lock();
    let mut last_size = (cols, rows);
    let mut last_resize_check = Instant::now();
    let mut input_buffer = [0u8; 16 * 1024];
    let mut renderer = Renderer::with_config(config.clone());
    let mut input_decoder = InputDecoder::configured(&config);
    let mut rename_prompt = None;

    loop {
        if interrupted.load(Ordering::Acquire) {
            let _ = write_frame(&mut server, 3, &[]);
            break;
        }
        if let Some(action) = input_decoder.expire(Instant::now()) {
            handle_input_action(
                &mut server,
                &mut input_decoder,
                &mut renderer,
                &mut rename_prompt,
                &mut output,
                last_size,
                action,
            )?;
        }
        let timeout = input_decoder
            .remaining_timeout(Instant::now())
            .map_or(100, |remaining| remaining.as_millis().clamp(1, 100) as i32);
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
                let _ = write_frame(&mut server, 3, &[]);
                break;
            }
            send_input(
                &mut server,
                &mut input_decoder,
                &mut renderer,
                &mut rename_prompt,
                &mut output,
                last_size,
                &input_buffer[..length],
            )?;
        }
        if descriptors[1].revents & libc::POLLIN != 0 {
            match read_frame(&mut server)? {
                Some((BUFFER_SNAPSHOT, bytes)) => {
                    renderer.apply_frame(BUFFER_SNAPSHOT, &bytes)?;
                    renderer.draw(&mut output, last_size.0, last_size.1)?;
                }
                Some((BUFFER_OUTPUT, bytes)) => {
                    renderer.apply_frame(BUFFER_OUTPUT, &bytes)?;
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
                Some((CLOSE, _)) | None => break,
                Some(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "invalid server frame",
                    ));
                }
            }
        }
        if descriptors[1].revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
            break;
        }

        if last_resize_check.elapsed() >= Duration::from_millis(200) {
            last_resize_check = Instant::now();
            let size = terminal_size();
            if size != last_size {
                let mut payload = Vec::with_capacity(4);
                payload.extend_from_slice(&size.1.max(1).to_be_bytes());
                payload.extend_from_slice(&size.0.max(1).to_be_bytes());
                write_frame(&mut server, RESIZE, &payload)?;
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
    if !decoder.should_decode(bytes) {
        return write_frame(server, INPUT, bytes);
    }
    let actions = decoder.feed(bytes);
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
        InputAction::Command(command) => write_frame(server, command.kind, command.payload),
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
            b"not-a-command\x1b",
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
    fn resize_step_requires_server_capability_only_when_nondefault() {
        assert!(ensure_resize_support(5, false).is_ok());
        assert!(ensure_resize_support(12, true).is_ok());
        assert_eq!(
            ensure_resize_support(12, false).unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
    }
}

pub(crate) fn terminal_size() -> (u16, u16) {
    match crossterm::terminal::size() {
        Ok((cols, rows)) if cols > 0 && rows > 0 => (cols, rows),
        _ => (80, 24),
    }
}
