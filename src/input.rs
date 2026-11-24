use std::time::{Duration, Instant};

use crate::server::{
    CLOSE_BUFFER, CLOSE_WINDOW, EQUALIZE_WINDOWS, FOCUS_WINDOW, MOVE_WINDOW, NEW_BUFFER,
    NEXT_BUFFER, PREVIOUS_BUFFER, RENAME_BUFFER, REORDER_BUFFER_LEFT, REORDER_BUFFER_RIGHT,
    RESIZE_WINDOW, SPLIT_HORIZONTAL, SPLIT_VERTICAL, VIM_KEY,
};

const SEQUENCE_TIMEOUT: Duration = Duration::from_millis(40);
const BRACKETED_PASTE_START: &[u8] = b"\x1b[200~";
const BRACKETED_PASTE_END: &[u8] = b"\x1b[201~";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct InputCommand {
    pub kind: u8,
    pub payload: &'static [u8],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InputContext {
    AppDisabled,
    Insert,
    Normal,
    Visual,
    Search,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum InputAction {
    Bytes(Vec<u8>),
    Command(InputCommand),
}

struct Binding {
    sequence: Vec<u8>,
    command: InputCommand,
    vim_contexts: Option<&'static [InputContext]>,
    vim_action: Option<String>,
}

impl Binding {
    fn active_in(&self, context: InputContext) -> bool {
        self.vim_contexts
            .is_none_or(|contexts| contexts.contains(&context))
    }

    fn overlaps_contexts(&self, contexts: &[InputContext]) -> bool {
        self.vim_contexts.is_none_or(|binding_contexts| {
            binding_contexts.iter().any(|context| contexts.contains(context))
        })
    }
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
            vim_contexts: None,
            vim_action: None,
        })
        .collect();
    for action in config.bindings.keys().filter(|action| action.starts_with("vim_")) {
        if let Some(default_sequence) = default_vim_sequence(action) {
            if action != "vim_normal" {
                bindings.push(Binding {
                    sequence: default_sequence.to_vec(),
                    command: InputCommand {
                        kind: VIM_KEY,
                        payload: b"",
                    },
                    vim_contexts: Some(action_contexts(action)),
                    vim_action: Some(action.clone()),
                });
            }
        }
    }
    for (action, key) in &config.bindings {
        let Some((kind, payload)) = action_command(action) else {
            continue;
        };
        let Some(sequence) = crate::config::parse_key(key) else {
            continue;
        };
        let contexts: &'static [InputContext] = if action.starts_with("vim_") {
            if action == "vim_normal"
                && sequence.starts_with(b"\x1b")
                && sequence != b"\x1b"
            {
                &[InputContext::Insert, InputContext::Normal, InputContext::Visual]
            } else {
                action_contexts(action)
            }
        } else {
            &[]
        };
        bindings.retain(|binding| {
            if action.starts_with("vim_") {
                let old_default_on_new_key =
                    binding.vim_action.as_deref() == Some(action.as_str())
                        && binding.sequence == sequence;
                !old_default_on_new_key
                    && (binding.sequence != sequence || !binding.overlaps_contexts(contexts))
            } else {
                binding.command.kind != kind || binding.command.payload != payload
            }
        });
        if action == "vim_normal" {
            let normal_contexts = if sequence.starts_with(b"\x1b") && sequence != b"\x1b" {
                &[InputContext::Normal, InputContext::Visual][..]
            } else {
                &[InputContext::Normal, InputContext::Visual, InputContext::Search][..]
            };
            bindings.push(Binding {
                sequence: sequence.clone(),
                command: InputCommand { kind, payload: b"\x1b\x1b" },
                vim_contexts: Some(&[InputContext::Insert]),
                vim_action: Some(action.clone()),
            });
            bindings.push(Binding {
                sequence,
                command: InputCommand { kind, payload: b"\x1b" },
                vim_contexts: Some(normal_contexts),
                vim_action: Some(action.clone()),
            });
        } else {
            bindings.push(Binding {
                sequence,
                command: InputCommand { kind, payload },
                vim_contexts: action.starts_with("vim_").then_some(action_contexts(action)),
                vim_action: action.starts_with("vim_").then_some(action.clone()),
            });
        }
    }
    bindings
}

fn action_contexts(action: &str) -> &'static [InputContext] {
    use InputContext::*;
    match action {
        "vim_normal" => &[Insert, Normal, Visual, Search],
        "vim_insert_search" => &[Insert],
        "vim_insert" | "vim_visual" | "vim_visual_line" | "vim_visual_block" => &[Normal],
        "vim_scroll_insert_down" | "vim_scroll_insert_up" => &[Insert],
        _ => &[Normal, Visual],
    }
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
        "vim_left" => (VIM_KEY, b"h"),
        "vim_down" => (VIM_KEY, b"j"),
        "vim_up" => (VIM_KEY, b"k"),
        "vim_right" => (VIM_KEY, b"l"),
        "vim_next_match" => (VIM_KEY, b"n"),
        "vim_previous_match" => (VIM_KEY, b"N"),
        "vim_visual" => (VIM_KEY, b"v"),
        "vim_visual_line" => (VIM_KEY, b"V"),
        "vim_search" => (VIM_KEY, b"/"),
        "vim_search_backward" => (VIM_KEY, b"?"),
        "vim_yank" => (VIM_KEY, b"y"),
        "vim_insert" => (VIM_KEY, b"i"),
        "vim_normal" => (VIM_KEY, b"\x1b\x1b"),
        "vim_insert_search" => (VIM_KEY, b"\x06"),
        "vim_scroll_half_down" => (VIM_KEY, b"\x04"),
        "vim_scroll_half_up" => (VIM_KEY, b"\x15"),
        "vim_scroll_page_down" => (VIM_KEY, b"\x1b[6~"),
        "vim_scroll_page_up" => (VIM_KEY, b"\x02"),
        "vim_scroll_insert_down" => (VIM_KEY, b"\x1bd"),
        "vim_scroll_insert_up" => (VIM_KEY, b"\x1bf"),
        "vim_word_forward" => (VIM_KEY, b"w"),
        "vim_word_backward" => (VIM_KEY, b"b"),
        "vim_word_end" => (VIM_KEY, b"e"),
        "vim_line_start" => (VIM_KEY, b"0"),
        "vim_first_nonblank" => (VIM_KEY, b"^"),
        "vim_line_end" => (VIM_KEY, b"$"),
        "vim_document_end" => (VIM_KEY, b"G"),
        "vim_visual_block" => (VIM_KEY, b"\x16"),
        "vim_document_start" => (VIM_KEY, b"gg"),
        "vim_word_forward_big" => (VIM_KEY, b"W"),
        "vim_word_backward_big" => (VIM_KEY, b"B"),
        "vim_word_end_big" => (VIM_KEY, b"E"),
        "vim_view_top" => (VIM_KEY, b"H"),
        "vim_view_middle" => (VIM_KEY, b"M"),
        "vim_view_bottom" => (VIM_KEY, b"L"),
        "vim_view_cursor_top" => (VIM_KEY, b"K"),
        "vim_view_cursor_bottom" => (VIM_KEY, b"J"),
        "vim_match_forward" => (VIM_KEY, b","),
        "vim_match_backward" => (VIM_KEY, b";"),
        "vim_match_next" => (VIM_KEY, b"n"),
        "vim_match_previous" => (VIM_KEY, b"N"),
        _ => return None,
    })
}
fn default_vim_sequence(action: &str) -> Option<&'static [u8]> {
    Some(match action {
        "vim_left" => b"h",
        "vim_down" => b"j",
        "vim_up" => b"k",
        "vim_right" => b"l",
        "vim_next_match" | "vim_match_next" => b"n",
        "vim_previous_match" | "vim_match_previous" => b"N",
        "vim_visual" => b"v",
        "vim_visual_line" => b"V",
        "vim_visual_block" => b"\x16",
        "vim_search" => b"/",
        "vim_search_backward" => b"?",
        "vim_yank" => b"y",
        "vim_insert" => b"i",
        "vim_normal" => b"\x1b",
        "vim_insert_search" => b"\x06",
        "vim_scroll_half_down" => b"\x04",
        "vim_scroll_half_up" => b"\x15",
        "vim_scroll_page_down" => b"\x06",
        "vim_scroll_page_up" => b"\x02",
        "vim_scroll_insert_down" => b"\x1bd",
        "vim_scroll_insert_up" => b"\x1bf",
        "vim_word_forward" => b"w",
        "vim_word_backward" => b"b",
        "vim_word_end" => b"e",
        "vim_word_forward_big" => b"W",
        "vim_word_backward_big" => b"B",
        "vim_word_end_big" => b"E",
        "vim_line_start" => b"0",
        "vim_first_nonblank" => b"^",
        "vim_line_end" => b"$",
        "vim_document_start" => b"gg",
        "vim_document_end" => b"G",
        "vim_view_top" => b"H",
        "vim_view_middle" => b"M",
        "vim_view_bottom" => b"L",
        "vim_view_cursor_top" => b"K",
        "vim_view_cursor_bottom" => b"J",
        "vim_match_forward" => b",",
        "vim_match_backward" => b";",
        _ => return None,
    })
}
fn binding(sequence: &'static [u8], kind: u8, payload: &'static [u8]) -> Binding {
    Binding {
        sequence: sequence.to_vec(),
        command: InputCommand { kind, payload },
        vim_contexts: None,
        vim_action: None,
    }
}

pub(crate) struct InputDecoder {
    pending: Vec<u8>,
    last_input: Option<Instant>,
    pending_context: InputContext,
    search_return_context: Option<InputContext>,
    bindings: Vec<Binding>,
    in_bracketed_paste: bool,
}
impl Default for InputDecoder {
    fn default() -> Self {
        Self {
            pending: Vec::new(),
            last_input: None,
            pending_context: InputContext::AppDisabled,
            search_return_context: None,
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
            pending_context: InputContext::AppDisabled,
            search_return_context: None,
            bindings: configured_bindings(config),
            in_bracketed_paste: false,
        }
    }
    pub fn is_idle(&self) -> bool {
        self.pending.is_empty() && !self.in_bracketed_paste
    }
    pub fn should_decode(&self, bytes: &[u8]) -> bool {
        self.should_decode_with_context(bytes, InputContext::Normal)
    }

    pub fn should_decode_with_context(&self, bytes: &[u8], context: InputContext) -> bool {
        if !self.is_idle() {
            return true;
        }
        let mut context = context;
        for &byte in bytes {
            if BRACKETED_PASTE_START.first() == Some(&byte)
                || self.bindings.iter().any(|binding| {
                    binding.active_in(context) && binding.sequence.first() == Some(&byte)
                })
            {
                return true;
            }
            match (context, byte) {
                (InputContext::Normal | InputContext::Visual, b'/' | b'?')
                | (InputContext::Insert | InputContext::Normal | InputContext::Visual, 0x06) => {
                    return true;
                }
                (InputContext::Search, b'\x1b' | b'\r' | b'\n') => return true,
                (InputContext::Search, _) => {}
                (InputContext::Normal, b'i') => context = InputContext::Insert,
                _ => {}
            }
        }
        false
    }

    pub fn reset(&mut self) {
        self.pending.clear();
        self.last_input = None;
        self.pending_context = InputContext::AppDisabled;
        self.search_return_context = None;
        self.in_bracketed_paste = false;
    }

    pub fn feed(&mut self, bytes: &[u8]) -> Vec<InputAction> {
        self.feed_with_context(bytes, InputContext::Normal)
    }

    pub fn feed_with_context(
        &mut self,
        bytes: &[u8],
        context: InputContext,
    ) -> Vec<InputAction> {
        self.feed_at_context(bytes, Instant::now(), context)
    }

    fn feed_at(&mut self, bytes: &[u8], now: Instant) -> Vec<InputAction> {
        self.feed_at_context(bytes, now, InputContext::Normal)
    }

    fn feed_at_context(
        &mut self,
        bytes: &[u8],
        now: Instant,
        context: InputContext,
    ) -> Vec<InputAction> {
        let mut actions = Vec::new();
        let mut passthrough = Vec::new();
        let mut context = context;

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

            if self.pending == b"\x1b" && byte == b'\x1b'
                && self.pending_context == InputContext::Insert
                && !self.bindings.iter().any(|binding| binding.active_in(self.pending_context) && binding.sequence == b"\x1b\x1b")
            {
                passthrough.extend_from_slice(b"\x1b\x1b");
                flush_passthrough(&mut actions, &mut passthrough);
                self.pending.clear();
                self.last_input = None;
                context = InputContext::Normal;
                continue;
            }
            if self.pending == b"\x1b" && byte == b'\x1b' {
                if let Some(command) = self
                    .bindings
                    .iter()
                    .find(|binding| {
                        binding.sequence == self.pending
                            && binding.active_in(self.pending_context)
                    })
                    .map(|binding| binding.command)
                {
                    flush_passthrough(&mut actions, &mut passthrough);
                    actions.push(InputAction::Command(command));
                    context = self.context_after_command(context, command);
                    self.pending.clear();
                    self.last_input = None;
                }
            }

            if self.pending.is_empty()
                && byte != b'\x1b'
                && !self.bindings.iter().any(|binding| {
                    binding.active_in(context)
                        && binding.sequence.len() > 1
                        && binding.sequence.first() == Some(&byte)
                })
            {
                if let Some(binding) = self.bindings.iter().find(|binding| {
                    binding.active_in(context)
                        && binding.sequence.len() == 1
                        && binding.sequence[0] == byte
                }) {
                    flush_passthrough(&mut actions, &mut passthrough);
                    if !(binding.command.kind == VIM_KEY && binding.command.payload.is_empty()) {
                        actions.push(InputAction::Command(binding.command));
                        context = self.context_after_command(context, binding.command);
                    }
                } else {
                    passthrough.push(byte);
                    context = self.context_after_raw_byte(context, byte);
                    }
                continue;
            }

            if self.pending.is_empty() {
                self.pending_context = context;
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

                let exact = self.bindings.iter().find(|binding| {
                    binding.active_in(self.pending_context) && binding.sequence == self.pending
                });
                let has_longer = self.bindings.iter().any(|binding| {
                    binding.active_in(self.pending_context)
                        && binding.sequence.len() > self.pending.len()
                        && binding.sequence.starts_with(&self.pending)
                });
                if !has_longer {
                    if let Some(binding) = exact {
                        flush_passthrough(&mut actions, &mut passthrough);
                        if !(binding.command.kind == VIM_KEY && binding.command.payload.is_empty()) {
                            actions.push(InputAction::Command(binding.command));
                            context = self.context_after_command(context, binding.command);
                        }
                        self.pending.clear();
                        self.last_input = None;
                        break;
                    }
                }
                if BRACKETED_PASTE_START.starts_with(&self.pending) || has_longer {
                    break;
                }
                if let Some((length, command)) = self
                    .bindings
                    .iter()
                    .filter(|binding| {
                        binding.active_in(self.pending_context)
                            && self.pending.len() > binding.sequence.len()
                            && self.pending.starts_with(&binding.sequence)
                    })
                    .max_by_key(|binding| binding.sequence.len())
                    .map(|binding| (binding.sequence.len(), binding.command))
                {
                    flush_passthrough(&mut actions, &mut passthrough);
                    if length == 1 && self.pending[0] == b'\x1b' {
                        passthrough.extend_from_slice(&self.pending);
                        self.pending.clear();
                        self.last_input = None;
                        break;
                    }
                    actions.push(InputAction::Command(command));
                    context = self.context_after_command(context, command);
                    self.pending.drain(..length);
                    if self.pending.is_empty() {
                        self.last_input = None;
                        break;
                    }
                    continue;
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

    fn context_after_command(
        &mut self,
        context: InputContext,
        command: InputCommand,
    ) -> InputContext {
        let next = context_after_command(context, command);
        if next == InputContext::Search && context != InputContext::Search {
            self.search_return_context = Some(context);
        } else if context == InputContext::Search && next != InputContext::Search {
            self.search_return_context = None;
        }
        next
    }

    fn context_after_raw_byte(&mut self, context: InputContext, byte: u8) -> InputContext {
        if context == InputContext::Search {
            if byte == b'\x1b' {
                return self.search_return_context.take().unwrap_or(InputContext::Normal);
            }
            if matches!(byte, b'\r' | b'\n') {
                let return_context = self.search_return_context.take().unwrap_or(InputContext::Normal);
                return if return_context == InputContext::Insert {
                    InputContext::Normal
                } else {
                    return_context
                };
            }
            return context;
        }
        if matches!(context, InputContext::Normal | InputContext::Visual)
            && matches!(byte, b'/' | b'?')
            || matches!(
                context,
                InputContext::Insert | InputContext::Normal | InputContext::Visual
            ) && byte == 0x06
        {
            self.search_return_context = Some(context);
            return InputContext::Search;
        }
        if byte == b'i' && context == InputContext::Normal {
            return InputContext::Insert;
        }
        context
    }

    pub fn remaining_timeout(&self, now: Instant) -> Option<Duration> {
        if self.in_bracketed_paste
            || (self.pending.len() > 1 && BRACKETED_PASTE_START.starts_with(&self.pending))
        {
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
            if let Some(binding) = self.bindings.iter().find(|binding| {
                binding.sequence == self.pending && binding.active_in(self.pending_context)
            }) {
                let command = binding.command;
                self.pending.clear();
                return Some(InputAction::Command(command));
            }
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
fn context_after_command(context: InputContext, command: InputCommand) -> InputContext {
    if matches!(command.kind, PREVIOUS_BUFFER | NEXT_BUFFER) {
        return InputContext::AppDisabled;
    }
    if command.kind != VIM_KEY {
        return context;
    }
    match command.payload {
        b"\x1b" | b"\x1b\x1b" => InputContext::Normal,
        b"i" => InputContext::Insert,
        b"v" | b"V" | b"\x16" => InputContext::Visual,
        b"/" | b"?" | b"\x06" => InputContext::Search,
        _ => context,
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
    fn vim_bindings_are_contextual_but_rmux_bindings_are_always_active() {
        let mut config = crate::config::Config::default();
        config.bindings.insert("vim_next_match".into(), "n".into());
        config
            .bindings
            .insert("vim_previous_match".into(), "N".into());
        let mut decoder = InputDecoder::configured(&config);

        assert_eq!(
            decoder.feed_with_context(b"nN", InputContext::AppDisabled),
            vec![InputAction::Bytes(b"nN".to_vec())]
        );
        assert_eq!(
            decoder.feed_with_context(b"\x12", InputContext::AppDisabled),
            vec![InputAction::Command(InputCommand {
                kind: RENAME_BUFFER,
                payload: b"",
            })]
        );
        assert_eq!(
            decoder.feed_with_context(b"nN", InputContext::Normal),
            vec![
                InputAction::Command(InputCommand {
                    kind: VIM_KEY,
                    payload: b"n",
                }),
                InputAction::Command(InputCommand {
                    kind: VIM_KEY,
                    payload: b"N",
                }),
            ]
        );
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
    fn configured_vim_binding_remaps_action_and_suppresses_default_key() {
        let mut config = crate::config::Config::default();
        config
            .bindings
            .insert("vim_next_match".into(), "x".into());
        let mut decoder = InputDecoder::configured(&config);
        let now = Instant::now();
        assert_eq!(
            decoder.feed_at(b"x", now),
            vec![InputAction::Command(InputCommand {
                kind: VIM_KEY,
                payload: b"n"
            })]
        );
        assert_eq!(decoder.feed_at(b"n", now), Vec::<InputAction>::new());
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
    #[test]
    fn configurable_vim_actions_emit_semantic_keys_and_disable_old_defaults() {
        let mut config = crate::config::Config::default();
        config.bindings.extend([
            ("vim_next_match".into(), "alt+n".into()),
            ("vim_previous_match".into(), "alt+shift+n".into()),
            ("vim_visual_block".into(), "ctrl+g".into()),
            ("vim_document_start".into(), "alt+g".into()),
        ]);
        let mut decoder = InputDecoder::configured(&config);
        assert_eq!(
            decoder.feed(b"\x1bn"),
            vec![InputAction::Command(InputCommand {
                kind: VIM_KEY,
                payload: b"n",
            })]
        );
        assert_eq!(decoder.feed(b"n"), Vec::<InputAction>::new());
        assert_eq!(
            decoder.feed(b"\x1bN"),
            vec![InputAction::Command(InputCommand {
                kind: VIM_KEY,
                payload: b"N",
            })]
        );
        assert_eq!(
            decoder.feed(b"\x07"),
            vec![InputAction::Command(InputCommand {
                kind: VIM_KEY,
                payload: b"\x16",
            })]
        );
        assert_eq!(
            decoder.feed(b"\x1bg"),
            vec![InputAction::Command(InputCommand {
                kind: VIM_KEY,
                payload: b"gg",
            })]
        );
        assert_eq!(decoder.feed(b"gg"), Vec::<InputAction>::new());
        assert_eq!(
            decoder.feed_with_context(b"n\x07", InputContext::AppDisabled),
            vec![InputAction::Bytes(b"n\x07".to_vec())]
        );
    }
    #[test]
    fn remapped_escape_does_not_shadow_alt_shortcuts_and_expires_as_a_binding() {
        let mut config = crate::config::Config::default();
        config
            .bindings
            .insert("vim_normal".into(), "ctrl+q".into());
        let mut decoder = InputDecoder::configured(&config);
        let now = Instant::now();
        assert!(decoder.feed_at(b"\x1b", now).is_empty());
        assert_eq!(
            decoder.feed_at(b"v", now + Duration::from_millis(1)),
            vec![InputAction::Command(InputCommand {
                kind: SPLIT_VERTICAL,
                payload: b"",
            })]
        );
        assert_eq!(decoder.expire(now + SEQUENCE_TIMEOUT), None);

        let mut decoder = InputDecoder::configured(&config);
        assert!(decoder.feed_at(b"\x1b", now).is_empty());
        assert_eq!(
            decoder.expire(now + SEQUENCE_TIMEOUT),
            Some(InputAction::Bytes(b"\x1b".to_vec()))
        );
    }
    #[test]
    fn escape_binding_waits_for_alt_shortcuts_then_activates_on_timeout() {
        let mut config = crate::config::Config::default();
        config
            .bindings
            .insert("vim_normal".into(), "esc".into());
        let mut decoder = InputDecoder::configured(&config);
        let now = Instant::now();
        assert!(decoder.feed_at(b"\x1b", now).is_empty());
        assert_eq!(
            decoder.expire(now + SEQUENCE_TIMEOUT),
            Some(InputAction::Command(InputCommand {
                kind: VIM_KEY,
                payload: b"\x1b",
            }))
        );
    }
    #[test]
    fn mode_specific_bindings_preserve_insert_search_and_app_input() {
        let mut config = crate::config::Config::default();
        config.bindings.extend([
            ("vim_normal".into(), "alt+z".into()),
            ("vim_scroll_insert_down".into(), "alt+x".into()),
            ("vim_insert_search".into(), "alt+c".into()),
            ("vim_scroll_half_down".into(), "ctrl+g".into()),
        ]);
        let mut decoder = InputDecoder::configured(&config);
        assert!(decoder.should_decode_with_context(b"\x1bz", InputContext::Insert));
        assert!(decoder.should_decode_with_context(b"\x1bc", InputContext::Insert));
        assert!(!decoder.should_decode_with_context(b"n", InputContext::Insert));
        assert!(!decoder.should_decode_with_context(b"\x07", InputContext::Insert));
        assert_eq!(
            decoder.feed_with_context(b"literal", InputContext::Insert),
            vec![InputAction::Bytes(b"literal".to_vec())]
        );
        assert_eq!(
            decoder.feed_with_context(b"\x1bz", InputContext::Insert),
            vec![InputAction::Command(InputCommand {
                kind: VIM_KEY,
                payload: b"\x1b\x1b",
            })]
        );
        assert_eq!(
            decoder.feed_with_context(b"\x1bx", InputContext::Insert),
            vec![InputAction::Command(InputCommand {
                kind: VIM_KEY,
                payload: b"\x1bd",
            })]
        );
        assert_eq!(
            decoder.feed_with_context(b"\x1bc", InputContext::Insert),
            vec![InputAction::Command(InputCommand {
                kind: VIM_KEY,
                payload: b"\x06",
            })]
        );
        assert_eq!(
            decoder.feed_with_context(b"\x07", InputContext::Normal),
            vec![InputAction::Command(InputCommand {
                kind: VIM_KEY,
                payload: b"\x04",
            })]
        );
        assert_eq!(
            decoder.feed_with_context(b"\x1bx", InputContext::Normal),
            vec![InputAction::Bytes(b"\x1bx".to_vec())]
        );
        let actions = decoder.feed_with_context(b"query n\x1bz", InputContext::Search);
        assert!(actions
            .iter()
            .all(|action| matches!(action, InputAction::Bytes(_))));
        assert_eq!(
            actions
                .into_iter()
                .flat_map(|action| match action {
                    InputAction::Bytes(bytes) => bytes,
                    InputAction::Command(_) => unreachable!(),
                })
                .collect::<Vec<_>>(),
            b"query n\x1bz"
        );
        assert_eq!(
            decoder.feed_with_context(b"\x1bz", InputContext::AppDisabled),
            vec![InputAction::Bytes(b"\x1bz".to_vec())]
        );
        assert_eq!(
            decoder.feed_with_context(b"\x12", InputContext::Search),
            vec![InputAction::Command(InputCommand {
                kind: RENAME_BUFFER,
                payload: b"",
            })]
        );
    }
    #[test]
    fn remapped_normal_exit_is_available_from_search() {
        let mut config = crate::config::Config::default();
        config
            .bindings
            .insert("vim_normal".into(), "ctrl+q".into());
        let mut decoder = InputDecoder::configured(&config);
        assert_eq!(
            decoder.feed_with_context(b"\x11", InputContext::Search),
            vec![InputAction::Command(InputCommand {
                kind: VIM_KEY,
                payload: b"\x1b",
            })]
        );
        assert_eq!(
            decoder.feed_with_context(b"typed", InputContext::Search),
            vec![InputAction::Bytes(b"typed".to_vec())]
        );
    }

    #[test]
    fn remapped_normal_uses_alt_escape_only_in_insert() {
        let mut config = crate::config::Config::default();
        config.bindings.insert("vim_normal".into(), "ctrl+q".into());
        let mut decoder = InputDecoder::configured(&config);
        assert_eq!(
            decoder.feed_with_context(b"\x11", InputContext::Insert),
            vec![InputAction::Command(InputCommand {
                kind: VIM_KEY,
                payload: b"\x1b\x1b",
            })]
        );
        for context in [InputContext::Normal, InputContext::Visual] {
            assert_eq!(
                decoder.feed_with_context(b"\x11", context),
                vec![InputAction::Command(InputCommand {
                    kind: VIM_KEY,
                    payload: b"\x1b",
                })]
            );
        }
    }

    #[test]
    fn insert_transition_is_applied_before_later_binding_in_same_batch() {
        let mut config = crate::config::Config::default();
        config.bindings.insert("vim_scroll_insert_down".into(), "x".into());
        let mut decoder = InputDecoder::configured(&config);
        assert!(decoder.should_decode_with_context(b"ix", InputContext::Normal));
        assert_eq!(
            decoder.feed_with_context(b"ix", InputContext::Normal),
            vec![
                InputAction::Bytes(b"i".to_vec()),
                InputAction::Command(InputCommand {
                    kind: VIM_KEY,
                    payload: b"\x1bd",
                }),
            ]
        );
    }

    #[test]
    fn buffer_switch_preserves_following_bytes_without_old_vim_context() {
        let mut config = crate::config::Config::default();
        config.bindings.insert("vim_next_match".into(), "x".into());
        let mut decoder = InputDecoder::configured(&config);
        assert_eq!(
            decoder.feed_with_context(b"\x08x", InputContext::Normal),
            vec![
                InputAction::Command(InputCommand {
                    kind: PREVIOUS_BUFFER,
                    payload: b"",
                }),
                InputAction::Bytes(b"x".to_vec()),
            ]
        );
    }

    #[test]
    fn page_down_has_a_distinct_semantic_payload_and_old_ctrl_f_is_suppressed() {
        let mut config = crate::config::Config::default();
        config.bindings.insert("vim_scroll_page_down".into(), "alt+p".into());
        let mut decoder = InputDecoder::configured(&config);
        assert_eq!(
            decoder.feed_with_context(b"\x1bp", InputContext::Normal),
            vec![InputAction::Command(InputCommand {
                kind: VIM_KEY,
                payload: b"\x1b[6~",
            })]
        );
        assert_eq!(
            decoder.feed_with_context(b"\x06", InputContext::Normal),
            Vec::<InputAction>::new()
        );
    }

    #[test]
    fn complete_alt_escape_is_preserved_in_insert_instead_of_matching_escape_prefix() {
        let mut config = crate::config::Config::default();
        config.bindings.insert("vim_normal".into(), "ctrl+q".into());
        let mut decoder = InputDecoder::configured(&config);
        assert_eq!(
            decoder.feed_with_context(b"\x1b\x1b", InputContext::Insert),
            vec![InputAction::Bytes(b"\x1b\x1b".to_vec())]
        );
    }

    #[test]
    fn same_semantic_vim_action_survives_when_mapped_in_two_contexts() {
        let mut config = crate::config::Config::default();
        config.bindings.extend([
            ("vim_next_match".into(), "x".into()),
            ("vim_match_next".into(), "alt+x".into()),
        ]);
        let mut decoder = InputDecoder::configured(&config);
        for sequence in [b"x".as_slice(), b"\x1bx".as_slice()] {
            assert_eq!(
                decoder.feed_with_context(sequence, InputContext::Normal),
                vec![InputAction::Command(InputCommand {
                    kind: VIM_KEY,
                    payload: b"n",
                })]
            );
        }
    }

    #[test]
    fn remapped_alt_escape_matches_as_a_complete_sequence_in_insert() {
        let mut config = crate::config::Config::default();
        config
            .bindings
            .insert("vim_normal".into(), "alt+ctrl+[".into());
        let mut decoder = InputDecoder::configured(&config);
        let now = Instant::now();
        assert!(decoder.feed_at_context(b"\x1b", now, InputContext::Insert).is_empty());
        assert_eq!(
            decoder.feed_at_context(b"\x1b", now + Duration::from_millis(1), InputContext::Insert),
            vec![InputAction::Command(InputCommand {
                kind: VIM_KEY,
                payload: b"\x1b\x1b",
            })]
        );
    }

    #[test]
    fn disjoint_context_defaults_and_remaps_survive_shared_semantic_key() {
        let mut config = crate::config::Config::default();
        config.bindings.extend([
            ("vim_insert_search".into(), "alt+c".into()),
            ("vim_scroll_page_down".into(), "alt+p".into()),
        ]);
        let mut decoder = InputDecoder::configured(&config);
        assert_eq!(
            decoder.feed_with_context(b"\x1bc", InputContext::Insert),
            vec![InputAction::Command(InputCommand {
                kind: VIM_KEY,
                payload: b"\x06",
            })]
        );
        assert_eq!(
            decoder.feed_with_context(b"\x1bp", InputContext::Normal),
            vec![InputAction::Command(InputCommand {
                kind: VIM_KEY,
                payload: b"\x1b[6~",
            })]
        );
        assert_eq!(
            decoder.feed_with_context(b"\x06", InputContext::Insert),
            Vec::<InputAction>::new()
        );
        assert_eq!(
            decoder.feed_with_context(b"\x06", InputContext::Normal),
            Vec::<InputAction>::new()
        );
    }

    #[test]
    fn search_query_is_literal_despite_normal_mode_remap() {
        let mut config = crate::config::Config::default();
        config.bindings.insert("vim_left".into(), "x".into());
        let mut decoder = InputDecoder::configured(&config);

        assert!(decoder.should_decode_with_context(b"/x\r", InputContext::Normal));
        let actions = decoder.feed_with_context(b"/x\r", InputContext::Normal);
        assert!(!actions.iter().any(
            |action| matches!(action, InputAction::Command(command) if command.payload == b"h")
        ));
        let bytes: Vec<u8> = actions
            .into_iter()
            .filter_map(|action| match action {
                InputAction::Bytes(bytes) => Some(bytes),
                InputAction::Command(_) => None,
            })
            .flatten()
            .collect();
        assert_eq!(bytes, b"/x\r");
    }

    #[test]
    fn remapped_mode_exit_and_entry_apply_within_one_read() {
        let mut config = crate::config::Config::default();
        config.bindings.insert("vim_normal".into(), "ctrl+q".into());
        config.bindings.insert("vim_insert".into(), "x".into());
        let mut decoder = InputDecoder::configured(&config);

        assert_eq!(
            decoder.feed_with_context(b"\x11x", InputContext::Normal),
            vec![
                InputAction::Command(InputCommand {
                    kind: VIM_KEY,
                    payload: b"\x1b",
                }),
                InputAction::Command(InputCommand {
                    kind: VIM_KEY,
                    payload: b"i",
                }),
            ]
        );
    }
}
