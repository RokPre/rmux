//! Buffer-scoped application-control requests carried in a terminal OSC.
//!
//! The request payload is `777;rmux-app;acquire;<pid>` or
//! `777;rmux-app;release;<pid>` (OSC introducer and terminator excluded).
//! A parser caller must associate the request with the process that emitted
//! it and resolve that PID to its current start time before applying it. The
//! PID in the payload is checked against that identity; it is not sufficient
//! by itself because PIDs can be reused.
//!
//! Each rmux buffer owns one `AppControl`. Requests affect only that buffer.
//! Acquires are idempotent per process identity, and a release removes only
//! that exact identity. The server should periodically call `prune_dead` with
//! a PID-and-start-time liveness check to recover from abnormal process exits.

use std::collections::HashSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppAction {
    Acquire,
    Release,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AppRequest {
    pub action: AppAction,
    pub pid: u32,
}

/// Process identity must include start time to distinguish reused PIDs.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub start_time: u64,
}

/// Parse the OSC payload (excluding `ESC ]` and BEL / `ESC \\`).
///
/// Unknown commands, extra fields, invalid UTF-8, PID zero, and overflow are
/// ignored rather than interpreted as control requests.
pub fn parse_request(payload: &[u8]) -> Option<AppRequest> {
    let payload = std::str::from_utf8(payload).ok()?;
    let mut fields = payload.split(';');
    if fields.next()? != "777" || fields.next()? != "rmux-app" {
        return None;
    }
    let action = match fields.next()? {
        "acquire" => AppAction::Acquire,
        "release" => AppAction::Release,
        _ => return None,
    };
    let pid = fields.next()?.parse::<u32>().ok()?;
    if pid == 0 || fields.next().is_some() {
        return None;
    }
    Some(AppRequest { action, pid })
}

#[derive(Clone, Debug, Default)]
pub struct AppControl {
    owners: HashSet<ProcessIdentity>,
}

impl AppControl {
    pub fn is_enabled(&self) -> bool {
        !self.owners.is_empty()
    }

    pub fn owner_count(&self) -> usize {
        self.owners.len()
    }

    /// Apply only when the resolved emitter identity matches the OSC PID.
    /// Returns whether the request was accepted.
    pub fn apply(&mut self, request: AppRequest, emitter: ProcessIdentity) -> bool {
        if request.pid != emitter.pid {
            return false;
        }
        match request.action {
            AppAction::Acquire => {
                self.owners.insert(emitter);
            }
            AppAction::Release => {
                self.owners.remove(&emitter);
            }
        }
        true
    }

    /// Remove owners whose exact PID/start-time identity is no longer alive.
    pub fn prune_dead(&mut self, mut is_alive: impl FnMut(ProcessIdentity) -> bool) {
        self.owners.retain(|identity| is_alive(*identity));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(pid: u32, start_time: u64) -> ProcessIdentity {
        ProcessIdentity { pid, start_time }
    }

    #[test]
    fn parses_only_exact_supported_requests() {
        assert_eq!(
            parse_request(b"777;rmux-app;acquire;42"),
            Some(AppRequest {
                action: AppAction::Acquire,
                pid: 42,
            })
        );
        assert_eq!(
            parse_request(b"777;rmux-app;release;42"),
            Some(AppRequest {
                action: AppAction::Release,
                pid: 42,
            })
        );
        for invalid in [
            &b"777;rmux-app;other;42"[..],
            b"777;rmux-app;acquire;0",
            b"777;rmux-app;acquire;42;extra",
            b"777;rmux-app;acquire;4294967296",
            b"777;rmux-app;acquire;-1",
            b"777;rmux-app;acquire;\xff",
            b"777;other;acquire;42",
        ] {
            assert_eq!(parse_request(invalid), None);
        }
    }

    #[test]
    fn release_removes_only_its_owner_and_is_idempotent() {
        let first = identity(10, 100);
        let second = identity(20, 200);
        let acquire = AppRequest {
            action: AppAction::Acquire,
            pid: 10,
        };
        let mut control = AppControl::default();
        assert!(control.apply(acquire, first));
        assert!(control.apply(acquire, first));
        assert!(control.apply(
            AppRequest {
                pid: 20,
                action: AppAction::Acquire
            },
            second
        ));
        assert_eq!(control.owner_count(), 2);
        assert!(control.apply(
            AppRequest {
                pid: 10,
                action: AppAction::Release
            },
            first
        ));
        assert!(control.is_enabled());
        assert_eq!(control.owner_count(), 1);
        assert!(control.apply(
            AppRequest {
                pid: 10,
                action: AppAction::Release
            },
            first
        ));
        assert_eq!(control.owner_count(), 1);
    }

    #[test]
    fn mismatched_pid_and_stale_pid_reuse_cannot_release_owner() {
        let owner = identity(7, 100);
        let reused_pid = identity(7, 200);
        let mut control = AppControl::default();
        assert!(control.apply(
            AppRequest {
                pid: 7,
                action: AppAction::Acquire
            },
            owner
        ));
        assert!(!control.apply(
            AppRequest {
                pid: 8,
                action: AppAction::Release
            },
            owner
        ));
        assert!(control.apply(
            AppRequest {
                pid: 7,
                action: AppAction::Release
            },
            reused_pid
        ));
        assert!(control.is_enabled());
        assert_eq!(control.owner_count(), 1);
    }

    #[test]
    fn dead_owner_pruning_preserves_live_nested_owner() {
        let first = identity(1, 11);
        let second = identity(2, 22);
        let mut control = AppControl::default();
        for owner in [first, second] {
            control.apply(
                AppRequest {
                    action: AppAction::Acquire,
                    pid: owner.pid,
                },
                owner,
            );
        }
        control.prune_dead(|owner| owner == second);
        assert!(control.is_enabled());
        assert_eq!(control.owner_count(), 1);
        control.prune_dead(|_| false);
        assert!(!control.is_enabled());
    }
}
