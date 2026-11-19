use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::Request;
use crate::render::{BUFFER_OUTPUT, BUFFER_SNAPSHOT, Renderer};
use crate::runtime::{SessionPaths, read_response, write_request};
use crate::server::{CLOSE, INPUT, RESIZE, WINDOW_STATE, read_frame, write_frame};

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
            crossterm::style::ResetColor,
            crossterm::cursor::Show,
            crossterm::terminal::LeaveAlternateScreen
        );
    }
}

pub(crate) fn attach(paths: &SessionPaths, name: &str) -> io::Result<()> {
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
        },
    )?;
    let mut reader = std::io::BufReader::new(writer.try_clone()?);
    let response = read_response(&mut reader)?;
    if !response.ok {
        return Err(io::Error::other(response.error));
    }

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
    let mut renderer = Renderer::default();

    loop {
        if interrupted.load(Ordering::Acquire) {
            let _ = write_frame(&mut server, 3, &[]);
            break;
        }
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
        let ready = unsafe { libc::poll(descriptors.as_mut_ptr(), descriptors.len() as _, 100) };
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
            write_frame(&mut server, INPUT, &input_buffer[..length])?;
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

pub(crate) fn terminal_size() -> (u16, u16) {
    match crossterm::terminal::size() {
        Ok((cols, rows)) if cols > 0 && rows > 0 => (cols, rows),
        _ => (80, 24),
    }
}
