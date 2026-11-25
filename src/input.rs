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
    Operator,
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
            binding_contexts
                .iter()
                .any(|context| contexts.contains(context))
        })
    }
}

fn matching_binding<'a>(
    bindings: &'a [Binding],
    sequence: &[u8],
    context: InputContext,
) -> Option<&'a Binding> {
    bindings
        .iter()
        .filter(|binding| binding.active_in(context) && binding.sequence == sequence)
        .max_by_key(|binding| binding.vim_contexts.is_some())
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
    for action in config
        .bindings
        .keys()
        .filter(|action| action.starts_with("vim_"))
    {
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
        let Some(mut sequence) = crate::config::parse_key(key) else {
            continue;
        };
        if action == "vim_insert_normal" && key == "alt+esc" {
            sequence = b"\x1b\x1b".to_vec();
        }
        let search_action = action_contexts(action).contains(&InputContext::Search);
        if search_action && sequence.iter().all(u8::is_ascii_graphic) {
            continue;
        }
        let contexts: &'static [InputContext] = if action.starts_with("vim_") {
            action_contexts(action)
        } else {
            &[]
        };
        bindings.retain(|binding| {
            if action.starts_with("vim_") {
                let old_default_on_new_key = binding.vim_action.as_deref() == Some(action.as_str())
                    && binding.sequence == sequence;
                !old_default_on_new_key
                    && (binding.vim_contexts.is_none()
                        || binding.sequence != sequence
                        || !binding.overlaps_contexts(contexts))
            } else {
                binding.command.kind != kind || binding.command.payload != payload
            }
        });
        if action == "vim_normal" {
            let normal_contexts = if sequence.starts_with(b"\x1b") && sequence != b"\x1b" {
                &[
                    InputContext::Normal,
                    InputContext::Visual,
                    InputContext::Operator,
                ][..]
            } else {
                &[
                    InputContext::Normal,
                    InputContext::Visual,
                    InputContext::Operator,
                    InputContext::Search,
                ][..]
            };
            if !config.bindings.contains_key("vim_insert_normal") {
                bindings.push(Binding {
                    sequence: sequence.clone(),
                    command: InputCommand {
                        kind,
                        payload: b"\x1b\x1b",
                    },
                    vim_contexts: Some(&[InputContext::Insert]),
                    vim_action: Some(action.clone()),
                });
            }
            bindings.push(Binding {
                sequence,
                command: InputCommand {
                    kind,
                    payload: b"\x1b",
                },
                vim_contexts: Some(normal_contexts),
                vim_action: Some(action.clone()),
            });
        } else {
            bindings.push(Binding {
                sequence,
                command: InputCommand { kind, payload },
                vim_contexts: action
                    .starts_with("vim_")
                    .then_some(action_contexts(action)),
                vim_action: action.starts_with("vim_").then_some(action.clone()),
            });
        }
    }
    bindings
}

pub(crate) fn action_contexts(action: &str) -> &'static [InputContext] {
    use InputContext::*;
    match action {
        "vim_normal" => &[Normal, Visual, Operator],
        "vim_insert_normal" => &[Insert],
        "vim_insert_search" => &[Insert],
        "vim_insert" => &[Normal],
        "vim_yank" => &[Normal, Visual, Operator],
        "vim_visual" | "vim_visual_line" | "vim_visual_block" => &[Normal, Visual],
        "vim_visual_swap" | "vim_visual_cycle" => &[Visual],
        "vim_search_accept" | "vim_search_cancel" | "vim_search_backspace" => &[Search],
        "vim_search_control" => &[Normal, Visual],
        "vim_yank_inner" | "vim_yank_around" => &[Operator],
        "vim_left"
        | "vim_down"
        | "vim_up"
        | "vim_right"
        | "vim_word_forward"
        | "vim_word_backward"
        | "vim_word_end"
        | "vim_word_forward_big"
        | "vim_word_backward_big"
        | "vim_word_end_big"
        | "vim_line_start"
        | "vim_first_nonblank"
        | "vim_line_end"
        | "vim_document_start"
        | "vim_document_end" => &[Normal, Visual, Operator],
        "vim_scroll_insert_down" | "vim_scroll_insert_up" => &[Insert, Normal, Visual],
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
        "vim_visual_swap" => (VIM_KEY, b"o"),
        "vim_visual_cycle" => (VIM_KEY, b"t"),
        "vim_search_accept" => (VIM_KEY, b"\r"),
        "vim_search_cancel" => (VIM_KEY, b"\x1b"),
        "vim_insert_normal" => (VIM_KEY, b"\x1b\x1b"),
        "vim_search_control" => (VIM_KEY, b"\x06"),
        "vim_document_line_start" => (VIM_KEY, b"gh"),
        "vim_document_line_end" => (VIM_KEY, b"gl"),
        "vim_search_backspace" => (VIM_KEY, b"\x7f"),
        "vim_yank_inner" => (VIM_KEY, b"i"),
        "vim_yank_around" => (VIM_KEY, b"a"),
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
        "vim_yank_inner" => b"i",
        "vim_yank_around" => b"a",
        "vim_visual_swap" => b"o",
        "vim_visual_cycle" => b"t",
        "vim_search_accept" => b"\r",
        "vim_search_cancel" => b"\x1b",
        "vim_insert_normal" => b"\x1b\x1b",
        "vim_search_control" => b"\x06",
        "vim_document_line_start" => b"gh",
        "vim_document_line_end" => b"gl",
        "vim_search_backspace" => b"\x7f",
        "vim_insert" => b"i",
        "vim_normal" => b"\x1b",
        "vim_insert_search" => b"\x06",
        "vim_scroll_half_down" => b"\x04",
        "vim_scroll_half_up" => b"\x15",
        "vim_scroll_page_down" => b"\x1b[6~",
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
    operator_pending: bool,
    operator_selector_pending: bool,
    operator_selector_bytes: u8,
    operator_origin: Option<InputContext>,
    operator_count_started: bool,
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
            operator_pending: false,
            operator_selector_pending: false,
            operator_selector_bytes: 0,
            operator_origin: None,
            operator_count_started: false,
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
            operator_pending: false,
            operator_selector_pending: false,
            operator_selector_bytes: 0,
            operator_origin: None,
            operator_count_started: false,
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
        if self.operator_origin.is_some_and(|origin| origin != context)
            || !matches!(context, InputContext::Normal | InputContext::Visual)
                && self.operator_origin.is_some()
        {
            return true;
        }
        if self.operator_pending
            || self.operator_selector_pending
            || self.operator_selector_bytes > 0
        {
            return true;
        }
        if matches!(context, InputContext::Normal | InputContext::Visual)
            && self
                .bindings
                .iter()
                .any(|binding| binding.active_in(InputContext::Operator))
            && bytes.contains(&b'y')
        {
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
                (InputContext::Normal, b'v' | b'V' | 0x16) => context = InputContext::Visual,
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
        self.clear_operator();
    }
    pub(crate) fn sync_operator(&mut self, prefix: Option<&str>, count_started: bool) {
        self.clear_operator();
        match prefix {
            Some("y") => {
                self.operator_pending = true;
                self.operator_origin = Some(InputContext::Normal);
            }
            Some("yi" | "ya") => {
                self.operator_selector_pending = true;
                self.operator_origin = Some(InputContext::Normal);
            }
            _ => return,
        }
        self.operator_count_started = count_started;
    }

    pub fn feed(&mut self, bytes: &[u8]) -> Vec<InputAction> {
        self.feed_with_context(bytes, InputContext::Normal)
    }

    pub fn feed_with_context(&mut self, bytes: &[u8], context: InputContext) -> Vec<InputAction> {
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
        if !matches!(context, InputContext::Normal | InputContext::Visual)
            || self.operator_origin.is_some_and(|origin| origin != context)
        {
            self.clear_operator();
        }
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
            let binding_context = if self.operator_pending
                && matches!(context, InputContext::Normal | InputContext::Visual)
            {
                InputContext::Operator
            } else {
                context
            };
            let operator_binding = if self.pending.is_empty()
                && self.operator_pending
                && matches!(byte, b'i' | b'a')
            {
                matching_binding(&self.bindings, &[byte], binding_context)
            } else {
                None
            };
            let has_operator_binding = operator_binding.is_some_and(|binding| {
                binding.command.kind != VIM_KEY || !binding.command.payload.is_empty()
            });
            let suppress_operator_default = operator_binding.is_some_and(|binding| {
                binding.command.kind == VIM_KEY
                    && binding.command.payload.is_empty()
                    && matches!(
                        binding.vim_action.as_deref(),
                        Some("vim_yank_inner" | "vim_yank_around")
                    )
            });
            if self.operator_pending
                && self.pending.is_empty()
                && byte == b'0'
                && !self.operator_count_started
            {
                self.clear_operator();
            }
            if self.operator_pending
                && self.pending.is_empty()
                && matches!(byte, b'1'..=b'9' | b'0')
                && (byte != b'0' || self.operator_count_started)
            {
                passthrough.push(byte);
                self.operator_count_started = true;
                continue;
            }
            if suppress_operator_default && self.pending.is_empty() {
                continue;
            }
            if self.operator_pending
                && self.pending.is_empty()
                && matches!(byte, b'i' | b'a')
                && !has_operator_binding
            {
                passthrough.push(byte);
                self.operator_pending = false;
                self.operator_selector_pending = true;
                continue;
            }
            if self.operator_selector_bytes > 0 {
                passthrough.push(byte);
                self.operator_selector_bytes -= 1;
                if self.operator_selector_bytes == 0 {
                    self.operator_origin = None;
                    self.operator_count_started = false;
                }
                continue;
            }
            let selector_has_binding = self.bindings.iter().any(|binding| {
                let matches_first_byte = if byte == b'\x1b' {
                    binding.sequence.first() == Some(&byte)
                } else {
                    binding.sequence.len() == 1 && binding.sequence[0] == byte
                };
                matches_first_byte
                    && binding.active_in(context)
                    && (binding.vim_contexts.is_none()
                        || (binding.vim_action.as_deref() == Some("vim_normal")
                            && binding.sequence.first().is_some_and(u8::is_ascii_control)))
            });
            if self.operator_selector_pending && self.pending.is_empty() && !selector_has_binding {
                passthrough.push(byte);
                if byte.is_ascii_digit() && (byte != b'0' || self.operator_count_started) {
                    self.operator_count_started = true;
                    continue;
                }
                let extra = utf8_char_len(byte).saturating_sub(1);
                self.operator_selector_pending = false;
                self.operator_selector_bytes = extra as u8;
                if extra == 0 {
                    self.operator_origin = None;
                    self.operator_count_started = false;
                }
                continue;
            }

            if self.pending == b"\x1b"
                && byte == b'\x1b'
                && self.pending_context == InputContext::Insert
                && !self.bindings.iter().any(|binding| {
                    binding.active_in(self.pending_context) && binding.sequence == b"\x1b\x1b"
                })
            {
                passthrough.extend_from_slice(b"\x1b\x1b");
                flush_passthrough(&mut actions, &mut passthrough);
                self.pending.clear();
                self.last_input = None;
                context = InputContext::Normal;
                continue;
            }
            if self.pending == b"\x1b" && byte == b'\x1b' {
                let has_escape_sequence = self.bindings.iter().any(|binding| {
                    binding.active_in(self.pending_context)
                        && binding.sequence.starts_with(b"\x1b\x1b")
                });
                if !has_escape_sequence {
                    if let Some(command) =
                        matching_binding(&self.bindings, &self.pending, self.pending_context)
                            .map(|binding| binding.command)
                    {
                        flush_passthrough(&mut actions, &mut passthrough);
                        actions.push(InputAction::Command(command));
                        context = self.context_after_command(context, command);
                        self.pending.clear();
                        self.last_input = None;
                    }
                }
            }

            if self.pending.is_empty()
                && byte != b'\x1b'
                && !self.bindings.iter().any(|binding| {
                    binding.active_in(binding_context)
                        && binding.sequence.len() > 1
                        && binding.sequence.first() == Some(&byte)
                })
            {
                if let Some(binding) = matching_binding(&self.bindings, &[byte], binding_context) {
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
                self.pending_context = binding_context;
            }
            self.pending.push(byte);
            self.last_input = Some(now);
            loop {
                if self.pending == BRACKETED_PASTE_START {
                    self.clear_operator();
                    flush_passthrough(&mut actions, &mut passthrough);
                    passthrough.extend_from_slice(&self.pending);
                    self.pending.clear();
                    self.last_input = None;
                    self.in_bracketed_paste = true;
                    break;
                }

                let exact = matching_binding(&self.bindings, &self.pending, self.pending_context);
                let has_longer = self.bindings.iter().any(|binding| {
                    binding.active_in(self.pending_context)
                        && binding.sequence.len() > self.pending.len()
                        && binding.sequence.starts_with(&self.pending)
                });
                if !has_longer {
                    if let Some(binding) = exact {
                        flush_passthrough(&mut actions, &mut passthrough);
                        if !(binding.command.kind == VIM_KEY && binding.command.payload.is_empty())
                        {
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
                        if self.pending_context == InputContext::Search
                            && command.kind == VIM_KEY
                            && command.payload.is_empty()
                        {
                            self.pending.drain(..length);
                            if self.pending.is_empty() {
                                self.last_input = None;
                                break;
                            }
                            continue;
                        }
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
                let raw = self.pending.remove(0);
                passthrough.push(raw);
                self.track_vim_input(self.pending_context, &[raw]);
                if self.pending.is_empty() {
                    self.last_input = None;
                    break;
                }
            }
        }

        flush_passthrough(&mut actions, &mut passthrough);
        actions
    }

    fn clear_operator(&mut self) {
        self.operator_pending = false;
        self.operator_selector_pending = false;
        self.operator_selector_bytes = 0;
        self.operator_origin = None;
        self.operator_count_started = false;
    }

    fn track_vim_input(&mut self, context: InputContext, bytes: &[u8]) {
        if context == InputContext::Operator {
            if bytes == b"i" || bytes == b"a" {
                self.operator_pending = false;
                self.operator_selector_pending = true;
            } else {
                self.clear_operator();
            }
            return;
        }
        if !matches!(context, InputContext::Normal | InputContext::Visual) {
            self.clear_operator();
            return;
        }
        for &byte in bytes {
            if self.operator_pending {
                self.operator_pending = false;
                if byte == b'i' || byte == b'a' {
                    self.operator_selector_pending = true;
                    continue;
                }
                self.operator_origin = None;
                if byte == b'y' {
                    continue;
                }
            }
            if byte == b'y' {
                self.operator_pending = true;
                self.operator_origin = Some(context);
                self.operator_count_started = false;
            }
        }
    }

    fn context_after_command(
        &mut self,
        context: InputContext,
        command: InputCommand,
    ) -> InputContext {
        if context == InputContext::Visual && command.kind == VIM_KEY && command.payload == b"y" {
            self.clear_operator();
            return InputContext::Insert;
        }
        let operator_selector = self.operator_pending
            && command.kind == VIM_KEY
            && matches!(command.payload, b"i" | b"a");
        self.track_vim_input(
            context,
            if command.kind == VIM_KEY {
                command.payload
            } else {
                b""
            },
        );
        if operator_selector {
            return context;
        }
        if context == InputContext::Search
            && command.kind == VIM_KEY
            && matches!(command.payload, b"\x1b" | b"\r" | b"\n")
        {
            return self.context_after_raw_byte(context, command.payload[0]);
        }
        let next = context_after_command(context, command);
        if next == InputContext::Search && context != InputContext::Search {
            self.search_return_context = Some(context);
        } else if context == InputContext::Search && next != InputContext::Search {
            self.search_return_context = None;
        }
        if !matches!(next, InputContext::Normal | InputContext::Visual) {
            self.clear_operator();
        }
        next
    }

    fn context_after_raw_byte(&mut self, context: InputContext, byte: u8) -> InputContext {
        let operator_selector = self.operator_pending
            && matches!(context, InputContext::Normal | InputContext::Visual)
            && matches!(byte, b'i' | b'a');
        if context == InputContext::Visual && byte == b'y' {
            self.clear_operator();
            return InputContext::Insert;
        }
        if matches!(context, InputContext::Normal | InputContext::Visual) {
            self.track_vim_input(context, &[byte]);
        }
        if operator_selector {
            return context;
        }
        if context == InputContext::Search {
            if byte == b'\x1b' {
                return self
                    .search_return_context
                    .take()
                    .unwrap_or(InputContext::Normal);
            }
            if matches!(byte, b'\r' | b'\n') {
                let return_context = self
                    .search_return_context
                    .take()
                    .unwrap_or(InputContext::Normal);
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
        if context == InputContext::Normal && matches!(byte, b'v' | b'V' | 0x16) {
            return InputContext::Visual;
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
            if let Some(binding) =
                matching_binding(&self.bindings, &self.pending, self.pending_context)
            {
                let command = binding.command;
                let context = self.pending_context;
                self.pending.clear();
                self.context_after_command(context, command);
                return Some(InputAction::Command(command));
            }
            self.clear_operator();
            return Some(InputAction::Bytes(std::mem::take(&mut self.pending)));
        }
        None
    }
}

fn utf8_char_len(first: u8) -> usize {
    if first < 0x80 {
        1
    } else if first & 0xe0 == 0xc0 {
        2
    } else if first & 0xf0 == 0xe0 {
        3
    } else if first & 0xf8 == 0xf0 {
        4
    } else {
        1
    }
}

fn flush_passthrough(actions: &mut Vec<InputAction>, bytes: &mut Vec<u8>) {
    if !bytes.is_empty() {
        actions.push(InputAction::Bytes(std::mem::take(bytes)));
    }
}
fn context_after_command(context: InputContext, command: InputCommand) -> InputContext {
    if matches!(
        command.kind,
        PREVIOUS_BUFFER | NEXT_BUFFER | NEW_BUFFER | CLOSE_BUFFER | FOCUS_WINDOW | CLOSE_WINDOW
    ) {
        return InputContext::AppDisabled;
    }
    if command.kind != VIM_KEY {
        return context;
    }
    match command.payload {
        b"\x1b\x1b" => InputContext::Normal,
        b"\x1b" => match context {
            InputContext::Normal => InputContext::Insert,
            InputContext::Visual => InputContext::Normal,
            _ => context,
        },
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
        config.bindings.insert("vim_next_match".into(), "x".into());
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
        config.bindings.insert("vim_normal".into(), "ctrl+q".into());
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
        config.bindings.insert("vim_normal".into(), "esc".into());
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
            vec![InputAction::Command(InputCommand {
                kind: VIM_KEY,
                payload: b"\x1bd",
            })]
        );
        let actions = decoder.feed_with_context(b"query n\x1bz", InputContext::Search);
        assert!(
            actions
                .iter()
                .all(|action| matches!(action, InputAction::Bytes(_)))
        );
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
    fn configured_vim_insert_scroll_bindings_repeat_in_insert_normal_and_visual() {
        let mut config = crate::config::Config::default();
        config.bindings.extend([
            ("vim_scroll_insert_down".into(), "alt+x".into()),
            ("vim_scroll_insert_up".into(), "alt+y".into()),
        ]);
        let mut decoder = InputDecoder::configured(&config);
        let input = b"\x1bx\x1by\x1bx\x1by";
        let expected = vec![
            InputAction::Command(InputCommand {
                kind: VIM_KEY,
                payload: b"\x1bd",
            }),
            InputAction::Command(InputCommand {
                kind: VIM_KEY,
                payload: b"\x1bf",
            }),
            InputAction::Command(InputCommand {
                kind: VIM_KEY,
                payload: b"\x1bd",
            }),
            InputAction::Command(InputCommand {
                kind: VIM_KEY,
                payload: b"\x1bf",
            }),
        ];

        for context in [
            InputContext::Insert,
            InputContext::Normal,
            InputContext::Visual,
        ] {
            assert_eq!(
                decoder.feed_with_context(input, context),
                expected,
                "context {context:?}"
            );
        }
        assert_eq!(
            decoder.feed_with_context(input, InputContext::Search),
            vec![InputAction::Bytes(input.to_vec())]
        );
    }

    #[test]
    fn configured_visual_shape_switches_are_available_while_visual() {
        let mut config = crate::config::Config::default();
        config.bindings.extend([
            ("vim_visual".into(), "alt+x".into()),
            ("vim_visual_line".into(), "alt+y".into()),
            ("vim_visual_block".into(), "ctrl+g".into()),
        ]);
        let mut decoder = InputDecoder::configured(&config);

        assert_eq!(
            decoder.feed_with_context(b"\x1bx", InputContext::Visual),
            vec![InputAction::Command(InputCommand {
                kind: VIM_KEY,
                payload: b"v",
            })]
        );
        assert_eq!(
            decoder.feed_with_context(b"\x1by", InputContext::Visual),
            vec![InputAction::Command(InputCommand {
                kind: VIM_KEY,
                payload: b"V",
            })]
        );
        assert_eq!(
            decoder.feed_with_context(b"\x07", InputContext::Visual),
            vec![InputAction::Command(InputCommand {
                kind: VIM_KEY,
                payload: b"\x16",
            })]
        );
    }
    #[test]
    fn remapped_normal_exit_is_available_from_search() {
        let mut config = crate::config::Config::default();
        config.bindings.insert("vim_normal".into(), "ctrl+q".into());
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
        config
            .bindings
            .insert("vim_scroll_insert_down".into(), "x".into());
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
    fn focus_change_does_not_apply_old_buffer_vim_bindings() {
        let mut config = crate::config::Config::default();
        config.bindings.insert("vim_left".into(), "x".into());
        let mut decoder = InputDecoder::configured(&config);
        assert_eq!(
            decoder.feed_with_context(b"\x1blx", InputContext::Normal),
            vec![
                InputAction::Command(InputCommand {
                    kind: FOCUS_WINDOW,
                    payload: b"l"
                }),
                InputAction::Bytes(b"x".to_vec()),
            ]
        );
    }

    #[test]
    fn vim_remap_preserves_global_binding_in_other_modes() {
        let mut config = crate::config::Config::default();
        config.bindings.insert("vim_left".into(), "ctrl+h".into());
        let mut decoder = InputDecoder::configured(&config);
        assert_eq!(
            decoder.feed_with_context(b"\x08", InputContext::Insert),
            vec![InputAction::Command(InputCommand {
                kind: PREVIOUS_BUFFER,
                payload: b""
            })]
        );
        assert_eq!(
            decoder.feed_with_context(b"\x08", InputContext::Normal),
            vec![InputAction::Command(InputCommand {
                kind: VIM_KEY,
                payload: b"h"
            })]
        );
    }

    #[test]
    fn remapped_escape_updates_context_before_next_key() {
        let mut config = crate::config::Config::default();
        config.bindings.insert("vim_normal".into(), "ctrl+q".into());
        config.bindings.insert("vim_left".into(), "x".into());
        let mut decoder = InputDecoder::configured(&config);
        assert_eq!(
            decoder.feed_with_context(b"\x11x", InputContext::Normal),
            vec![
                InputAction::Command(InputCommand {
                    kind: VIM_KEY,
                    payload: b"\x1b"
                }),
                InputAction::Bytes(b"x".to_vec()),
            ]
        );
    }

    #[test]
    fn page_down_remap_preserves_ctrl_f_search() {
        let mut config = crate::config::Config::default();
        config
            .bindings
            .insert("vim_scroll_page_down".into(), "alt+p".into());
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
            vec![InputAction::Bytes(b"\x06".to_vec())]
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
        assert!(
            decoder
                .feed_at_context(b"\x1b", now, InputContext::Insert)
                .is_empty()
        );
        assert_eq!(
            decoder.feed_at_context(
                b"\x1b",
                now + Duration::from_millis(1),
                InputContext::Insert
            ),
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
            vec![InputAction::Bytes(b"\x06".to_vec())]
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
            decoder.feed_with_context(b"\x11x", InputContext::Visual),
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
    #[test]
    fn remapped_visual_and_search_actions_are_contextual() {
        let mut config = crate::config::Config::default();
        config.bindings.extend([
            ("vim_visual_swap".into(), "x".into()),
            ("vim_search_accept".into(), "ctrl+e".into()),
            ("vim_search_cancel".into(), "ctrl+c".into()),
            ("vim_search_backspace".into(), "ctrl+h".into()),
            ("vim_search_control".into(), "ctrl+g".into()),
        ]);
        let mut decoder = InputDecoder::configured(&config);

        assert_eq!(
            decoder.feed_with_context(b"x", InputContext::Visual),
            vec![InputAction::Command(InputCommand {
                kind: VIM_KEY,
                payload: b"o"
            })]
        );
        assert_eq!(
            decoder.feed_with_context(b"o", InputContext::Visual),
            Vec::<InputAction>::new()
        );
        assert_eq!(
            decoder.feed_with_context(b"x", InputContext::Normal),
            vec![InputAction::Bytes(b"x".to_vec())]
        );
        assert_eq!(
            decoder.feed_with_context(b"\x07", InputContext::Normal),
            vec![InputAction::Command(InputCommand {
                kind: VIM_KEY,
                payload: b"\x06"
            })]
        );
        assert_eq!(
            decoder.feed_with_context(b"\x05", InputContext::Search),
            vec![InputAction::Command(InputCommand {
                kind: VIM_KEY,
                payload: b"\r"
            })]
        );
        assert_eq!(
            decoder.feed_with_context(b"\x03", InputContext::Search),
            vec![InputAction::Command(InputCommand {
                kind: VIM_KEY,
                payload: b"\x1b"
            })]
        );
        assert_eq!(
            decoder.feed_with_context(b"\x08", InputContext::Search),
            vec![InputAction::Command(InputCommand {
                kind: VIM_KEY,
                payload: b"\x7f"
            })]
        );
        assert_eq!(
            decoder.feed_with_context(b"\r\x1b", InputContext::Search),
            Vec::<InputAction>::new()
        );
        assert_eq!(
            decoder.feed_with_context(b"q", InputContext::Search),
            vec![InputAction::Bytes(b"q".to_vec())]
        );
    }

    #[test]
    fn remapped_operator_keeps_literal_text_object_selectors_across_reads() {
        let mut config = crate::config::Config::default();
        config.bindings.extend([
            ("vim_yank".into(), "ctrl+y".into()),
            ("vim_insert".into(), "x".into()),
            ("vim_word_forward".into(), "z".into()),
        ]);
        let mut decoder = InputDecoder::configured(&config);
        let now = Instant::now();
        assert_eq!(
            decoder.feed_at_context(b"\x19iw", now, InputContext::Normal),
            vec![
                InputAction::Command(InputCommand {
                    kind: VIM_KEY,
                    payload: b"y"
                }),
                InputAction::Bytes(b"iw".to_vec()),
            ]
        );

        assert_eq!(
            decoder.feed_at_context(b"\x19", now, InputContext::Normal),
            vec![InputAction::Command(InputCommand {
                kind: VIM_KEY,
                payload: b"y"
            })]
        );
        assert_eq!(
            decoder.feed_at_context(b"iw", now + Duration::from_millis(1), InputContext::Normal),
            vec![InputAction::Bytes(b"iw".to_vec())]
        );

        assert_eq!(
            decoder.feed_at_context(
                b"\x19z",
                now + Duration::from_millis(2),
                InputContext::Normal
            ),
            vec![
                InputAction::Command(InputCommand {
                    kind: VIM_KEY,
                    payload: b"y"
                }),
                InputAction::Command(InputCommand {
                    kind: VIM_KEY,
                    payload: b"w"
                }),
            ]
        );

        assert_eq!(
            decoder.feed_at_context(
                b"\x19",
                now + Duration::from_millis(3),
                InputContext::Normal
            ),
            vec![InputAction::Command(InputCommand {
                kind: VIM_KEY,
                payload: b"y"
            })]
        );
        assert_eq!(
            decoder.feed_at_context(b"aw", now + Duration::from_millis(4), InputContext::Normal),
            vec![InputAction::Bytes(b"aw".to_vec())]
        );
    }
    #[test]
    fn operator_specific_inner_and_around_remaps_only_apply_after_yank() {
        let mut config = crate::config::Config::default();
        config.bindings.extend([
            ("vim_yank_inner".into(), "x".into()),
            ("vim_yank_around".into(), "q".into()),
        ]);
        let mut decoder = InputDecoder::configured(&config);

        assert_eq!(
            decoder.feed_with_context(b"xq", InputContext::Normal),
            vec![InputAction::Bytes(b"xq".to_vec())]
        );
        assert_eq!(
            decoder.feed_with_context(b"yxw", InputContext::Normal),
            vec![
                InputAction::Bytes(b"y".to_vec()),
                InputAction::Command(InputCommand {
                    kind: VIM_KEY,
                    payload: b"i"
                }),
                InputAction::Bytes(b"w".to_vec()),
            ]
        );
        assert_eq!(
            decoder.feed_with_context(b"yq(", InputContext::Normal),
            vec![
                InputAction::Bytes(b"y".to_vec()),
                InputAction::Command(InputCommand {
                    kind: VIM_KEY,
                    payload: b"a"
                }),
                InputAction::Bytes(b"(".to_vec()),
            ]
        );
    }
    #[test]
    fn raw_visual_entry_updates_context_before_remapped_visual_action() {
        let mut config = crate::config::Config::default();
        config.bindings.insert("vim_visual_swap".into(), "x".into());
        let mut decoder = InputDecoder::configured(&config);

        assert!(decoder.should_decode_with_context(b"vx", InputContext::Normal));
        assert_eq!(
            decoder.feed_with_context(b"vx", InputContext::Normal),
            vec![
                InputAction::Bytes(b"v".to_vec()),
                InputAction::Command(InputCommand {
                    kind: VIM_KEY,
                    payload: b"o"
                }),
            ]
        );
    }

    #[test]
    fn operator_count_digits_do_not_clear_pending_operator_or_selector() {
        let mut config = crate::config::Config::default();
        config.bindings.extend([
            ("vim_yank".into(), "ctrl+y".into()),
            ("vim_yank_inner".into(), "x".into()),
            ("vim_insert".into(), "x".into()),
            ("vim_word_forward".into(), "z".into()),
        ]);
        let mut decoder = InputDecoder::configured(&config);

        assert_eq!(
            decoder.feed_with_context(b"\x192xw", InputContext::Normal),
            vec![
                InputAction::Command(InputCommand {
                    kind: VIM_KEY,
                    payload: b"y"
                }),
                InputAction::Bytes(b"2".to_vec()),
                InputAction::Command(InputCommand {
                    kind: VIM_KEY,
                    payload: b"i"
                }),
                InputAction::Bytes(b"w".to_vec()),
            ]
        );
        assert_eq!(
            decoder.feed_with_context(b"\x19x2w", InputContext::Normal),
            vec![
                InputAction::Command(InputCommand {
                    kind: VIM_KEY,
                    payload: b"y"
                }),
                InputAction::Command(InputCommand {
                    kind: VIM_KEY,
                    payload: b"i"
                }),
                InputAction::Bytes(b"2w".to_vec()),
            ]
        );
        let mut config = crate::config::Config::default();
        config.bindings.extend([
            ("vim_yank".into(), "ctrl+y".into()),
            ("vim_insert".into(), "x".into()),
            ("vim_word_forward".into(), "z".into()),
        ]);
        let mut decoder = InputDecoder::configured(&config);
        assert_eq!(
            decoder.feed_with_context(b"\x19i2w", InputContext::Normal),
            vec![
                InputAction::Command(InputCommand {
                    kind: VIM_KEY,
                    payload: b"y"
                }),
                InputAction::Bytes(b"i2w".to_vec()),
            ]
        );
    }

    #[test]
    fn remapped_search_cancel_alt_escape_is_matched_as_one_sequence() {
        let mut config = crate::config::Config::default();
        config
            .bindings
            .insert("vim_search_cancel".into(), "alt+esc".into());
        let mut decoder = InputDecoder::configured(&config);

        assert!(
            decoder
                .feed_with_context(b"\x1b", InputContext::Search)
                .is_empty()
        );
        assert_eq!(
            decoder.feed_with_context(b"\x1b", InputContext::Search),
            vec![InputAction::Command(InputCommand {
                kind: VIM_KEY,
                payload: b"\x1b"
            })]
        );
    }
    #[test]
    fn configured_inner_selector_key_is_resolved_before_literal_fallback() {
        let mut config = crate::config::Config::default();
        config.bindings.extend([
            ("vim_yank".into(), "ctrl+y".into()),
            ("vim_yank_inner".into(), "a".into()),
        ]);
        let mut decoder = InputDecoder::configured(&config);
        assert_eq!(
            decoder.feed_with_context(b"\x19aw", InputContext::Normal),
            vec![
                InputAction::Command(InputCommand {
                    kind: VIM_KEY,
                    payload: b"y"
                }),
                InputAction::Command(InputCommand {
                    kind: VIM_KEY,
                    payload: b"i"
                }),
                InputAction::Bytes(b"w".to_vec()),
            ]
        );
    }

    #[test]
    fn operator_prefix_digits_do_not_consume_csi_terminal_sequences() {
        let mut decoder = InputDecoder::default();
        decoder.sync_operator(Some("y"), false);
        assert_eq!(
            decoder.feed_with_context(b"\x1b[104;6u", InputContext::Normal),
            vec![InputAction::Command(InputCommand {
                kind: REORDER_BUFFER_LEFT,
                payload: b"",
            })]
        );
    }

    #[test]
    fn first_zero_after_y_is_a_motion_not_a_count() {
        let mut config = crate::config::Config::default();
        config.bindings.extend([
            ("vim_yank".into(), "ctrl+y".into()),
            ("vim_yank_inner".into(), "x".into()),
        ]);
        let mut decoder = InputDecoder::configured(&config);
        assert_eq!(
            decoder.feed_with_context(b"\x190x", InputContext::Normal),
            vec![
                InputAction::Command(InputCommand {
                    kind: VIM_KEY,
                    payload: b"y"
                }),
                InputAction::Bytes(b"0x".to_vec()),
            ]
        );
    }
    #[test]
    fn remapped_inner_default_is_suppressed_but_insert_remap_preserves_yiw() {
        let mut config = crate::config::Config::default();
        config.bindings.extend([
            ("vim_yank".into(), "ctrl+y".into()),
            ("vim_yank_inner".into(), "x".into()),
        ]);
        let mut decoder = InputDecoder::configured(&config);
        assert_eq!(
            decoder.feed_with_context(b"\x19iq", InputContext::Normal),
            vec![
                InputAction::Command(InputCommand {
                    kind: VIM_KEY,
                    payload: b"y"
                }),
                InputAction::Bytes(b"q".to_vec()),
            ]
        );

        let mut config = crate::config::Config::default();
        config.bindings.extend([
            ("vim_yank".into(), "ctrl+y".into()),
            ("vim_insert".into(), "x".into()),
        ]);
        let mut decoder = InputDecoder::configured(&config);
        assert_eq!(
            decoder.feed_with_context(b"\x19iw", InputContext::Normal),
            vec![
                InputAction::Command(InputCommand {
                    kind: VIM_KEY,
                    payload: b"y"
                }),
                InputAction::Bytes(b"iw".to_vec()),
            ]
        );
    }

    #[test]
    fn expiring_remapped_inner_escape_preserves_selector_state() {
        let mut config = crate::config::Config::default();
        config.bindings.extend([
            ("vim_yank".into(), "ctrl+y".into()),
            ("vim_yank_inner".into(), "esc".into()),
        ]);
        let mut decoder = InputDecoder::configured(&config);
        let now = Instant::now();

        assert_eq!(
            decoder.feed_at_context(b"\x19", now, InputContext::Normal),
            vec![InputAction::Command(InputCommand {
                kind: VIM_KEY,
                payload: b"y"
            })]
        );
        assert!(
            decoder
                .feed_at_context(b"\x1b", now, InputContext::Normal)
                .is_empty()
        );
        assert_eq!(
            decoder.expire(now + SEQUENCE_TIMEOUT),
            Some(InputAction::Command(InputCommand {
                kind: VIM_KEY,
                payload: b"i"
            }))
        );
        assert_eq!(
            decoder.feed_at_context(b"w", now + SEQUENCE_TIMEOUT, InputContext::Normal),
            vec![InputAction::Bytes(b"w".to_vec())]
        );
    }
    #[test]
    fn remapped_control_mode_exit_wins_over_pending_text_object_selector() {
        let mut config = crate::config::Config::default();
        config.bindings.extend([
            ("vim_yank".into(), "ctrl+y".into()),
            ("vim_yank_inner".into(), "x".into()),
            ("vim_normal".into(), "ctrl+q".into()),
        ]);
        let mut decoder = InputDecoder::configured(&config);

        assert_eq!(
            decoder.feed_with_context(b"\x19x\x11", InputContext::Normal),
            vec![
                InputAction::Command(InputCommand {
                    kind: VIM_KEY,
                    payload: b"y",
                }),
                InputAction::Command(InputCommand {
                    kind: VIM_KEY,
                    payload: b"i",
                }),
                InputAction::Command(InputCommand {
                    kind: VIM_KEY,
                    payload: b"\x1b",
                }),
            ]
        );
    }

    #[test]
    fn printable_mode_exit_remap_remains_a_literal_text_object_selector() {
        let mut config = crate::config::Config::default();
        config.bindings.extend([
            ("vim_yank".into(), "ctrl+y".into()),
            ("vim_yank_inner".into(), "x".into()),
            ("vim_normal".into(), "w".into()),
        ]);
        let mut decoder = InputDecoder::configured(&config);

        assert_eq!(
            decoder.feed_with_context(b"\x19xw", InputContext::Normal),
            vec![
                InputAction::Command(InputCommand {
                    kind: VIM_KEY,
                    payload: b"y",
                }),
                InputAction::Command(InputCommand {
                    kind: VIM_KEY,
                    payload: b"i",
                }),
                InputAction::Bytes(b"w".to_vec()),
            ]
        );
    }
    #[test]
    fn application_shortcuts_and_escape_prefixes_win_over_operator_selector() {
        for (key, kind) in [
            (b"\x14".as_slice(), NEW_BUFFER),
            (b"\x17".as_slice(), CLOSE_BUFFER),
        ] {
            let mut config = crate::config::Config::default();
            config.bindings.insert("vim_yank".into(), "ctrl+y".into());
            let mut decoder = InputDecoder::configured(&config);
            assert_eq!(
                decoder.feed_with_context(b"\x19i", InputContext::Normal),
                vec![
                    InputAction::Command(InputCommand {
                        kind: VIM_KEY,
                        payload: b"y"
                    }),
                    InputAction::Bytes(b"i".to_vec()),
                ]
            );
            assert_eq!(
                decoder.feed_with_context(key, InputContext::Normal),
                vec![InputAction::Command(InputCommand { kind, payload: b"" })]
            );
        }

        let mut config = crate::config::Config::default();
        config.bindings.insert("vim_yank".into(), "ctrl+y".into());
        let mut decoder = InputDecoder::configured(&config);
        decoder.feed_with_context(b"\x19i", InputContext::Normal);
        assert!(
            decoder
                .feed_with_context(b"\x1b", InputContext::Normal)
                .is_empty()
        );
        assert_eq!(
            decoder.feed_with_context(b"h", InputContext::Normal),
            vec![InputAction::Command(InputCommand {
                kind: FOCUS_WINDOW,
                payload: b"h",
            })]
        );
    }
}
