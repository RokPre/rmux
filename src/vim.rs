use vt100::Screen;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VisualKind {
    Character,
    Line,
    Block,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Insert,
    Normal,
    Visual(VisualKind),
    Search,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Position {
    /// Absolute row in retained buffer history, zero at the oldest retained row.
    pub row: usize,
    /// Terminal-cell column.
    pub col: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct YankFlash {
    pub range: CellRange,
    pub kind: VisualKind,
    pub viewport_top: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellRange {
    pub start: Position,
    pub end: Position,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Ctrl(char),
    Alt(char),
    Escape,
    AltEscape,
    Enter,
    Backspace,
    PageDown,
    Bytes(Vec<u8>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    /// Forward input unchanged to the buffer's PTY.
    Forward(Vec<u8>),
    /// Text to deliver through the client's clipboard mechanism.
    Yank(String),
    SearchPrompt(String),
    SearchClosed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Direction {
    Forward,
    Backward,
}

/// Vim navigation state owned by one buffer and shared by every view of it.
#[derive(Clone, Debug)]
pub struct Vim {
    mode: Mode,
    cursor: Option<Position>,
    anchor: Option<Position>,
    count: usize,
    pending_count: usize,
    search: String,
    search_direction: Direction,
    matches: Vec<Vec<Position>>,
    active_match: Option<usize>,
    prompt: String,
    prompt_direction: Direction,
    return_mode: Mode,
    search_origin: Option<Position>,
    escape_pending: bool,
    pending: String,
    insert_pending: Vec<u8>,
    bracketed_paste: bool,
    search_utf8: Vec<u8>,
    last_yank: Option<String>,
    yank_flash: Option<YankFlash>,
}

impl Default for Vim {
    fn default() -> Self {
        Self::new()
    }
}
impl Vim {
    pub fn new() -> Self {
        Self {
            mode: Mode::Insert,
            cursor: None,
            anchor: None,
            count: 0,
            pending_count: 0,
            search: String::new(),
            search_direction: Direction::Forward,
            matches: Vec::new(),
            active_match: None,
            prompt: String::new(),
            prompt_direction: Direction::Forward,
            return_mode: Mode::Normal,
            escape_pending: false,
            pending: String::new(),
            insert_pending: Vec::new(),
            bracketed_paste: false,
            search_origin: None,
            yank_flash: None,
            search_utf8: Vec::new(),
            last_yank: None,
        }
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }
    pub(crate) fn operator_pending(&self) -> Option<(&str, bool)> {
        (self.mode == Mode::Normal && matches!(self.pending.as_str(), "y" | "yi" | "ya"))
            .then_some((self.pending.as_str(), self.count != 0))
    }
    pub(crate) fn has_pending_sequence(&self) -> bool {
        self.escape_pending
            || !self.search_utf8.is_empty()
            || (self.mode == Mode::Insert
                && (!self.insert_pending.is_empty() || self.bracketed_paste))
            || (matches!(self.mode, Mode::Normal | Mode::Visual(_))
                && (self.count != 0 || self.pending_count != 0 || !self.pending.is_empty()))
    }

    pub(crate) fn cancel_pending_sequence(&mut self) {
        self.count = 0;
        self.pending_count = 0;
        self.pending.clear();
        self.insert_pending.clear();
        self.bracketed_paste = false;
        self.search_utf8.clear();
        self.escape_pending = false;
    }
    pub fn search_text(&self) -> &str {
        &self.search
    }
    pub fn search_matches(&self) -> &[Vec<Position>] {
        &self.matches
    }
    pub fn prompt_text(&self) -> &str {
        &self.prompt
    }
    pub fn yank_flash(&self) -> Option<YankFlash> {
        self.yank_flash
    }
    pub fn clear_yank_flash(&mut self) {
        self.yank_flash = None;
    }
    pub fn selection_mode(&self) -> Option<&'static str> {
        let mode = if self.mode == Mode::Search {
            self.return_mode
        } else {
            self.mode
        };
        match mode {
            Mode::Visual(VisualKind::Character) => Some("visual"),
            Mode::Visual(VisualKind::Line) => Some("visual-line"),
            Mode::Visual(VisualKind::Block) => Some("visual-block"),
            _ => None,
        }
    }

    pub fn cursor(&mut self, screen: &mut Screen) -> Position {
        self.ensure_cursor(screen);
        self.cursor.unwrap()
    }

    pub fn selection(&mut self, screen: &mut Screen) -> Option<CellRange> {
        self.ensure_cursor(screen);
        let (a, b) = (self.anchor?, self.cursor?);
        Some(CellRange {
            start: a.min(b),
            end: a.max(b),
        })
    }

    /// Decode raw terminal input. Search mode retains incomplete UTF-8 between reads.
    pub fn handle_bytes(&mut self, screen: &mut Screen, bytes: &[u8]) -> Vec<Effect> {
        if self.mode == Mode::Insert {
            return self.handle_insert_bytes(screen, bytes);
        }
        if self.mode == Mode::Search {
            let mut effects = Vec::new();
            let mut offset = 0;
            while offset < bytes.len() {
                let byte = bytes[offset];
                if !self.search_utf8.is_empty() && byte < 0x80 {
                    self.search_utf8.clear();
                }
                match byte {
                    0x1b => {
                        effects.extend(self.handle_search_key(screen, Key::Escape));
                        offset += 1;
                        if self.mode != Mode::Search {
                            effects.extend(self.handle_bytes(screen, &bytes[offset..]));
                            break;
                        }
                    }
                    b'\r' | b'\n' => {
                        effects.extend(self.handle_search_key(screen, Key::Enter));
                        offset += 1;
                        if self.mode != Mode::Search {
                            effects.extend(self.handle_bytes(screen, &bytes[offset..]));
                            break;
                        }
                    }
                    0x7f | 0x08 => {
                        effects.extend(self.handle_search_key(screen, Key::Backspace));
                        offset += 1;
                    }
                    0x00..=0x1f => {
                        offset += 1;
                    }
                    0x20..=0x7e => {
                        effects.extend(self.handle_search_key(screen, Key::Char(char::from(byte))));
                        offset += 1;
                    }
                    _ => {
                        let expected =
                            utf8_char_len(self.search_utf8.first().copied().unwrap_or(byte));
                        self.search_utf8.push(byte);
                        offset += 1;
                        while self.search_utf8.len() < expected && offset < bytes.len() {
                            self.search_utf8.push(bytes[offset]);
                            offset += 1;
                        }
                        let decoded = std::str::from_utf8(&self.search_utf8).map(str::to_owned);
                        match decoded {
                            Ok(text) => {
                                self.search_utf8.clear();
                                for c in text.chars() {
                                    effects.extend(self.handle_search_key(screen, Key::Char(c)));
                                }
                            }
                            Err(error) if error.error_len().is_none() => break,
                            Err(_) => self.search_utf8.clear(),
                        }
                    }
                }
            }
            return effects;
        }
        let mut effects = Vec::new();
        let mut rest = bytes;

        if rest.starts_with(b"\x1b[6~") {
            effects.extend(self.handle_key(screen, Key::PageDown));
            rest = &rest[4..];
        }
        if self.escape_pending {
            self.escape_pending = false;
            if rest.first() == Some(&0x1b) {
                effects.extend(self.handle_key(screen, Key::AltEscape));
                rest = &rest[1..];
            } else if let Some(&b) = rest.first() {
                if b == b'd' || b == b'f' {
                    effects.extend(self.handle_key(screen, Key::Alt(b as char)));
                    rest = &rest[1..];
                } else {
                    effects.extend(self.handle_key(screen, Key::Escape));
                }
            } else {
                self.escape_pending = true;
                return effects;
            }
        }
        while !rest.is_empty() {
            if rest.starts_with(b"\x1b[6~") {
                effects.extend(self.handle_key(screen, Key::PageDown));
                rest = &rest[4..];
                continue;
            }
            let byte = rest[0];
            if byte == 0x1b {
                if rest.len() == 1 {
                    self.escape_pending = true;
                    break;
                }
                if rest[1] == 0x1b {
                    effects.extend(self.handle_key(screen, Key::AltEscape));
                    rest = &rest[2..];
                    continue;
                }
                if rest[1] == b'd' || rest[1] == b'f' {
                    effects.extend(self.handle_key(screen, Key::Alt(rest[1] as char)));
                    rest = &rest[2..];
                    continue;
                }
                effects.extend(self.handle_key(screen, Key::Bytes(vec![byte])));
                rest = &rest[1..];
                continue;
            }
            if byte == 0x06 {
                effects.extend(self.handle_key(screen, Key::Ctrl('f')));
                rest = &rest[1..];
                continue;
            }
            if byte == b'\r' || byte == b'\n' {
                effects.extend(self.handle_key(screen, Key::Enter));
                rest = &rest[1..];
                continue;
            }
            if byte == 0x7f || byte == 0x08 {
                effects.extend(self.handle_key(screen, Key::Backspace));
                rest = &rest[1..];
                continue;
            }
            if byte < 0x20 {
                effects.extend(
                    self.handle_key(screen, Key::Ctrl(char::from(byte.saturating_add(b'a' - 1)))),
                );
                rest = &rest[1..];
                continue;
            }
            let char_len = utf8_char_len(byte);
            if rest.len() < char_len {
                effects.extend(self.handle_key(screen, Key::Bytes(rest.to_vec())));
                break;
            }
            let chunk = &rest[..char_len];
            if let Ok(text) = std::str::from_utf8(chunk) {
                for c in text.chars() {
                    effects.extend(self.handle_key(screen, Key::Char(c)));
                }
            } else {
                effects.extend(self.handle_key(screen, Key::Bytes(chunk.to_vec())));
            }
            rest = &rest[char_len..];
        }
        effects
    }
    fn handle_insert_bytes(&mut self, screen: &mut Screen, bytes: &[u8]) -> Vec<Effect> {
        const PASTE_START: &[u8] = b"\x1b[200~";
        const PASTE_END: &[u8] = b"\x1b[201~";
        self.insert_pending.extend_from_slice(bytes);
        let mut consumed = 0;
        let mut effects = Vec::new();
        let mut forwarded = Vec::new();

        loop {
            let pending = &self.insert_pending[consumed..];
            if pending.is_empty() {
                break;
            }

            if self.bracketed_paste {
                if pending.starts_with(PASTE_END) {
                    forwarded.extend_from_slice(PASTE_END);
                    consumed += PASTE_END.len();
                    self.bracketed_paste = false;
                    continue;
                }
                if PASTE_END.starts_with(pending) {
                    break;
                }
                forwarded.push(pending[0]);
                consumed += 1;
                continue;
            }

            if pending[0] == 0x06 {
                consumed += 1;
                self.insert_pending.drain(..consumed);
                consumed = 0;
                if !forwarded.is_empty() {
                    effects.push(Effect::Forward(std::mem::take(&mut forwarded)));
                }
                effects.extend(self.handle_key(screen, Key::Ctrl('f')));
                if self.mode != Mode::Insert {
                    let rest = std::mem::take(&mut self.insert_pending);
                    effects.extend(self.handle_bytes(screen, &rest));
                    break;
                }
                continue;
            }

            if pending.starts_with(PASTE_START) {
                forwarded.extend_from_slice(PASTE_START);
                consumed += PASTE_START.len();
                self.bracketed_paste = true;
                continue;
            }

            let shortcut = if pending.starts_with(b"\x1b\x1b") {
                Some((2, Key::AltEscape))
            } else if pending.starts_with(b"\x1bd") {
                Some((2, Key::Alt('d')))
            } else if pending.starts_with(b"\x1bf") {
                Some((2, Key::Alt('f')))
            } else {
                None
            };
            if let Some((len, key)) = shortcut {
                consumed += len;
                self.insert_pending.drain(..consumed);
                consumed = 0;
                if !forwarded.is_empty() {
                    effects.push(Effect::Forward(std::mem::take(&mut forwarded)));
                }
                effects.extend(self.handle_key(screen, key));
                if self.mode != Mode::Insert {
                    let rest = std::mem::take(&mut self.insert_pending);
                    effects.extend(self.handle_bytes(screen, &rest));
                    break;
                }
                continue;
            }

            if [b"\x1b\x1b".as_slice(), b"\x1bd", b"\x1bf", PASTE_START]
                .iter()
                .any(|sequence| sequence.starts_with(pending))
            {
                break;
            }

            forwarded.push(pending[0]);
            consumed += 1;
        }
        self.insert_pending.drain(..consumed);

        if !forwarded.is_empty() {
            effects.push(Effect::Forward(forwarded));
        }
        effects
    }

    /// Resolve a deferred ESC after the caller's key-sequence timeout.
    pub fn flush_pending(&mut self, screen: &mut Screen) -> Vec<Effect> {
        if self.mode == Mode::Insert && !self.insert_pending.is_empty() {
            return vec![Effect::Forward(std::mem::take(&mut self.insert_pending))];
        }
        if !std::mem::take(&mut self.escape_pending) {
            return Vec::new();
        }
        self.handle_key(screen, Key::Escape)
    }

    /// Handle a decoded key. Insert-mode keys pass through except the explicit
    /// mode-entry bindings; `handle_bytes` is available for fragmented input.
    /// Handle a decoded semantic key after configuration remapping. In search
    /// mode, `Key::Char` is always literal prompt text, never a Vim command.
    pub fn handle_decoded_key(&mut self, screen: &mut Screen, key: Key) -> Vec<Effect> {
        self.handle_key(screen, key)
    }

    pub fn handle_key(&mut self, screen: &mut Screen, key: Key) -> Vec<Effect> {
        self.ensure_cursor(screen);
        if self.mode == Mode::Insert {
            match key {
                Key::Ctrl('f') => {
                    self.sync_cursor(screen);
                    return self.open_search(Mode::Insert, Direction::Forward);
                }
                Key::Alt('d') => {
                    self.sync_cursor(screen);
                    self.mode = Mode::Normal;
                    self.scroll(screen, 5);
                    return Vec::new();
                }
                Key::Alt('f') => {
                    self.sync_cursor(screen);
                    self.mode = Mode::Normal;
                    self.scroll(screen, -5);
                    return Vec::new();
                }
                Key::AltEscape => {
                    self.sync_cursor(screen);
                    self.mode = Mode::Normal;
                    return Vec::new();
                }
                key => return forward(key),
            }
        }
        if self.mode == Mode::Search {
            return self.handle_search_key(screen, key);
        }
        match key {
            Key::Ctrl('f') => self.open_search(self.mode, Direction::Forward),
            Key::Alt('d') => {
                self.scroll(screen, 5);
                Vec::new()
            }
            Key::Alt('f') => {
                self.scroll(screen, -5);
                Vec::new()
            }
            Key::PageDown => {
                self.scroll(screen, -(screen.size().0.get() as i32));
                Vec::new()
            }
            Key::Ctrl('d') => {
                let count = std::mem::take(&mut self.count).max(1);
                let page = (screen.size().0.get() as usize / 2)
                    .max(1)
                    .saturating_mul(count)
                    .min(i32::MAX as usize) as i32;
                self.scroll(screen, -page);
                Vec::new()
            }
            Key::Ctrl('u') => {
                let count = std::mem::take(&mut self.count).max(1);
                let page = (screen.size().0.get() as usize / 2)
                    .max(1)
                    .saturating_mul(count)
                    .min(i32::MAX as usize) as i32;
                self.scroll(screen, page);
                Vec::new()
            }
            Key::Ctrl('b') => {
                let count = std::mem::take(&mut self.count).max(1);
                self.scroll(
                    screen,
                    (screen.size().0.get() as usize)
                        .saturating_mul(count)
                        .min(i32::MAX as usize) as i32,
                );
                Vec::new()
            }
            Key::Escape if matches!(self.mode, Mode::Visual(_)) => {
                self.clear_search();
                self.mode = Mode::Normal;
                self.anchor = None;
                self.count = 0;
                self.pending_count = 0;
                self.pending.clear();
                Vec::new()
            }
            Key::Escape => {
                self.clear_search();
                screen.set_scrollback(0);
                self.mode = Mode::Insert;
                self.anchor = None;
                self.count = 0;
                self.pending_count = 0;
                self.pending.clear();
                Vec::new()
            }
            Key::Char('i') if self.mode == Mode::Normal && self.pending.is_empty() => {
                screen.set_scrollback(0);
                self.mode = Mode::Insert;
                Vec::new()
            }
            Key::Char('v') => self.enter_visual(VisualKind::Character),
            Key::Char('V') => self.enter_visual(VisualKind::Line),
            Key::Ctrl('v') => self.enter_visual(VisualKind::Block),
            Key::Char('o') if matches!(self.mode, Mode::Visual(_)) => {
                if let (Some(anchor), Some(cursor)) = (self.anchor, self.cursor) {
                    self.anchor = Some(cursor);
                    self.set_cursor(screen, anchor);
                }
                Vec::new()
            }
            Key::Char('t') if matches!(self.mode, Mode::Visual(_)) => {
                self.mode = Mode::Visual(match self.mode {
                    Mode::Visual(VisualKind::Character) => VisualKind::Line,
                    Mode::Visual(VisualKind::Line) => VisualKind::Block,
                    _ => VisualKind::Character,
                });
                Vec::new()
            }
            Key::Char('y') if matches!(self.mode, Mode::Visual(_)) => {
                let text = self.yank(screen);
                screen.set_scrollback(0);
                self.mode = Mode::Insert;
                self.anchor = None;
                text.map(Effect::Yank).into_iter().collect()
            }
            Key::Char(c) if c.is_ascii_digit() && (c != '0' || self.count != 0) => {
                self.count = self
                    .count
                    .saturating_mul(10)
                    .saturating_add(c.to_digit(10).unwrap() as usize);
                Vec::new()
            }
            Key::Char(c) => {
                let explicit_count = self.count != 0;
                let n = std::mem::take(&mut self.count).max(1);
                if !self.pending.is_empty() {
                    let prefix = std::mem::take(&mut self.pending);
                    let count = self.pending_count.max(1).saturating_mul(n);
                    self.pending_count = 0;
                    if prefix == "g" && c == 'g' {
                        self.command(screen, 'g', count);
                        return Vec::new();
                    } else if prefix == "g" && (c == 'h' || c == 'l') {
                        if let Some(mut p) = self.cursor {
                            p.col = if c == 'h' {
                                first_nonblank(screen, p)
                            } else {
                                last_column(screen, p)
                            };
                            self.set_cursor(screen, p);
                        }
                        return Vec::new();
                    } else if prefix == "y" {
                        if c == 'y' {
                            self.yank_lines(screen, count);
                        } else if c == 'i' || c == 'a' {
                            self.pending.push_str(if c == 'i' { "yi" } else { "ya" });
                            self.pending_count = count;
                            return Vec::new();
                        } else {
                            self.yank_motion(screen, c, count);
                        }
                    } else if prefix == "yi" || prefix == "ya" {
                        self.yank_object(screen, prefix == "ya", c, count);
                    } else {
                        return Vec::new();
                    }
                    return self
                        .last_yank
                        .take()
                        .map(Effect::Yank)
                        .into_iter()
                        .collect();
                }
                if c == 'g' || c == 'y' {
                    self.pending.push(c);
                    self.pending_count = n;
                    if c == 'y' {
                        self.last_yank = None;
                    }
                    Vec::new()
                } else {
                    self.pending_count = 0;
                    self.command(screen, c, if c == 'G' && !explicit_count { 0 } else { n });
                    Vec::new()
                }
            }
            _ => {
                self.count = 0;
                self.pending_count = 0;
                self.pending.clear();
                Vec::new()
            }
        }
    }

    fn command(&mut self, screen: &mut Screen, key: char, count: usize) {
        let Some(mut p) = self.cursor else {
            return;
        };
        match key {
            'h' | 'l' => {
                let forward = key == 'l';
                for _ in 0..count.min(usize::from(screen.size().1.get() - 1)) {
                    let previous = p;
                    step_horizontal(screen, &mut p, forward);
                    if p == previous {
                        break;
                    }
                }
            }
            'j' => p.row = p.row.saturating_add(count),
            'k' => p.row = p.row.saturating_sub(count),
            '0' => p.col = 0,
            '^' => p.col = first_nonblank(screen, p),
            '$' => p.col = last_column(screen, p),
            'w' => p = word_motion_kind(screen, p, count, true, false),
            'b' => p = word_motion_kind(screen, p, count, false, false),
            'e' => p = word_end_kind(screen, p, count, false),
            'W' => p = word_motion_kind(screen, p, count, true, true),
            'B' => p = word_motion_kind(screen, p, count, false, true),
            'E' => p = word_end_kind(screen, p, count, true),
            'G' => {
                p.row = if count > 0 {
                    count - 1
                } else {
                    last_row(screen)
                }
            }
            'g' => p.row = count.saturating_sub(1),
            'K' => {
                self.scroll(screen, count.min(i32::MAX as usize) as i32);
                return;
            }
            'J' => {
                self.scroll(screen, -(count.min(i32::MAX as usize) as i32));
                return;
            }
            'H' => p.row = top_visible_row(screen),
            'M' => p.row = middle_visible_row(screen),
            'L' => p.row = bottom_visible_row(screen),
            'n' => {
                self.advance_match(screen, false);
                return;
            }
            'N' => {
                self.advance_match(screen, true);
                return;
            }
            ',' => {
                self.advance_match_in(screen, Direction::Forward);
                return;
            }
            ';' => {
                self.advance_match_in(screen, Direction::Backward);
                return;
            }
            '/' | '?' => {
                let direction = if key == '/' {
                    Direction::Forward
                } else {
                    Direction::Backward
                };
                self.open_search(self.mode, direction);
                return;
            }
            _ => return,
        }
        self.set_cursor(screen, p);
    }

    fn yank_lines(&mut self, screen: &mut Screen, count: usize) {
        let Some(p) = self.cursor else {
            return;
        };
        let end = p
            .row
            .saturating_add(count.saturating_sub(1))
            .min(last_row(screen));
        let mut text = String::new();
        for row in p.row..=end {
            if row != p.row {
                text.push('\n');
            }
            text.push_str(&row_text(screen, row, 0, screen.size().1.get() - 1));
        }
        self.last_yank = Some(text);
    }

    fn yank_motion(&mut self, screen: &mut Screen, motion: char, count: usize) {
        let Some(start) = self.cursor else {
            return;
        };
        let mut end = start;
        if motion == 'j' || motion == 'k' {
            let first = if motion == 'k' {
                start.row.saturating_sub(count)
            } else {
                start.row
            };
            let last = if motion == 'j' {
                start.row.saturating_add(count).min(last_row(screen))
            } else {
                start.row
            };
            let mut text = String::new();
            for row in first..=last {
                if row != first {
                    text.push('\n');
                }
                text.push_str(&row_text(screen, row, 0, screen.size().1.get() - 1));
            }
            self.last_yank = Some(text);
            return;
        }
        let exclusive = matches!(motion, 'w' | 'W');
        match motion {
            'w' => end = word_motion_kind(screen, start, count, true, false),
            'W' => end = word_motion_kind(screen, start, count, true, true),
            'e' => end = word_end_kind(screen, start, count, false),
            'E' => end = word_end_kind(screen, start, count, true),
            'b' => end = word_motion_kind(screen, start, count, false, false),
            'B' => end = word_motion_kind(screen, start, count, false, true),
            'h' | 'l' => {
                let forward = motion == 'l';
                for _ in 0..count.min(usize::from(screen.size().1.get() - 1)) {
                    let previous = end;
                    step_horizontal(screen, &mut end, forward);
                    if end == previous {
                        break;
                    }
                }
            }
            '$' => end.col = last_column(screen, start),
            '0' => end.col = 0,
            'j' => end.row = end.row.saturating_add(count).min(last_row(screen)),
            'k' => end.row = end.row.saturating_sub(count),
            _ => return,
        }
        let (a, b) = (start.min(end), start.max(end));
        let mut text = String::new();
        for row in a.row..=b.row {
            if row != a.row {
                text.push('\n');
            }
            if exclusive && row == b.row && b.col == 0 {
                continue;
            }
            let first = if row == a.row { a.col } else { 0 };
            let last = if row == b.row {
                if exclusive {
                    b.col.saturating_sub(1)
                } else {
                    b.col
                }
            } else {
                screen.size().1.get() - 1
            };
            if first <= last {
                text.push_str(&row_text(screen, row, first, last));
            }
        }
        self.last_yank = Some(text);
    }

    fn yank_object(&mut self, screen: &mut Screen, around: bool, object: char, count: usize) {
        let Some(p) = self.cursor else {
            return;
        };
        if matches!(object, 'w' | 'W') {
            let big = object == 'W';
            let class = motion_class(screen, p, big);
            let (mut first, mut end) = (p, p);
            if class == 0 {
                while first.row > 0 || first.col > 0 {
                    let mut prior = first;
                    retreat(&mut prior, screen);
                    if motion_class(screen, prior, big) != 0 {
                        break;
                    }
                    first = prior;
                }
                while end.row < last_row(screen) || end.col + 1 < screen.size().1.get() {
                    let mut next = end;
                    advance(&mut next, screen);
                    if motion_class(screen, next, big) != 0 {
                        break;
                    }
                    end = next;
                }
            } else {
                while first.row > 0 || first.col > 0 {
                    let mut prior = first;
                    retreat(&mut prior, screen);
                    if motion_class(screen, prior, big) != class {
                        break;
                    }
                    first = prior;
                }
                while end.row < last_row(screen) || end.col + 1 < screen.size().1.get() {
                    let mut next = end;
                    advance(&mut next, screen);
                    if motion_class(screen, next, big) != class {
                        break;
                    }
                    end = next;
                }
                for _ in 1..count {
                    if end.row == last_row(screen) && end.col + 1 == screen.size().1.get() {
                        break;
                    }
                    let mut next = end;
                    while next.row < last_row(screen) || next.col + 1 < screen.size().1.get() {
                        advance(&mut next, screen);
                        if motion_class(screen, next, big) != 0 {
                            break;
                        }
                    }
                    if motion_class(screen, next, big) == 0 {
                        break;
                    }
                    let next_class = motion_class(screen, next, big);
                    end = next;
                    while end.row < last_row(screen) || end.col + 1 < screen.size().1.get() {
                        let mut following = end;
                        advance(&mut following, screen);
                        if motion_class(screen, following, big) != next_class {
                            break;
                        }
                        end = following;
                    }
                }
            }
            if around && class != 0 {
                let mut next = end;
                let mut has_trailing_space = false;
                while next.row < last_row(screen) || next.col + 1 < screen.size().1.get() {
                    advance(&mut next, screen);
                    if motion_class(screen, next, big) != 0 {
                        break;
                    }
                    end = next;
                    has_trailing_space = true;
                }
                if !has_trailing_space {
                    while first.row > 0 || first.col > 0 {
                        let mut prior = first;
                        retreat(&mut prior, screen);
                        if motion_class(screen, prior, big) != 0 {
                            break;
                        }
                        first = prior;
                    }
                }
            }
            self.last_yank = Some(range_text(screen, first, end));
            return;
        }
        let pair = match object {
            '(' | ')' | 'b' => ('(', ')'),
            '[' | ']' => ('[', ']'),
            '{' | '}' | 'B' => ('{', '}'),
            '<' | '>' => ('<', '>'),
            '"' => ('"', '"'),
            '\'' => ('\'', '\''),
            '`' => ('`', '`'),
            _ => return,
        };
        let (left, right) = find_object(screen, p, pair, count);
        let (Some(left), Some(right)) = (left, right) else {
            return;
        };
        let first = if around {
            left
        } else {
            advance_position(left, screen)
        };
        let last = if around {
            right
        } else {
            retreat_position(right, screen)
        };
        if first <= last {
            self.last_yank = Some(range_text(screen, first, last));
        }
    }
    fn open_search(&mut self, from: Mode, direction: Direction) -> Vec<Effect> {
        self.count = 0;
        self.pending_count = 0;
        self.pending.clear();
        self.return_mode = from;
        self.search_origin = self.cursor;
        self.prompt_direction = direction;
        self.prompt.clear();
        self.mode = Mode::Search;
        vec![Effect::SearchPrompt(String::new())]
    }

    fn clear_search(&mut self) {
        self.search.clear();
        self.prompt.clear();
        self.matches.clear();
        self.active_match = None;
    }

    fn handle_search_key(&mut self, screen: &mut Screen, key: Key) -> Vec<Effect> {
        match key {
            Key::Escape => {
                self.mode = self.return_mode;
                if let Some(origin) = self.search_origin.take() {
                    self.set_cursor(screen, origin);
                }
                if self.mode == Mode::Insert {
                    screen.set_scrollback(0);
                }
                self.clear_search();
                self.search_utf8.clear();
                vec![Effect::SearchClosed]
            }
            Key::Enter => {
                let preview_cursor = self.cursor;
                self.search.clone_from(&self.prompt);
                self.search_direction = self.prompt_direction;
                self.rebuild_matches(screen);
                self.mode = if self.return_mode == Mode::Insert {
                    Mode::Normal
                } else {
                    self.return_mode
                };
                let preview_match = preview_cursor.and_then(|cursor| {
                    self.matches
                        .iter()
                        .position(|positions| positions.first() == Some(&cursor))
                });
                if let Some(index) = preview_match.or_else(|| {
                    next_match(
                        &self.matches,
                        self.search_origin.unwrap_or(Position { row: 0, col: 0 }),
                        self.search_direction,
                        false,
                    )
                }) {
                    self.active_match = Some(index);
                    if let Some(position) = self.matches[index].first().copied() {
                        self.set_cursor(screen, position);
                    }
                }
                if self.matches.is_empty() {
                    if let Some(origin) = self.search_origin {
                        self.set_cursor(screen, origin);
                    }
                }
                self.search_origin = None;
                self.prompt.clear();
                vec![Effect::SearchClosed]
            }
            Key::Backspace => {
                self.prompt.pop();
                self.rebuild_prompt_matches(screen);
                vec![Effect::SearchPrompt(self.prompt.clone())]
            }
            Key::Char(c) if !c.is_control() => {
                self.prompt.push(c);
                self.rebuild_prompt_matches(screen);
                vec![Effect::SearchPrompt(self.prompt.clone())]
            }
            _ => Vec::new(),
        }
    }

    fn rebuild_prompt_matches(&mut self, screen: &mut Screen) {
        let saved = std::mem::replace(&mut self.search, self.prompt.clone());
        self.rebuild_matches(screen);
        self.search = saved;
        if let Some(origin) = self.search_origin {
            if let Some(index) = next_match(&self.matches, origin, self.prompt_direction, false) {
                self.active_match = Some(index);
                if let Some(position) = self.matches[index].first().copied() {
                    self.set_cursor(screen, position);
                }
            }
        }
    }
    fn rebuild_matches(&mut self, screen: &mut Screen) {
        self.matches = scan_matches(screen, &self.search);
        self.active_match = None;
    }
    fn rebuild_current_matches(&mut self, screen: &mut Screen) {
        if self.mode == Mode::Search {
            let saved = std::mem::replace(&mut self.search, self.prompt.clone());
            self.rebuild_matches(screen);
            self.search = saved;
        } else {
            self.rebuild_matches(screen);
        }
    }

    fn advance_match(&mut self, screen: &mut Screen, reverse: bool) {
        let direction = match (self.search_direction, reverse) {
            (Direction::Forward, false) | (Direction::Backward, true) => Direction::Forward,
            _ => Direction::Backward,
        };
        self.advance_match_in(screen, direction);
    }
    fn advance_match_in(&mut self, screen: &mut Screen, direction: Direction) {
        let Some(current) = self.cursor else {
            return;
        };
        if let Some(index) = next_match(&self.matches, current, direction, false) {
            self.active_match = Some(index);
            if let Some(position) = self.matches[index].first().copied() {
                self.set_cursor(screen, position);
            }
        }
    }

    fn enter_visual(&mut self, kind: VisualKind) -> Vec<Effect> {
        if !matches!(self.mode, Mode::Visual(_)) {
            self.anchor = self.cursor;
        }
        self.mode = Mode::Visual(kind);
        Vec::new()
    }

    fn yank(&mut self, screen: &mut Screen) -> Option<String> {
        let (anchor, cursor) = (self.anchor?, self.cursor?);
        let kind = match self.mode {
            Mode::Visual(kind) => kind,
            _ => return None,
        };
        let start = anchor.min(cursor);
        let end = anchor.max(cursor);
        self.yank_flash = Some(YankFlash {
            range: CellRange { start, end },
            kind,
            viewport_top: top_visible_row(screen),
        });
        let cols = screen.size().1.get();
        let mut text = String::new();
        match kind {
            VisualKind::Character | VisualKind::Line => {
                let (first, last) = if kind == VisualKind::Line {
                    (0, cols - 1)
                } else {
                    (start.col, end.col)
                };
                for row in start.row..=end.row {
                    let left = if kind == VisualKind::Line || row != start.row {
                        0
                    } else {
                        first
                    };
                    let right = if kind == VisualKind::Line || row != end.row {
                        cols - 1
                    } else {
                        last
                    };
                    text.push_str(&row_text(screen, row, left, right));
                    if row < end.row && !is_wrapped(screen, row) {
                        text.push('\n');
                    }
                }
            }
            VisualKind::Block => {
                for row in start.row..=end.row {
                    if row != start.row {
                        text.push('\n');
                    }
                    text.push_str(&block_text(
                        screen,
                        row,
                        start.col.min(end.col),
                        start.col.max(end.col),
                    ));
                }
            }
        }
        Some(text)
    }

    /// Reconcile absolute navigation positions after terminal history changes.
    pub fn history_changed(
        &mut self,
        screen: &mut Screen,
        dropped_rows: Option<usize>,
        alternate_changed: bool,
    ) {
        if alternate_changed || dropped_rows.is_none() {
            self.cursor = None;
            self.anchor = None;
            self.search_origin = None;
            self.matches.clear();
            self.active_match = None;
            self.yank_flash = None;
        } else if let Some(dropped_rows) = dropped_rows.filter(|rows| *rows > 0) {
            let rebase = |p: Position| {
                Some(Position {
                    row: p.row.checked_sub(dropped_rows)?,
                    col: p.col,
                })
            };
            if let Some(mut flash) = self.yank_flash {
                self.yank_flash = match (rebase(flash.range.start), rebase(flash.range.end)) {
                    (Some(start), Some(end)) => {
                        flash.range = CellRange { start, end };
                        flash.viewport_top = flash.viewport_top.saturating_sub(dropped_rows);
                        Some(flash)
                    }
                    _ => None,
                };
            }
            self.cursor = self.cursor.and_then(rebase);
            self.anchor = self.anchor.and_then(rebase);
            self.search_origin = self.search_origin.and_then(rebase);
            self.matches
                .retain(|positions| positions.iter().all(|p| p.row >= dropped_rows));
            for positions in &mut self.matches {
                for position in positions {
                    *position = rebase(*position).unwrap();
                }
            }
            self.active_match = None;
        }
        if matches!(self.mode, Mode::Visual(_)) && self.anchor.is_none() {
            self.mode = Mode::Normal;
        }
        self.clamp_anchor(screen);
        self.clamp_cursor(screen);
        self.rebuild_current_matches(screen);
    }

    pub fn resized(&mut self, screen: &mut Screen) {
        self.yank_flash = None;
        self.clamp_anchor(screen);
        self.clamp_cursor(screen);
        self.search_origin = self.search_origin.map(|mut p| {
            let (_, cols) = screen.size();
            p.row = p.row.min(last_row(screen));
            p.col = p.col.min(cols.get() - 1);
            p
        });
        self.rebuild_current_matches(screen);
    }

    fn scroll(&mut self, screen: &mut Screen, delta: i32) {
        let old = screen.scrollback() as i64;
        let max = screen.scrollback_len() as i64;
        let new = (old + i64::from(delta)).clamp(0, max) as usize;
        screen.set_scrollback(new);
        let shift = (old - new as i64) as isize;
        let (rows, _) = screen.size();
        let top = top_visible_row(screen);
        let bottom = top + usize::from(rows.get()) - 1;
        if let Some(mut p) = self.cursor {
            p.row = p.row.saturating_add_signed(shift).clamp(top, bottom);
            self.cursor = Some(p);
            self.clamp_cursor(screen);
        }
        self.clamp_anchor(screen);
    }

    fn clamp_anchor(&mut self, screen: &Screen) {
        if let Some(mut p) = self.anchor {
            let (_, cols) = screen.size();
            p.row = p.row.min(last_row(screen));
            p.col = p.col.min(cols.get() - 1);
            self.anchor = Some(p);
        }
    }

    fn ensure_cursor(&mut self, screen: &mut Screen) {
        if self.cursor.is_none() {
            let (row, col) = screen.cursor_position();
            self.cursor = Some(Position {
                row: top_visible_row(screen) + usize::from(row),
                col,
            });
        }
        self.clamp_cursor(screen);
        self.clamp_anchor(screen);
    }
    fn sync_cursor(&mut self, screen: &mut Screen) {
        self.cursor = None;
        self.ensure_cursor(screen);
    }
    fn clamp_cursor(&mut self, screen: &mut Screen) {
        if let Some(mut p) = self.cursor {
            let (rows, cols) = screen.size();
            let top = top_visible_row(screen);
            p.row = p.row.clamp(top, top + usize::from(rows.get()) - 1);
            p.col = p.col.min(cols.get() - 1);
            if is_wide_continuation_at(screen, p.row, p.col) && p.col > 0 {
                p.col -= 1;
            }
            self.cursor = Some(p);
        }
    }
    fn set_cursor(&mut self, screen: &mut Screen, p: Position) {
        self.cursor = Some(p);
        let (rows, _) = screen.size();
        let top = top_visible_row(screen);
        let bottom = top + usize::from(rows.get()) - 1;
        if p.row < top {
            screen.set_scrollback((screen.scrollback() + top - p.row).min(screen.scrollback_len()));
        } else if p.row > bottom {
            screen.set_scrollback(screen.scrollback().saturating_sub(p.row - bottom));
        }
        self.clamp_cursor(screen);
    }
}
fn advance_position(mut p: Position, screen: &Screen) -> Position {
    advance(&mut p, screen);
    p
}
fn retreat_position(mut p: Position, screen: &Screen) -> Position {
    retreat(&mut p, screen);
    p
}
fn range_text(screen: &mut Screen, start: Position, end: Position) -> String {
    let (a, b) = (start.min(end), start.max(end));
    let mut result = String::new();
    for row in a.row..=b.row {
        if row != a.row {
            result.push('\n');
        }
        let first = if row == a.row { a.col } else { 0 };
        let last = if row == b.row {
            b.col
        } else {
            screen.size().1.get() - 1
        };
        if first <= last {
            result.push_str(&row_text(screen, row, first, last));
        }
    }
    result
}
fn find_object(
    screen: &mut Screen,
    p: Position,
    pair: (char, char),
    count: usize,
) -> (Option<Position>, Option<Position>) {
    if pair.0 == pair.1 {
        for row in 0..=last_row(screen) {
            let mut quote = None;
            for col in 0..screen.size().1.get() {
                let at = Position { row, col };
                if char_at(screen, at) != pair.0 {
                    continue;
                }
                if let Some(left) = quote.take() {
                    if left <= p && p <= at {
                        return (Some(left), Some(at));
                    }
                } else {
                    quote = Some(at);
                }
            }
        }
        return (None, None);
    }
    let mut candidates = Vec::new();
    for row in 0..=last_row(screen) {
        for col in 0..screen.size().1.get() {
            let at = Position { row, col };
            if char_at(screen, at) == pair.0 {
                candidates.push(at);
            }
        }
    }
    for candidate in candidates
        .into_iter()
        .filter(|candidate| *candidate <= p)
        .rev()
    {
        let mut depth = 0usize;
        let mut right = None;
        let mut cursor = candidate;
        loop {
            let c = char_at(screen, cursor);
            if c == pair.0 {
                depth += 1;
            }
            if c == pair.1 {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    right = Some(cursor);
                    break;
                }
            }
            if cursor.row == last_row(screen) && cursor.col + 1 == screen.size().1.get() {
                break;
            }
            advance(&mut cursor, screen);
            if cursor.row > last_row(screen) {
                break;
            }
        }
        let Some(right) = right else {
            continue;
        };
        if p > right {
            continue;
        }
        let mut left = candidate;
        let mut end = right;
        for _ in 1..count.max(1) {
            let mut before = left;
            let mut found_left = None;
            while before.row > 0 || before.col > 0 {
                retreat(&mut before, screen);
                if char_at(screen, before) == pair.0 {
                    found_left = Some(before);
                    break;
                }
            }
            let mut after = end;
            let mut found_right = None;
            while after.row < last_row(screen) || after.col + 1 < screen.size().1.get() {
                advance(&mut after, screen);
                if char_at(screen, after) == pair.1 {
                    found_right = Some(after);
                    break;
                }
            }
            let (Some(found_left), Some(found_right)) = (found_left, found_right) else {
                break;
            };
            left = found_left;
            end = found_right;
        }
        return (Some(left), Some(end));
    }
    (None, None)
}

fn forward(key: Key) -> Vec<Effect> {
    let bytes = match key {
        Key::Char(c) => c.to_string().into_bytes(),
        Key::Ctrl(c) => vec![
            (c.to_ascii_lowercase() as u8)
                .wrapping_sub(b'a')
                .wrapping_add(1),
        ],
        Key::Alt(c) => {
            let mut bytes = vec![0x1b];
            bytes.extend(c.to_string().bytes());
            bytes
        }
        Key::Escape => vec![0x1b],
        Key::AltEscape => vec![0x1b, 0x1b],
        Key::Enter => vec![b'\r'],
        Key::Backspace => vec![0x7f],
        Key::PageDown => b"\x1b[6~".to_vec(),
        Key::Bytes(bytes) => bytes,
    };
    vec![Effect::Forward(bytes)]
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
fn top_visible_row(screen: &Screen) -> usize {
    screen.scrollback_len().saturating_sub(screen.scrollback())
}
fn bottom_visible_row(screen: &Screen) -> usize {
    top_visible_row(screen) + usize::from(screen.size().0.get()) - 1
}
fn middle_visible_row(screen: &Screen) -> usize {
    top_visible_row(screen) + usize::from(screen.size().0.get()) / 2
}
fn last_row(screen: &Screen) -> usize {
    screen.scrollback_len() + usize::from(screen.size().0.get()) - 1
}
fn row_in_screen(screen: &Screen, row: usize) -> (usize, u16) {
    let history = screen.scrollback_len();
    if row <= history {
        (history - row, 0)
    } else {
        (0, (row - history) as u16)
    }
}
fn is_wide_continuation_at(screen: &mut Screen, row: usize, col: u16) -> bool {
    let old = screen.scrollback();
    let (offset, visible_row) = row_in_screen(screen, row);
    screen.set_scrollback(offset);
    let result = screen
        .cell(visible_row, col)
        .is_some_and(|cell| cell.is_wide_continuation());
    screen.set_scrollback(old);
    result
}
fn row_text(screen: &mut Screen, row: usize, first: u16, last: u16) -> String {
    let old = screen.scrollback();
    let (offset, visible_row) = row_in_screen(screen, row);
    screen.set_scrollback(offset);
    let mut text = String::new();
    let mut last_content_end = 0;
    for col in first..=last {
        if let Some(cell) = screen.cell(visible_row, col) {
            if !cell.is_wide_continuation() {
                let contents = cell.contents();
                if contents.is_empty() {
                    text.push(' ');
                } else {
                    text.push_str(contents);
                    last_content_end = text.len();
                }
            }
        } else {
            text.push(' ');
        }
    }
    text.truncate(last_content_end);
    screen.set_scrollback(old);
    text
}
fn block_text(screen: &mut Screen, row: usize, first: u16, last: u16) -> String {
    let old = screen.scrollback();
    let (offset, visible_row) = row_in_screen(screen, row);
    screen.set_scrollback(offset);
    let mut text = String::new();
    for col in first..=last {
        if let Some(cell) = screen.cell(visible_row, col) {
            if !cell.is_wide_continuation() {
                let contents = cell.contents();
                if contents.is_empty() {
                    text.push(' ');
                } else {
                    text.push_str(contents);
                }
            }
        } else {
            text.push(' ');
        }
    }
    screen.set_scrollback(old);
    text
}
fn is_wrapped(screen: &mut Screen, row: usize) -> bool {
    let old = screen.scrollback();
    let (offset, visible_row) = row_in_screen(screen, row);
    screen.set_scrollback(offset);
    let wrapped = screen.row_wrapped(visible_row);
    screen.set_scrollback(old);
    wrapped
}
fn step_horizontal(screen: &mut Screen, p: &mut Position, forward: bool) {
    let last = screen.size().1.get() - 1;
    if forward {
        if p.col < last {
            p.col += 1;
        }
        if is_wide_continuation_at(screen, p.row, p.col) && p.col < last {
            p.col += 1;
        }
    } else {
        if p.col > 0 {
            p.col -= 1;
        }
        if is_wide_continuation_at(screen, p.row, p.col) && p.col > 0 {
            p.col -= 1;
        }
    }
}

fn cell_char(screen: &mut Screen, row: usize, col: u16) -> Option<char> {
    row_text(screen, row, col, col).chars().next()
}
fn last_column(screen: &mut Screen, p: Position) -> u16 {
    (0..screen.size().1.get())
        .rev()
        .find(|&c| cell_char(screen, p.row, c).is_some())
        .unwrap_or(0)
}
fn first_nonblank(screen: &mut Screen, p: Position) -> u16 {
    (0..screen.size().1.get())
        .find(|&c| cell_char(screen, p.row, c).is_some_and(|ch| !ch.is_whitespace()))
        .unwrap_or(0)
}
fn char_at(screen: &mut Screen, p: Position) -> char {
    cell_char(screen, p.row, p.col).unwrap_or(' ')
}
fn is_space(screen: &mut Screen, p: Position) -> bool {
    char_at(screen, p).is_whitespace()
}
fn word_class(screen: &mut Screen, p: Position) -> u8 {
    let c = char_at(screen, p);
    if c.is_alphanumeric() || c == '_' {
        1
    } else if c.is_whitespace() {
        0
    } else {
        2
    }
}
fn advance(p: &mut Position, screen: &Screen) {
    let cols = screen.size().1.get();
    if p.col + 1 >= cols {
        p.col = 0;
        p.row = (p.row + 1).min(last_row(screen));
    } else {
        p.col += 1;
    }
}
fn retreat(p: &mut Position, screen: &Screen) {
    if p.col == 0 {
        p.row = p.row.saturating_sub(1);
        p.col = screen.size().1.get() - 1;
    } else {
        p.col -= 1;
    }
}
fn motion_class(screen: &mut Screen, p: Position, big: bool) -> u8 {
    if big {
        u8::from(!is_space(screen, p))
    } else {
        word_class(screen, p)
    }
}
fn word_motion_kind(
    screen: &mut Screen,
    mut p: Position,
    count: usize,
    forward: bool,
    big: bool,
) -> Position {
    for _ in 0..count {
        let previous = p;
        if forward {
            let class = motion_class(screen, p, big);
            if class != 0 {
                loop {
                    if p.row == last_row(screen) && p.col + 1 == screen.size().1.get() {
                        return p;
                    }
                    let mut next = p;
                    advance(&mut next, screen);
                    if motion_class(screen, next, big) != class {
                        p = next;
                        break;
                    }
                    p = next;
                }
            }
            while (p.row < last_row(screen) || p.col + 1 < screen.size().1.get())
                && motion_class(screen, p, big) == 0
            {
                advance(&mut p, screen);
            }
        } else {
            if p.row == 0 && p.col == 0 {
                return p;
            }
            retreat(&mut p, screen);
            while (p.row > 0 || p.col > 0) && motion_class(screen, p, big) == 0 {
                retreat(&mut p, screen);
            }
            let class = motion_class(screen, p, big);
            while p.row > 0 || p.col > 0 {
                let mut previous = p;
                retreat(&mut previous, screen);
                if motion_class(screen, previous, big) != class {
                    break;
                }
                p = previous;
            }
        }
        if p == previous {
            break;
        }
    }
    p
}
fn word_end_kind(screen: &mut Screen, mut p: Position, count: usize, big: bool) -> Position {
    for _ in 0..count {
        if p.row == last_row(screen) && p.col + 1 == screen.size().1.get() {
            break;
        }
        let previous = p;
        let class = motion_class(screen, p, big);
        if class != 0 {
            let mut next = p;
            advance(&mut next, screen);
            if motion_class(screen, next, big) == class {
                p = next;
                while p.row < last_row(screen) || p.col + 1 < screen.size().1.get() {
                    let mut next = p;
                    advance(&mut next, screen);
                    if motion_class(screen, next, big) != class {
                        break;
                    }
                    p = next;
                }
                continue;
            }
        }
        if p.row < last_row(screen) || p.col + 1 < screen.size().1.get() {
            advance(&mut p, screen);
        }
        while (p.row < last_row(screen) || p.col + 1 < screen.size().1.get())
            && motion_class(screen, p, big) == 0
        {
            advance(&mut p, screen);
        }
        let class = motion_class(screen, p, big);
        while p.row < last_row(screen) || p.col + 1 < screen.size().1.get() {
            let mut next = p;
            advance(&mut next, screen);
            if motion_class(screen, next, big) != class {
                break;
            }
            p = next;
        }
        if p == previous {
            break;
        }
    }
    p
}
fn word_motion(screen: &mut Screen, p: Position, count: usize, forward: bool) -> Position {
    word_motion_kind(screen, p, count, forward, false)
}
fn word_end(screen: &mut Screen, p: Position, count: usize) -> Position {
    word_end_kind(screen, p, count, false)
}
fn next_match(
    matches: &[Vec<Position>],
    from: Position,
    direction: Direction,
    reverse: bool,
) -> Option<usize> {
    if matches.is_empty() {
        return None;
    }
    let direction = if reverse {
        match direction {
            Direction::Forward => Direction::Backward,
            Direction::Backward => Direction::Forward,
        }
    } else {
        direction
    };
    match direction {
        Direction::Forward => matches
            .iter()
            .position(|m| m.first().is_some_and(|p| *p > from))
            .or(Some(0)),
        Direction::Backward => matches
            .iter()
            .rposition(|m| m.first().is_some_and(|p| *p < from))
            .or(Some(matches.len() - 1)),
    }
}
fn scan_matches(screen: &mut Screen, query: &str) -> Vec<Vec<Position>> {
    if query.is_empty() {
        return Vec::new();
    }
    let old = screen.scrollback();
    let total_rows = screen.scrollback_len() + usize::from(screen.size().0.get());
    let q: Vec<char> = query.chars().collect();
    let insensitive = !query.chars().any(char::is_uppercase);
    let mut chars = Vec::<(char, Position)>::new();
    for row in 0..total_rows {
        let (offset, visible_row) = row_in_screen(screen, row);
        screen.set_scrollback(offset);
        for col in 0..screen.size().1.get() {
            match screen.cell(visible_row, col) {
                Some(cell) if cell.is_wide_continuation() => {}
                Some(cell) if !cell.contents().is_empty() => {
                    chars.extend(cell.contents().chars().map(|c| (c, Position { row, col })));
                }
                _ => chars.push((' ', Position { row, col })),
            }
        }
        if row + 1 < total_rows && !screen.row_wrapped(visible_row) {
            chars.push((
                '\n',
                Position {
                    row,
                    col: screen.size().1.get(),
                },
            ));
        }
    }
    let mut found = Vec::new();
    for start in 0..=chars.len().saturating_sub(q.len()) {
        if start + q.len() > chars.len() {
            break;
        }
        if chars[start..start + q.len()]
            .iter()
            .zip(&q)
            .all(|((a, _), b)| {
                if insensitive {
                    a.to_lowercase().eq(b.to_lowercase())
                } else {
                    a == b
                }
            })
        {
            found.push(
                chars[start..start + q.len()]
                    .iter()
                    .map(|(_, p)| *p)
                    .collect(),
            );
        }
    }
    screen.set_scrollback(old);
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::num::NonZeroU16;
    fn test_screen() -> Screen {
        let mut parser =
            vt100::Parser::new(NonZeroU16::new(3).unwrap(), NonZeroU16::new(8).unwrap(), 20);
        parser.process(b"one two\r\nABC def\r\nwide \xe7\x95\x8c\x1b[1;1H");
        parser.screen().clone()
    }

    #[test]
    fn visual_yank_keeps_exact_range_flash_and_returns_to_insert() {
        let mut screen = test_screen();
        let mut vim = Vim::new();
        vim.handle_key(&mut screen, Key::AltEscape);
        vim.handle_key(&mut screen, Key::Char('v'));
        vim.handle_key(&mut screen, Key::Char('l'));
        assert_eq!(
            vim.handle_key(&mut screen, Key::Char('y')),
            vec![Effect::Yank("on".into())]
        );
        assert_eq!(vim.mode(), Mode::Insert);
        assert_eq!(
            vim.yank_flash(),
            Some(YankFlash {
                range: CellRange {
                    start: Position { row: 0, col: 0 },
                    end: Position { row: 0, col: 1 }
                },
                kind: VisualKind::Character,
                viewport_top: 0,
            })
        );
    }

    #[test]
    fn visual_yank_flash_pins_scrollback_view() {
        let mut parser =
            vt100::Parser::new(NonZeroU16::new(3).unwrap(), NonZeroU16::new(8).unwrap(), 20);
        parser.process(b"one\r\ntwo\r\nthree\r\nfour\r\nfive");
        let mut screen = parser.screen().clone();
        screen.set_scrollback(2);
        let mut vim = Vim::new();
        vim.mode = Mode::Visual(VisualKind::Line);
        vim.anchor = Some(Position { row: 2, col: 1 });
        vim.cursor = Some(Position { row: 2, col: 4 });
        vim.handle_key(&mut screen, Key::Char('y'));
        assert_eq!(vim.mode(), Mode::Insert);
        assert_eq!(
            vim.yank_flash(),
            Some(YankFlash {
                range: CellRange {
                    start: Position { row: 2, col: 1 },
                    end: Position { row: 2, col: 4 }
                },
                kind: VisualKind::Line,
                viewport_top: 0,
            })
        );
        assert_eq!(screen.scrollback(), 0);
    }

    #[test]
    fn history_eviction_rebases_yank_range_and_absolute_viewport() {
        let mut screen = test_screen();
        let mut vim = Vim::new();
        vim.yank_flash = Some(YankFlash {
            range: CellRange {
                start: Position { row: 6, col: 1 },
                end: Position { row: 7, col: 3 },
            },
            kind: VisualKind::Character,
            viewport_top: 4,
        });
        vim.history_changed(&mut screen, Some(2), false);
        assert_eq!(
            vim.yank_flash(),
            Some(YankFlash {
                range: CellRange {
                    start: Position { row: 4, col: 1 },
                    end: Position { row: 5, col: 3 },
                },
                kind: VisualKind::Character,
                viewport_top: 2,
            })
        );
    }

    #[test]
    fn search_confirmation_and_cancellation_redispatch_remaining_bytes() {
        let mut screen = test_screen();
        let mut vim = Vim::new();
        vim.handle_key(&mut screen, Key::Ctrl('f'));
        assert_eq!(
            vim.handle_bytes(&mut screen, b"one\ri"),
            vec![
                Effect::SearchPrompt("o".into()),
                Effect::SearchPrompt("on".into()),
                Effect::SearchPrompt("one".into()),
                Effect::SearchClosed,
            ]
        );
        assert_eq!(vim.mode(), Mode::Insert);

        vim.handle_key(&mut screen, Key::AltEscape);
        vim.handle_key(&mut screen, Key::Ctrl('f'));
        assert_eq!(
            vim.handle_bytes(&mut screen, b"query\x1bxxx"),
            vec![
                Effect::SearchPrompt("q".into()),
                Effect::SearchPrompt("qu".into()),
                Effect::SearchPrompt("que".into()),
                Effect::SearchPrompt("quer".into()),
                Effect::SearchPrompt("query".into()),
                Effect::SearchClosed,
            ]
        );
        assert_eq!(vim.mode(), Mode::Normal);
    }

    #[test]
    fn visual_mode_cancels_when_history_eviction_drops_its_anchor() {
        let mut screen = test_screen();
        let mut vim = Vim::new();
        normal(&mut vim, &mut screen);
        key(&mut vim, &mut screen, 'v');
        assert_eq!(vim.mode(), Mode::Visual(VisualKind::Character));

        vim.history_changed(&mut screen, Some(1), false);

        assert_eq!(vim.mode(), Mode::Normal);
        assert_eq!(vim.selection(&mut screen), None);
    }
    #[test]
    fn lf_without_cr_preserves_column_in_search_coordinates() {
        let mut parser =
            vt100::Parser::new(NonZeroU16::new(3).unwrap(), NonZeroU16::new(8).unwrap(), 20);
        parser.process(b"one two\nABC");
        let mut screen = parser.screen().clone();
        let mut vim = Vim::new();
        vim.handle_key(&mut screen, Key::Ctrl('f'));
        for c in "abc".chars() {
            vim.handle_key(&mut screen, Key::Char(c));
        }
        assert!(vim.search_matches().iter().any(|m| m
            == &[
                Position { row: 1, col: 7 },
                Position { row: 2, col: 0 },
                Position { row: 2, col: 1 },
            ]));
    }
    #[test]
    fn insert_escape_passthrough_and_alt_escape_enters_normal() {
        let mut screen = test_screen();
        let mut vim = Vim::new();
        assert_eq!(
            vim.handle_key(&mut screen, Key::Char('x')),
            vec![Effect::Forward(b"x".to_vec())]
        );
        assert_eq!(
            vim.handle_key(&mut screen, Key::Escape),
            vec![Effect::Forward(vec![0x1b])]
        );
        assert_eq!(vim.mode(), Mode::Insert);
        assert!(vim.handle_key(&mut screen, Key::AltEscape).is_empty());
        assert_eq!(vim.mode(), Mode::Normal);
    }
    #[test]
    fn alt_scroll_enters_normal_from_insert() {
        for key in ['d', 'f'] {
            let mut screen = test_screen();
            let mut vim = Vim::new();
            vim.handle_key(&mut screen, Key::Alt(key));
            assert_eq!(vim.mode(), Mode::Normal);
        }
    }
    #[test]
    fn insert_bytes_preserve_line_endings_backspace_and_utf8_fragments() {
        let mut screen = test_screen();
        let mut vim = Vim::new();

        assert_eq!(
            vim.handle_bytes(&mut screen, b"\n"),
            vec![Effect::Forward(b"\n".to_vec())]
        );
        assert_eq!(
            vim.handle_bytes(&mut screen, b"\x08"),
            vec![Effect::Forward(vec![0x08])]
        );
        assert_eq!(
            vim.handle_bytes(&mut screen, b"\x7f"),
            vec![Effect::Forward(vec![0x7f])]
        );
        assert_eq!(
            vim.handle_bytes(&mut screen, b"\xe2"),
            vec![Effect::Forward(vec![0xe2])]
        );
        assert_eq!(
            vim.handle_bytes(&mut screen, b"\x82\xac"),
            vec![Effect::Forward(vec![0x82, 0xac])]
        );
        assert!(vim.handle_bytes(&mut screen, b"\x1b[").is_empty());
        assert_eq!(
            vim.handle_bytes(&mut screen, b"A"),
            vec![Effect::Forward(b"\x1b[A".to_vec())]
        );
        assert_eq!(
            vim.handle_bytes(&mut screen, b"\x06"),
            vec![Effect::SearchPrompt(String::new())]
        );
        assert_eq!(vim.mode(), Mode::Search);
    }
    #[test]
    fn cancelling_search_from_insert_restores_live_scrollback() {
        let mut parser =
            vt100::Parser::new(NonZeroU16::new(3).unwrap(), NonZeroU16::new(8).unwrap(), 20);
        parser.process(b"zero\r\none\r\ntwo\r\nthree\r\nfour\r\nfive");
        let mut screen = parser.screen().clone();
        let mut vim = Vim::new();

        vim.handle_key(&mut screen, Key::Ctrl('f'));
        vim.handle_key(&mut screen, Key::Char('z'));
        assert!(screen.scrollback() > 0);

        assert_eq!(
            vim.handle_key(&mut screen, Key::Escape),
            vec![Effect::SearchClosed]
        );
        assert_eq!(vim.mode(), Mode::Insert);
        assert_eq!(screen.scrollback(), 0);
        assert!(vim.search_text().is_empty());
        assert!(vim.search_matches().is_empty());
    }

    #[test]
    fn bracketed_paste_delimiters_and_contents_are_opaque_across_chunks() {
        let mut screen = test_screen();
        let mut vim = Vim::new();

        assert!(vim.handle_bytes(&mut screen, b"\x1b[20").is_empty());
        assert_eq!(
            vim.handle_bytes(&mut screen, b"0~"),
            vec![Effect::Forward(b"\x1b[200~".to_vec())]
        );
        let pasted = b"\x06\x1b\x1b\x1bd";
        assert_eq!(
            vim.handle_bytes(&mut screen, &pasted[..1]),
            vec![Effect::Forward(pasted[..1].to_vec())]
        );
        assert_eq!(
            vim.handle_bytes(&mut screen, &pasted[1..]),
            vec![Effect::Forward(pasted[1..].to_vec())]
        );
        assert!(vim.handle_bytes(&mut screen, b"\x1b[201").is_empty());
        assert_eq!(
            vim.handle_bytes(&mut screen, b"~"),
            vec![Effect::Forward(b"\x1b[201~".to_vec())]
        );
        assert_eq!(vim.mode(), Mode::Insert);
    }

    #[test]
    fn normal_escape_and_i_enter_insert() {
        let mut screen = test_screen();
        let mut vim = Vim::new();
        normal(&mut vim, &mut screen);
        assert!(vim.handle_key(&mut screen, Key::Escape).is_empty());
        assert_eq!(vim.mode(), Mode::Insert);
        normal(&mut vim, &mut screen);
        key(&mut vim, &mut screen, 'i');
        assert_eq!(vim.mode(), Mode::Insert);
    }
    #[test]
    fn visual_escape_clears_selection_and_visual_yank_enters_insert() {
        let mut screen = test_screen();
        let mut vim = Vim::new();
        normal(&mut vim, &mut screen);
        key(&mut vim, &mut screen, 'v');
        key(&mut vim, &mut screen, 'l');
        assert!(vim.selection(&mut screen).is_some());
        assert!(vim.handle_key(&mut screen, Key::Escape).is_empty());
        assert_eq!(vim.mode(), Mode::Normal);
        assert_eq!(vim.selection(&mut screen), None);

        key(&mut vim, &mut screen, 'h');
        key(&mut vim, &mut screen, 'v');
        key(&mut vim, &mut screen, 'l');
        assert!(
            matches!(vim.handle_key(&mut screen, Key::Char('y')).as_slice(), [Effect::Yank(s)] if s == "on")
        );
        assert_eq!(vim.mode(), Mode::Insert);
        assert_eq!(vim.selection(&mut screen), None);
    }
    #[test]
    fn insert_transitions_resync_cursor_after_pty_output() {
        let mut parser =
            vt100::Parser::new(NonZeroU16::new(3).unwrap(), NonZeroU16::new(8).unwrap(), 20);
        parser.process(b"one two\r\nABC def\r\n");
        let mut vim = Vim::new();
        vim.cursor(parser.screen_mut());
        vim.handle_key(parser.screen_mut(), Key::AltEscape);
        assert_eq!(vim.cursor(parser.screen_mut()), Position { row: 2, col: 0 });

        vim.handle_key(parser.screen_mut(), Key::Char('i'));
        vim.cursor(parser.screen_mut());
        parser.process(b"\x1b[3;2H");
        vim.handle_key(parser.screen_mut(), Key::Ctrl('f'));
        assert_eq!(vim.cursor(parser.screen_mut()), Position { row: 2, col: 1 });
    }
    #[test]
    fn fragmented_alt_escape_and_counted_motion() {
        let mut screen = test_screen();
        let mut vim = Vim::new();
        assert!(vim.handle_bytes(&mut screen, b"\x1b").is_empty());
        vim.handle_bytes(&mut screen, b"\x1b");
        assert_eq!(vim.mode(), Mode::Normal);
        vim.handle_key(&mut screen, Key::Char('2'));
        vim.handle_key(&mut screen, Key::Char('l'));
        assert_eq!(vim.cursor(&mut screen).col, 2);
    }
    #[test]
    fn smart_case_search_tracks_history_coordinates() {
        let mut screen = test_screen();
        let mut vim = Vim::new();
        vim.handle_key(&mut screen, Key::Ctrl('f'));
        for c in "abc".chars() {
            vim.handle_key(&mut screen, Key::Char(c));
        }
        assert!(
            vim.search_matches()
                .iter()
                .any(|m| m.first() == Some(&Position { row: 1, col: 0 }))
        );
        vim.handle_key(&mut screen, Key::Enter);
        assert_eq!(vim.mode(), Mode::Normal);
    }
    #[test]
    fn escape_after_accepted_search_clears_highlights_and_enters_insert() {
        let mut screen = test_screen();
        let mut vim = Vim::new();
        normal(&mut vim, &mut screen);
        vim.handle_key(&mut screen, Key::Ctrl('f'));
        for c in "one".chars() {
            vim.handle_key(&mut screen, Key::Char(c));
        }
        vim.handle_key(&mut screen, Key::Enter);
        assert_eq!(vim.search_text(), "one");
        assert!(!vim.search_matches().is_empty());

        assert!(vim.handle_key(&mut screen, Key::Escape).is_empty());
        assert_eq!(vim.mode(), Mode::Insert);
        assert!(vim.search_text().is_empty());
        assert!(vim.search_matches().is_empty());
    }

    #[test]
    fn prompt_escape_clears_previous_query_and_highlights() {
        let mut screen = test_screen();
        let mut vim = Vim::new();
        normal(&mut vim, &mut screen);
        vim.handle_key(&mut screen, Key::Ctrl('f'));
        for c in "one".chars() {
            vim.handle_key(&mut screen, Key::Char(c));
        }
        vim.handle_key(&mut screen, Key::Enter);
        assert!(!vim.search_matches().is_empty());

        vim.handle_key(&mut screen, Key::Ctrl('f'));
        for c in "ABC".chars() {
            vim.handle_key(&mut screen, Key::Char(c));
        }
        assert_eq!(vim.prompt_text(), "ABC");
        assert!(!vim.search_matches().is_empty());
        assert_eq!(
            vim.handle_key(&mut screen, Key::Escape),
            vec![Effect::SearchClosed]
        );

        assert_eq!(vim.mode(), Mode::Normal);
        assert!(vim.search_text().is_empty());
        assert!(vim.prompt_text().is_empty());
        assert!(vim.search_matches().is_empty());
    }
    #[test]
    fn visual_yank_returns_client_clipboard_text() {
        let mut screen = test_screen();
        let mut vim = Vim::new();
        vim.handle_key(&mut screen, Key::AltEscape);
        vim.handle_key(&mut screen, Key::Char('l'));
        vim.handle_key(&mut screen, Key::Char('v'));
        vim.handle_key(&mut screen, Key::Char('l'));
        assert!(
            matches!(vim.handle_key(&mut screen, Key::Char('y')).as_slice(), [Effect::Yank(s)] if s == "ne")
        );
    }
    #[test]
    fn wide_character_yank_skips_continuation_cell() {
        let mut screen = test_screen();
        let mut vim = Vim::new();
        vim.handle_key(&mut screen, Key::AltEscape);
        for _ in 0..2 {
            vim.handle_key(&mut screen, Key::Char('j'));
        }
        for _ in 0..5 {
            vim.handle_key(&mut screen, Key::Char('l'));
        }
        vim.handle_key(&mut screen, Key::Char('v'));
        vim.handle_key(&mut screen, Key::Char('l'));
        assert!(
            matches!(vim.handle_key(&mut screen, Key::Char('y')).as_slice(), [Effect::Yank(s)] if s == "界")
        );
    }
    fn normal(vim: &mut Vim, screen: &mut Screen) {
        vim.handle_key(screen, Key::AltEscape);
    }
    fn key(vim: &mut Vim, screen: &mut Screen, c: char) {
        vim.handle_key(screen, Key::Char(c));
    }
    #[test]
    fn small_and_big_word_motions_and_ends() {
        let mut screen = test_screen();
        let mut vim = Vim::new();
        normal(&mut vim, &mut screen);
        key(&mut vim, &mut screen, 'w');
        assert_eq!(vim.cursor(&mut screen), Position { row: 0, col: 4 });
        key(&mut vim, &mut screen, 'b');
        assert_eq!(vim.cursor(&mut screen), Position { row: 0, col: 0 });
        key(&mut vim, &mut screen, 'e');
        assert_eq!(vim.cursor(&mut screen), Position { row: 0, col: 2 });
        key(&mut vim, &mut screen, 'W');
        assert_eq!(vim.cursor(&mut screen), Position { row: 0, col: 4 });
        key(&mut vim, &mut screen, 'B');
        assert_eq!(vim.cursor(&mut screen), Position { row: 0, col: 0 });
        key(&mut vim, &mut screen, 'E');
        assert_eq!(vim.cursor(&mut screen), Position { row: 0, col: 2 });
    }
    #[test]
    fn small_word_classes_and_counted_motion_differ_from_big_words() {
        let mut parser =
            vt100::Parser::new(NonZeroU16::new(2).unwrap(), NonZeroU16::new(24).unwrap(), 8);
        parser.process(b"a-b c\x1b[1;1H");
        let mut screen = parser.screen().clone();
        let mut vim = Vim::new();
        normal(&mut vim, &mut screen);
        key(&mut vim, &mut screen, 'w');
        assert_eq!(vim.cursor(&mut screen).col, 1);
        key(&mut vim, &mut screen, 'B');
        assert_eq!(vim.cursor(&mut screen).col, 0);
        key(&mut vim, &mut screen, '2');
        key(&mut vim, &mut screen, 'w');
        assert_eq!(vim.cursor(&mut screen).col, 2);
        key(&mut vim, &mut screen, 'W');
        assert_eq!(vim.cursor(&mut screen).col, 4);
        key(&mut vim, &mut screen, 'B');
        assert_eq!(vim.cursor(&mut screen).col, 0);
    }
    #[test]
    fn counted_g_and_viewport_motions_keep_history_coordinates() {
        let mut parser =
            vt100::Parser::new(NonZeroU16::new(3).unwrap(), NonZeroU16::new(8).unwrap(), 20);
        parser.process(b"zero\r\none\r\ntwo\r\nthree\r\nfour\r\nfive");
        let mut screen = parser.screen().clone();
        let mut vim = Vim::new();
        normal(&mut vim, &mut screen);
        key(&mut vim, &mut screen, '2');
        key(&mut vim, &mut screen, 'g');
        key(&mut vim, &mut screen, 'g');
        assert_eq!(vim.cursor(&mut screen).row, 1);
        key(&mut vim, &mut screen, '3');
        key(&mut vim, &mut screen, 'G');
        assert_eq!(vim.cursor(&mut screen).row, 2);
        key(&mut vim, &mut screen, 'G');
        key(&mut vim, &mut screen, '2');
        key(&mut vim, &mut screen, 'K');
        assert_eq!(screen.scrollback(), 2);
        key(&mut vim, &mut screen, 'J');
        assert_eq!(screen.scrollback(), 1);
        key(&mut vim, &mut screen, 'G');
        assert_eq!(vim.cursor(&mut screen).row, last_row(&screen));
        let before = top_visible_row(&screen);
        vim.handle_key(&mut screen, Key::Ctrl('u'));
        let scrolled_up = top_visible_row(&screen);
        assert!(scrolled_up <= before);
        vim.handle_key(&mut screen, Key::Ctrl('d'));
        assert!(top_visible_row(&screen) >= scrolled_up);
        key(&mut vim, &mut screen, '2');
        let old_offset = screen.scrollback();
        let old_screen_row = vim.cursor(&mut screen).row - top_visible_row(&screen);
        vim.handle_key(&mut screen, Key::Ctrl('b'));
        assert!(screen.scrollback() >= old_offset);
        assert_eq!(
            vim.cursor(&mut screen).row - top_visible_row(&screen),
            old_screen_row
        );
        key(&mut vim, &mut screen, '2');
        let before_scroll_down = screen.scrollback();
        vim.handle_key(&mut screen, Key::Ctrl('d'));
        assert!(screen.scrollback() < before_scroll_down);
    }
    #[test]
    fn exiting_navigation_restores_live_view() {
        for exit in [Key::Escape, Key::Char('i'), Key::Char('y')] {
            let mut parser =
                vt100::Parser::new(NonZeroU16::new(3).unwrap(), NonZeroU16::new(8).unwrap(), 20);
            parser.process(b"zero\r\none\r\ntwo\r\nthree\r\nfour\r\nfive");
            let mut screen = parser.screen().clone();
            let mut vim = Vim::new();
            vim.handle_key(&mut screen, Key::Alt('d'));
            assert!(screen.scrollback() > 0);
            if exit == Key::Char('y') {
                vim.handle_key(&mut screen, Key::Char('v'));
            }
            vim.handle_key(&mut screen, exit);
            assert_eq!(vim.mode(), Mode::Insert);
            assert_eq!(screen.scrollback(), 0);
        }
    }
    #[test]
    fn visual_anchor_stays_on_buffer_row_when_viewport_scrolls_into_history() {
        let mut parser =
            vt100::Parser::new(NonZeroU16::new(3).unwrap(), NonZeroU16::new(8).unwrap(), 20);
        parser.process(b"zero\r\none\r\ntwo\r\nthree\r\nfour\r\nfive");
        let mut screen = parser.screen().clone();
        let mut vim = Vim::new();
        normal(&mut vim, &mut screen);
        key(&mut vim, &mut screen, 'G');
        key(&mut vim, &mut screen, '0');
        key(&mut vim, &mut screen, 'v');
        assert_eq!(vim.selection(&mut screen).unwrap().start.row, 5);

        key(&mut vim, &mut screen, '3');
        vim.handle_key(&mut screen, Key::Ctrl('u'));

        assert_eq!(top_visible_row(&screen), 0);
        assert_eq!(
            vim.selection(&mut screen),
            Some(CellRange {
                start: Position { row: 2, col: 0 },
                end: Position { row: 5, col: 0 },
            })
        );
    }
    #[test]
    fn yank_operators_support_counts_and_text_objects() {
        let mut parser = vt100::Parser::new(
            NonZeroU16::new(3).unwrap(),
            NonZeroU16::new(24).unwrap(),
            20,
        );
        parser.process(b"one two three\r\n(a (b) c)\r\n\x1b[1;1H");
        let mut screen = parser.screen().clone();
        let mut vim = Vim::new();
        normal(&mut vim, &mut screen);
        key(&mut vim, &mut screen, 'y');
        assert!(
            matches!(vim.handle_key(&mut screen, Key::Char('w')).as_slice(), [Effect::Yank(s)] if s == "one ")
        );
        assert_eq!(vim.mode(), Mode::Normal);
        key(&mut vim, &mut screen, '2');
        key(&mut vim, &mut screen, 'y');
        assert!(
            matches!(vim.handle_key(&mut screen, Key::Char('y')).as_slice(), [Effect::Yank(s)] if s.starts_with("one two"))
        );
        vim.cursor = Some(Position { row: 1, col: 4 });
        key(&mut vim, &mut screen, 'y');
        key(&mut vim, &mut screen, 'i');
        assert!(
            matches!(vim.handle_key(&mut screen, Key::Char('(')).as_slice(), [Effect::Yank(s)] if s == "b")
        );
        vim.cursor = Some(Position { row: 0, col: 0 });
        key(&mut vim, &mut screen, '2');
        key(&mut vim, &mut screen, 'y');
        assert!(
            matches!(vim.handle_key(&mut screen, Key::Char('w')).as_slice(), [Effect::Yank(s)] if s == "one two ")
        );
        vim.cursor = Some(Position { row: 0, col: 0 });
        key(&mut vim, &mut screen, 'y');
        assert!(
            matches!(vim.handle_key(&mut screen, Key::Char('j')).as_slice(), [Effect::Yank(s)] if s.starts_with("one two three\n(a (b) c)"))
        );
        vim.cursor = Some(Position { row: 1, col: 4 });
        key(&mut vim, &mut screen, 'y');
        key(&mut vim, &mut screen, 'a');
        assert!(
            matches!(vim.handle_key(&mut screen, Key::Char('(')).as_slice(), [Effect::Yank(s)] if s == "(b)")
        );
    }

    #[test]
    fn search_cancels_pending_yank_operator() {
        let mut parser = vt100::Parser::new(
            NonZeroU16::new(3).unwrap(),
            NonZeroU16::new(24).unwrap(),
            20,
        );
        parser.process(b"one two three\r\n(a (b) c)\r\n\x1b[1;1H");
        let mut screen = parser.screen().clone();
        let mut vim = Vim::new();
        normal(&mut vim, &mut screen);
        key(&mut vim, &mut screen, 'y');
        assert_eq!(vim.operator_pending(), Some(("y", false)));

        vim.handle_key(&mut screen, Key::Ctrl('f'));
        assert_eq!(vim.operator_pending(), None);
        for c in "two".chars() {
            vim.handle_key(&mut screen, Key::Char(c));
        }
        vim.handle_key(&mut screen, Key::Enter);
        assert_eq!(vim.mode(), Mode::Normal);
        assert!(vim.handle_key(&mut screen, Key::Char('w')).is_empty());
        assert_eq!(vim.cursor(&mut screen), Position { row: 0, col: 8 });
    }

    #[test]
    fn huge_counted_line_yank_stops_at_last_row() {
        let mut screen = test_screen();
        let mut vim = Vim::new();
        normal(&mut vim, &mut screen);
        for _ in 0..20 {
            key(&mut vim, &mut screen, '9');
        }
        key(&mut vim, &mut screen, 'y');
        assert!(matches!(
            vim.handle_key(&mut screen, Key::Char('y')).as_slice(),
            [Effect::Yank(text)] if text.starts_with("one two\nABC def\nwide")
        ));
    }

    #[test]
    fn huge_motion_counts_stop_at_reachable_boundaries() {
        let mut screen = test_screen();
        let mut vim = Vim::new();
        normal(&mut vim, &mut screen);
        for _ in 0..20 {
            key(&mut vim, &mut screen, '9');
        }
        key(&mut vim, &mut screen, 'l');
        assert_eq!(vim.cursor(&mut screen).col, 7);
        for _ in 0..20 {
            key(&mut vim, &mut screen, '9');
        }
        key(&mut vim, &mut screen, 'h');
        assert_eq!(vim.cursor(&mut screen).col, 0);
        key(&mut vim, &mut screen, 'y');
        for _ in 0..20 {
            key(&mut vim, &mut screen, '9');
        }
        assert_eq!(
            vim.handle_key(&mut screen, Key::Char('l')),
            vec![Effect::Yank("one two".into())]
        );
        for _ in 0..20 {
            key(&mut vim, &mut screen, '9');
        }
        key(&mut vim, &mut screen, 'b');
        assert_eq!(vim.cursor(&mut screen), Position { row: 0, col: 0 });
        key(&mut vim, &mut screen, 'G');
        key(&mut vim, &mut screen, '$');
        for _ in 0..20 {
            key(&mut vim, &mut screen, '9');
        }
        key(&mut vim, &mut screen, 'e');
        assert_eq!(
            vim.cursor(&mut screen),
            Position {
                row: last_row(&screen),
                col: 7
            }
        );
        key(&mut vim, &mut screen, 'w');
        assert_eq!(
            vim.cursor(&mut screen),
            Position {
                row: last_row(&screen),
                col: 7
            }
        );
    }

    #[test]
    fn huge_word_object_count_stops_at_last_nonblank_cell() {
        let mut parser =
            vt100::Parser::new(NonZeroU16::new(1).unwrap(), NonZeroU16::new(3).unwrap(), 0);
        parser.process(b"abc\x1b[1;1H");
        let mut screen = parser.screen().clone();
        let mut vim = Vim::new();
        normal(&mut vim, &mut screen);
        for _ in 0..20 {
            key(&mut vim, &mut screen, '9');
        }
        key(&mut vim, &mut screen, 'y');
        key(&mut vim, &mut screen, 'i');
        assert_eq!(
            vim.handle_key(&mut screen, Key::Char('w')),
            vec![Effect::Yank("abc".into())]
        );
    }

    #[test]
    fn exclusive_word_yank_omits_first_cell_of_next_row() {
        let mut parser =
            vt100::Parser::new(NonZeroU16::new(2).unwrap(), NonZeroU16::new(8).unwrap(), 8);
        parser.process(b"foo\r\nbar\x1b[1;1H");
        let mut screen = parser.screen().clone();
        let mut vim = Vim::new();
        normal(&mut vim, &mut screen);
        key(&mut vim, &mut screen, 'y');
        assert_eq!(
            vim.handle_key(&mut screen, Key::Char('w')),
            vec![Effect::Yank("foo\n".into())]
        );
    }
    #[test]
    fn visual_endpoint_swap_preserves_reversed_selection() {
        let mut screen = test_screen();
        let mut vim = Vim::new();
        normal(&mut vim, &mut screen);
        key(&mut vim, &mut screen, 'l');
        key(&mut vim, &mut screen, 'l');
        key(&mut vim, &mut screen, 'v');
        key(&mut vim, &mut screen, 'h');
        key(&mut vim, &mut screen, 'o');
        assert_eq!(
            vim.selection(&mut screen),
            Some(CellRange {
                start: Position { row: 0, col: 1 },
                end: Position { row: 0, col: 2 }
            })
        );
        key(&mut vim, &mut screen, 't');
        assert_eq!(vim.mode(), Mode::Visual(VisualKind::Line));
        assert!(matches!(
            vim.handle_key(&mut screen, Key::Char('y')).as_slice(),
            [Effect::Yank(_)]
        ));
    }
    #[test]
    fn visual_line_yank_is_linewise_in_both_directions() {
        let mut screen = test_screen();
        let mut vim = Vim::new();
        normal(&mut vim, &mut screen);
        key(&mut vim, &mut screen, 'V');
        key(&mut vim, &mut screen, 'j');
        assert!(
            matches!(vim.handle_key(&mut screen, Key::Char('y')).as_slice(), [Effect::Yank(text)] if text == "one two\nABC def")
        );

        normal(&mut vim, &mut screen);
        key(&mut vim, &mut screen, 'j');
        key(&mut vim, &mut screen, 'V');
        key(&mut vim, &mut screen, 'k');
        assert!(
            matches!(vim.handle_key(&mut screen, Key::Char('y')).as_slice(), [Effect::Yank(text)] if text == "one two\nABC def")
        );
    }

    #[test]
    fn visual_block_yank_preserves_blank_cells_and_wide_characters() {
        let mut parser =
            vt100::Parser::new(NonZeroU16::new(3).unwrap(), NonZeroU16::new(8).unwrap(), 20);
        parser.process("a\r\n界x\r\nz\x1b[1;1H".as_bytes());
        let mut screen = parser.screen().clone();
        let mut vim = Vim::new();
        normal(&mut vim, &mut screen);
        key(&mut vim, &mut screen, 'v');
        key(&mut vim, &mut screen, 't');
        key(&mut vim, &mut screen, 't');
        key(&mut vim, &mut screen, 'l');
        key(&mut vim, &mut screen, 'l');
        key(&mut vim, &mut screen, 'j');
        key(&mut vim, &mut screen, 'j');
        assert!(
            matches!(vim.handle_key(&mut screen, Key::Char('y')).as_slice(), [Effect::Yank(text)] if text == "a  \n界x\nz  ")
        );

        normal(&mut vim, &mut screen);
        key(&mut vim, &mut screen, 'j');
        key(&mut vim, &mut screen, 'j');
        key(&mut vim, &mut screen, 'l');
        key(&mut vim, &mut screen, 'l');
        key(&mut vim, &mut screen, 'v');
        key(&mut vim, &mut screen, 't');
        key(&mut vim, &mut screen, 't');
        key(&mut vim, &mut screen, 'k');
        key(&mut vim, &mut screen, 'k');
        key(&mut vim, &mut screen, 'h');
        key(&mut vim, &mut screen, 'h');
        assert!(
            matches!(vim.handle_key(&mut screen, Key::Char('y')).as_slice(), [Effect::Yank(text)] if text == "a  \n界x\nz  ")
        );
    }

    #[test]
    fn visual_search_preview_enter_and_escape_keep_the_anchor() {
        let mut parser =
            vt100::Parser::new(NonZeroU16::new(3).unwrap(), NonZeroU16::new(8).unwrap(), 20);
        parser.process(b"one\r\ntwo\r\nthree\x1b[1;1H");
        let mut screen = parser.screen().clone();
        let mut vim = Vim::new();
        normal(&mut vim, &mut screen);
        key(&mut vim, &mut screen, 'v');
        key(&mut vim, &mut screen, 'l');
        let anchor = Position { row: 0, col: 0 };

        vim.handle_key(&mut screen, Key::Ctrl('f'));
        for c in "two".chars() {
            vim.handle_key(&mut screen, Key::Char(c));
        }
        assert_eq!(vim.cursor(&mut screen), Position { row: 1, col: 0 });
        assert_eq!(vim.selection(&mut screen).unwrap().start, anchor);
        vim.handle_key(&mut screen, Key::Escape);
        assert_eq!(vim.mode(), Mode::Visual(VisualKind::Character));
        assert_eq!(vim.cursor(&mut screen), Position { row: 0, col: 1 });
        assert_eq!(
            vim.selection(&mut screen),
            Some(CellRange {
                start: anchor,
                end: Position { row: 0, col: 1 }
            })
        );
        assert!(vim.search_text().is_empty());
        assert!(vim.search_matches().is_empty());

        vim.handle_key(&mut screen, Key::Ctrl('f'));
        for c in "three".chars() {
            vim.handle_key(&mut screen, Key::Char(c));
        }
        vim.handle_key(&mut screen, Key::Enter);
        assert_eq!(vim.mode(), Mode::Visual(VisualKind::Character));
        assert_eq!(vim.cursor(&mut screen), Position { row: 2, col: 0 });
        assert_eq!(
            vim.selection(&mut screen),
            Some(CellRange {
                start: anchor,
                end: Position { row: 2, col: 0 }
            })
        );
    }

    #[test]
    fn visual_line_yank_omits_terminal_padding_and_blank_rows() {
        let mut parser =
            vt100::Parser::new(NonZeroU16::new(3).unwrap(), NonZeroU16::new(8).unwrap(), 20);
        parser.process(b"first\r\n\r\nlast\x1b[1;1H");
        let mut screen = parser.screen().clone();
        let mut vim = Vim::new();
        normal(&mut vim, &mut screen);
        key(&mut vim, &mut screen, 'V');
        key(&mut vim, &mut screen, 'j');
        key(&mut vim, &mut screen, 'j');
        assert!(
            matches!(vim.handle_key(&mut screen, Key::Char('y')).as_slice(), [Effect::Yank(text)] if text == "first\n\nlast")
        );
    }
}
