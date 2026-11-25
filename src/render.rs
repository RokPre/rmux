use std::collections::{HashMap, HashSet};
use std::io::{self, Write};

use crossterm::cursor::{Hide, MoveTo, Show};
use crossterm::queue;
use crossterm::style::{
    Attribute, Color, Print, SetAttribute, SetBackgroundColor, SetForegroundColor,
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
use vt100::{Cell, Color as VtColor, Parser, Screen};

use crate::input::InputContext;
use crate::layout::{Divider, LayoutGeometry, LayoutSnapshot, PaneRect};

pub(crate) const BUFFER_SNAPSHOT: u8 = 4;
pub(crate) const BUFFER_OUTPUT: u8 = 5;
pub(crate) const VIM_STATE: u8 = crate::server::VIM_STATE;

#[derive(Clone, Debug, serde::Deserialize)]
struct VimView {
    buffer: u64,
    mode: String,
    cursor_row: usize,
    cursor_col: u16,
    selection: Option<(usize, u16, usize, u16)>,
    #[serde(default)]
    yank_selection: Option<(usize, u16, usize, u16)>,
    #[serde(default)]
    yank_mode: Option<String>,
    #[serde(default)]
    selection_mode: Option<String>,
    matches: Vec<Vec<(usize, u16)>>,
    active_match: Option<usize>,
    prompt: Option<String>,
    app_control: bool,
    history_len: usize,
    scrollback: usize,
    #[serde(default)]
    operator_pending: Option<String>,
    #[serde(default)]
    operator_count_started: bool,
}
fn incremental_view(view: &VimView) -> bool {
    view.mode == "insert"
        && !view.app_control
        && view.selection.is_none()
        && view.yank_selection.is_none()
        && view.matches.is_empty()
        && view.prompt.is_none()
        && view.scrollback == 0
}

pub(crate) struct Renderer {
    snapshot: Option<LayoutSnapshot>,
    parsers: HashMap<u64, Parser>,
    history_parsers: HashMap<u64, Parser>,
    history_offsets: HashMap<u64, usize>,
    vim: HashMap<u64, VimView>,
    operator_sync_pending: HashSet<u64>,
    prompt: Option<(String, usize)>,
    bracketed_paste: Option<bool>,
    config: crate::config::Config,
    safe_output: Option<u64>,
    rendered: Vec<RenderedPane>,
}

struct RenderedPane {
    window: u64,
    buffer: u64,
    rect: PaneRect,
    origin: (u16, u16, u16, u16),
    terminal: (u16, u16),
    cells: Vec<Option<Cell>>,
}

impl Default for Renderer {
    fn default() -> Self {
        Self::with_config(crate::config::Config::default())
    }
}

impl Renderer {
    pub(crate) fn with_config(config: crate::config::Config) -> Self {
        Self {
            snapshot: None,
            parsers: HashMap::new(),
            history_parsers: HashMap::new(),
            history_offsets: HashMap::new(),
            vim: HashMap::new(),
            operator_sync_pending: HashSet::new(),
            prompt: None,
            bracketed_paste: None,
            config,
            safe_output: None,
            rendered: Vec::new(),
        }
    }
    pub(crate) fn apply_frame(&mut self, kind: u8, payload: &[u8]) -> io::Result<()> {
        match kind {
            BUFFER_SNAPSHOT => {
                self.safe_output = None;
                self.rendered.clear();
                if payload.len() < 12 {
                    return Err(invalid_frame("short buffer snapshot"));
                }
                let buffer = read_u64(payload, 0);
                let rows = read_u16(payload, 8);
                let cols = read_u16(payload, 10);
                let rows = std::num::NonZeroU16::new(rows)
                    .ok_or_else(|| invalid_frame("zero-sized buffer snapshot"))?;
                let cols = std::num::NonZeroU16::new(cols)
                    .ok_or_else(|| invalid_frame("zero-sized buffer snapshot"))?;
                let mut parser = Parser::new(rows, cols, 0);
                parser.process(&payload[12..]);
                self.parsers.insert(buffer, parser);
            }
            BUFFER_OUTPUT => {
                if payload.len() < 8 {
                    return Err(invalid_frame("short buffer output"));
                }
                let buffer = read_u64(payload, 0);
                let bytes = &payload[8..];
                let parser = self
                    .parsers
                    .get_mut(&buffer)
                    .ok_or_else(|| invalid_frame("buffer output before snapshot"))?;
                self.safe_output = (!bytes.is_empty()
                    && bytes.iter().all(|byte| (0x20..=0x7e).contains(byte)))
                .then_some(buffer);
                parser.process(bytes);
            }
            crate::server::HISTORY_SNAPSHOT => {
                self.safe_output = None;
                self.rendered.clear();
                if payload.len() < 16 {
                    return Err(invalid_frame("short history snapshot"));
                }
                let buffer = read_u64(payload, 0);
                let rows = std::num::NonZeroU16::new(read_u16(payload, 8))
                    .ok_or_else(|| invalid_frame("zero-sized history snapshot"))?;
                let cols = std::num::NonZeroU16::new(read_u16(payload, 10))
                    .ok_or_else(|| invalid_frame("zero-sized history snapshot"))?;
                let offset = u32::from_be_bytes(payload[12..16].try_into().unwrap());
                let continuation = offset & (1 << 31) != 0;
                let offset = offset & !(1 << 31);
                if continuation {
                    let parser = self
                        .history_parsers
                        .get_mut(&buffer)
                        .ok_or_else(|| invalid_frame("history continuation before snapshot"))?;
                    if parser.screen().size() != (rows, cols) {
                        return Err(invalid_frame("history continuation size changed"));
                    }
                    parser.process(&payload[16..]);
                } else if offset == 0 {
                    self.history_parsers.remove(&buffer);
                    self.history_offsets.remove(&buffer);
                } else {
                    let mut parser = Parser::new(rows, cols, 0);
                    parser.process(&payload[16..]);
                    self.history_parsers.insert(buffer, parser);
                    self.history_offsets.insert(buffer, offset as usize);
                }
            }
            VIM_STATE => {
                let view: VimView = serde_json::from_slice(payload)
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
                let preserve = self.vim.get(&view.buffer).is_some_and(|previous| {
                    incremental_view(previous)
                        && incremental_view(&view)
                        && previous.history_len == view.history_len
                        && previous.scrollback == view.scrollback
                });
                if !preserve {
                    self.safe_output = None;
                    self.rendered.clear();
                }
                self.operator_sync_pending.insert(view.buffer);
                self.vim.insert(view.buffer, view);
            }
            _ => return Err(invalid_frame("invalid renderer frame")),
        }
        Ok(())
    }
    pub(crate) fn set_layout(&mut self, snapshot: LayoutSnapshot) {
        let previous_active = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.active_buffer);
        let active_buffer = snapshot.active_buffer;
        self.safe_output = None;
        self.rendered.clear();
        self.parsers
            .retain(|buffer, _| snapshot.buffers.contains(buffer));
        self.history_parsers
            .retain(|buffer, _| snapshot.buffers.contains(buffer));
        self.history_offsets
            .retain(|buffer, _| snapshot.buffers.contains(buffer));
        self.vim
            .retain(|buffer, _| snapshot.buffers.contains(buffer));
        self.operator_sync_pending
            .retain(|buffer| snapshot.buffers.contains(buffer));
        self.snapshot = Some(snapshot);
        if previous_active != active_buffer
            && let Some(buffer) = active_buffer
            && self.vim.contains_key(&buffer)
        {
            self.operator_sync_pending.insert(buffer);
        }
    }

    pub(crate) fn active_buffer(&self) -> Option<(u64, String)> {
        let snapshot = self.snapshot.as_ref()?;
        let buffer = snapshot.active_buffer?;
        let name = snapshot
            .buffer_names
            .get(&buffer)
            .cloned()
            .unwrap_or_else(|| buffer.to_string());
        Some((buffer, name))
    }

    pub(crate) fn take_operator_pending(&mut self) -> Option<(Option<String>, bool)> {
        let buffer = self.snapshot.as_ref()?.active_buffer?;
        if !self.operator_sync_pending.remove(&buffer) {
            return None;
        }
        self.vim
            .get(&buffer)
            .map(|view| (view.operator_pending.clone(), view.operator_count_started))
    }

    pub(crate) fn input_context(&self) -> InputContext {
        let Some(view) = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.active_buffer)
            .and_then(|buffer| self.vim.get(&buffer))
        else {
            return InputContext::AppDisabled;
        };
        if view.app_control {
            return InputContext::AppDisabled;
        }
        match view.mode.as_str() {
            "insert" => InputContext::Insert,
            "normal" => InputContext::Normal,
            "visual" | "visual-line" | "visual-block" => InputContext::Visual,
            "search" => InputContext::Search,
            _ => InputContext::AppDisabled,
        }
    }
    pub(crate) fn has_buffer(&self, buffer: u64) -> bool {
        self.snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.buffers.contains(&buffer))
    }
    pub(crate) fn set_prompt(&mut self, prompt: Option<(String, usize)>) {
        self.safe_output = None;
        self.rendered.clear();
        self.prompt = prompt;
    }
    pub(crate) fn draw(&mut self, output: &mut impl Write, cols: u16, rows: u16) -> io::Result<()> {
        let Some(snapshot) = self.snapshot.as_ref() else {
            return Ok(());
        };
        let bracketed_paste = snapshot.active_buffer.is_some_and(|buffer| {
            let showing_flash = self
                .vim
                .get(&buffer)
                .is_some_and(|view| view.yank_selection.is_some());
            let live_setting = self
                .parsers
                .get(&buffer)
                .is_some_and(|parser| parser.screen().bracketed_paste());
            bracketed_paste_enabled(
                self.prompt.is_some(),
                self.history_parsers.contains_key(&buffer),
                showing_flash,
                live_setting,
            )
        });
        if self.bracketed_paste != Some(bracketed_paste) {
            if bracketed_paste {
                queue!(output, crossterm::event::EnableBracketedPaste)?;
            } else {
                queue!(output, crossterm::event::DisableBracketedPaste)?;
            }
            self.bracketed_paste = Some(bracketed_paste);
        }
        let geometry = snapshot.geometry(cols, rows);
        let vim_colors = ResolvedVimColors::new(&self.config);
        queue!(output, Hide)?;
        if rows >= 3 {
            if let Some((prompt, _)) = self.prompt.as_ref() {
                draw_header_line(output, prompt, cols, &self.config)?;
            } else if let Some(prompt) = snapshot
                .active_buffer
                .and_then(|buffer| self.vim.get(&buffer))
                .and_then(|view| view.prompt.as_ref())
            {
                draw_search_prompt_line(
                    output,
                    &format!("/{prompt}"),
                    cols,
                    &self.config,
                    vim_colors,
                )?;
            } else {
                draw_buffer_line(output, snapshot, cols, &geometry, &self.config)?;
            }
        }
        let can_cache = self.prompt.is_none()
            && rows >= 3
            && geometry.windows.iter().all(|placement| {
                !self.history_parsers.contains_key(&placement.buffer)
                    && self
                        .vim
                        .get(&placement.buffer)
                        .is_some_and(incremental_view)
                    && self
                        .parsers
                        .get(&placement.buffer)
                        .is_some_and(|parser| !parser.screen().alternate_screen())
            });
        let can_diff = can_cache && self.safe_output.is_some();
        let mut next_rendered = Vec::new();
        let mut old_rendered = std::mem::take(&mut self.rendered);
        for placement in &geometry.windows {
            let view = self.vim.get(&placement.buffer);
            let history_offset = self
                .history_offsets
                .get(&placement.buffer)
                .copied()
                .unwrap_or_else(|| view.map_or(0, |view| view.scrollback));
            let absolute_top =
                view.map_or(0, |view| view.history_len.saturating_sub(history_offset));
            let parser = self
                .history_parsers
                .get(&placement.buffer)
                .or_else(|| self.parsers.get(&placement.buffer));
            let origin = parser.map(|parser| {
                let size = parser.screen().size();
                centered_origin(placement.rect, (size.0.get(), size.1.get()))
            });
            let prior = old_rendered
                .iter()
                .position(|prior| {
                    can_diff
                        && prior.window == placement.id
                        && prior.buffer == placement.buffer
                        && prior.rect == placement.rect
                        && Some(prior.origin) == origin
                        && prior.terminal == (cols, rows)
                })
                .map(|index| old_rendered.remove(index));
            if can_diff && Some(placement.buffer) != self.safe_output && prior.is_some() {
                next_rendered.push(prior.unwrap());
                continue;
            }
            if let (Some(parser), Some(origin)) = (parser, origin) {
                let current = can_cache.then(|| rendered_cells(parser.screen(), origin));
                if let Some(prior) = prior.as_ref() {
                    draw_pane_diff(
                        output,
                        placement.rect,
                        origin,
                        current.as_deref().unwrap(),
                        prior,
                    )?;
                } else {
                    draw_pane(
                        output,
                        placement.rect,
                        Some(parser),
                        view,
                        absolute_top,
                        &self.config,
                        vim_colors,
                    )?;
                }
                if let Some(cells) = current {
                    next_rendered.push(RenderedPane {
                        window: placement.id,
                        buffer: placement.buffer,
                        rect: placement.rect,
                        origin,
                        terminal: (cols, rows),
                        cells,
                    });
                }
            } else {
                draw_pane(
                    output,
                    placement.rect,
                    parser,
                    view,
                    absolute_top,
                    &self.config,
                    vim_colors,
                )?;
            }
        }
        self.rendered = next_rendered;
        self.safe_output = None;
        draw_dividers(output, &geometry, &self.config)?;
        if let Some((prompt, cursor_byte)) = self.prompt.as_ref() {
            if rows >= 3 && cols > 0 {
                let cursor_byte = (*cursor_byte).min(prompt.len());
                let cursor_byte = (0..=cursor_byte)
                    .rev()
                    .find(|&index| prompt.is_char_boundary(index))
                    .unwrap_or(0);
                let cursor_col = UnicodeWidthStr::width(&prompt[..cursor_byte])
                    .min(cols.saturating_sub(1) as usize) as u16;
                queue!(output, MoveTo(cursor_col, 0), Show)?;
            }
        } else if let Some(prompt) = snapshot
            .active_buffer
            .and_then(|buffer| self.vim.get(&buffer))
            .and_then(|view| view.prompt.as_ref())
        {
            let cursor_col = UnicodeWidthStr::width(prompt.as_str())
                .saturating_add(1)
                .min(cols.saturating_sub(1) as usize) as u16;
            queue!(output, MoveTo(cursor_col, 0), Show)?;
        } else if let Some(placement) = geometry
            .windows
            .iter()
            .find(|window| window.id == snapshot.focused_window)
        {
            if let Some(parser) = self
                .history_parsers
                .get(&placement.buffer)
                .or_else(|| self.parsers.get(&placement.buffer))
            {
                let screen = parser.screen();
                let size = screen.size();
                let (left, top, width, height) =
                    centered_origin(placement.rect, (size.0.get(), size.1.get()));
                let vim_cursor = self.vim.get(&placement.buffer).filter(|view| {
                    !view.app_control
                        && matches!(
                            view.mode.as_str(),
                            "normal" | "visual" | "visual-line" | "visual-block"
                        )
                });
                let (cursor_row, cursor_col) = if let Some(view) = vim_cursor {
                    let offset = self
                        .history_offsets
                        .get(&placement.buffer)
                        .copied()
                        .unwrap_or(view.scrollback);
                    let view_top = view.history_len.saturating_sub(offset);
                    (
                        view.cursor_row.saturating_sub(view_top),
                        usize::from(view.cursor_col),
                    )
                } else {
                    let (row, col) = screen.cursor_position();
                    (usize::from(row), usize::from(col))
                };
                let native_cursor_visible = vim_cursor.is_some()
                    || (!self.history_parsers.contains_key(&placement.buffer)
                        && !screen.hide_cursor());
                if native_cursor_visible
                    && cursor_row < usize::from(height)
                    && cursor_col < usize::from(width)
                {
                    queue!(
                        output,
                        MoveTo(left + cursor_col as u16, top + cursor_row as u16),
                        Show
                    )?;
                }
            }
        }
        queue!(output, SetAttribute(Attribute::Reset))?;
        output.flush()
    }
}

fn draw_buffer_line(
    output: &mut impl Write,
    snapshot: &LayoutSnapshot,
    cols: u16,
    geometry: &LayoutGeometry,
    config: &crate::config::Config,
) -> io::Result<()> {
    let color = |value: &str| crate::config::parse_color(value).unwrap();
    let line_background = color(&config.buffer_line.background);
    let intensity = if config.buffer_line.bold {
        Attribute::Bold
    } else {
        Attribute::NormalIntensity
    };
    let mut width = 0usize;
    queue!(output, MoveTo(0, 0))?;
    for (index, buffer) in snapshot.buffers.iter().enumerate() {
        if width >= cols as usize {
            break;
        }
        if index > 0 {
            let spaces = (cols as usize - width).min(2);
            queue!(
                output,
                SetForegroundColor(line_background),
                SetBackgroundColor(line_background),
                SetAttribute(intensity),
                Print(if spaces == 1 { " " } else { "  " })
            )?;
            width += spaces;
        }
        if width >= cols as usize {
            break;
        }
        let active = Some(*buffer) == snapshot.active_buffer;
        let shown = geometry
            .windows
            .iter()
            .any(|window| window.buffer == *buffer);
        let (fg, bg, prefix, suffix, prefix_fg, prefix_bg, suffix_fg, suffix_bg) = if active {
            (
                &config.buffer_line.focused_foreground,
                &config.buffer_line.focused_background,
                &config.buffer_line.focused_prefix,
                &config.buffer_line.focused_suffix,
                &config.buffer_line.focused_prefix_foreground,
                &config.buffer_line.focused_prefix_background,
                &config.buffer_line.focused_suffix_foreground,
                &config.buffer_line.focused_suffix_background,
            )
        } else if shown {
            (
                &config.buffer_line.shown_foreground,
                &config.buffer_line.shown_background,
                &config.buffer_line.shown_prefix,
                &config.buffer_line.shown_suffix,
                &config.buffer_line.shown_prefix_foreground,
                &config.buffer_line.shown_prefix_background,
                &config.buffer_line.shown_suffix_foreground,
                &config.buffer_line.shown_suffix_background,
            )
        } else {
            (
                &config.buffer_line.hidden_foreground,
                &config.buffer_line.hidden_background,
                &config.buffer_line.hidden_prefix,
                &config.buffer_line.hidden_suffix,
                &config.buffer_line.hidden_prefix_foreground,
                &config.buffer_line.hidden_prefix_background,
                &config.buffer_line.hidden_suffix_foreground,
                &config.buffer_line.hidden_suffix_background,
            )
        };
        let foreground = color(fg);
        let background = color(bg);
        let marker_foreground = if config.buffer_line.marker_color {
            line_background
        } else {
            foreground
        };
        let prefix_foreground = prefix_fg.as_deref().map(color).unwrap_or(marker_foreground);
        let prefix_background = prefix_bg.as_deref().map(color).unwrap_or(background);
        let suffix_foreground = suffix_fg.as_deref().map(color).unwrap_or(marker_foreground);
        let suffix_background = suffix_bg.as_deref().map(color).unwrap_or(background);
        queue!(
            output,
            SetForegroundColor(prefix_foreground),
            SetBackgroundColor(prefix_background),
            SetAttribute(intensity)
        )?;
        if !draw_clipped_text(output, prefix, cols, &mut width)? {
            break;
        }
        queue!(
            output,
            SetForegroundColor(foreground),
            SetBackgroundColor(background),
            SetAttribute(intensity)
        )?;
        let name = snapshot
            .buffer_names
            .get(buffer)
            .map(String::as_str)
            .unwrap_or_else(|| "");
        if name.is_empty() {
            if !draw_clipped_text(output, &buffer.to_string(), cols, &mut width)? {
                break;
            }
        } else if !draw_clipped_text(output, name, cols, &mut width)? {
            break;
        }
        queue!(
            output,
            SetForegroundColor(suffix_foreground),
            SetBackgroundColor(suffix_background),
            SetAttribute(intensity)
        )?;
        if !draw_clipped_text(output, suffix, cols, &mut width)? {
            break;
        }
    }
    queue!(
        output,
        SetForegroundColor(line_background),
        SetBackgroundColor(line_background),
        SetAttribute(intensity),
        Print(" ".repeat(cols as usize - width.min(cols as usize))),
        SetAttribute(Attribute::Reset)
    )
}

fn draw_clipped_text(
    output: &mut impl Write,
    text: &str,
    cols: u16,
    width: &mut usize,
) -> io::Result<bool> {
    for ch in text.chars() {
        let char_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if *width + char_width > cols as usize {
            return Ok(false);
        }
        queue!(output, Print(ch))?;
        *width += char_width;
    }
    Ok(true)
}

fn draw_header_line(
    output: &mut impl Write,
    line: &str,
    cols: u16,
    config: &crate::config::Config,
) -> io::Result<()> {
    let foreground = config
        .buffer_line
        .rename_foreground
        .as_deref()
        .unwrap_or(&config.buffer_line.focused_foreground);
    let background = config
        .buffer_line
        .rename_background
        .as_deref()
        .unwrap_or(&config.buffer_line.background);
    draw_styled_header_line(
        output,
        line,
        cols,
        crate::config::parse_color(foreground).unwrap(),
        crate::config::parse_color(background).unwrap(),
        config.buffer_line.bold,
    )
}

fn draw_search_prompt_line(
    output: &mut impl Write,
    line: &str,
    cols: u16,
    config: &crate::config::Config,
    colors: ResolvedVimColors,
) -> io::Result<()> {
    draw_styled_header_line(
        output,
        line,
        cols,
        colors.search_prompt_foreground,
        colors.search_prompt_background,
        config.buffer_line.bold,
    )
}

fn draw_styled_header_line(
    output: &mut impl Write,
    line: &str,
    cols: u16,
    foreground: Color,
    background: Color,
    bold: bool,
) -> io::Result<()> {
    let intensity = if bold {
        Attribute::Bold
    } else {
        Attribute::NormalIntensity
    };
    queue!(
        output,
        MoveTo(0, 0),
        SetBackgroundColor(background),
        SetForegroundColor(foreground),
        SetAttribute(intensity)
    )?;
    let mut visible = String::new();
    let mut width = 0;
    for character in line.chars() {
        let character_width = UnicodeWidthChar::width(character).unwrap_or(0);
        if width + character_width > cols as usize {
            break;
        }
        visible.push(character);
        width += character_width;
    }
    queue!(
        output,
        Print(visible),
        Print(" ".repeat(cols as usize - width)),
        SetAttribute(Attribute::Reset)
    )
}

fn draw_pane(
    output: &mut impl Write,
    rect: PaneRect,
    parser: Option<&Parser>,
    vim: Option<&VimView>,
    absolute_top: usize,
    config: &crate::config::Config,
    vim_colors: ResolvedVimColors,
) -> io::Result<()> {
    let Some(parser) = parser else {
        return draw_pattern(output, rect, config);
    };
    let screen = parser.screen();
    let size = screen.size();
    let (origin_x, origin_y, width, height) = centered_origin(rect, (size.0.get(), size.1.get()));
    draw_pattern_margins(output, rect, origin_x, origin_y, width, height, config)?;

    let mut previous_style = None;
    for row in 0..height {
        queue!(output, MoveTo(origin_x, origin_y + row))?;
        for col in 0..width {
            let Some(cell) = screen.cell(row, col) else {
                continue;
            };
            if cell.is_wide_continuation() {
                continue;
            }
            let mut style = CellStyle {
                foreground: convert_color(cell.fgcolor()),
                background: convert_color(cell.bgcolor()),
                bold: cell.bold(),
                dim: cell.dim(),
                italic: cell.italic(),
                underline: cell.underline(),
                inverse: cell.inverse(),
            };
            if let Some(view) = vim {
                let absolute_row = absolute_top + usize::from(row);
                let selected = selection_contains(view, absolute_row, col);
                let yanked = yank_selection_contains(view, absolute_row, col);
                let matched = match_contains(view, absolute_row, col);
                let active_match = view
                    .active_match
                    .and_then(|index| view.matches.get(index))
                    .is_some_and(|positions| positions.contains(&(absolute_row, col)));
                apply_match_style(
                    &mut style,
                    selected,
                    yanked,
                    matched,
                    active_match,
                    vim_colors,
                );
            }
            apply_cell_style(output, previous_style, style)?;
            previous_style = Some(style);
            queue!(
                output,
                Print(if cell.contents().is_empty() {
                    " "
                } else {
                    cell.contents()
                })
            )?;
        }
    }
    queue!(output, SetAttribute(Attribute::Reset))
}
fn rendered_cells(screen: &Screen, origin: (u16, u16, u16, u16)) -> Vec<Option<Cell>> {
    let (_, _, width, height) = origin;
    let mut cells = Vec::with_capacity(usize::from(width) * usize::from(height));
    for row in 0..height {
        for col in 0..width {
            cells.push(screen.cell(row, col).cloned());
        }
    }
    cells
}

fn draw_pane_diff(
    output: &mut impl Write,
    _rect: PaneRect,
    origin: (u16, u16, u16, u16),
    current: &[Option<Cell>],
    previous: &RenderedPane,
) -> io::Result<()> {
    let (x, y, width, _) = origin;
    let mut previous_style = None;
    for (index, cell) in current.iter().enumerate() {
        if previous.cells.get(index) == Some(cell) {
            continue;
        }
        if cell
            .as_ref()
            .is_some_and(|cell| cell.is_wide_continuation())
        {
            continue;
        }
        let row = index / usize::from(width);
        let col = index % usize::from(width);
        let style = cell.as_ref().map_or(
            CellStyle {
                foreground: Color::Reset,
                background: Color::Reset,
                bold: false,
                dim: false,
                italic: false,
                underline: false,
                inverse: false,
            },
            |cell| CellStyle {
                foreground: convert_color(cell.fgcolor()),
                background: convert_color(cell.bgcolor()),
                bold: cell.bold(),
                dim: cell.dim(),
                italic: cell.italic(),
                underline: cell.underline(),
                inverse: cell.inverse(),
            },
        );
        queue!(output, MoveTo(x + col as u16, y + row as u16))?;
        apply_cell_style(output, previous_style, style)?;
        previous_style = Some(style);
        queue!(
            output,
            Print(cell.as_ref().map_or(" ", |cell| {
                if cell.contents().is_empty() {
                    " "
                } else {
                    cell.contents()
                }
            }))
        )?;
    }
    queue!(output, SetAttribute(Attribute::Reset))
}

fn bracketed_paste_enabled(
    prompt_active: bool,
    showing_history: bool,
    showing_yank_flash: bool,
    live_setting: bool,
) -> bool {
    !prompt_active && (!showing_history || showing_yank_flash) && live_setting
}

fn selection_contains(view: &VimView, row: usize, col: u16) -> bool {
    let Some((start_row, start_col, end_row, end_col)) = view.selection else {
        return false;
    };
    if row < start_row || row > end_row {
        return false;
    }
    match view.selection_mode.as_deref().unwrap_or(&view.mode) {
        "visual-line" => true,
        "visual-block" => (start_col.min(end_col)..=start_col.max(end_col)).contains(&col),
        "visual" => (row > start_row || col >= start_col) && (row < end_row || col <= end_col),
        _ => false,
    }
}

fn yank_selection_contains(view: &VimView, row: usize, col: u16) -> bool {
    let Some((start_row, start_col, end_row, end_col)) = view.yank_selection else {
        return false;
    };
    if row < start_row || row > end_row {
        return false;
    }
    match view.yank_mode.as_deref() {
        Some("visual-line") => true,
        Some("visual-block") => (start_col.min(end_col)..=start_col.max(end_col)).contains(&col),
        Some("visual") => {
            (row > start_row || col >= start_col) && (row < end_row || col <= end_col)
        }
        _ => false,
    }
}

fn match_contains(view: &VimView, row: usize, col: u16) -> bool {
    view.matches
        .iter()
        .flatten()
        .any(|&(match_row, match_col)| match_row == row && match_col == col)
}

#[derive(Clone, Copy)]
struct ResolvedVimColors {
    search_prompt_foreground: Color,
    search_prompt_background: Color,
    search_match_foreground: Color,
    search_match_background: Color,
    search_current_foreground: Color,
    search_current_background: Color,
    selection_foreground: Color,
    selection_background: Color,
    yank_foreground: Color,
    yank_background: Color,
}

impl ResolvedVimColors {
    fn new(config: &crate::config::Config) -> Self {
        let colors = &config.vim_colors;
        let parse = |value: &str| crate::config::parse_color(value).unwrap();
        let fallback_prompt_foreground = config
            .buffer_line
            .rename_foreground
            .as_deref()
            .unwrap_or(&config.buffer_line.focused_foreground);
        let fallback_prompt_background = config
            .buffer_line
            .rename_background
            .as_deref()
            .unwrap_or(&config.buffer_line.background);
        Self {
            search_prompt_foreground: parse(
                colors
                    .search_prompt_foreground
                    .as_deref()
                    .unwrap_or(fallback_prompt_foreground),
            ),
            search_prompt_background: parse(
                colors
                    .search_prompt_background
                    .as_deref()
                    .unwrap_or(fallback_prompt_background),
            ),
            search_match_foreground: parse(&colors.search_match_foreground),
            search_match_background: parse(&colors.search_match_background),
            search_current_foreground: parse(&colors.search_current_foreground),
            search_current_background: parse(&colors.search_current_background),
            selection_foreground: parse(&colors.selection_foreground),
            selection_background: parse(&colors.selection_background),
            yank_foreground: parse(&colors.yank_foreground),
            yank_background: parse(&colors.yank_background),
        }
    }
}

fn apply_match_style(
    style: &mut CellStyle,
    selected: bool,
    yanked: bool,
    matched: bool,
    active_match: bool,
    colors: ResolvedVimColors,
) {
    let color = if yanked {
        Some((colors.yank_foreground, colors.yank_background))
    } else if selected {
        Some((colors.selection_foreground, colors.selection_background))
    } else if active_match {
        Some((
            colors.search_current_foreground,
            colors.search_current_background,
        ))
    } else if matched {
        Some((
            colors.search_match_foreground,
            colors.search_match_background,
        ))
    } else {
        None
    };
    if let Some((foreground, background)) = color {
        style.foreground = foreground;
        style.background = background;
        style.inverse = false;
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct CellStyle {
    foreground: Color,
    background: Color,
    bold: bool,
    dim: bool,
    italic: bool,
    underline: bool,
    inverse: bool,
}

fn apply_cell_style(
    output: &mut impl Write,
    previous: Option<CellStyle>,
    style: CellStyle,
) -> io::Result<()> {
    if previous.is_none() {
        queue!(output, SetAttribute(Attribute::Reset))?;
    }
    if previous.is_none_or(|previous| previous.foreground != style.foreground) {
        queue!(output, SetForegroundColor(style.foreground))?;
    }
    if previous.is_none_or(|previous| previous.background != style.background) {
        queue!(output, SetBackgroundColor(style.background))?;
    }
    if previous.is_none_or(|previous| previous.bold != style.bold || previous.dim != style.dim) {
        if previous.is_some() {
            queue!(output, SetAttribute(Attribute::NormalIntensity))?;
        }
        if style.bold {
            queue!(output, SetAttribute(Attribute::Bold))?;
        }
        if style.dim {
            queue!(output, SetAttribute(Attribute::Dim))?;
        }
    }
    if previous.is_none_or(|previous| previous.italic != style.italic) && style.italic {
        queue!(output, SetAttribute(Attribute::Italic))?;
    } else if previous.is_some_and(|previous| previous.italic != style.italic) {
        queue!(output, SetAttribute(Attribute::NoItalic))?;
    }
    if previous.is_none_or(|previous| previous.underline != style.underline) && style.underline {
        queue!(output, SetAttribute(Attribute::Underlined))?;
    } else if previous.is_some_and(|previous| previous.underline != style.underline) {
        queue!(output, SetAttribute(Attribute::NoUnderline))?;
    }
    if previous.is_none_or(|previous| previous.inverse != style.inverse) && style.inverse {
        queue!(output, SetAttribute(Attribute::Reverse))?;
    } else if previous.is_some_and(|previous| previous.inverse != style.inverse) {
        queue!(output, SetAttribute(Attribute::NoReverse))?;
    }
    Ok(())
}

fn draw_pattern_margins(
    output: &mut impl Write,
    rect: PaneRect,
    x: u16,
    y: u16,
    width: u16,
    height: u16,
    config: &crate::config::Config,
) -> io::Result<()> {
    let right = rect.x + rect.width;
    let bottom = rect.y + rect.height;
    if y > rect.y {
        draw_pattern(
            output,
            PaneRect {
                height: y - rect.y,
                ..rect
            },
            config,
        )?;
    }
    let content_bottom = y + height;
    if content_bottom < bottom {
        draw_pattern(
            output,
            PaneRect {
                y: content_bottom,
                height: bottom - content_bottom,
                ..rect
            },
            config,
        )?;
    }
    if x > rect.x {
        draw_pattern(
            output,
            PaneRect {
                y,
                width: x - rect.x,
                height,
                ..rect
            },
            config,
        )?;
    }
    let content_right = x + width;
    if content_right < right {
        draw_pattern(
            output,
            PaneRect {
                x: content_right,
                y,
                width: right - content_right,
                height,
                ..rect
            },
            config,
        )?;
    }
    Ok(())
}

fn draw_pattern(
    output: &mut impl Write,
    rect: PaneRect,
    config: &crate::config::Config,
) -> io::Result<()> {
    if rect.width == 0 || rect.height == 0 {
        return Ok(());
    }
    queue!(
        output,
        SetAttribute(Attribute::Reset),
        SetForegroundColor(crate::config::parse_color(&config.margin.foreground).unwrap()),
        SetBackgroundColor(crate::config::parse_color(&config.margin.background).unwrap())
    )?;
    for row in 0..rect.height {
        queue!(output, MoveTo(rect.x, rect.y + row))?;
        for col in 0..rect.width {
            queue!(
                output,
                Print(if (row + col) % 2 == 0 {
                    config.margin.fill.as_str()
                } else {
                    " "
                })
            )?;
        }
    }
    Ok(())
}

fn draw_dividers(
    output: &mut impl Write,
    geometry: &LayoutGeometry,
    config: &crate::config::Config,
) -> io::Result<()> {
    queue!(
        output,
        SetForegroundColor(crate::config::parse_color(&config.divider.foreground).unwrap()),
        SetBackgroundColor(Color::Reset)
    )?;
    for divider in &geometry.dividers {
        match *divider {
            Divider::Vertical { x, top, height } => {
                for row in top..top + height {
                    queue!(
                        output,
                        MoveTo(x, row),
                        Print(config.divider.vertical.as_str())
                    )?;
                }
            }
            Divider::Horizontal { y, left, width } => {
                for col in left..left + width {
                    queue!(
                        output,
                        MoveTo(col, y),
                        Print(config.divider.horizontal.as_str())
                    )?;
                }
            }
        }
    }
    Ok(())
}

fn centered_origin(rect: PaneRect, (screen_rows, screen_cols): (u16, u16)) -> (u16, u16, u16, u16) {
    let width = rect.width.min(screen_cols);
    let height = rect.height.min(screen_rows);
    (
        rect.x + (rect.width - width) / 2,
        rect.y + (rect.height - height) / 2,
        width,
        height,
    )
}

fn convert_color(color: VtColor) -> Color {
    match color {
        VtColor::Default => Color::Reset,
        VtColor::Idx(index) => Color::AnsiValue(index),
        VtColor::Rgb(red, green, blue) => Color::Rgb {
            r: red,
            g: green,
            b: blue,
        },
    }
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_be_bytes(
        bytes[offset..offset + 8]
            .try_into()
            .expect("validated frame length"),
    )
}

fn read_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_be_bytes(
        bytes[offset..offset + 2]
            .try_into()
            .expect("validated frame length"),
    )
}

fn invalid_frame(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bracketed_paste_follows_focused_buffer_and_rename_prompt() {
        let mut renderer = Renderer::default();
        for (id, mode) in [(1u64, b"\x1b[?2004h".as_slice()), (2, b"".as_slice())] {
            let mut snapshot = Vec::from(id.to_be_bytes());
            snapshot.extend_from_slice(&2u16.to_be_bytes());
            snapshot.extend_from_slice(&10u16.to_be_bytes());
            snapshot.extend_from_slice(mode);
            renderer.apply_frame(BUFFER_SNAPSHOT, &snapshot).unwrap();
        }
        let layout = |buffer| LayoutSnapshot {
            tree: crate::layout::WindowNode::Window { id: 1, buffer },
            focused_window: 1,
            active_buffer: Some(buffer),
            buffers: vec![1, 2],
            buffer_names: HashMap::new(),
        };
        renderer.set_layout(layout(1));
        let mut output = Vec::new();
        renderer.draw(&mut output, 10, 3).unwrap();
        assert!(output.windows(8).any(|bytes| bytes == b"\x1b[?2004h"));
        output.clear();
        renderer.set_prompt(Some(("rename".into(), 6)));
        renderer.draw(&mut output, 10, 3).unwrap();
        assert!(output.windows(8).any(|bytes| bytes == b"\x1b[?2004l"));
        output.clear();
        renderer.set_prompt(None);
        renderer.draw(&mut output, 10, 3).unwrap();
        assert!(output.windows(8).any(|bytes| bytes == b"\x1b[?2004h"));
        output.clear();
        renderer.set_layout(layout(2));
        renderer.draw(&mut output, 10, 3).unwrap();
        assert!(output.windows(8).any(|bytes| bytes == b"\x1b[?2004l"));
    }
    #[test]
    fn input_context_tracks_mode_and_application_control() {
        let mut renderer = Renderer::default();
        assert_eq!(renderer.input_context(), InputContext::AppDisabled);
        renderer.set_layout(LayoutSnapshot {
            tree: crate::layout::WindowNode::Window { id: 1, buffer: 1 },
            focused_window: 1,
            active_buffer: Some(1),
            buffers: vec![1],
            buffer_names: HashMap::new(),
        });
        for (mode, expected) in [
            ("insert", InputContext::Insert),
            ("normal", InputContext::Normal),
            ("visual", InputContext::Visual),
            ("visual-line", InputContext::Visual),
            ("visual-block", InputContext::Visual),
            ("search", InputContext::Search),
        ] {
            let state = serde_json::json!({
                "buffer": 1,
                "mode": mode,
                "cursor_row": 0,
                "cursor_col": 0,
                "selection": null,
                "matches": [],
                "active_match": null,
                "prompt": null,
                "app_control": false,
                "history_len": 0,
                "scrollback": 0
            });
            renderer
                .apply_frame(VIM_STATE, &serde_json::to_vec(&state).unwrap())
                .unwrap();
            assert_eq!(renderer.input_context(), expected);
        }
        let state = serde_json::json!({
            "buffer": 1,
            "mode": "normal",
            "cursor_row": 0,
            "cursor_col": 0,
            "selection": null,
            "matches": [],
            "active_match": null,
            "prompt": null,
            "app_control": true,
            "history_len": 0,
            "scrollback": 0
        });
        renderer
            .apply_frame(VIM_STATE, &serde_json::to_vec(&state).unwrap())
            .unwrap();
        assert_eq!(renderer.input_context(), InputContext::AppDisabled);
    }

    #[test]
    fn shared_operator_snapshot_keeps_remapped_inner_object_selector_for_each_client() {
        let mut config = crate::config::Config::default();
        config.bindings.insert("vim_yank_inner".into(), "x".into());
        let mut clients = [
            (
                Renderer::with_config(config.clone()),
                crate::input::InputDecoder::configured(&config),
            ),
            (
                Renderer::with_config(config.clone()),
                crate::input::InputDecoder::configured(&config),
            ),
        ];
        let layout = || LayoutSnapshot {
            tree: crate::layout::WindowNode::Window { id: 1, buffer: 1 },
            focused_window: 1,
            active_buffer: Some(1),
            buffers: vec![1],
            buffer_names: HashMap::new(),
        };
        let snapshot = |operator_pending: &str, operator_count_started: bool| {
            serde_json::json!({
                "buffer": 1,
                "mode": "normal",
                "selection_mode": null,
                "cursor_row": 0,
                "cursor_col": 0,
                "selection": null,
                "matches": [],
                "active_match": null,
                "prompt": null,
                "app_control": false,
                "history_len": 0,
                "scrollback": 0,
                "operator_pending": operator_pending,
                "operator_count_started": operator_count_started
            })
        };
        for (renderer, decoder) in &mut clients {
            renderer.set_layout(layout());
            let state = serde_json::to_vec(&snapshot("y", false)).unwrap();
            renderer.apply_frame(VIM_STATE, &state).unwrap();
            if let Some((operator_pending, count_started)) = renderer.take_operator_pending() {
                decoder.sync_operator(operator_pending.as_deref(), count_started);
            }
        }

        let (_, client_decoder) = &mut clients[1];
        let actions = client_decoder.feed_with_context(b"x", InputContext::Normal);
        assert_eq!(
            actions,
            vec![crate::input::InputAction::Command(
                crate::input::InputCommand {
                    kind: crate::server::VIM_KEY,
                    payload: b"i",
                }
            )]
        );

        let state = serde_json::to_vec(&snapshot("yi", false)).unwrap();
        for (renderer, decoder) in &mut clients {
            renderer.apply_frame(VIM_STATE, &state).unwrap();
            if let Some((operator_pending, count_started)) = renderer.take_operator_pending() {
                decoder.sync_operator(operator_pending.as_deref(), count_started);
            }
            assert_eq!(
                decoder.feed_with_context(b"w", InputContext::Normal),
                vec![crate::input::InputAction::Bytes(b"w".to_vec())]
            );
        }
    }

    #[test]
    fn operator_count_started_survives_snapshot_and_zero_selector_input() {
        let config = crate::config::Config::default();
        let mut renderer = Renderer::with_config(config.clone());
        renderer.set_layout(LayoutSnapshot {
            tree: crate::layout::WindowNode::Window { id: 1, buffer: 1 },
            focused_window: 1,
            active_buffer: Some(1),
            buffers: vec![1],
            buffer_names: HashMap::new(),
        });
        let state = serde_json::json!({
            "buffer": 1,
            "mode": "normal",
            "cursor_row": 0,
            "cursor_col": 0,
            "selection": null,
            "matches": [],
            "active_match": null,
            "prompt": null,
            "app_control": false,
            "history_len": 0,
            "scrollback": 0,
            "operator_pending": "yi",
            "operator_count_started": true
        });
        renderer
            .apply_frame(VIM_STATE, &serde_json::to_vec(&state).unwrap())
            .unwrap();
        let mut decoder = crate::input::InputDecoder::configured(&config);
        let (operator_pending, count_started) = renderer.take_operator_pending().unwrap();
        assert_eq!(operator_pending.as_deref(), Some("yi"));
        assert!(count_started);
        decoder.sync_operator(operator_pending.as_deref(), count_started);
        assert_eq!(
            decoder.feed_with_context(b"0w", InputContext::Normal),
            vec![crate::input::InputAction::Bytes(b"0w".to_vec())]
        );
    }

    #[test]
    fn absent_operator_snapshot_does_not_clear_local_pending_state() {
        let mut renderer = Renderer::default();
        let mut decoder = crate::input::InputDecoder::default();
        renderer.set_layout(LayoutSnapshot {
            tree: crate::layout::WindowNode::Window { id: 1, buffer: 1 },
            focused_window: 1,
            active_buffer: Some(1),
            buffers: vec![1],
            buffer_names: HashMap::new(),
        });
        let legacy_state = serde_json::json!({
            "buffer": 1,
            "mode": "normal",
            "cursor_row": 0,
            "cursor_col": 0,
            "selection": null,
            "matches": [],
            "active_match": null,
            "prompt": null,
            "app_control": false,
            "history_len": 0,
            "scrollback": 0
        });
        renderer
            .apply_frame(VIM_STATE, &serde_json::to_vec(&legacy_state).unwrap())
            .unwrap();
        if let Some((operator_pending, count_started)) = renderer.take_operator_pending() {
            decoder.sync_operator(operator_pending.as_deref(), count_started);
        }
        decoder.feed_with_context(b"y", InputContext::Normal);
        assert_eq!(renderer.take_operator_pending(), None);
        assert_eq!(
            decoder.feed_with_context(b"w", InputContext::Normal),
            vec![crate::input::InputAction::Bytes(b"w".to_vec())]
        );
    }

    #[test]
    fn snapshots_initialize_and_output_updates_the_matching_buffer() {
        let mut renderer = Renderer::default();
        let mut snapshot = Vec::from(7u64.to_be_bytes());
        snapshot.extend_from_slice(&2u16.to_be_bytes());
        snapshot.extend_from_slice(&4u16.to_be_bytes());
        snapshot.extend_from_slice(b"abcd");
        renderer.apply_frame(BUFFER_SNAPSHOT, &snapshot).unwrap();
        assert_eq!(
            renderer.parsers[&7].screen().cell(0, 0).unwrap().contents(),
            "a"
        );

        let mut output = Vec::from(7u64.to_be_bytes());
        output.extend_from_slice(b"\x1b[1;2H!");
        renderer.apply_frame(BUFFER_OUTPUT, &output).unwrap();
        assert_eq!(
            renderer.parsers[&7].screen().cell(0, 1).unwrap().contents(),
            "!"
        );
        assert!(!renderer.parsers.contains_key(&8));
    }
    #[test]
    fn historical_view_does_not_stop_live_parser_and_can_return_to_live() {
        let mut renderer = Renderer::default();
        let mut live = Vec::from(7u64.to_be_bytes());
        live.extend_from_slice(&2u16.to_be_bytes());
        live.extend_from_slice(&4u16.to_be_bytes());
        live.extend_from_slice(b"live");
        renderer.apply_frame(BUFFER_SNAPSHOT, &live).unwrap();

        let mut history = Vec::from(7u64.to_be_bytes());
        history.extend_from_slice(&2u16.to_be_bytes());
        history.extend_from_slice(&4u16.to_be_bytes());
        history.extend_from_slice(&1u32.to_be_bytes());
        history.extend_from_slice(b"past");
        renderer
            .apply_frame(crate::server::HISTORY_SNAPSHOT, &history)
            .unwrap();
        assert_eq!(
            renderer.history_parsers[&7]
                .screen()
                .cell(0, 0)
                .unwrap()
                .contents(),
            "p"
        );

        let mut output = Vec::from(7u64.to_be_bytes());
        output.extend_from_slice(b"\x1b[1;1H!");
        renderer.apply_frame(BUFFER_OUTPUT, &output).unwrap();
        assert_eq!(
            renderer.parsers[&7].screen().cell(0, 0).unwrap().contents(),
            "!"
        );
        assert_eq!(
            renderer.history_parsers[&7]
                .screen()
                .cell(0, 0)
                .unwrap()
                .contents(),
            "p"
        );

        history[12..16].copy_from_slice(&0u32.to_be_bytes());
        renderer
            .apply_frame(crate::server::HISTORY_SNAPSHOT, &history)
            .unwrap();
        assert!(!renderer.history_parsers.contains_key(&7));
    }

    #[test]
    fn drawing_centers_the_buffer_over_the_pattern() {
        let mut renderer = Renderer::default();
        let mut snapshot = Vec::from(7u64.to_be_bytes());
        snapshot.extend_from_slice(&2u16.to_be_bytes());
        snapshot.extend_from_slice(&4u16.to_be_bytes());
        snapshot.extend_from_slice("界x".as_bytes());
        renderer.apply_frame(BUFFER_SNAPSHOT, &snapshot).unwrap();
        renderer.set_layout(LayoutSnapshot {
            tree: crate::layout::WindowNode::Window { id: 1, buffer: 7 },
            focused_window: 1,
            active_buffer: Some(7),
            buffers: vec![7],
            buffer_names: HashMap::from([(7, "7".into())]),
        });

        let mut output = Vec::new();
        renderer.draw(&mut output, 10, 6).unwrap();
        assert!(!output.windows(5).any(|bytes| bytes == b"\x1b[59m"));
        let mut terminal = Parser::new(
            std::num::NonZeroU16::new(6).unwrap(),
            std::num::NonZeroU16::new(10).unwrap(),
            0,
        );
        terminal.process(&output);
        let screen = terminal.screen();
        assert_eq!(screen.cell(0, 0).unwrap().contents(), "[");
        assert_eq!(screen.cell(1, 0).unwrap().contents(), ".");
        assert_eq!(screen.cell(2, 3).unwrap().contents(), "界");
        assert!(screen.cell(2, 4).unwrap().is_wide_continuation());
        assert_eq!(screen.cell(2, 5).unwrap().contents(), "x");
    }

    #[test]
    fn first_pane_cell_retains_its_colors_before_default_text() {
        let mut renderer = Renderer::default();
        let mut snapshot = Vec::from(1u64.to_be_bytes());
        snapshot.extend_from_slice(&2u16.to_be_bytes());
        snapshot.extend_from_slice(&4u16.to_be_bytes());
        snapshot.extend_from_slice(b"\x1b[31;44mX\x1b[0mY");
        renderer.apply_frame(BUFFER_SNAPSHOT, &snapshot).unwrap();
        renderer.set_layout(LayoutSnapshot {
            tree: crate::layout::WindowNode::Window { id: 1, buffer: 1 },
            focused_window: 1,
            active_buffer: Some(1),
            buffers: vec![1],
            buffer_names: HashMap::from([(1, "1".into())]),
        });

        let mut output = Vec::new();
        renderer.draw(&mut output, 4, 3).unwrap();
        let mut terminal = Parser::new(
            std::num::NonZeroU16::new(3).unwrap(),
            std::num::NonZeroU16::new(4).unwrap(),
            0,
        );
        terminal.process(&output);
        let screen = terminal.screen();
        let first = screen.cell(1, 0).unwrap();
        assert_eq!(first.contents(), "X");
        if std::env::var("NO_COLOR").is_ok_and(|value| !value.is_empty()) {
            assert_eq!(first.fgcolor(), VtColor::Default);
            assert_eq!(first.bgcolor(), VtColor::Default);
        } else {
            assert_eq!(first.fgcolor(), VtColor::Idx(1));
            assert_eq!(first.bgcolor(), VtColor::Idx(4));
        }
        let next = screen.cell(1, 1).unwrap();
        assert_eq!(next.contents(), "Y");
        assert_eq!(next.fgcolor(), VtColor::Default);
        assert_eq!(next.bgcolor(), VtColor::Default);
    }

    #[test]
    fn buffer_line_displays_shared_names_and_marks_the_active_buffer() {
        let mut renderer = Renderer::default();
        renderer.set_layout(LayoutSnapshot {
            tree: crate::layout::WindowNode::Window { id: 1, buffer: 8 },
            focused_window: 1,
            active_buffer: Some(8),
            buffers: vec![7, 8],
            buffer_names: HashMap::from([(7, "shell".into()), (8, "雪".into())]),
        });

        let mut output = Vec::new();
        renderer.draw(&mut output, 20, 3).unwrap();
        let mut terminal = Parser::new(
            std::num::NonZeroU16::new(3).unwrap(),
            std::num::NonZeroU16::new(20).unwrap(),
            0,
        );
        terminal.process(&output);
        let screen = terminal.screen();

        assert_eq!(screen.cell(0, 0).unwrap().contents(), "s");
        assert_eq!(screen.cell(0, 7).unwrap().contents(), "[");
        assert_eq!(screen.cell(0, 8).unwrap().contents(), "雪");
        assert!(screen.cell(0, 9).unwrap().is_wide_continuation());
        assert_eq!(screen.cell(0, 10).unwrap().contents(), "]");
        if !std::env::var("NO_COLOR").is_ok_and(|value| !value.is_empty()) {
            let marker = screen.cell(0, 7).unwrap();
            let name = screen.cell(0, 8).unwrap();
            assert_eq!(marker.fgcolor(), name.fgcolor());
            assert_eq!(marker.bgcolor(), name.bgcolor());
            assert_eq!(screen.cell(0, 10).unwrap().fgcolor(), name.fgcolor());
            assert_eq!(screen.cell(0, 10).unwrap().bgcolor(), name.bgcolor());
        }
    }
    #[test]
    fn configured_buffer_markers_use_per_state_text_and_separator_colors() {
        let mut config = crate::config::Config::default();
        config.buffer_line.focused_prefix = "".into();
        config.buffer_line.focused_suffix = "".into();
        config.buffer_line.shown_prefix = "<".into();
        config.buffer_line.shown_suffix = ">".into();
        config.buffer_line.hidden_prefix = "{".into();
        config.buffer_line.hidden_suffix = "}".into();
        config.buffer_line.marker_color = true;
        config.buffer_line.focused_background = "blue".into();
        config.buffer_line.shown_background = "green".into();
        config.buffer_line.hidden_background = "red".into();
        config.buffer_line.focused_prefix_foreground = Some("red".into());
        config.buffer_line.focused_prefix_background = Some("blue".into());
        config.buffer_line.focused_suffix_foreground = Some("yellow".into());
        config.buffer_line.shown_prefix_background = Some("magenta".into());
        config.buffer_line.shown_suffix_foreground = Some("cyan".into());
        config.buffer_line.hidden_suffix_foreground = Some("black".into());
        config.buffer_line.hidden_suffix_background = Some("white".into());
        let mut renderer = Renderer::with_config(config);
        renderer.set_layout(LayoutSnapshot {
            tree: crate::layout::WindowNode::Split {
                axis: crate::layout::Axis::Vertical,
                ratio: 50,
                first: Box::new(crate::layout::WindowNode::Window { id: 1, buffer: 1 }),
                second: Box::new(crate::layout::WindowNode::Window { id: 2, buffer: 2 }),
            },
            focused_window: 1,
            active_buffer: Some(1),
            buffers: vec![1, 2, 3],
            buffer_names: HashMap::from([(1, "A".into()), (2, "B".into()), (3, "C".into())]),
        });

        let mut output = Vec::new();
        renderer.draw(&mut output, 20, 3).unwrap();
        let mut terminal = Parser::new(
            std::num::NonZeroU16::new(3).unwrap(),
            std::num::NonZeroU16::new(20).unwrap(),
            0,
        );
        terminal.process(&output);
        let screen = terminal.screen();
        assert_eq!(screen.cell(0, 0).unwrap().contents(), "");
        assert_eq!(screen.cell(0, 1).unwrap().contents(), "A");
        assert_eq!(screen.cell(0, 2).unwrap().contents(), "");
        assert_eq!(screen.cell(0, 5).unwrap().contents(), "<");
        assert_eq!(screen.cell(0, 6).unwrap().contents(), "B");
        assert_eq!(screen.cell(0, 7).unwrap().contents(), ">");
        assert_eq!(screen.cell(0, 10).unwrap().contents(), "{");
        assert_eq!(screen.cell(0, 11).unwrap().contents(), "C");
        assert_eq!(screen.cell(0, 12).unwrap().contents(), "}");
        if !std::env::var("NO_COLOR").is_ok_and(|value| !value.is_empty()) {
            let focused_marker = screen.cell(0, 0).unwrap();
            assert_eq!(focused_marker.fgcolor(), VtColor::Idx(9));
            assert_eq!(focused_marker.bgcolor(), VtColor::Idx(12));
            let focused_name = screen.cell(0, 1).unwrap();
            assert_eq!(focused_name.fgcolor(), VtColor::Idx(15));
            assert_eq!(focused_name.bgcolor(), VtColor::Idx(12));
            assert_eq!(screen.cell(0, 2).unwrap().fgcolor(), VtColor::Idx(11));
            assert_eq!(screen.cell(0, 2).unwrap().bgcolor(), VtColor::Idx(12));
            let shown_marker = screen.cell(0, 5).unwrap();
            assert_eq!(shown_marker.fgcolor(), VtColor::Idx(8));
            assert_eq!(shown_marker.bgcolor(), VtColor::Idx(13));
            let shown_suffix = screen.cell(0, 7).unwrap();
            assert_eq!(shown_suffix.fgcolor(), VtColor::Idx(14));
            assert_eq!(shown_suffix.bgcolor(), VtColor::Idx(10));
            let hidden_marker = screen.cell(0, 10).unwrap();
            assert_eq!(hidden_marker.fgcolor(), VtColor::Idx(8));
            assert_eq!(hidden_marker.bgcolor(), VtColor::Idx(9));
            let hidden_suffix = screen.cell(0, 12).unwrap();
            assert_eq!(hidden_suffix.fgcolor(), VtColor::Idx(0));
            assert_eq!(hidden_suffix.bgcolor(), VtColor::Idx(15));
        }
    }

    #[test]
    fn custom_buffer_markers_clip_on_unicode_cell_boundaries() {
        let mut config = crate::config::Config::default();
        config.buffer_line.focused_prefix = "界".into();
        config.buffer_line.focused_suffix = "".into();
        let mut renderer = Renderer::with_config(config);
        renderer.set_layout(LayoutSnapshot {
            tree: crate::layout::WindowNode::Window { id: 1, buffer: 1 },
            focused_window: 1,
            active_buffer: Some(1),
            buffers: vec![1],
            buffer_names: HashMap::from([(1, "雪".into())]),
        });

        let mut output = Vec::new();
        renderer.draw(&mut output, 3, 3).unwrap();
        let mut terminal = Parser::new(
            std::num::NonZeroU16::new(3).unwrap(),
            std::num::NonZeroU16::new(3).unwrap(),
            0,
        );
        terminal.process(&output);
        let screen = terminal.screen();
        assert_eq!(screen.cell(0, 0).unwrap().contents(), "界");
        assert!(screen.cell(0, 1).unwrap().is_wide_continuation());
        assert_eq!(screen.cell(0, 2).unwrap().contents(), " ");
    }

    #[test]
    fn rename_prompt_shows_edit_text_then_restores_buffer_line() {
        let mut renderer = Renderer::default();
        renderer.set_layout(LayoutSnapshot {
            tree: crate::layout::WindowNode::Window { id: 1, buffer: 7 },
            focused_window: 1,
            active_buffer: Some(7),
            buffers: vec![7],
            buffer_names: HashMap::from([(7, "shell".into())]),
        });
        renderer.set_prompt(Some(("Rename 7: shell".into(), 15)));

        let mut output = Vec::new();
        renderer.draw(&mut output, 20, 3).unwrap();
        let mut terminal = Parser::new(
            std::num::NonZeroU16::new(3).unwrap(),
            std::num::NonZeroU16::new(20).unwrap(),
            0,
        );
        terminal.process(&output);
        {
            let screen = terminal.screen();
            assert_eq!(screen.cell(0, 0).unwrap().contents(), "R");
            assert_eq!(screen.cell(0, 10).unwrap().contents(), "s");
            assert_eq!(screen.cursor_position(), (0, 15));
        }

        renderer.set_prompt(None);
        output.clear();
        renderer.draw(&mut output, 20, 3).unwrap();
        terminal.process(&output);
        assert_eq!(terminal.screen().cell(0, 0).unwrap().contents(), "[");
    }

    #[test]
    fn rename_prompt_positions_cursor_by_unicode_display_width() {
        let mut renderer = Renderer::default();
        renderer.set_layout(LayoutSnapshot {
            tree: crate::layout::WindowNode::Window { id: 1, buffer: 7 },
            focused_window: 1,
            active_buffer: Some(7),
            buffers: vec![7],
            buffer_names: HashMap::new(),
        });
        let prompt = "名前: x";
        renderer.set_prompt(Some((prompt.into(), "名前: ".len())));

        let mut output = Vec::new();
        renderer.draw(&mut output, 20, 3).unwrap();
        let mut terminal = Parser::new(
            std::num::NonZeroU16::new(3).unwrap(),
            std::num::NonZeroU16::new(20).unwrap(),
            0,
        );
        terminal.process(&output);
        assert_eq!(terminal.screen().cursor_position(), (0, 6));
    }

    #[test]
    fn rename_prompt_inherits_colors_and_accepts_overrides() {
        for (overrides, expected_fg, expected_bg) in [
            ("", VtColor::Rgb(255, 158, 100), VtColor::Rgb(22, 22, 30)),
            (
                "rename_foreground = \"#010203\"\nrename_background = \"#040506\"\n",
                VtColor::Rgb(1, 2, 3),
                VtColor::Rgb(4, 5, 6),
            ),
        ] {
            let config: crate::config::Config = toml::from_str(&format!(
                "[buffer_line]\nbold = false\nfocused_foreground = \"#ff9e64\"\nbackground = \"#16161e\"\n{overrides}"
            )).unwrap();
            let mut output = Vec::new();
            draw_header_line(&mut output, "Rename 7: shell", 20, &config).unwrap();
            let mut terminal = Parser::new(
                std::num::NonZeroU16::new(3).unwrap(),
                std::num::NonZeroU16::new(20).unwrap(),
                0,
            );
            terminal.process(&output);
            let cell = terminal.screen().cell(0, 0).unwrap();
            assert_eq!(cell.contents(), "R");
            assert!(!cell.bold());
            if !std::env::var("NO_COLOR").is_ok_and(|value| !value.is_empty()) {
                assert_eq!(cell.fgcolor(), expected_fg);
                assert_eq!(cell.bgcolor(), expected_bg);
                assert_eq!(
                    terminal.screen().cell(0, 19).unwrap().bgcolor(),
                    expected_bg
                );
            }
        }
    }

    #[test]
    fn buffer_line_background_does_not_bleed_into_the_pane() {
        let mut renderer = Renderer::default();
        let mut snapshot = Vec::from(1u64.to_be_bytes());
        snapshot.extend_from_slice(&4u16.to_be_bytes());
        snapshot.extend_from_slice(&10u16.to_be_bytes());
        snapshot.extend_from_slice(b"prompt");
        renderer.apply_frame(BUFFER_SNAPSHOT, &snapshot).unwrap();
        renderer.set_layout(LayoutSnapshot {
            tree: crate::layout::WindowNode::Window { id: 1, buffer: 1 },
            focused_window: 1,
            active_buffer: Some(1),
            buffers: vec![1, 2, 3, 4, 5],
            buffer_names: HashMap::from([
                (1, "1".into()),
                (2, "2".into()),
                (3, "3".into()),
                (4, "4".into()),
                (5, "5".into()),
            ]),
        });

        let mut output = Vec::new();
        renderer.draw(&mut output, 10, 5).unwrap();
        let mut terminal = Parser::new(
            std::num::NonZeroU16::new(5).unwrap(),
            std::num::NonZeroU16::new(10).unwrap(),
            0,
        );
        terminal.process(&output);
        let screen = terminal.screen();

        let header_background = screen.cell(0, 0).unwrap().bgcolor();
        let pane_background = screen.cell(1, 0).unwrap().bgcolor();
        if std::env::var("NO_COLOR").is_ok_and(|value| !value.is_empty()) {
            assert_eq!(header_background, pane_background);
        } else {
            assert_ne!(header_background, pane_background);
        }
    }

    #[test]
    fn full_screen_redraw_stays_compact() {
        let mut renderer = Renderer::default();
        let mut snapshot = Vec::from(1u64.to_be_bytes());
        snapshot.extend_from_slice(&23u16.to_be_bytes());
        snapshot.extend_from_slice(&80u16.to_be_bytes());
        renderer.apply_frame(BUFFER_SNAPSHOT, &snapshot).unwrap();
        renderer.set_layout(LayoutSnapshot {
            tree: crate::layout::WindowNode::Window { id: 1, buffer: 1 },
            focused_window: 1,
            active_buffer: Some(1),
            buffers: vec![1],
            buffer_names: HashMap::from([(1, "1".into())]),
        });

        let mut output = Vec::new();
        renderer.draw(&mut output, 80, 24).unwrap();
        assert!(output.len() < 4096, "redraw wrote {} bytes", output.len());
    }

    #[test]
    fn identified_output_requires_an_initial_snapshot() {
        let mut renderer = Renderer::default();
        let mut output = Vec::from(8u64.to_be_bytes());
        output.extend_from_slice(b"x");
        assert_eq!(
            renderer
                .apply_frame(BUFFER_OUTPUT, &output)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }
    #[test]
    fn buffer_line_uses_focused_shown_hidden_and_fill_styles() {
        let mut config = crate::config::Config::default();
        config.buffer_line.focused_foreground = "#ff0000".into();
        config.buffer_line.focused_background = "#010203".into();
        config.buffer_line.shown_foreground = "#00ff00".into();
        config.buffer_line.shown_background = "#040506".into();
        config.buffer_line.hidden_foreground = "#0000ff".into();
        config.buffer_line.hidden_background = "#070809".into();
        config.buffer_line.background = "#0a0b0c".into();
        config.divider.foreground = "#abcdef".into();
        config.divider.vertical = "|".into();
        config.margin.foreground = "#123456".into();
        config.margin.background = "#654321".into();
        config.margin.fill = "~".into();
        let mut renderer = Renderer::with_config(config);
        renderer.set_layout(LayoutSnapshot {
            tree: crate::layout::WindowNode::Split {
                axis: crate::layout::Axis::Vertical,
                ratio: 50,
                first: Box::new(crate::layout::WindowNode::Window { id: 1, buffer: 1 }),
                second: Box::new(crate::layout::WindowNode::Window { id: 2, buffer: 2 }),
            },
            focused_window: 1,
            active_buffer: Some(1),
            buffers: vec![1, 2, 3],
            buffer_names: HashMap::from([(1, "a".into()), (2, "b".into()), (3, "c".into())]),
        });
        let mut output = Vec::new();
        renderer.draw(&mut output, 20, 3).unwrap();
        let mut terminal = Parser::new(
            std::num::NonZeroU16::new(3).unwrap(),
            std::num::NonZeroU16::new(20).unwrap(),
            0,
        );
        terminal.process(&output);
        let screen = terminal.screen();
        if std::env::var("NO_COLOR").is_ok_and(|value| !value.is_empty()) {
            for cell in [
                screen.cell(0, 0).unwrap(),
                screen.cell(0, 5).unwrap(),
                screen.cell(0, 8).unwrap(),
                screen.cell(1, 0).unwrap(),
            ] {
                assert_eq!(cell.fgcolor(), VtColor::Default);
                assert_eq!(cell.bgcolor(), VtColor::Default);
            }
        } else {
            assert_eq!(
                screen.cell(0, 0).unwrap().fgcolor(),
                VtColor::Rgb(255, 0, 0)
            );
            assert_eq!(screen.cell(0, 0).unwrap().bgcolor(), VtColor::Rgb(1, 2, 3));
            assert_eq!(
                screen.cell(0, 5).unwrap().fgcolor(),
                VtColor::Rgb(0, 255, 0)
            );
            assert_eq!(screen.cell(0, 5).unwrap().bgcolor(), VtColor::Rgb(4, 5, 6));
            assert_eq!(
                screen.cell(0, 8).unwrap().fgcolor(),
                VtColor::Rgb(0, 0, 255)
            );
            assert_eq!(screen.cell(0, 8).unwrap().bgcolor(), VtColor::Rgb(7, 8, 9));
            assert_eq!(
                screen.cell(0, 12).unwrap().bgcolor(),
                VtColor::Rgb(10, 11, 12)
            );
            assert_eq!(
                screen.cell(1, 0).unwrap().fgcolor(),
                VtColor::Rgb(18, 52, 86)
            );
            assert_eq!(
                screen.cell(1, 0).unwrap().bgcolor(),
                VtColor::Rgb(101, 67, 33)
            );
            assert_eq!(
                screen.cell(1, 9).unwrap().fgcolor(),
                VtColor::Rgb(171, 205, 239)
            );
        }
        assert!(screen.cell(0, 0).unwrap().bold());
        assert!(screen.cell(0, 5).unwrap().bold());
        assert!(screen.cell(0, 8).unwrap().bold());
        assert_eq!(screen.cell(1, 9).unwrap().contents(), "|");
    }

    #[test]
    fn buffer_line_bold_setting_controls_all_buffer_states() {
        let snapshot = LayoutSnapshot {
            tree: crate::layout::WindowNode::Split {
                axis: crate::layout::Axis::Vertical,
                ratio: 50,
                first: Box::new(crate::layout::WindowNode::Window { id: 1, buffer: 1 }),
                second: Box::new(crate::layout::WindowNode::Window { id: 2, buffer: 2 }),
            },
            focused_window: 1,
            active_buffer: Some(1),
            buffers: vec![1, 2, 3],
            buffer_names: HashMap::from([(1, "a".into()), (2, "b".into()), (3, "c".into())]),
        };
        for bold in [false, true] {
            let config: crate::config::Config =
                toml::from_str(&format!("[buffer_line]\nbold = {bold}\n")).unwrap();
            let mut output = b"\x1b[1m".to_vec();
            draw_buffer_line(
                &mut output,
                &snapshot,
                20,
                &snapshot.geometry(20, 3),
                &config,
            )
            .unwrap();
            let mut terminal = Parser::new(
                std::num::NonZeroU16::new(3).unwrap(),
                std::num::NonZeroU16::new(20).unwrap(),
                0,
            );
            terminal.process(&output);
            for col in [1, 5, 8] {
                assert_eq!(terminal.screen().cell(0, col).unwrap().bold(), bold);
            }
        }
    }

    #[test]
    fn narrow_buffer_line_does_not_wrap_separator_into_panes() {
        let snapshot = LayoutSnapshot {
            tree: crate::layout::WindowNode::Window { id: 1, buffer: 1 },
            focused_window: 1,
            active_buffer: Some(1),
            buffers: vec![1, 2],
            buffer_names: HashMap::from([(1, "a".into()), (2, "b".into())]),
        };
        let geometry = snapshot.geometry(3, 1);
        let mut output = Vec::new();
        draw_buffer_line(
            &mut output,
            &snapshot,
            3,
            &geometry,
            &crate::config::Config::default(),
        )
        .unwrap();
        let mut terminal = Parser::new(
            std::num::NonZeroU16::new(2).unwrap(),
            std::num::NonZeroU16::new(3).unwrap(),
            0,
        );
        terminal.process(&output);
        assert_eq!(terminal.screen().cell(1, 0).unwrap().contents(), "");
    }

    #[test]
    fn absolute_visual_ranges_clip_at_viewport_and_keep_their_shape() {
        let mut view = VimView {
            buffer: 1,
            mode: "visual".into(),
            cursor_row: 8,
            cursor_col: 4,
            selection: Some((4, 2, 8, 4)),
            yank_selection: None,
            yank_mode: None,
            selection_mode: None,
            matches: Vec::new(),
            active_match: None,
            prompt: None,
            app_control: false,
            history_len: 12,
            scrollback: 6,
            operator_pending: None,
            operator_count_started: false,
        };

        // The local viewport is absolute rows 6..8: a selection beginning in
        // scrollback still covers the visible part, not a saturated row zero.
        assert!(selection_contains(&view, 6, 0));
        assert!(selection_contains(&view, 8, 4));
        assert!(!selection_contains(&view, 8, 5));
        assert!(!selection_contains(&view, 9, 3));
        view.selection = Some((1, 1, 3, 4));
        assert!(!selection_contains(&view, 6, 0));

        view.matches = vec![vec![(6, 2), (8, 4)]];
        assert!(match_contains(&view, 6, 2));
        assert!(match_contains(&view, 8, 4));
        assert!(!match_contains(&view, 0, 2));
        assert!(!match_contains(&view, 9, 4));
        view.mode = "visual-line".into();
        view.selection = Some((6, 2, 8, 4));
        assert!(selection_contains(&view, 7, 0));
        view.mode = "visual-block".into();
        assert!(selection_contains(&view, 7, 3));
        view.mode = "search".into();
        view.selection_mode = Some("visual-block".into());
        assert!(selection_contains(&view, 7, 3));
        assert!(!selection_contains(&view, 7, 1));
        assert!(!selection_contains(&view, 7, 1));
    }

    #[test]
    fn yank_flash_selection_uses_original_visual_shape() {
        let view = VimView {
            buffer: 1,
            mode: "insert".into(),
            cursor_row: 0,
            cursor_col: 0,
            selection: None,
            yank_selection: Some((4, 2, 6, 4)),
            yank_mode: Some("visual-block".into()),
            selection_mode: None,
            matches: Vec::new(),
            active_match: None,
            prompt: None,
            app_control: false,
            history_len: 8,
            scrollback: 4,
            operator_pending: None,
            operator_count_started: false,
        };
        assert!(yank_selection_contains(&view, 5, 3));
        assert!(!yank_selection_contains(&view, 5, 1));
        assert!(!yank_selection_contains(&view, 7, 3));
    }

    #[test]
    fn search_and_visual_highlights_use_configured_colors_and_selection_priority() {
        let config = crate::config::Config::default();
        let colors = ResolvedVimColors::new(&config);
        let mut style = CellStyle {
            foreground: Color::White,
            background: Color::Black,
            bold: false,
            dim: false,
            italic: false,
            underline: false,
            inverse: true,
        };

        apply_match_style(&mut style, false, false, true, false, colors);
        assert_eq!(
            style.foreground,
            Color::Rgb {
                r: 0x1a,
                g: 0x1b,
                b: 0x26
            }
        );
        assert_eq!(
            style.background,
            Color::Rgb {
                r: 0x7a,
                g: 0xa2,
                b: 0xf7
            }
        );
        assert!(!style.inverse);

        apply_match_style(&mut style, false, false, true, true, colors);
        assert_eq!(
            style.foreground,
            Color::Rgb {
                r: 0x1a,
                g: 0x1b,
                b: 0x26
            }
        );
        assert_eq!(
            style.background,
            Color::Rgb {
                r: 0xff,
                g: 0x9e,
                b: 0x64
            }
        );

        apply_match_style(&mut style, true, false, true, true, colors);
        assert_eq!(
            style.foreground,
            Color::Rgb {
                r: 0xc0,
                g: 0xca,
                b: 0xf5
            }
        );
        assert_eq!(
            style.background,
            Color::Rgb {
                r: 0x28,
                g: 0x34,
                b: 0x57
            }
        );
        assert!(!style.inverse);
        apply_match_style(&mut style, true, true, true, true, colors);
        assert_eq!(
            style.foreground,
            Color::Rgb {
                r: 0x1a,
                g: 0x1b,
                b: 0x26
            }
        );
        assert_eq!(
            style.background,
            Color::Rgb {
                r: 0xff,
                g: 0x9e,
                b: 0x64
            }
        );
    }

    #[test]
    fn configured_vim_colors_style_rendered_cells_with_expected_priority() {
        let mut config = crate::config::Config::default();
        config.vim_colors.search_match_foreground = "#010203".into();
        config.vim_colors.search_match_background = "#040506".into();
        config.vim_colors.search_current_foreground = "#070809".into();
        config.vim_colors.search_current_background = "#0a0b0c".into();
        config.vim_colors.selection_foreground = "#0d0e0f".into();
        config.vim_colors.selection_background = "#101112".into();
        config.vim_colors.yank_foreground = "#131415".into();
        config.vim_colors.yank_background = "#161718".into();
        let vim_colors = ResolvedVimColors::new(&config);
        let mut parser = Parser::new(
            std::num::NonZeroU16::new(1).unwrap(),
            std::num::NonZeroU16::new(6).unwrap(),
            0,
        );
        parser.process(b"abcdef");
        let view = VimView {
            buffer: 1,
            mode: "visual".into(),
            cursor_row: 0,
            cursor_col: 3,
            selection: Some((0, 2, 0, 3)),
            yank_selection: Some((0, 3, 0, 4)),
            yank_mode: Some("visual".into()),
            selection_mode: None,
            matches: vec![vec![(0, 1)], vec![(0, 0), (0, 2), (0, 4)]],
            active_match: Some(0),
            prompt: None,
            app_control: false,
            history_len: 0,
            scrollback: 0,
            operator_pending: None,
            operator_count_started: false,
        };
        let mut output = Vec::new();
        draw_pane(
            &mut output,
            PaneRect {
                x: 0,
                y: 0,
                width: 6,
                height: 1,
            },
            Some(&parser),
            Some(&view),
            0,
            &config,
            vim_colors,
        )
        .unwrap();
        let mut terminal = Parser::new(
            std::num::NonZeroU16::new(1).unwrap(),
            std::num::NonZeroU16::new(6).unwrap(),
            0,
        );
        terminal.process(&output);
        if !std::env::var("NO_COLOR").is_ok_and(|value| !value.is_empty()) {
            let screen = terminal.screen();
            assert_eq!(screen.cell(0, 0).unwrap().fgcolor(), VtColor::Rgb(1, 2, 3));
            assert_eq!(screen.cell(0, 0).unwrap().bgcolor(), VtColor::Rgb(4, 5, 6));
            assert_eq!(screen.cell(0, 1).unwrap().fgcolor(), VtColor::Rgb(7, 8, 9));
            assert_eq!(
                screen.cell(0, 1).unwrap().bgcolor(),
                VtColor::Rgb(10, 11, 12)
            );
            assert_eq!(
                screen.cell(0, 2).unwrap().fgcolor(),
                VtColor::Rgb(13, 14, 15)
            );
            assert_eq!(
                screen.cell(0, 2).unwrap().bgcolor(),
                VtColor::Rgb(16, 17, 18)
            );
            assert_eq!(
                screen.cell(0, 3).unwrap().bgcolor(),
                VtColor::Rgb(22, 23, 24)
            );
            assert_eq!(
                screen.cell(0, 4).unwrap().bgcolor(),
                VtColor::Rgb(22, 23, 24)
            );
            assert_eq!(screen.cell(0, 5).unwrap().fgcolor(), VtColor::Default);
        }
    }

    #[test]
    fn search_prompt_colors_are_independent_from_rename_prompt_colors() {
        let config: crate::config::Config = toml::from_str(
            "[buffer_line]\nrename_foreground = \"#010203\"\nrename_background = \"#040506\"\n\
             [vim_colors]\nsearch_prompt_foreground = \"#070809\"\nsearch_prompt_background = \"#0a0b0c\"\n",
        )
        .unwrap();
        let mut search_output = Vec::new();
        draw_search_prompt_line(
            &mut search_output,
            "/needle",
            10,
            &config,
            ResolvedVimColors::new(&config),
        )
        .unwrap();
        let mut rename_output = Vec::new();
        draw_header_line(&mut rename_output, "Rename:", 10, &config).unwrap();
        let mut search_terminal = Parser::new(
            std::num::NonZeroU16::new(3).unwrap(),
            std::num::NonZeroU16::new(10).unwrap(),
            0,
        );
        search_terminal.process(&search_output);
        let mut rename_terminal = Parser::new(
            std::num::NonZeroU16::new(3).unwrap(),
            std::num::NonZeroU16::new(10).unwrap(),
            0,
        );
        rename_terminal.process(&rename_output);
        if !std::env::var("NO_COLOR").is_ok_and(|value| !value.is_empty()) {
            let search_cell = search_terminal.screen().cell(0, 0).unwrap();
            assert_eq!(search_cell.fgcolor(), VtColor::Rgb(7, 8, 9));
            assert_eq!(search_cell.bgcolor(), VtColor::Rgb(10, 11, 12));
            let rename_cell = rename_terminal.screen().cell(0, 0).unwrap();
            assert_eq!(rename_cell.fgcolor(), VtColor::Rgb(1, 2, 3));
            assert_eq!(rename_cell.bgcolor(), VtColor::Rgb(4, 5, 6));
        }
        let mut fallback_config = config.clone();
        fallback_config.vim_colors.search_prompt_foreground = None;
        fallback_config.vim_colors.search_prompt_background = None;
        let mut fallback_output = Vec::new();
        draw_search_prompt_line(
            &mut fallback_output,
            "/needle",
            10,
            &fallback_config,
            ResolvedVimColors::new(&fallback_config),
        )
        .unwrap();
        let mut fallback_terminal = Parser::new(
            std::num::NonZeroU16::new(3).unwrap(),
            std::num::NonZeroU16::new(10).unwrap(),
            0,
        );
        fallback_terminal.process(&fallback_output);
        let fallback_search_cell = fallback_terminal.screen().cell(0, 0).unwrap();
        if !std::env::var("NO_COLOR").is_ok_and(|value| !value.is_empty()) {
            assert_eq!(fallback_search_cell.fgcolor(), VtColor::Rgb(1, 2, 3));
            assert_eq!(fallback_search_cell.bgcolor(), VtColor::Rgb(4, 5, 6));
        }
    }
    #[test]
    fn yank_flash_history_view_keeps_live_bracketed_paste_enabled() {
        assert!(bracketed_paste_enabled(false, true, true, true));
        assert!(!bracketed_paste_enabled(false, true, false, true));
        assert!(!bracketed_paste_enabled(true, true, true, true));
        assert!(!bracketed_paste_enabled(false, true, true, false));
    }

    #[test]
    fn incremental_pane_redraw_writes_changed_cells_and_clears_wide_tail() {
        let mut parser = Parser::new(
            std::num::NonZeroU16::new(1).unwrap(),
            std::num::NonZeroU16::new(4).unwrap(),
            0,
        );
        parser.process("界".as_bytes());
        let origin = (0, 0, 4, 1);
        let prior = RenderedPane {
            window: 1,
            buffer: 1,
            rect: PaneRect {
                x: 0,
                y: 0,
                width: 4,
                height: 1,
            },
            origin,
            terminal: (4, 3),
            cells: rendered_cells(parser.screen(), origin),
        };

        parser.process(b"\x1b[1;1Ha");
        let current = rendered_cells(parser.screen(), origin);
        let mut output = Vec::new();
        draw_pane_diff(&mut output, prior.rect, origin, &current, &prior).unwrap();
        assert!(output.windows(1).any(|bytes| bytes == b"a"));
        assert!(output.windows(1).any(|bytes| bytes == b" "));
        assert!(output.len() < 100);
    }

    #[test]
    fn ordinary_insert_output_does_not_redraw_the_entire_pane() {
        let mut renderer = Renderer::default();
        let mut snapshot = Vec::from(1u64.to_be_bytes());
        snapshot.extend_from_slice(&24u16.to_be_bytes());
        snapshot.extend_from_slice(&80u16.to_be_bytes());
        renderer.apply_frame(BUFFER_SNAPSHOT, &snapshot).unwrap();
        renderer.set_layout(LayoutSnapshot {
            tree: crate::layout::WindowNode::Window { id: 1, buffer: 1 },
            focused_window: 1,
            active_buffer: Some(1),
            buffers: vec![1],
            buffer_names: HashMap::new(),
        });
        let state = serde_json::json!({
            "buffer": 1,
            "mode": "insert",
            "cursor_row": 0,
            "cursor_col": 0,
            "selection": null,
            "matches": [],
            "active_match": null,
            "prompt": null,
            "app_control": false,
            "history_len": 0,
            "scrollback": 0
        });
        renderer
            .apply_frame(VIM_STATE, &serde_json::to_vec(&state).unwrap())
            .unwrap();
        renderer.draw(&mut Vec::new(), 80, 24).unwrap();

        let mut output_frame = Vec::from(1u64.to_be_bytes());
        output_frame.push(b'x');
        renderer.apply_frame(BUFFER_OUTPUT, &output_frame).unwrap();
        let mut output = Vec::new();
        renderer.draw(&mut output, 80, 24).unwrap();
        assert!(output.windows(1).any(|bytes| bytes == b"x"));
        assert!(
            output.len() < 500,
            "incremental draw wrote {} bytes",
            output.len()
        );
    }
    #[test]
    fn split_views_diff_duplicates_and_layout_change_invalidates_cache() {
        let mut renderer = Renderer::default();
        let mut snapshot = Vec::from(1u64.to_be_bytes());
        snapshot.extend_from_slice(&24u16.to_be_bytes());
        snapshot.extend_from_slice(&80u16.to_be_bytes());
        renderer.apply_frame(BUFFER_SNAPSHOT, &snapshot).unwrap();
        let layout = |ratio| LayoutSnapshot {
            tree: crate::layout::WindowNode::Split {
                axis: crate::layout::Axis::Vertical,
                ratio,
                first: Box::new(crate::layout::WindowNode::Window { id: 1, buffer: 1 }),
                second: Box::new(crate::layout::WindowNode::Window { id: 2, buffer: 1 }),
            },
            focused_window: 1,
            active_buffer: Some(1),
            buffers: vec![1],
            buffer_names: HashMap::new(),
        };
        renderer.set_layout(layout(50));
        let state = serde_json::json!({
            "buffer": 1,
            "mode": "insert",
            "cursor_row": 0,
            "cursor_col": 0,
            "selection": null,
            "matches": [],
            "active_match": null,
            "prompt": null,
            "app_control": false,
            "history_len": 0,
            "scrollback": 0
        });
        renderer
            .apply_frame(VIM_STATE, &serde_json::to_vec(&state).unwrap())
            .unwrap();
        renderer.draw(&mut Vec::new(), 80, 24).unwrap();

        let mut frame = Vec::from(1u64.to_be_bytes());
        frame.push(b'x');
        renderer.apply_frame(BUFFER_OUTPUT, &frame).unwrap();
        let mut output = Vec::new();
        renderer.draw(&mut output, 80, 24).unwrap();
        assert_eq!(output.iter().filter(|&&byte| byte == b'x').count(), 2);
        assert!(output.len() < 500);

        renderer.set_layout(layout(45));
        frame.truncate(8);
        frame.push(b'y');
        renderer.apply_frame(BUFFER_OUTPUT, &frame).unwrap();
        let mut output = Vec::new();
        renderer.draw(&mut output, 80, 24).unwrap();
        assert!(output.len() > 500, "layout change must force a full redraw");
    }
    #[test]
    fn split_redraw_skips_unchanged_other_buffer_pane() {
        let mut renderer = Renderer::default();
        for (id, initial) in [(1u64, b"".as_slice()), (2, b"z".as_slice())] {
            let mut snapshot = Vec::from(id.to_be_bytes());
            snapshot.extend_from_slice(&24u16.to_be_bytes());
            snapshot.extend_from_slice(&80u16.to_be_bytes());
            snapshot.extend_from_slice(initial);
            renderer.apply_frame(BUFFER_SNAPSHOT, &snapshot).unwrap();
        }
        renderer.set_layout(LayoutSnapshot {
            tree: crate::layout::WindowNode::Split {
                axis: crate::layout::Axis::Vertical,
                ratio: 50,
                first: Box::new(crate::layout::WindowNode::Window { id: 1, buffer: 1 }),
                second: Box::new(crate::layout::WindowNode::Window { id: 2, buffer: 2 }),
            },
            focused_window: 1,
            active_buffer: Some(1),
            buffers: vec![1, 2],
            buffer_names: HashMap::new(),
        });
        for id in [1u64, 2] {
            let state = serde_json::json!({
                "buffer": id,
                "mode": "insert",
                "cursor_row": 0,
                "cursor_col": 0,
                "selection": null,
                "matches": [],
                "active_match": null,
                "prompt": null,
                "app_control": false,
                "history_len": 0,
                "scrollback": 0
            });
            renderer
                .apply_frame(VIM_STATE, &serde_json::to_vec(&state).unwrap())
                .unwrap();
        }
        renderer.draw(&mut Vec::new(), 80, 24).unwrap();

        let mut frame = Vec::from(1u64.to_be_bytes());
        frame.push(b'x');
        renderer.apply_frame(BUFFER_OUTPUT, &frame).unwrap();
        let mut output = Vec::new();
        renderer.draw(&mut output, 80, 24).unwrap();
        assert!(output.windows(1).any(|byte| byte == b"x"));
        assert!(!output.windows(1).any(|byte| byte == b"z"));
        assert!(output.len() < 500);
    }
}
