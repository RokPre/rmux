use std::collections::HashMap;
use std::io::{self, Write};

use crossterm::cursor::{Hide, MoveTo, Show};
use crossterm::queue;
use crossterm::style::{
    Attribute, Color, Print, ResetColor, SetAttribute, SetBackgroundColor, SetForegroundColor,
};
use vt100::{Color as VtColor, Parser};

use crate::layout::{Divider, LayoutGeometry, LayoutSnapshot, PaneRect};

pub(crate) const BUFFER_SNAPSHOT: u8 = 4;
pub(crate) const BUFFER_OUTPUT: u8 = 5;

#[derive(Default)]
pub(crate) struct Renderer {
    snapshot: Option<LayoutSnapshot>,
    parsers: HashMap<u64, Parser>,
}

impl Renderer {
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

    pub(crate) fn draw(&self, output: &mut impl Write, cols: u16, rows: u16) -> io::Result<()> {
        let Some(snapshot) = self.snapshot.as_ref() else {
            return Ok(());
        };
        let geometry = snapshot.geometry(cols, rows);
        queue!(output, Hide)?;
        if rows >= 3 {
            draw_buffer_line(output, snapshot, cols)?;
        }
        for placement in &geometry.windows {
            draw_pane(output, placement.rect, self.parsers.get(&placement.buffer))?;
        }
        draw_dividers(output, &geometry)?;
        if let Some(placement) = geometry
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
        queue!(output, ResetColor, SetAttribute(Attribute::Reset))?;
        output.flush()
    }
}

fn draw_buffer_line(
    output: &mut impl Write,
    snapshot: &LayoutSnapshot,
    cols: u16,
) -> io::Result<()> {
    queue!(
        output,
        MoveTo(0, 0),
        SetBackgroundColor(Color::DarkGrey),
        SetForegroundColor(Color::White),
        SetAttribute(Attribute::Bold)
    )?;
    let mut line = String::new();
    for (index, buffer) in snapshot.buffers.iter().enumerate() {
        if index > 0 {
            line.push_str("  ");
        }
        if Some(*buffer) == snapshot.active_buffer {
            line.push('[');
        }
        line.push_str(&buffer.to_string());
        if Some(*buffer) == snapshot.active_buffer {
            line.push(']');
        }
    }
    let visible: String = line.chars().take(cols as usize).collect();
    let padding = (cols as usize).saturating_sub(line.len());
    queue!(
        output,
        Print(visible),
        Print(" ".repeat(padding)),
        ResetColor,
        SetAttribute(Attribute::Reset)
    )
}

fn draw_pane(output: &mut impl Write, rect: PaneRect, parser: Option<&Parser>) -> io::Result<()> {
    let Some(parser) = parser else {
        return draw_pattern(output, rect);
    };
    let screen = parser.screen();
    let size = screen.size();
    let (origin_x, origin_y, width, height) = centered_origin(rect, (size.0.get(), size.1.get()));
    draw_pattern_margins(output, rect, origin_x, origin_y, width, height)?;

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
    queue!(output, ResetColor, SetAttribute(Attribute::Reset))
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
    if previous.is_none_or(|previous| previous.foreground != style.foreground) {
        queue!(output, SetForegroundColor(style.foreground))?;
    }
    if previous.is_none_or(|previous| previous.background != style.background) {
        queue!(output, SetBackgroundColor(style.background))?;
    }
    if previous.is_none() {
        queue!(output, SetAttribute(Attribute::Reset))?;
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
    content_x: u16,
    content_y: u16,
    content_width: u16,
    content_height: u16,
) -> io::Result<()> {
    let right = rect.x + rect.width;
    let bottom = rect.y + rect.height;
    if content_y > rect.y {
        draw_pattern(
            output,
            PaneRect {
                height: content_y - rect.y,
                ..rect
            },
        )?;
    }
    let content_bottom = content_y + content_height;
    if content_bottom < bottom {
        draw_pattern(
            output,
            PaneRect {
                y: content_bottom,
                height: bottom - content_bottom,
                ..rect
            },
        )?;
    }
    if content_x > rect.x {
        draw_pattern(
            output,
            PaneRect {
                y: content_y,
                width: content_x - rect.x,
                height: content_height,
                ..rect
            },
        )?;
    }
    let content_right = content_x + content_width;
    if content_right < right {
        draw_pattern(
            output,
            PaneRect {
                x: content_right,
                y: content_y,
                width: right - content_right,
                height: content_height,
                ..rect
            },
        )?;
    }
    Ok(())
}

fn draw_pattern(output: &mut impl Write, rect: PaneRect) -> io::Result<()> {
    if rect.width == 0 || rect.height == 0 {
        return Ok(());
    }
    queue!(
        output,
        SetForegroundColor(Color::DarkGrey),
        SetBackgroundColor(Color::Reset),
        SetAttribute(Attribute::Reset)
    )?;
    for row in 0..rect.height {
        queue!(output, MoveTo(rect.x, rect.y + row))?;
        for col in 0..rect.width {
            queue!(output, Print(if (row + col) % 2 == 0 { "." } else { " " }))?;
        }
    }
    Ok(())
}

fn draw_dividers(output: &mut impl Write, geometry: &LayoutGeometry) -> io::Result<()> {
    queue!(
        output,
        SetForegroundColor(Color::DarkGrey),
        SetBackgroundColor(Color::Reset)
    )?;
    for divider in &geometry.dividers {
        match *divider {
            Divider::Vertical { x, top, height } => {
                for row in top..top + height {
                    queue!(output, MoveTo(x, row), Print("│"))?;
                }
            }
            Divider::Horizontal { y, left, width } => {
                for col in left..left + width {
                    queue!(output, MoveTo(col, y), Print("─"))?;
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
        });

        let mut output = Vec::new();
        renderer.draw(&mut output, 10, 6).unwrap();
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
}
