//! One input owner for terminal events and direct PNG capability replies.

#[cfg(unix)]
mod unix {
    use std::collections::VecDeque;
    use std::fs::File;
    use std::io::{self, IsTerminal, Read, Write};
    use std::os::fd::{AsFd, BorrowedFd};
    use std::os::unix::net::UnixStream;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::{Duration, Instant};
    use termina::{Event, Parser};

    const ESC_DELAY: Duration = Duration::from_millis(25);
    const PROBE_TIMEOUT: Duration = Duration::from_millis(250);
    const CONTROL_LIMIT: usize = 4096;
    // A complete 1×1 PNG. a=q validates decoding without storing an image.
    const PROBE_PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=";
    static NEXT_PROBE: AtomicU32 = AtomicU32::new(0x5e00_0001);

    #[derive(Default)]
    enum Frame {
        #[default]
        Ground,
        Escape(Instant),
        Csi(Vec<u8>),
        Paste(Vec<u8>),
        Control {
            kind: u8,
            bytes: Vec<u8>,
            escaped: bool,
            overflow: bool,
        },
    }

    #[derive(Default)]
    struct Decoder {
        parser: Parser,
        events: VecDeque<Event>,
        frame: Frame,
        probe: Option<u32>,
        reply: Option<bool>,
        normal: Vec<u8>,
    }

    impl Decoder {
        fn parse(&mut self, bytes: &[u8]) {
            if !bytes.is_empty() {
                self.parser.parse(bytes, true);
                self.drain_parser();
            }
        }

        fn drain_parser(&mut self) {
            while let Some(event) = self.parser.pop() {
                self.events.push_back(event);
            }
        }

        fn escape_wait(&self) -> Option<Duration> {
            match self.frame {
                Frame::Escape(start) => Some(ESC_DELAY.saturating_sub(start.elapsed())),
                _ => None,
            }
        }

        fn expire_escape(&mut self) {
            if self.escape_wait() == Some(Duration::ZERO) {
                self.frame = Frame::Ground;
                // Flush only an owned, standalone ESC. Never finalize partial CSI/UTF-8.
                self.parser.parse(b"\x1b", false);
                self.drain_parser();
            }
        }

        fn feed(&mut self, input: &[u8]) {
            let mut normal = std::mem::take(&mut self.normal);
            normal.reserve(input.len());
            for &byte in input {
                let frame = std::mem::take(&mut self.frame);
                self.frame = match frame {
                    Frame::Ground if byte == 0x1b => Frame::Escape(Instant::now()),
                    Frame::Ground => {
                        normal.push(byte);
                        Frame::Ground
                    }
                    Frame::Escape(_) => match byte {
                        b'_' | b']' | b'P' => Frame::Control {
                            kind: byte,
                            bytes: vec![0x1b, byte],
                            escaped: false,
                            overflow: false,
                        },
                        b'[' => Frame::Csi(vec![0x1b, byte]),
                        0x1b => {
                            // Two ESC bytes mean Escape followed by another possible sequence.
                            self.parse(&normal);
                            normal.clear();
                            self.parser.parse(b"\x1b", false);
                            self.drain_parser();
                            Frame::Escape(Instant::now())
                        }
                        _ => {
                            normal.extend_from_slice(&[0x1b, byte]);
                            Frame::Ground
                        }
                    },
                    Frame::Csi(mut bytes) => {
                        bytes.push(byte);
                        // X10 mouse reports have three binary bytes after CSI M.
                        let x10 = bytes.get(2) == Some(&b'M');
                        let old_function = bytes.get(2) == Some(&b'[');
                        let complete = if x10 {
                            bytes.len() == 6
                        } else if old_function {
                            bytes.len() == 4
                        } else {
                            (0x40..=0x7e).contains(&byte)
                        };
                        if bytes == b"\x1b[200~" {
                            self.parse(&normal);
                            normal.clear();
                            Frame::Paste(bytes)
                        } else if complete {
                            normal.extend_from_slice(&bytes);
                            Frame::Ground
                        } else if bytes.len() >= CONTROL_LIMIT {
                            // Discard an oversized CSI through its final byte.
                            Frame::Control {
                                kind: b'[',
                                bytes: Vec::new(),
                                escaped: false,
                                overflow: true,
                            }
                        } else {
                            Frame::Csi(bytes)
                        }
                    }
                    Frame::Paste(mut bytes) => {
                        bytes.push(byte);
                        if bytes.ends_with(b"\x1b[201~") {
                            // One contiguous parse avoids reparsing a growing paste per input read.
                            self.parse(&bytes);
                            Frame::Ground
                        } else {
                            Frame::Paste(bytes)
                        }
                    }
                    Frame::Control {
                        kind,
                        mut bytes,
                        escaped,
                        mut overflow,
                    } => {
                        if bytes.len() < CONTROL_LIMIT {
                            bytes.push(byte);
                        } else {
                            overflow = true;
                        }
                        let ended = (escaped && byte == b'\\')
                            || (kind == b']' && byte == 7)
                            || (kind == b'[' && (0x40..=0x7e).contains(&byte));
                        if ended {
                            if !overflow {
                                if kind == b'_' {
                                    self.graphics_reply(&bytes);
                                } else {
                                    normal.extend_from_slice(&bytes);
                                }
                            }
                            Frame::Ground
                        } else if matches!(byte, 0x18 | 0x1a) {
                            // ECMA-48 CAN/SUB cancel a malformed control without leaking its text.
                            Frame::Ground
                        } else {
                            Frame::Control {
                                kind,
                                bytes,
                                escaped: byte == 0x1b,
                                overflow,
                            }
                        }
                    }
                };
            }
            self.parse(&normal);
            normal.clear();
            self.normal = normal;
        }

        fn graphics_reply(&mut self, bytes: &[u8]) {
            let Some(id) = self.probe else { return };
            let Some(payload) = bytes
                .strip_prefix(b"\x1b_G")
                .and_then(|b| b.strip_suffix(b"\x1b\\"))
            else {
                return;
            };
            let Some(separator) = payload.iter().position(|&b| b == b';') else {
                return;
            };
            let Ok(header) = std::str::from_utf8(&payload[..separator]) else {
                return;
            };
            let mut found = None;
            for parameter in header.split(',') {
                let Some((key, value)) = parameter.split_once('=') else {
                    return;
                };
                if key == "i" {
                    if found.is_some()
                        || value.is_empty()
                        || !value.bytes().all(|b| b.is_ascii_digit())
                    {
                        return;
                    }
                    found = value.parse::<u32>().ok();
                    if found.is_none() {
                        return;
                    }
                } else if key != "p"
                    || value.is_empty()
                    || !value.bytes().all(|b| b.is_ascii_digit())
                {
                    return;
                }
            }
            if found == Some(id) && self.reply.is_none() {
                self.reply = Some(&payload[separator + 1..] == b"OK");
            }
        }
    }

    pub(crate) struct InputReader {
        input: File,
        resize: UnixStream,
        signal: signal_hook::SigId,
        decoder: Decoder,
    }

    impl InputReader {
        pub(crate) fn new() -> io::Result<Self> {
            let input = if io::stdin().is_terminal() {
                File::from(rustix::io::dup(io::stdin())?)
            } else {
                File::open("/dev/tty")?
            };
            let (resize, sender) = UnixStream::pair()?;
            resize.set_nonblocking(true)?;
            sender.set_nonblocking(true)?;
            let signal =
                signal_hook::low_level::pipe::register(signal_hook::consts::SIGWINCH, sender)?;
            Ok(Self {
                input,
                resize,
                signal,
                decoder: Decoder::default(),
            })
        }

        pub(crate) fn probe_graphics(&mut self, writer: &mut impl Write) -> io::Result<bool> {
            let id = loop {
                let id = NEXT_PROBE.fetch_add(1, Ordering::Relaxed) & i32::MAX as u32;
                if id != 0 {
                    break id;
                }
            };
            self.decoder.probe = Some(id);
            self.decoder.reply = None;
            let result = (|| {
                write!(writer, "\x1b_Ga=q,t=d,f=100,i={id};{PROBE_PNG}\x1b\\")?;
                writer.flush()?;
                let deadline = Instant::now() + PROBE_TIMEOUT;
                loop {
                    if let Some(supported) = self.decoder.reply {
                        return Ok(supported);
                    }
                    let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                        return Ok(false);
                    };
                    self.pump(Some(remaining))?;
                }
            })();
            self.decoder.probe = None;
            self.decoder.reply = None;
            result
        }

        pub(crate) fn poll(
            &mut self,
            timeout: Option<Duration>,
            filter: impl Fn(&Event) -> bool,
        ) -> io::Result<bool> {
            let deadline = timeout.map(|duration| Instant::now() + duration);
            loop {
                if self.decoder.events.iter().any(&filter) {
                    return Ok(true);
                }
                let remaining = deadline.map(|end| end.saturating_duration_since(Instant::now()));
                self.pump(remaining)?;
                if self.decoder.events.iter().any(&filter) {
                    return Ok(true);
                }
                if deadline.is_some_and(|end| Instant::now() >= end) {
                    return Ok(false);
                }
            }
        }

        pub(crate) fn read(&mut self, filter: impl Fn(&Event) -> bool) -> io::Result<Event> {
            self.poll(None, &filter)?;
            let index = self
                .decoder
                .events
                .iter()
                .position(filter)
                .expect("poll found an event");
            Ok(self
                .decoder
                .events
                .remove(index)
                .expect("event index exists"))
        }

        fn pump(&mut self, timeout: Option<Duration>) -> io::Result<()> {
            if self.decoder.escape_wait() == Some(Duration::ZERO) {
                self.decoder.expire_escape();
                return Ok(());
            }
            let timeout = match (timeout, self.decoder.escape_wait()) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            let ready = match poll_fds([self.input.as_fd(), self.resize.as_fd()], timeout) {
                Ok(ready) => ready,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => return Ok(()),
                Err(error) => return Err(error),
            };
            if ready[0] {
                let mut bytes = [0; 8192];
                match self.input.read(&mut bytes) {
                    Ok(0) => {
                        return Err(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "terminal input closed",
                        ));
                    }
                    Ok(count) => self.decoder.feed(&bytes[..count]),
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(error) => return Err(error),
                }
            }
            if ready[1] {
                let mut bytes = [0; 1024];
                loop {
                    match self.resize.read(&mut bytes) {
                        Ok(0) => break,
                        Ok(_) => {}
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                        Err(error) => return Err(error),
                    }
                }
                let size = rustix::termios::tcgetwinsize(&self.input)?;
                self.decoder
                    .events
                    .push_back(Event::WindowResized(size.into()));
            }
            self.decoder.expire_escape();
            Ok(())
        }
    }

    impl Drop for InputReader {
        fn drop(&mut self) {
            signal_hook::low_level::unregister(self.signal);
        }
    }

    // Adapted from Termina 0.3.3's Unix event source:
    //
    // Copyright (c) 2025 Michael Davis
    //
    // Permission is hereby granted, free of charge, to any person obtaining a copy
    // of this software and associated documentation files (the "Software"), to deal
    // in the Software without restriction, including without limitation the rights
    // to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
    // copies of the Software, and to permit persons to whom the Software is
    // furnished to do so, subject to the following conditions:
    //
    // The above copyright notice and this permission notice shall be included in
    // all copies or substantial portions of the Software.
    //
    // THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
    // IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
    // FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
    // AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
    // LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
    // OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
    // THE SOFTWARE.
    // macOS requires select for /dev/tty; poll is not reliable for that descriptor.
    fn poll_fds(fds: [BorrowedFd<'_>; 2], timeout: Option<Duration>) -> io::Result<[bool; 2]> {
        let timeout = timeout.map(|duration| duration.try_into().expect("bounded terminal wait"));
        #[cfg(not(target_os = "macos"))]
        {
            use rustix::event::{PollFd, PollFlags};
            let mut fds = [
                PollFd::new(&fds[0], PollFlags::IN),
                PollFd::new(&fds[1], PollFlags::IN),
            ];
            rustix::event::poll(&mut fds, timeout.as_ref())?;
            let ready = |fd: &PollFd<'_>| {
                fd.revents()
                    .intersects(PollFlags::IN | PollFlags::HUP | PollFlags::ERR)
            };
            Ok([ready(&fds[0]), ready(&fds[1])])
        }
        #[cfg(target_os = "macos")]
        {
            use rustix::event::{FdSetElement, FdSetIter, fd_set_insert, fd_set_num_elements};
            use std::os::fd::AsRawFd;
            let fds = [fds[0].as_raw_fd(), fds[1].as_raw_fd()];
            let nfds = fds[0].max(fds[1]) + 1;
            let mut readfds = vec![FdSetElement::default(); fd_set_num_elements(fds.len(), nfds)];
            for fd in fds {
                fd_set_insert(&mut readfds, fd);
            }
            // SAFETY: readfds has the required size and contains only our live descriptors.
            unsafe {
                rustix::event::select(nfds, Some(&mut readfds), None, None, timeout.as_ref())
            }?;
            Ok(fds.map(|fd| FdSetIter::new(&readfds).any(|ready| ready == fd)))
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use termina::escape::csi::{Csi, Cursor};
        use termina::event::{KeyCode, Modifiers};

        fn decoder() -> Decoder {
            Decoder {
                probe: Some(42),
                ..Decoder::default()
            }
        }

        #[test]
        fn png_query_payload_is_a_decodable_tiny_png() {
            use base64::Engine;
            let png = base64::engine::general_purpose::STANDARD
                .decode(PROBE_PNG)
                .unwrap();
            let image = image::load_from_memory_with_format(&png, image::ImageFormat::Png).unwrap();
            assert_eq!((image.width(), image.height()), (1, 1));
        }

        #[test]
        fn large_fragmented_paste_remains_one_opaque_event() {
            let mut decoder = decoder();
            let text = "é \x1b_Gi=42;OK\x1b\\ ".repeat(8192);
            decoder.feed(b"\x1b[200~");
            for chunk in text.as_bytes().chunks(8192) {
                decoder.feed(chunk);
            }
            assert!(decoder.events.is_empty());
            decoder.feed(b"\x1b[201~q");
            assert!(
                matches!(decoder.events.pop_front(), Some(Event::Paste(paste)) if paste == text)
            );
            assert!(
                matches!(decoder.events.pop_front(), Some(Event::Key(key)) if key.code == KeyCode::Char('q'))
            );
            assert_eq!(decoder.reply, None);
        }

        #[test]
        fn fragmented_png_probe_accepts_only_complete_matching_ok() {
            let response = b"\x1b_Gi=42;OK\x1b\\";
            for split in 0..response.len() {
                let mut decoder = decoder();
                decoder.feed(&response[..split]);
                assert_eq!(decoder.reply, None);
                decoder.feed(&response[split..]);
                assert_eq!(decoder.reply, Some(true));
                assert!(decoder.events.is_empty());
            }
            let mut decoder = decoder();
            for byte in response {
                decoder.feed(&[*byte]);
            }
            assert_eq!(decoder.reply, Some(true));
        }

        #[test]
        fn graphics_replies_reject_malformed_wrong_id_and_error() {
            for response in [
                b"\x1b_Gi=41;OK\x1b\\".as_slice(),
                b"\x1b_Gi=42junk;OK\x1b\\",
                b"\x1b_Gi=42,i=42;OK\x1b\\",
                b"\x1b_Gi=42949672960;OK\x1b\\",
                b"\x1b_Gi=42;OK extra\x1b\\",
                b"\x1b_Gi=42;EINVAL: unsupported PNG\x1b\\",
                b"\x1b_Gi=42 OK\x1b\\",
                b"\x1b_G;OK\x1b\\",
                b"\x1b[?1;2c",
                b"",
            ] {
                let mut decoder = decoder();
                decoder.feed(response);
                assert_ne!(decoder.reply, Some(true), "{response:?}");
                assert!(
                    !decoder
                        .events
                        .iter()
                        .any(|event| matches!(event, Event::Key(_)))
                );
            }
        }

        #[test]
        fn stale_replies_never_become_keys_or_enable_graphics() {
            let mut decoder = decoder();
            decoder.feed(b"a\x1b_Gi=42;O");
            decoder.probe = None;
            decoder.feed(b"K\x1b\\b\x1b_Gi=42;OK\x1b\\c");
            assert_eq!(decoder.reply, None);
            let keys: Vec<_> = decoder
                .events
                .into_iter()
                .map(|event| match event {
                    Event::Key(key) => key.code,
                    event => panic!("unexpected {event:?}"),
                })
                .collect();
            assert_eq!(
                keys,
                [KeyCode::Char('a'), KeyCode::Char('b'), KeyCode::Char('c')]
            );
        }

        #[test]
        fn paste_and_opaque_controls_do_not_expose_apc_contents() {
            let mut decoder = decoder();
            let mut paste = b"\x1b[200~".to_vec();
            paste.extend_from_slice(b"text \x1b_Gi=42;OK\x1b\\ more");
            paste.extend_from_slice(b"\x1b[201~");
            for chunk in paste.chunks(3) {
                decoder.feed(chunk);
            }
            assert_eq!(decoder.reply, None);
            assert!(
                matches!(decoder.events.pop_front(), Some(Event::Paste(text)) if text == "text \x1b_Gi=42;OK\x1b\\ more")
            );
            for control in [
                b"\x1b]unknown;\x1b_Gi=42;OK\x1b\\".as_slice(),
                b"\x1bPunknown\x1b_Gi=42;OK\x1b\\",
            ] {
                decoder.feed(control);
            }
            assert_eq!(decoder.reply, None);
            assert!(decoder.events.is_empty());
        }

        #[test]
        fn oversized_control_is_bounded_and_recovers_at_terminator() {
            let mut decoder = decoder();
            decoder.feed(b"\x1b_G");
            decoder.feed(&vec![b'x'; CONTROL_LIMIT * 4]);
            assert!(
                matches!(&decoder.frame, Frame::Control { bytes, overflow: true, .. } if bytes.len() <= CONTROL_LIMIT)
            );
            decoder.feed(b"\x1b\\q");
            assert!(
                matches!(decoder.events.pop_front(), Some(Event::Key(key)) if key.code == KeyCode::Char('q'))
            );
            assert_eq!(decoder.reply, None);
        }

        #[test]
        fn escape_timeout_preserves_alt_csi_and_utf8() {
            let mut decoder = decoder();
            decoder.feed(b"\x1b");
            assert!(decoder.events.is_empty());
            decoder.frame = Frame::Escape(Instant::now() - ESC_DELAY);
            decoder.expire_escape();
            assert!(
                matches!(decoder.events.pop_front(), Some(Event::Key(key)) if key.code == KeyCode::Escape)
            );
            decoder.feed(b"\x1bx");
            assert!(
                matches!(decoder.events.pop_front(), Some(Event::Key(key)) if key.code == KeyCode::Char('x') && key.modifiers.contains(Modifiers::ALT))
            );
            decoder.feed(b"\x1b[");
            decoder.expire_escape();
            decoder.feed(b"A");
            assert!(
                matches!(decoder.events.pop_front(), Some(Event::Key(key)) if key.code == KeyCode::Up)
            );
            decoder.feed(&[0xc3]);
            decoder.expire_escape();
            decoder.feed(&[0xa9]);
            assert!(
                matches!(decoder.events.pop_front(), Some(Event::Key(key)) if key.code == KeyCode::Char('é'))
            );
        }

        #[test]
        fn cpr_filter_retains_keys_paste_mouse_and_resize_in_fifo() {
            use std::os::fd::OwnedFd;
            let (input, _sender) = UnixStream::pair().unwrap();
            let (resize, sender) = UnixStream::pair().unwrap();
            let signal =
                signal_hook::low_level::pipe::register(signal_hook::consts::SIGWINCH, sender)
                    .unwrap();
            let mut reader = InputReader {
                input: File::from(OwnedFd::from(input)),
                resize,
                signal,
                decoder: decoder(),
            };
            reader
                .decoder
                .feed(b"a\x1b[200~paste\x1b[201~\x1b[<0;2;3M\x1b[4;1Rb");
            reader
                .decoder
                .events
                .push_back(Event::WindowResized(termina::WindowSize {
                    cols: 120,
                    rows: 38,
                    pixel_width: None,
                    pixel_height: None,
                }));
            let is_cpr = |event: &Event| {
                matches!(
                    event,
                    Event::Csi(Csi::Cursor(Cursor::ActivePositionReport { .. }))
                )
            };
            assert!(reader.poll(Some(Duration::ZERO), is_cpr).unwrap());
            assert!(is_cpr(&reader.read(is_cpr).unwrap()));
            assert!(
                matches!(reader.read(|_| true).unwrap(), Event::Key(key) if key.code == KeyCode::Char('a'))
            );
            assert!(
                matches!(reader.read(|_| true).unwrap(), Event::Paste(text) if text == "paste")
            );
            assert!(matches!(reader.read(|_| true).unwrap(), Event::Mouse(_)));
            assert!(
                matches!(reader.read(|_| true).unwrap(), Event::Key(key) if key.code == KeyCode::Char('b'))
            );
            assert!(
                matches!(reader.read(|_| true).unwrap(), Event::WindowResized(size) if size.cols == 120)
            );
        }
    }
}

#[cfg(unix)]
pub(crate) use unix::InputReader;

// Keep the existing native event source on platforms without a Unix byte stream.
// These platforms cannot confirm direct PNG graphics and never enable it.
#[cfg(not(unix))]
pub(crate) struct InputReader {
    _terminal: termina::PlatformTerminal,
    reader: termina::EventReader,
}

#[cfg(not(unix))]
impl InputReader {
    pub(crate) fn new() -> std::io::Result<Self> {
        use termina::Terminal;
        let terminal = termina::PlatformTerminal::new()?;
        let reader = terminal.event_reader();
        Ok(Self {
            _terminal: terminal,
            reader,
        })
    }
    pub(crate) fn probe_graphics(&mut self, _: &mut impl std::io::Write) -> std::io::Result<bool> {
        Ok(false)
    }
    pub(crate) fn poll(
        &mut self,
        timeout: Option<std::time::Duration>,
        filter: impl Fn(&termina::Event) -> bool,
    ) -> std::io::Result<bool> {
        self.reader.poll(timeout, filter)
    }
    pub(crate) fn read(
        &mut self,
        filter: impl Fn(&termina::Event) -> bool,
    ) -> std::io::Result<termina::Event> {
        self.reader.read(filter)
    }
}
