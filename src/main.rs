mod client;
mod input;
mod layout;
mod render;
mod runtime;
mod server;

use std::env;
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use runtime::SessionPaths;
use serde::{Deserialize, Serialize};

const USAGE: &str = "usage: rmux {new|up} NAME [--cwd DIRECTORY] [-- COMMAND [ARG...]] | rmux {attach|kill} NAME | rmux ls";

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Request {
    pub op: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub rows: u16,
    #[serde(default)]
    pub cols: u16,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Response {
    pub ok: bool,
    #[serde(default)]
    pub error: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub pid: u32,
    #[serde(default)]
    pub start_time: u64,
}

fn main() {
    if let Err(error) = run(env::args_os().skip(1).collect()) {
        eprintln!("rmux: {error}");
        std::process::exit(1);
    }
}

fn run(args: Vec<OsString>) -> Result<(), String> {
    if args.first().is_some_and(|arg| arg == "__serve") {
        return server::run_server(&args[1..]).map_err(|error| error.to_string());
    }

    if args.len() == 1 && args[0] == "--help" {
        println!("{USAGE}");
        return Ok(());
    }

    let (mode, name, cwd, command) = parse_args(&args)?;
    let paths = SessionPaths::new().map_err(|error| error.to_string())?;

    if mode == "ls" {
        for name in paths.live_sessions().map_err(|error| error.to_string())? {
            println!("{name}");
        }
        return Ok(());
    }

    let name = name.expect("session commands require a name");
    let lock = paths.lock(&name).map_err(|error| error.to_string())?;
    let existing = paths.probe(&name).map_err(|error| error.to_string())?;

    match mode.as_str() {
        "new" if existing => return Err(format!("session already exists: {name}")),
        "attach" if !existing => return Err(format!("session does not exist: {name}")),
        "kill" if !existing => {
            return Err(format!(
                "session does not exist or cannot be reached: {name}"
            ));
        }
        "new" | "up" if !existing => {
            paths
                .remove_stale(&name)
                .map_err(|error| error.to_string())?;
            start_server(&name, &paths, cwd.as_deref(), &command)?;
        }
        _ => {}
    }

    if mode == "kill" {
        paths.kill(&name).map_err(|error| error.to_string())?;
        drop(lock);
        return Ok(());
    }

    drop(lock);
    client::attach(&paths, &name).map_err(|error| error.to_string())
}

fn parse_args(
    args: &[OsString],
) -> Result<(String, Option<String>, Option<PathBuf>, Vec<OsString>), String> {
    let usage = || format!("invalid arguments\n{USAGE}");
    let Some(mode) = args.first().and_then(|arg| arg.to_str()) else {
        return Err(usage());
    };

    if mode == "ls" {
        return (args.len() == 1)
            .then(|| ("ls".into(), None, None, Vec::new()))
            .ok_or_else(usage);
    }
    if !matches!(mode, "new" | "up" | "attach" | "kill") || args.len() < 2 {
        return Err(usage());
    }

    let Some(name) = args[1].to_str() else {
        return Err("session name must be valid UTF-8".into());
    };
    validate_name(name)?;

    let mut index = 2;
    let mut cwd = None;
    let mut command = Vec::new();
    match mode {
        "new" | "up" => {
            if args.get(index).is_some_and(|arg| arg == "--cwd") {
                cwd = Some(PathBuf::from(args.get(index + 1).ok_or_else(usage)?));
                index += 2;
            }
            if index < args.len() {
                if args[index] != "--" || index + 1 == args.len() {
                    return Err(usage());
                }
                command.extend_from_slice(&args[index + 1..]);
            }
        }
        _ if index != args.len() => return Err(usage()),
        _ => {}
    }

    Ok((mode.into(), Some(name.into()), cwd, command))
}

fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.chars().any(char::is_control)
        || name.len() > 60
    {
        return Err(format!("invalid session name: {name:?}"));
    }
    Ok(())
}

fn start_server(
    name: &str,
    paths: &SessionPaths,
    cwd: Option<&std::path::Path>,
    command: &[OsString],
) -> Result<(), String> {
    if let Some(cwd) = cwd {
        let metadata = std::fs::metadata(cwd)
            .map_err(|error| format!("invalid working directory {}: {error}", cwd.display()))?;
        if !metadata.is_dir() {
            return Err(format!(
                "working directory is not a directory: {}",
                cwd.display()
            ));
        }
    }

    let (cols, rows) = client::terminal_size();
    let mut server = Command::new(env::current_exe().map_err(|error| error.to_string())?);
    server
        .arg("__serve")
        .arg(name)
        .arg(paths.socket_path(name))
        .arg(rows.to_string())
        .arg(cols.to_string());
    if let Some(cwd) = cwd {
        server.arg("--cwd").arg(cwd);
    }
    server.arg("--");
    if command.is_empty() {
        let shell = env::var_os("SHELL").unwrap_or_else(|| OsString::from("/bin/sh"));
        server.arg(shell).arg("-i");
    } else {
        server.args(command);
    }

    use std::os::unix::process::CommandExt;
    server
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = server
        .spawn()
        .map_err(|error| format!("could not create session: {error}"))?;

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if paths.probe(name).map_err(|error| error.to_string())? {
            return Ok(());
        }
        if let Some(status) = child.try_wait().map_err(|error| error.to_string())? {
            return Err(format!(
                "could not create session: server exited with {status}"
            ));
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            return Err("timed out waiting for session server".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_args, validate_name};
    use std::ffi::OsString;
    use std::path::PathBuf;

    #[test]
    fn parses_session_creation_and_preserves_command_arguments() {
        let args = [
            "new",
            "work",
            "--cwd",
            "/tmp/project",
            "--",
            "sh",
            "-c",
            "echo hello",
        ]
        .map(OsString::from);
        let (mode, name, cwd, command) = parse_args(&args).unwrap();
        assert_eq!(mode, "new");
        assert_eq!(name.as_deref(), Some("work"));
        assert_eq!(cwd.unwrap(), PathBuf::from("/tmp/project"));
        assert_eq!(command, ["sh", "-c", "echo hello"].map(OsString::from));
    }

    #[test]
    fn rejects_unsafe_session_names() {
        for name in ["", ".", "..", "../bad", "bad\nname", "bad\u{1b}name"] {
            assert!(validate_name(name).is_err(), "accepted {name:?}");
        }
    }
}
