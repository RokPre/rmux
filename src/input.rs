use std::time::{Duration, Instant};

use crate::server::{
    CLOSE_BUFFER, CLOSE_WINDOW, EQUALIZE_WINDOWS, FOCUS_WINDOW, MOVE_WINDOW, NEW_BUFFER,
    NEXT_BUFFER, PREVIOUS_BUFFER, RENAME_BUFFER, REORDER_BUFFER_LEFT, REORDER_BUFFER_RIGHT,
    RESIZE_WINDOW, SPLIT_HORIZONTAL, SPLIT_VERTICAL,
};

const SEQUENCE_TIMEOUT: Duration = Duration::from_millis(40);
const BRACKETED_PASTE_START: &[u8] = b"\x1b[200~";
const BRACKETED_PASTE_END: &[u8] = b"\x1b[201~";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct InputCommand {
    pub kind: u8,
    pub payload: &'static [u8],
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum InputAction {
    Bytes(Vec<u8>),
    Command(InputCommand),
}

struct Binding {
    sequence: Vec<u8>,
    command: InputCommand,
}

const BINDINGS: &[(&[u8], u8, &[u8])] = &[
    (b"\x08", PREVIOUS_BUFFER, b""),
    (b"\x0c", NEXT_BUFFER, b""),
    (b"\x12", RENAME_BUFFER, b""),
    (b"\x14", NEW_BUFFER, b""),
    (b"\x17", CLOSE_BUFFER, b""),
    (b"\x1b[104;6u", REORDER_BUFFER_LEFT, b""),
    (b"\x1b[108;6u", REORDER_BUFFER_RIGHT, b""),
    (b"\x1b\x08", RESIZE_WINDOW, b"h"),
    (b"\x1b\n", RESIZE_WINDOW, b"j"),
    (b"\x1b\x0b", RESIZE_WINDOW, b"k"),
    (b"\x1b\x0c", RESIZE_WINDOW, b"l"),
    (b"\x1bH", MOVE_WINDOW, b"h"),
    (b"\x1bJ", MOVE_WINDOW, b"j"),
    (b"\x1bK", MOVE_WINDOW, b"k"),
    (b"\x1bL", MOVE_WINDOW, b"l"),
    (b"\x1bh", FOCUS_WINDOW, b"h"),
    (b"\x1bj", FOCUS_WINDOW, b"j"),
    (b"\x1bk", FOCUS_WINDOW, b"k"),
    (b"\x1bl", FOCUS_WINDOW, b"l"),
    (b"\x1bv", SPLIT_VERTICAL, b""),
    (b"\x1bb", SPLIT_HORIZONTAL, b""),
    (b"\x1bw", CLOSE_WINDOW, b""),
    (b"\x1b=", EQUALIZE_WINDOWS, b""),
];

fn configured_bindings(config: &crate::config::Config) -> Vec<Binding> {
    let mut bindings: Vec<Binding> = BINDINGS
        .iter()
        .map(|(sequence, kind, payload)| Binding {
            sequence: sequence.to_vec(),
            command: InputCommand {
                kind: *kind,
                payload,
            },
        })
        .collect();
    for (action, key) in &config.bindings {
        let Some((kind, payload)) = action_command(action) else {
            continue;
        };
        let Some(sequence) = crate::config::parse_key(key) else {
            continue;
        };
        bindings
            .retain(|binding| binding.command.kind != kind || binding.command.payload != payload);
        bindings.retain(|binding| binding.sequence != sequence);
        bindings.push(Binding {
            sequence,
            command: InputCommand { kind, payload },
        });
    }
    bindings
}

fn action_command(action: &str) -> Option<(u8, &'static [u8])> {
    Some(match action {
        "previous_buffer" => (PREVIOUS_BUFFER, b""),
        "next_buffer" => (NEXT_BUFFER, b""),
        "rename_buffer" => (RENAME_BUFFER, b""),
        "new_buffer" => (NEW_BUFFER, b""),
        "close_buffer" => (CLOSE_BUFFER, b""),
        "reorder_buffer_left" => (REORDER_BUFFER_LEFT, b""),
        "reorder_buffer_right" => (REORDER_BUFFER_RIGHT, b""),
        "resize_left" => (RESIZE_WINDOW, b"h"),
        "resize_down" => (RESIZE_WINDOW, b"j"),
        "resize_up" => (RESIZE_WINDOW, b"k"),
        "resize_right" => (RESIZE_WINDOW, b"l"),
        "move_left" => (MOVE_WINDOW, b"h"),
        "move_down" => (MOVE_WINDOW, b"j"),
        "move_up" => (MOVE_WINDOW, b"k"),
        "move_right" => (MOVE_WINDOW, b"l"),
        "focus_left" => (FOCUS_WINDOW, b"h"),
        "focus_down" => (FOCUS_WINDOW, b"j"),
        "focus_up" => (FOCUS_WINDOW, b"k"),
        "focus_right" => (FOCUS_WINDOW, b"l"),
        "split_vertical" => (SPLIT_VERTICAL, b""),
        "split_horizontal" => (SPLIT_HORIZONTAL, b""),
        "close_window" => (CLOSE_WINDOW, b""),
        "equalize" => (EQUALIZE_WINDOWS, b""),
        _ => return None,
    })
}

fn binding(sequence: &'static [u8], kind: u8, payload: &'static [u8]) -> Binding {
    Binding {
        sequence: sequence.to_vec(),
        command: InputCommand { kind, payload },
    }
}

pub(crate) struct InputDecoder {
    pending: Vec<u8>,
    last_input: Option<Instant>,
    bindings: Vec<Binding>,
    in_bracketed_paste: bool,
}

impl Default for InputDecoder {
    fn default() -> Self {
        Self {
            pending: Vec::new(),
            last_input: None,
            bindings: BINDINGS.iter().map(|(s, k, p)| binding(s, *k, p)).collect(),
            in_bracketed_paste: false,
        }
    }
}

impl InputDecoder {
    pub(crate) fn configured(config: &crate::config::Config) -> Self {
        Self {
            pending: Vec::new(),
            last_input: None,
            bindings: configured_bindings(config),
            in_bracketed_paste: false,
        }
    }
    pub fn is_idle(&self) -> bool {
        self.pending.is_empty() && !self.in_bracketed_paste
    }
    pub fn should_decode(&self, bytes: &[u8]) -> bool {
        !self.is_idle()
            || bytes.iter().any(|byte| {
                BRACKETED_PASTE_START.first() == Some(byte)
                    || self
                        .bindings
                        .iter()
                        .any(|binding| binding.sequence.first() == Some(byte))
            })
    }

    pub fn reset(&mut self) {
        self.pending.clear();
        self.last_input = None;
        self.in_bracketed_paste = false;
    }

    pub fn feed(&mut self, bytes: &[u8]) -> Vec<InputAction> {
        self.feed_at(bytes, Instant::now())
    }

    fn feed_at(&mut self, bytes: &[u8], now: Instant) -> Vec<InputAction> {
        let mut actions = Vec::new();
        let mut passthrough = Vec::new();

        for &byte in bytes {
            if self.in_bracketed_paste {
                self.pending.push(byte);
                loop {
                    if self.pending == BRACKETED_PASTE_END {
                        passthrough.extend_from_slice(&self.pending);
                        self.pending.clear();
                        self.in_bracketed_paste = false;
                        self.last_input = None;
                        break;
                    }
                    if BRACKETED_PASTE_END.starts_with(&self.pending) {
                        break;
                    }
                    passthrough.push(self.pending.remove(0));
                }
                continue;
            }

            if self.pending.is_empty() && byte != b'\x1b' {
                if let Some(binding) = self
                    .bindings
                    .iter()
                    .find(|binding| binding.sequence.len() == 1 && binding.sequence[0] == byte)
                {
                    flush_passthrough(&mut actions, &mut passthrough);
                    actions.push(InputAction::Command(binding.command));
                } else {
                    passthrough.push(byte);
                }
                continue;
            }

            self.pending.push(byte);
            self.last_input = Some(now);
            loop {
                if self.pending == BRACKETED_PASTE_START {
                    flush_passthrough(&mut actions, &mut passthrough);
                    passthrough.extend_from_slice(&self.pending);
                    self.pending.clear();
                    self.last_input = None;
                    self.in_bracketed_paste = true;
                    break;
                }
                if let Some(binding) = self
                    .bindings
                    .iter()
                    .find(|binding| binding.sequence == self.pending)
                {
                    flush_passthrough(&mut actions, &mut passthrough);
                    actions.push(InputAction::Command(binding.command));
                    self.pending.clear();
                    self.last_input = None;
                    break;
                }
                if BRACKETED_PASTE_START.starts_with(&self.pending)
                    || self
                        .bindings
                        .iter()
                        .any(|binding| binding.sequence.starts_with(&self.pending))
                {
                    break;
                }

                passthrough.push(self.pending.remove(0));
                if self.pending.is_empty() {
                    self.last_input = None;
                    break;
                }
            }
        }

        flush_passthrough(&mut actions, &mut passthrough);
        actions
    }

    pub fn remaining_timeout(&self, now: Instant) -> Option<Duration> {
        if self.in_bracketed_paste {
            return None;
        }
        self.last_input
            .map(|last_input| SEQUENCE_TIMEOUT.saturating_sub(now.duration_since(last_input)))
    }

    pub fn expire(&mut self, now: Instant) -> Option<InputAction> {
        if self
            .remaining_timeout(now)
            .is_some_and(|remaining| remaining.is_zero())
        {
            self.last_input = None;
            return Some(InputAction::Bytes(std::mem::take(&mut self.pending)));
        }
        None
    }
}

fn flush_passthrough(actions: &mut Vec<InputAction>, bytes: &mut Vec<u8>) {
    if !bytes.is_empty() {
        actions.push(InputAction::Bytes(std::mem::take(bytes)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_all_rmux_shortcuts() {
        let cases: &[(&[u8], u8, &[u8])] = &[
            (b"\x08", PREVIOUS_BUFFER, b""),
            (b"\x0c", NEXT_BUFFER, b""),
            (b"\x12", RENAME_BUFFER, b""),
            (b"\x14", NEW_BUFFER, b""),
            (b"\x17", CLOSE_BUFFER, b""),
            (b"\x1b[104;6u", REORDER_BUFFER_LEFT, b""),
            (b"\x1b[108;6u", REORDER_BUFFER_RIGHT, b""),
            (b"\x1b\x08", RESIZE_WINDOW, b"h"),
            (b"\x1b\n", RESIZE_WINDOW, b"j"),
            (b"\x1b\x0b", RESIZE_WINDOW, b"k"),
            (b"\x1b\x0c", RESIZE_WINDOW, b"l"),
            (b"\x1bH", MOVE_WINDOW, b"h"),
            (b"\x1bJ", MOVE_WINDOW, b"j"),
            (b"\x1bK", MOVE_WINDOW, b"k"),
            (b"\x1bL", MOVE_WINDOW, b"l"),
            (b"\x1bh", FOCUS_WINDOW, b"h"),
            (b"\x1bj", FOCUS_WINDOW, b"j"),
            (b"\x1bk", FOCUS_WINDOW, b"k"),
            (b"\x1bl", FOCUS_WINDOW, b"l"),
            (b"\x1bv", SPLIT_VERTICAL, b""),
            (b"\x1bb", SPLIT_HORIZONTAL, b""),
            (b"\x1bw", CLOSE_WINDOW, b""),
            (b"\x1b=", EQUALIZE_WINDOWS, b""),
        ];

        for &(sequence, kind, payload) in cases {
            let mut decoder = InputDecoder::default();
            assert_eq!(
                decoder.feed(sequence),
                vec![InputAction::Command(InputCommand { kind, payload })],
                "sequence {sequence:?}"
            );
        }
    }

    #[test]
    fn decodes_alt_shortcuts_split_across_reads_and_preserves_order() {
        let mut decoder = InputDecoder::default();
        let now = Instant::now();

        assert_eq!(
            decoder.feed_at(b"before\x1b", now),
            vec![InputAction::Bytes(b"before".to_vec())]
        );
        assert_eq!(
            decoder.feed_at(b"vafter", now + Duration::from_millis(1)),
            vec![
                InputAction::Command(InputCommand {
                    kind: SPLIT_VERTICAL,
                    payload: b"",
                }),
                InputAction::Bytes(b"after".to_vec()),
            ]
        );
        assert!(
            decoder
                .feed_at(b"\x1b", now + Duration::from_millis(2))
                .is_empty()
        );
        assert_eq!(
            decoder.feed_at(b"=after", now + Duration::from_millis(3)),
            vec![
                InputAction::Command(InputCommand {
                    kind: EQUALIZE_WINDOWS,
                    payload: b"",
                }),
                InputAction::Bytes(b"after".to_vec()),
            ]
        );
    }

    #[test]
    fn decodes_ctrl_r_and_preserves_surrounding_input_order() {
        let mut decoder = InputDecoder::default();

        assert_eq!(
            decoder.feed(b"before\x12after"),
            vec![
                InputAction::Bytes(b"before".to_vec()),
                InputAction::Command(InputCommand {
                    kind: RENAME_BUFFER,
                    payload: b"",
                }),
                InputAction::Bytes(b"after".to_vec()),
            ]
        );
    }

    #[test]
    fn passes_unbound_terminal_input_unchanged() {
        let mut decoder = InputDecoder::default();
        let bytes = b"text\x01\x03\x7f\x1b[1;5A";

        assert_eq!(
            decoder.feed(bytes),
            vec![InputAction::Bytes(bytes.to_vec())]
        );
    }

    #[test]
    fn passes_unbound_escape_prefix_sequences_unchanged() {
        let mut decoder = InputDecoder::default();
        let bytes = b"\x1bx";

        assert_eq!(
            decoder.feed(bytes),
            vec![InputAction::Bytes(bytes.to_vec())]
        );
    }

    #[test]
    fn flushes_incomplete_escape_sequences_after_timeout() {
        let mut decoder = InputDecoder::default();
        let now = Instant::now();

        assert!(decoder.feed_at(b"\x1b[104;", now).is_empty());
        assert_eq!(
            decoder.expire(now + SEQUENCE_TIMEOUT),
            Some(InputAction::Bytes(b"\x1b[104;".to_vec()))
        );
        assert!(decoder.is_idle());
    }

    #[test]
    fn configured_binding_replaces_only_its_old_sequence() {
        let mut config = crate::config::Config::default();
        config.bindings.insert("new_buffer".into(), "ctrl+g".into());
        let mut decoder = InputDecoder::configured(&config);
        assert_eq!(
            decoder.feed(b"\x07"),
            vec![InputAction::Command(InputCommand {
                kind: NEW_BUFFER,
                payload: b""
            })]
        );
        assert_eq!(
            decoder.feed(b"\x14"),
            vec![InputAction::Bytes(b"\x14".to_vec())]
        );
    }

    #[test]
    fn configured_escape_bindings_survive_split_reads() {
        let mut config = crate::config::Config::default();
        config
            .bindings
            .insert("split_vertical".into(), "alt+q".into());
        let mut decoder = InputDecoder::configured(&config);
        let now = Instant::now();
        assert!(decoder.feed_at(b"\x1b", now).is_empty());
        assert_eq!(
            decoder.feed_at(b"q", now + Duration::from_millis(1)),
            vec![InputAction::Command(InputCommand {
                kind: SPLIT_VERTICAL,
                payload: b""
            })]
        );
    }
    #[test]
    fn bracketed_paste_preserves_split_delimiters_and_ignores_shortcuts() {
        let mut decoder = InputDecoder::default();
        let now = Instant::now();

        assert_eq!(
            decoder.feed_at(b"before\x1b[20", now),
            vec![InputAction::Bytes(b"before".to_vec())]
        );
        assert_eq!(
            decoder.feed_at(b"0~\x12\n\x1bv", now + Duration::from_millis(1)),
            vec![InputAction::Bytes(b"\x1b[200~\x12\n\x1bv".to_vec())]
        );
        assert_eq!(
            decoder.feed_at(b"\x1b[20", now + Duration::from_millis(2)),
            Vec::<InputAction>::new()
        );
        assert_eq!(
            decoder.feed_at(b"1~\x12", now + Duration::from_millis(3)),
            vec![
                InputAction::Bytes(b"\x1b[201~".to_vec()),
                InputAction::Command(InputCommand {
                    kind: RENAME_BUFFER,
                    payload: b"",
                }),
            ]
        );
    }

    #[test]
    fn paste_mode_does_not_expire_partial_end_delimiter() {
        let mut decoder = InputDecoder::default();
        let now = Instant::now();
        assert_eq!(
            decoder.feed_at(b"\x1b[200~text\x1b[20", now),
            vec![InputAction::Bytes(b"\x1b[200~text".to_vec())]
        );
        assert_eq!(decoder.remaining_timeout(now + SEQUENCE_TIMEOUT), None);
        assert_eq!(decoder.expire(now + SEQUENCE_TIMEOUT), None);
        assert_eq!(
            decoder.feed_at(b"1~", now + SEQUENCE_TIMEOUT + Duration::from_secs(1)),
            vec![InputAction::Bytes(b"\x1b[201~".to_vec())]
        );
        assert!(decoder.is_idle());
    }
}
