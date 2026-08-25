use crossterm::cursor::{Hide, MoveDown, MoveToColumn, MoveUp, Show};
use crossterm::style::{Attribute, Print, SetAttribute, SetBackgroundColor, SetForegroundColor};
use crossterm::terminal::{Clear, ClearType as CrosstermClearType};
use crossterm::{execute, queue};
use ratatui::backend::{Backend, ClearType, IntoCrossterm, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Size};
use ratatui::style::{Color, Modifier};
use std::io::{self, Write};

/// A small Ratatui backend whose coordinates are relative to the cursor row at
/// startup. Unlike Ratatui's general inline viewport, it never asks the terminal
/// to report an absolute cursor position, so it also works in headless PTYs and
/// terminal layers that do not answer device-status reports.
#[derive(Debug)]
pub struct InlineBackend<W: Write> {
    writer: W,
    size: Size,
    cursor: Position,
}

impl<W: Write> InlineBackend<W> {
    pub fn new(mut writer: W, width: u16, height: u16) -> io::Result<Self> {
        reserve_rows(&mut writer, height)?;
        Ok(Self {
            writer,
            size: Size::new(width, height),
            cursor: Position::ORIGIN,
        })
    }

    pub fn resize_viewport(&mut self, width: u16, height: u16) -> io::Result<()> {
        self.move_to(Position::ORIGIN)?;
        execute!(self.writer, Clear(CrosstermClearType::FromCursorDown))?;
        reserve_rows(&mut self.writer, height)?;
        self.size = Size::new(width, height);
        self.cursor = Position::ORIGIN;
        Ok(())
    }

    pub fn finish(&mut self) -> io::Result<()> {
        self.clear_viewport()
    }

    fn move_to(&mut self, position: Position) -> io::Result<()> {
        match position.y.cmp(&self.cursor.y) {
            std::cmp::Ordering::Greater => {
                queue!(self.writer, MoveDown(position.y - self.cursor.y))?;
            }
            std::cmp::Ordering::Less => {
                queue!(self.writer, MoveUp(self.cursor.y - position.y))?;
            }
            std::cmp::Ordering::Equal => {}
        }
        queue!(self.writer, MoveToColumn(position.x))?;
        self.cursor = position;
        Ok(())
    }

    fn clear_viewport(&mut self) -> io::Result<()> {
        self.move_to(Position::ORIGIN)?;
        execute!(self.writer, Clear(CrosstermClearType::FromCursorDown))?;
        self.cursor = Position::ORIGIN;
        Ok(())
    }
}

impl<W: Write> Backend for InlineBackend<W> {
    type Error = io::Error;

    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        let mut last_position: Option<Position> = None;
        let mut last_style = None;

        for (x, y, cell) in content {
            let position = Position::new(x, y);
            if !matches!(last_position, Some(previous) if position == Position::new(previous.x + 1, previous.y))
            {
                self.move_to(position)?;
            }

            let style = (cell.fg, cell.bg, cell.modifier);
            if last_style != Some(style) {
                queue!(
                    self.writer,
                    SetAttribute(Attribute::Reset),
                    SetForegroundColor(cell.fg.into_crossterm()),
                    SetBackgroundColor(cell.bg.into_crossterm())
                )?;
                queue_modifiers(&mut self.writer, cell.modifier)?;
                last_style = Some(style);
            }

            queue!(self.writer, Print(cell.symbol()))?;
            self.cursor = Position::new(position.x.saturating_add(1), position.y);
            last_position = Some(position);
        }

        queue!(
            self.writer,
            SetAttribute(Attribute::Reset),
            SetForegroundColor(Color::Reset.into_crossterm()),
            SetBackgroundColor(Color::Reset.into_crossterm())
        )?;
        Ok(())
    }

    fn hide_cursor(&mut self) -> io::Result<()> {
        execute!(self.writer, Hide)
    }

    fn show_cursor(&mut self) -> io::Result<()> {
        execute!(self.writer, Show)
    }

    fn get_cursor_position(&mut self) -> io::Result<Position> {
        Ok(self.cursor)
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        self.move_to(position.into())
    }

    fn clear(&mut self) -> io::Result<()> {
        self.clear_viewport()
    }

    fn clear_region(&mut self, _clear_type: ClearType) -> io::Result<()> {
        // Clearing outside this viewport would erase the shell output that makes
        // the UI inline. A full viewport clear is safe for every Ratatui clear
        // request used by this application.
        self.clear_viewport()
    }

    fn size(&self) -> io::Result<Size> {
        Ok(self.size)
    }

    fn window_size(&mut self) -> io::Result<WindowSize> {
        Ok(WindowSize {
            columns_rows: self.size,
            pixels: Size::ZERO,
        })
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

fn reserve_rows(writer: &mut impl Write, height: u16) -> io::Result<()> {
    execute!(writer, MoveToColumn(0))?;
    for _ in 1..height {
        queue!(writer, Print("\n"))?;
    }
    execute!(
        writer,
        MoveUp(height.saturating_sub(1)),
        MoveToColumn(0),
        Hide
    )
}

fn queue_modifiers(writer: &mut impl Write, modifiers: Modifier) -> io::Result<()> {
    const MODIFIERS: &[(Modifier, Attribute)] = &[
        (Modifier::BOLD, Attribute::Bold),
        (Modifier::DIM, Attribute::Dim),
        (Modifier::ITALIC, Attribute::Italic),
        (Modifier::UNDERLINED, Attribute::Underlined),
        (Modifier::SLOW_BLINK, Attribute::SlowBlink),
        (Modifier::RAPID_BLINK, Attribute::RapidBlink),
        (Modifier::REVERSED, Attribute::Reverse),
        (Modifier::HIDDEN, Attribute::Hidden),
        (Modifier::CROSSED_OUT, Attribute::CrossedOut),
    ];

    for (modifier, attribute) in MODIFIERS {
        if modifiers.contains(*modifier) {
            queue!(writer, SetAttribute(*attribute))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;

    #[test]
    fn inline_backend_does_not_query_or_save_absolute_cursor_position() {
        let mut output = Vec::new();
        {
            let backend = InlineBackend::new(&mut output, 20, 4).unwrap();
            let mut terminal = Terminal::new(backend).unwrap();
            terminal
                .draw(|frame| frame.render_widget("hello", frame.area()))
                .unwrap();
            terminal.backend_mut().finish().unwrap();
        }

        assert!(!output.windows(4).any(|window| window == b"\x1b[6n"));
        assert!(!output.windows(2).any(|window| window == b"\x1b7"));
        assert!(!output.windows(2).any(|window| window == b"\x1b8"));
        assert!(output.windows(4).any(|window| window == b"\x1b[3A"));
    }
}
