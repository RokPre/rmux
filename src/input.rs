use std::time::{Duration, Instant};

use crate::server::{
    CLOSE_BUFFER, CLOSE_WINDOW, FOCUS_WINDOW, MOVE_WINDOW, NEW_BUFFER, NEXT_BUFFER,
    PREVIOUS_BUFFER, REORDER_BUFFER_LEFT, REORDER_BUFFER_RIGHT, RESIZE_WINDOW, SPLIT_HORIZONTAL,
    SPLIT_VERTICAL,
};

const SEQUENCE_TIMEOUT: Duration = Duration::from_millis(40);

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
    sequence: &'static [u8],
    command: InputCommand,
}

const BINDINGS: &[Binding] = &[
    binding(b"\x1b[116;5u", NEW_BUFFER, b""),
    binding(b"\x1b[119;5u", CLOSE_BUFFER, b""),
    binding(b"\x1b[104;5u", PREVIOUS_BUFFER, b""),
    binding(b"\x1b[108;5u", NEXT_BUFFER, b""),
    binding(b"\x1b[104;6u", REORDER_BUFFER_LEFT, b""),
    binding(b"\x1b[108;6u", REORDER_BUFFER_RIGHT, b""),
    binding(b"\x1b[118;3u", SPLIT_VERTICAL, b""),
    binding(b"\x1b[98;3u", SPLIT_HORIZONTAL, b""),
    binding(b"\x1b[119;3u", CLOSE_WINDOW, b""),
    binding(b"\x1b[104;3u", FOCUS_WINDOW, b"h"),
    binding(b"\x1b[106;3u", FOCUS_WINDOW, b"j"),
    binding(b"\x1b[107;3u", FOCUS_WINDOW, b"k"),
    binding(b"\x1b[108;3u", FOCUS_WINDOW, b"l"),
    binding(b"\x1b[104;4u", MOVE_WINDOW, b"h"),
    binding(b"\x1b[106;4u", MOVE_WINDOW, b"j"),
    binding(b"\x1b[107;4u", MOVE_WINDOW, b"k"),
    binding(b"\x1b[108;4u", MOVE_WINDOW, b"l"),
    binding(b"\x1b[104;7u", RESIZE_WINDOW, b"h"),
    binding(b"\x1b[106;7u", RESIZE_WINDOW, b"j"),
    binding(b"\x1b[107;7u", RESIZE_WINDOW, b"k"),
    binding(b"\x1b[108;7u", RESIZE_WINDOW, b"l"),
];

const fn binding(sequence: &'static [u8], kind: u8, payload: &'static [u8]) -> Binding {
    Binding {
        sequence,
        command: InputCommand { kind, payload },
    }
}

#[derive(Default)]
pub(crate) struct InputDecoder {
    pending: Vec<u8>,
    last_input: Option<Instant>,
}

impl InputDecoder {
    pub fn is_idle(&self) -> bool {
        self.pending.is_empty()
    }

    pub fn feed(&mut self, bytes: &[u8]) -> Vec<InputAction> {
        self.feed_at(bytes, Instant::now())
    }

    fn feed_at(&mut self, bytes: &[u8], now: Instant) -> Vec<InputAction> {
        let mut actions = Vec::new();
        let mut passthrough = Vec::new();

        for &byte in bytes {
            if self.pending.is_empty() && byte != b'\x1b' {
                passthrough.push(byte);
                continue;
            }

            self.pending.push(byte);
            self.last_input = Some(now);
            loop {
                if let Some(binding) = BINDINGS
                    .iter()
                    .find(|binding| binding.sequence == self.pending)
                {
                    flush_passthrough(&mut actions, &mut passthrough);
                    actions.push(InputAction::Command(binding.command));
                    self.pending.clear();
                    self.last_input = None;
                    break;
                }
                if BINDINGS
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
            (b"\x1b[116;5u", NEW_BUFFER, b""),
            (b"\x1b[119;5u", CLOSE_BUFFER, b""),
            (b"\x1b[104;5u", PREVIOUS_BUFFER, b""),
            (b"\x1b[108;5u", NEXT_BUFFER, b""),
            (b"\x1b[104;6u", REORDER_BUFFER_LEFT, b""),
            (b"\x1b[108;6u", REORDER_BUFFER_RIGHT, b""),
            (b"\x1b[118;3u", SPLIT_VERTICAL, b""),
            (b"\x1b[98;3u", SPLIT_HORIZONTAL, b""),
            (b"\x1b[119;3u", CLOSE_WINDOW, b""),
            (b"\x1b[104;3u", FOCUS_WINDOW, b"h"),
            (b"\x1b[106;3u", FOCUS_WINDOW, b"j"),
            (b"\x1b[107;3u", FOCUS_WINDOW, b"k"),
            (b"\x1b[108;3u", FOCUS_WINDOW, b"l"),
            (b"\x1b[104;4u", MOVE_WINDOW, b"h"),
            (b"\x1b[106;4u", MOVE_WINDOW, b"j"),
            (b"\x1b[107;4u", MOVE_WINDOW, b"k"),
            (b"\x1b[108;4u", MOVE_WINDOW, b"l"),
            (b"\x1b[104;7u", RESIZE_WINDOW, b"h"),
            (b"\x1b[106;7u", RESIZE_WINDOW, b"j"),
            (b"\x1b[107;7u", RESIZE_WINDOW, b"k"),
            (b"\x1b[108;7u", RESIZE_WINDOW, b"l"),
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
    fn decodes_shortcuts_split_across_reads_and_preserves_order() {
        let mut decoder = InputDecoder::default();
        let now = Instant::now();

        assert_eq!(
            decoder.feed_at(b"before\x1b[104;", now),
            vec![InputAction::Bytes(b"before".to_vec())]
        );
        assert_eq!(
            decoder.feed_at(b"5uafter", now + Duration::from_millis(1)),
            vec![
                InputAction::Command(InputCommand {
                    kind: PREVIOUS_BUFFER,
                    payload: b"",
                }),
                InputAction::Bytes(b"after".to_vec()),
            ]
        );
    }

    #[test]
    fn passes_unbound_terminal_input_unchanged() {
        let mut decoder = InputDecoder::default();
        let bytes = b"text\x14\x17\x08\x0c\x1b[1;5A";

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
}
