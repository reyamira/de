use crossterm::cursor::{Hide, MoveDown, MoveToColumn, MoveUp, Show};
use crossterm::style::{Attribute, Print, SetAttribute, SetBackgroundColor, SetForegroundColor};
use crossterm::terminal::{Clear, ClearType as CrosstermClearType};
use crossterm::{execute, queue};
use ratatui::backend::{Backend, ClearType, IntoCrossterm, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Rect, Size};
use ratatui::style::{Color, Modifier};
use std::io::{self, Write};
use std::sync::{
    Arc, LazyLock,
    atomic::{AtomicU32, Ordering},
};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug)]
struct DisplayedImage {
    id: u32,
    encoded: Arc<str>,
    area: Option<Rect>,
    offset: usize,
}

fn next_image_id() -> u32 {
    static NEXT: LazyLock<AtomicU32> = LazyLock::new(|| {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let seed = (nanos as u32) ^ ((nanos >> 32) as u32) ^ std::process::id().rotate_left(13);
        AtomicU32::new(seed)
    });
    loop {
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        if id != 0 {
            return id;
        }
    }
}

fn fitted_area(area: Rect, width: u32, height: u32, cell_width: u32, cell_height: u32) -> Rect {
    if area.is_empty() || width == 0 || height == 0 {
        return Rect::default();
    }
    let available_width = u64::from(area.width) * u64::from(cell_width);
    let available_height = u64::from(area.height) * u64::from(cell_height);
    let (columns, rows) =
        if available_width * u64::from(height) <= available_height * u64::from(width) {
            let rows = (available_width * u64::from(height))
                .div_ceil(u64::from(width) * u64::from(cell_height));
            (area.width, rows.clamp(1, u64::from(area.height)) as u16)
        } else {
            let columns = (available_height * u64::from(width))
                .div_ceil(u64::from(height) * u64::from(cell_width));
            (columns.clamp(1, u64::from(area.width)) as u16, area.height)
        };
    Rect::new(
        area.x + (area.width - columns) / 2,
        area.y + (area.height - rows) / 2,
        columns,
        rows,
    )
}

fn cell_size() -> (u32, u32) {
    match crossterm::terminal::window_size() {
        Ok(size)
            if size.columns > 0
                && size.rows > 0
                && size.width >= size.columns
                && size.height >= size.rows =>
        {
            (
                u32::from(size.width / size.columns),
                u32::from(size.height / size.rows),
            )
        }
        // Most terminal cells are approximately twice as tall as they are wide.
        _ => (1, 2),
    }
}

/// A small Ratatui backend whose coordinates are relative to the cursor row at
/// startup. Unlike Ratatui's general inline viewport, it never asks the terminal
/// to report an absolute cursor position, so it also works in headless PTYs and
/// terminal layers that do not answer device-status reports.
#[derive(Debug)]
pub struct InlineBackend<W: Write> {
    writer: W,
    size: Size,
    cursor: Position,
    image: Option<DisplayedImage>,
}

impl<W: Write> InlineBackend<W> {
    pub fn new(mut writer: W, width: u16, height: u16) -> io::Result<Self> {
        reserve_rows(&mut writer, height)?;
        Ok(Self {
            writer,
            size: Size::new(width, height),
            cursor: Position::ORIGIN,
            image: None,
        })
    }

    pub fn resize_viewport(&mut self, width: u16, height: u16) -> io::Result<()> {
        self.clear_placement()?;
        self.move_to(Position::ORIGIN)?;
        execute!(self.writer, Clear(CrosstermClearType::FromCursorDown))?;
        reserve_rows(&mut self.writer, height)?;
        self.size = Size::new(width, height);
        self.cursor = Position::ORIGIN;
        Ok(())
    }

    pub fn finish(&mut self) -> io::Result<()> {
        self.clear_image()?;
        self.clear_viewport()
    }

    /// Display an image in viewport-relative cells, or remove this backend's image.
    pub fn draw_image(
        &mut self,
        image: Option<(&crate::media::ImagePreview, Rect)>,
    ) -> io::Result<()> {
        let (cell_width, cell_height) = cell_size();
        let image = image.and_then(|(image, area)| {
            let area = fitted_area(area, image.width, image.height, cell_width, cell_height);
            (!area.is_empty()).then_some((image, area))
        });
        let same = match (&self.image, image) {
            (Some(current), Some((image, _))) => Arc::ptr_eq(&current.encoded, &image.encoded),
            (None, None) => return Ok(()),
            _ => false,
        };
        if !same {
            self.clear_image()?;
            if let Some((image, _)) = image {
                self.image = Some(DisplayedImage {
                    id: next_image_id(),
                    encoded: Arc::clone(&image.encoded),
                    area: None,
                    offset: 0,
                });
            }
        }
        let Some((_, area)) = image else {
            return self.writer.flush();
        };
        // Kitty permits at most 4096 encoded bytes per escape sequence. Limit
        // each call to 64 KiB so the event loop can read input between calls.
        if self.image_pending() {
            let current = self.image.as_mut().unwrap();
            let end = (current.offset + 64 * 1024).min(current.encoded.len());
            while current.offset < end {
                let chunk_end = (current.offset + 4096).min(end);
                let more = chunk_end < current.encoded.len();
                if current.offset == 0 {
                    write!(
                        self.writer,
                        "\x1b_Ga=t,f=100,i={},q=2,m={};",
                        current.id,
                        u8::from(more)
                    )?;
                } else {
                    write!(self.writer, "\x1b_Gm={},q=2;", u8::from(more))?;
                }
                self.writer
                    .write_all(&current.encoded.as_bytes()[current.offset..chunk_end])?;
                self.writer.write_all(b"\x1b\\")?;
                current.offset = chunk_end;
            }
        }
        if !self.image_pending() && self.image.as_ref().unwrap().area != Some(area) {
            self.clear_placement()?;
            let id = self.image.as_ref().unwrap().id;
            let cursor = self.cursor;
            self.move_to(Position::new(area.x, area.y))?;
            write!(
                self.writer,
                "\x1b_Ga=p,i={id},c={},r={},C=1,q=2\x1b\\",
                area.width, area.height
            )?;
            self.move_to(cursor)?;
            self.image.as_mut().unwrap().area = Some(area);
        }
        self.writer.flush()
    }

    pub fn image_pending(&self) -> bool {
        self.image
            .as_ref()
            .is_some_and(|image| image.offset < image.encoded.len())
    }

    fn clear_placement(&mut self) -> io::Result<()> {
        if let Some(image) = self.image.as_mut()
            && image.area.take().is_some()
        {
            write!(self.writer, "\x1b_Ga=d,d=i,i={},q=2\x1b\\", image.id)?;
        }
        Ok(())
    }

    fn clear_image(&mut self) -> io::Result<()> {
        if let Some(image) = self.image.take() {
            // Uppercase I deletes only this image and its placements, not other clients' images.
            write!(self.writer, "\x1b_Ga=d,d=I,i={},q=2\x1b\\", image.id)?;
        }
        Ok(())
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
        self.clear_placement()?;
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
    fn fits_image_within_cells_without_distortion() {
        assert_eq!(
            fitted_area(Rect::new(2, 3, 20, 10), 100, 100, 1, 2),
            Rect::new(2, 3, 20, 10)
        );
        assert_eq!(
            fitted_area(Rect::new(0, 0, 20, 10), 400, 100, 1, 2),
            Rect::new(0, 3, 20, 3)
        );
        assert_eq!(
            fitted_area(Rect::new(0, 0, 0, 10), 1, 1, 1, 2),
            Rect::default()
        );
    }

    #[test]
    fn image_protocol_tracks_cursor_and_cleans_up_only_owned_images() {
        let mut backend = InlineBackend::new(Vec::new(), 40, 12).unwrap();
        let first = crate::media::ImagePreview {
            width: 1,
            height: 1,
            encoded: Arc::from("A".repeat(5000)),
        };
        let second = crate::media::ImagePreview {
            width: 2,
            height: 1,
            encoded: Arc::from("AAAA"),
        };
        backend.move_to(Position::new(7, 4)).unwrap();
        let cursor = backend.cursor;
        backend
            .draw_image(Some((&first, Rect::new(3, 2, 10, 5))))
            .unwrap();
        assert_eq!(backend.cursor, cursor);
        let first_id = backend.image.as_ref().unwrap().id;
        let length = backend.writer.len();
        backend
            .draw_image(Some((&first, Rect::new(3, 2, 10, 5))))
            .unwrap();
        assert_eq!(backend.writer.len(), length);
        assert_eq!(backend.cursor, cursor);
        backend
            .draw_image(Some((&first, Rect::new(15, 2, 10, 5))))
            .unwrap();
        assert_eq!(backend.cursor, cursor);
        assert_eq!(backend.image.as_ref().unwrap().id, first_id);
        let placement = std::str::from_utf8(&backend.writer[length..]).unwrap();
        assert!(placement.contains(&format!("a=d,d=i,i={first_id},q=2")));
        assert!(placement.contains(&format!("a=p,i={first_id},")));
        assert!(!placement.contains("a=T"));
        backend
            .draw_image(Some((&second, Rect::new(3, 2, 10, 5))))
            .unwrap();
        assert_eq!(backend.cursor, cursor);
        let second_id = backend.image.as_ref().unwrap().id;
        assert_ne!(first_id, second_id);
        backend.resize_viewport(30, 8).unwrap();
        assert!(backend.image.as_ref().unwrap().area.is_none());
        let cursor = backend.cursor;
        let length = backend.writer.len();
        backend
            .draw_image(Some((&second, Rect::new(3, 2, 10, 5))))
            .unwrap();
        assert_eq!(backend.cursor, cursor);
        assert_eq!(backend.image.as_ref().unwrap().id, second_id);
        let placement = std::str::from_utf8(&backend.writer[length..]).unwrap();
        assert!(placement.contains(&format!("a=p,i={second_id},")));
        assert!(!placement.contains("a=T"));
        backend.clear().unwrap();
        assert!(backend.image.as_ref().unwrap().area.is_none());
        backend
            .draw_image(Some((&second, Rect::new(3, 2, 10, 5))))
            .unwrap();
        backend.finish().unwrap();
        let output = String::from_utf8(backend.writer).unwrap();
        assert!(output.contains("a=t,f=100,"));
        assert!(output.contains("\x1b_Gm=0,q=2;"));
        assert!(output.contains(&format!("a=d,d=I,i={first_id},q=2")));
        assert!(output.contains(&format!("a=d,d=I,i={second_id},q=2")));
        assert!(!output.contains("a=d,d=A"));
    }

    #[test]
    fn partial_upload_is_bounded_and_cancelled_before_replacement() {
        let mut backend = InlineBackend::new(Vec::new(), 40, 12).unwrap();
        let first = crate::media::ImagePreview {
            width: 1,
            height: 1,
            encoded: Arc::from("A".repeat(140_000)),
        };
        let second = crate::media::ImagePreview {
            width: 1,
            height: 1,
            encoded: Arc::from("AAAA"),
        };
        let area = Rect::new(0, 0, 10, 5);
        backend.draw_image(Some((&first, area))).unwrap();
        let id = backend.image.as_ref().unwrap().id;
        assert!(backend.image_pending());
        assert_eq!(backend.image.as_ref().unwrap().offset, 64 * 1024);
        assert!(backend.image.as_ref().unwrap().area.is_none());
        let start = backend.writer.len();
        backend.draw_image(Some((&second, area))).unwrap();
        assert!(!backend.image_pending());
        let output = std::str::from_utf8(&backend.writer[start..]).unwrap();
        let delete = output.find(&format!("a=d,d=I,i={id},q=2")).unwrap();
        let upload = output.find("a=t,f=100,").unwrap();
        let place = output.find("a=p,").unwrap();
        assert!(delete < upload && upload < place);
    }

    #[test]
    fn upload_finishes_over_multiple_calls_before_placement() {
        let mut backend = InlineBackend::new(Vec::new(), 40, 12).unwrap();
        let image = crate::media::ImagePreview {
            width: 1,
            height: 1,
            encoded: Arc::from("A".repeat(140_000)),
        };
        let area = Rect::new(0, 0, 10, 5);
        backend.draw_image(Some((&image, area))).unwrap();
        backend.draw_image(Some((&image, area))).unwrap();
        assert!(backend.image_pending());
        assert!(backend.image.as_ref().unwrap().area.is_none());
        backend.draw_image(Some((&image, area))).unwrap();
        assert!(!backend.image_pending());
        assert!(backend.image.as_ref().unwrap().area.is_some());
    }

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
