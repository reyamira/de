#![cfg(target_os = "linux")]

use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
use rustix::pty::{OpenptFlags, grantpt, ioctl_tiocgptpeer, openpt, unlockpt};
use rustix::termios::{Winsize, tcsetwinsize};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use unicode_width::UnicodeWidthChar;

// Always reap the session leader, including when an assertion panics.
struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn contains(output: &[u8], needle: &[u8]) -> bool {
    output.windows(needle.len()).any(|window| window == needle)
}

// Replay the terminal output because Ratatui sends cell differences, not complete
// lines. The test only needs cursor motion, erasure, and printable characters.
fn screen(output: &[u8]) -> String {
    const WIDTH: usize = 120;
    const HEIGHT: usize = 38;
    let mut cells = vec![vec![' '; WIDTH]; HEIGHT];
    let (mut x, mut y) = (0usize, 0usize);
    let text = String::from_utf8_lossy(output);
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\x1b' if chars.peek() == Some(&'[') => {
                chars.next();
                let mut params = String::new();
                let command = loop {
                    match chars.next() {
                        Some(c) if ('\u{40}'..='\u{7e}').contains(&c) => break c,
                        Some(c) => params.push(c),
                        None => {
                            return cells
                                .iter()
                                .map(|row| row.iter().collect::<String>())
                                .collect::<Vec<_>>()
                                .join("\n");
                        }
                    }
                };
                let number = params
                    .split(';')
                    .next()
                    .unwrap_or("")
                    .parse::<usize>()
                    .unwrap_or(0);
                let amount = number.max(1);
                match command {
                    'A' => y = y.saturating_sub(amount),
                    'B' => y = (y + amount).min(HEIGHT - 1),
                    'C' => x = (x + amount).min(WIDTH - 1),
                    'D' => x = x.saturating_sub(amount),
                    'G' => x = amount.saturating_sub(1).min(WIDTH - 1),
                    'H' | 'f' => {
                        let mut parts = params.split(';');
                        y = parts
                            .next()
                            .and_then(|p| p.parse::<usize>().ok())
                            .unwrap_or(1)
                            .saturating_sub(1)
                            .min(HEIGHT - 1);
                        x = parts
                            .next()
                            .and_then(|p| p.parse::<usize>().ok())
                            .unwrap_or(1)
                            .saturating_sub(1)
                            .min(WIDTH - 1);
                    }
                    'J' if number == 0 => {
                        cells[y][x.min(WIDTH)..].fill(' ');
                        for row in cells.iter_mut().skip(y + 1) {
                            row.fill(' ');
                        }
                    }
                    'J' if number == 2 => cells.iter_mut().for_each(|row| row.fill(' ')),
                    'K' if number == 0 => cells[y][x.min(WIDTH)..].fill(' '),
                    'K' if number == 2 => cells[y].fill(' '),
                    _ => {}
                }
            }
            '\x1b' if matches!(chars.peek(), Some(']') | Some('_') | Some('P')) => {
                chars.next();
                while let Some(c) = chars.next() {
                    if c == '\x07' {
                        break;
                    }
                    if c == '\x1b' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            }
            '\x1b' => {
                chars.next();
            }
            '\r' => x = 0,
            '\n' => {
                if y + 1 == HEIGHT {
                    cells.remove(0);
                    cells.push(vec![' '; WIDTH]);
                } else {
                    y += 1;
                }
            }
            c if !c.is_control() => {
                let width = UnicodeWidthChar::width(c).unwrap_or(0);
                if width == 0 {
                    continue;
                }
                if x + width > WIDTH {
                    x = 0;
                    if y + 1 == HEIGHT {
                        cells.remove(0);
                        cells.push(vec![' '; WIDTH]);
                    } else {
                        y += 1;
                    }
                }
                cells[y][x] = c;
                if width == 2 {
                    cells[y][x + 1] = ' ';
                }
                x += width;
            }
            _ => {}
        }
    }
    cells
        .iter()
        .map(|row| row.iter().collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

fn collect_until(
    master: &mut File,
    child: &mut Child,
    output: &mut Vec<u8>,
    description: &str,
    done: impl Fn(&[u8]) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut buffer = [0; 8192];
    loop {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {description}: {}",
            String::from_utf8_lossy(output)
        );
        match master.read(&mut buffer) {
            Ok(0) => panic!("PTY closed while waiting for {description}"),
            Ok(count) => {
                output.extend_from_slice(&buffer[..count]);
                assert!(output.len() < 4 * 1024 * 1024, "excessive PTY output");
                if done(output) {
                    return;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if let Some(status) = child.try_wait().expect("check child status") {
                    panic!(
                        "de exited with {status} while waiting for {description}: {}",
                        String::from_utf8_lossy(output)
                    );
                }
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => panic!("reading PTY while waiting for {description}: {error}"),
        }
    }
}

#[test]
fn queued_markdown_toggle_survives_unanswered_cursor_query() {
    let directory = tempfile::tempdir().unwrap();
    let config = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("fixture.md"), "# PTY_SOURCE_MARKER\n").unwrap();

    let master_fd = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY | OpenptFlags::CLOEXEC).unwrap();
    grantpt(&master_fd).unwrap();
    unlockpt(&master_fd).unwrap();
    let slave_fd = ioctl_tiocgptpeer(
        &master_fd,
        OpenptFlags::RDWR | OpenptFlags::NOCTTY | OpenptFlags::CLOEXEC,
    )
    .unwrap();
    tcsetwinsize(
        &slave_fd,
        Winsize {
            ws_row: 38,
            ws_col: 120,
            ws_xpixel: 0,
            ws_ypixel: 0,
        },
    )
    .unwrap();
    fcntl_setfl(
        &master_fd,
        fcntl_getfl(&master_fd).unwrap() | OFlags::NONBLOCK,
    )
    .unwrap();
    let mut master = File::from(master_fd);
    let slave = File::from(slave_fd);

    // setsid -c makes the slave the controlling terminal before execing de.
    let child = Command::new("setsid")
        .arg("--ctty")
        .arg(env!("CARGO_BIN_EXE_de"))
        .arg(directory.path())
        .env("TERM", "xterm-256color")
        .env("XDG_CONFIG_HOME", config.path())
        .env("XDG_CACHE_HOME", config.path())
        .env_remove("DE_THEME")
        .env_remove("KITTY_WINDOW_ID")
        .env_remove("GHOSTTY_RESOURCES_DIR")
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stderr(Stdio::from(slave))
        .stdout(Stdio::null())
        .spawn()
        .expect("start de in controlling PTY");
    let mut child = ChildGuard(child);
    let mut output = Vec::new();

    collect_until(
        &mut master,
        &mut child.0,
        &mut output,
        "loaded picker preview",
        |bytes| screen(bytes).contains("PTY_SOURCE_MARKER"),
    );
    // One write puts the toggle behind the viewer command while CPR is pending.
    assert_eq!(master.write(b"vm").unwrap(), 2);
    collect_until(
        &mut master,
        &mut child.0,
        &mut output,
        "Markdown source viewer",
        |bytes| {
            let screen = screen(bytes);
            screen.contains("source · fixture.md") && screen.contains("# PTY_SOURCE_MARKER")
        },
    );
    assert!(
        contains(&output, b"\x1b[6n"),
        "de did not request the cursor position"
    );

    // No cursor position response is ever sent. Leave the viewer and picker.
    assert_eq!(master.write(b"qq").unwrap(), 2);
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        let mut buffer = [0; 8192];
        match master.read(&mut buffer) {
            Ok(count) if count > 0 => output.extend_from_slice(&buffer[..count]),
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) => {}
            // Linux PTY masters report EIO after the slave closes.
            Err(error) if error.raw_os_error() == Some(5) => {}
            Err(error) => panic!("reading final PTY output: {error}"),
        }
        if let Some(status) = child.0.try_wait().expect("check child status") {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for de to exit"
        );
        thread::sleep(Duration::from_millis(10));
    };
    // Drain bytes written just before exit, including terminal mode changes.
    loop {
        let mut buffer = [0; 8192];
        match master.read(&mut buffer) {
            Ok(count) if count > 0 => output.extend_from_slice(&buffer[..count]),
            Ok(_) => break,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error)
                if error.kind() == io::ErrorKind::WouldBlock || error.raw_os_error() == Some(5) =>
            {
                break;
            }
            Err(error) => panic!("draining final PTY output: {error}"),
        }
    }
    assert!(status.success(), "de exited with {status}");
    for sequence in [
        b"\x1b[?1000h".as_slice(),
        b"\x1b[?1002h",
        b"\x1b[?1003h",
        b"\x1b[?1015h",
        b"\x1b[?1006h",
    ] {
        assert!(
            !contains(&output, sequence),
            "mouse capture was enabled: {sequence:?}"
        );
    }
}
