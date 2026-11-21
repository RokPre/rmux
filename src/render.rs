use std::collections::HashMap;
use std::io::{self, Write};

use crossterm::cursor::{Hide, MoveTo, Show};
use crossterm::queue;
use crossterm::style::{
    Attribute, Color, Print, SetAttribute, SetBackgroundColor, SetForegroundColor,
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
use vt100::{Color as VtColor, Parser};

use crate::layout::{Divider, LayoutGeometry, LayoutSnapshot, PaneRect};

pub(crate) const BUFFER_SNAPSHOT: u8 = 4;
pub(crate) const BUFFER_OUTPUT: u8 = 5;

pub(crate) struct Renderer {
    snapshot: Option<LayoutSnapshot>,
    parsers: HashMap<u64, Parser>,
    prompt: Option<(String, usize)>,
    bracketed_paste: Option<bool>,
    config: crate::config::Config,
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
            prompt: None,
            bracketed_paste: None,
            config,
        }
    }
    pub(crate) fn apply_frame(&mut self, kind: u8, payload: &[u8]) -> io::Result<()> {
        match kind {
            BUFFER_SNAPSHOT => {
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
                let parser = self
                    .parsers
                    .get_mut(&buffer)
                    .ok_or_else(|| invalid_frame("buffer output before snapshot"))?;
                parser.process(&payload[8..]);
            }
            _ => return Err(invalid_frame("invalid renderer frame")),
        }
        Ok(())
    }

    pub(crate) fn set_layout(&mut self, snapshot: LayoutSnapshot) {
        self.parsers
            .retain(|buffer, _| snapshot.buffers.contains(buffer));
        self.snapshot = Some(snapshot);
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

    pub(crate) fn has_buffer(&self, buffer: u64) -> bool {
        self.snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.buffers.contains(&buffer))
    }

    pub(crate) fn set_prompt(&mut self, prompt: Option<(String, usize)>) {
        self.prompt = prompt;
    }

    pub(crate) fn draw(&mut self, output: &mut impl Write, cols: u16, rows: u16) -> io::Result<()> {
        let Some(snapshot) = self.snapshot.as_ref() else {
            return Ok(());
        };
        let bracketed_paste = self.prompt.is_none()
            && snapshot.active_buffer
                .and_then(|buffer| self.parsers.get(&buffer))
                .is_some_and(|parser| parser.screen().bracketed_paste());
        if self.bracketed_paste != Some(bracketed_paste) {
            if bracketed_paste {
                queue!(output, crossterm::event::EnableBracketedPaste)?;
            } else {
                queue!(output, crossterm::event::DisableBracketedPaste)?;
            }
            self.bracketed_paste = Some(bracketed_paste);
        }
        let geometry = snapshot.geometry(cols, rows);
        queue!(output, Hide)?;
        if rows >= 3 {
            if let Some((prompt, _)) = self.prompt.as_ref() {
                draw_header_line(output, prompt, cols, &self.config)?;
            } else {
                draw_buffer_line(output, snapshot, cols, &geometry, &self.config)?;
            }
        }
        for placement in &geometry.windows {
            draw_pane(
                output,
                placement.rect,
                self.parsers.get(&placement.buffer),
                &self.config,
            )?;
        }
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
        } else if let Some(placement) = geometry
            .windows
            .iter()
            .find(|window| window.id == snapshot.focused_window)
        {
            if let Some(parser) = self.parsers.get(&placement.buffer) {
                let screen = parser.screen();
                let (cursor_row, cursor_col) = screen.cursor_position();
                let size = screen.size();
                let (left, top, width, height) =
                    centered_origin(placement.rect, (size.0.get(), size.1.get()));
                if !screen.hide_cursor() && cursor_row < height && cursor_col < width {
                    queue!(output, MoveTo(left + cursor_col, top + cursor_row), Show)?;
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
    let intensity = if config.buffer_line.bold {
        Attribute::Bold
    } else {
        Attribute::NormalIntensity
    };
    queue!(
        output,
        MoveTo(0, 0),
        SetBackgroundColor(crate::config::parse_color(background).unwrap()),
        SetForegroundColor(crate::config::parse_color(foreground).unwrap()),
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
    config: &crate::config::Config,
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
            let style = CellStyle {
                foreground: convert_color(cell.fgcolor()),
                background: convert_color(cell.bgcolor()),
                bold: cell.bold(),
                dim: cell.dim(),
                italic: cell.italic(),
                underline: cell.underline(),
                inverse: cell.inverse(),
            };
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
}
