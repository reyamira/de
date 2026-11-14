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

fn spawn_test_pty(
    directory: &std::path::Path,
    config: &std::path::Path,
) -> (File, ChildGuard, File) {
    let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY | OpenptFlags::CLOEXEC).unwrap();
    grantpt(&master).unwrap();
    unlockpt(&master).unwrap();
    let slave = ioctl_tiocgptpeer(
        &master,
        OpenptFlags::RDWR | OpenptFlags::NOCTTY | OpenptFlags::CLOEXEC,
    )
    .unwrap();
    tcsetwinsize(
        &slave,
        Winsize {
            ws_row: 38,
            ws_col: 120,
            ws_xpixel: 960,
            ws_ypixel: 608,
        },
    )
    .unwrap();
    fcntl_setfl(&master, fcntl_getfl(&master).unwrap() | OFlags::NONBLOCK).unwrap();
    let master = File::from(master);
    let slave = File::from(slave);
    let child = Command::new("setsid")
        .arg("--ctty")
        .arg(env!("CARGO_BIN_EXE_de"))
        .arg(directory)
        // Neither fake brands nor stale mux variables may decide support.
        .env("TERM", "unrecognized-terminal")
        .env("TERM_PROGRAM", "not-a-terminal-brand")
        .env("TMUX", "/not/a/socket,123,0")
        .env("STY", "not-a-screen-session")
        .env("XDG_CONFIG_HOME", config)
        .env("XDG_CACHE_HOME", config)
        .env_remove("DE_THEME")
        .env_remove("KITTY_WINDOW_ID")
        .env_remove("GHOSTTY_RESOURCES_DIR")
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stderr(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::piped())
        .spawn()
        .expect("start de in controlling PTY");
    (master, ChildGuard(child), slave)
}

fn probe_id(output: &[u8]) -> Option<u32> {
    let start = output.windows(3).position(|part| part == b"\x1b_G")?;
    let query = &output[start..];
    let end = query.windows(2).position(|part| part == b"\x1b\\")?;
    let header = std::str::from_utf8(&query[..end]).ok()?.split(';').next()?;
    assert!(header.contains("a=q") && header.contains("t=d") && header.contains("f=100"));
    assert!(!header.contains("a=t") && !header.contains("a=T"));
    header
        .split(',')
        .find_map(|part| part.strip_prefix("i=")?.parse().ok())
}

fn finish_pty(master: &mut File, child: &mut Child, output: &mut Vec<u8>) -> Vec<u8> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let mut bytes = [0; 8192];
        match master.read(&mut bytes) {
            Ok(count) if count > 0 => output.extend_from_slice(&bytes[..count]),
            Ok(_) => {}
            Err(error)
                if error.kind() == io::ErrorKind::WouldBlock
                    || error.kind() == io::ErrorKind::Interrupted
                    || error.raw_os_error() == Some(5) => {}
            Err(error) => panic!("read final terminal output: {error}"),
        }
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "de exited with {status}");
            // The child can exit with unread bytes still in the PTY.
            loop {
                match master.read(&mut bytes) {
                    Ok(count) if count > 0 => output.extend_from_slice(&bytes[..count]),
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    _ => break,
                }
            }
            let mut stdout = Vec::new();
            child
                .stdout
                .take()
                .unwrap()
                .read_to_end(&mut stdout)
                .unwrap();
            return stdout;
        }
        assert!(Instant::now() < deadline, "de did not exit");
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn queued_markdown_toggle_survives_unanswered_cursor_query() {
    let directory = tempfile::tempdir().unwrap();
    let config = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("fixture.md"), "# PTY_SOURCE_MARKER\n").unwrap();

    let (mut master, mut child, _slave) = spawn_test_pty(directory.path(), config.path());
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

#[test]
fn fragmented_png_confirmation_enables_initial_image_without_brand_gates() {
    let directory = tempfile::tempdir().unwrap();
    let config = tempfile::tempdir().unwrap();
    image::RgbaImage::from_pixel(12, 12, image::Rgba([255, 0, 90, 255]))
        .save(directory.path().join("fixture.png"))
        .unwrap();
    let (mut master, mut child, _slave) = spawn_test_pty(directory.path(), config.path());
    let mut output = Vec::new();
    collect_until(
        &mut master,
        &mut child.0,
        &mut output,
        "PNG capability query",
        |bytes| probe_id(bytes).is_some(),
    );
    assert!(!contains(&output, b"\x1b_Ga=t,"));
    let id = probe_id(&output).unwrap();
    assert_ne!(id, 0);
    // Some consumers parse numeric fields as signed 32-bit integers and return
    // zero on overflow. A legal full-u32 ID would fail exact reply correlation.
    let consumer_id = id.to_string().parse::<i32>().unwrap_or(0);
    let response = format!("\x1b_Gi={consumer_id};OK\x1b\\");
    for byte in response.bytes() {
        master.write_all(&[byte]).unwrap();
        thread::sleep(Duration::from_millis(1));
    }
    collect_until(
        &mut master,
        &mut child.0,
        &mut output,
        "confirmed PNG transfer",
        |bytes| contains(bytes, b"\x1b_Ga=t,f=100,"),
    );
    master.write_all(b"\x07").unwrap();
    let stdout = finish_pty(&mut master, &mut child.0, &mut output);
    assert_eq!(
        stdout,
        format!("{}\n", directory.path().display()).as_bytes()
    );
    assert_eq!(
        output.windows(4).filter(|bytes| *bytes == b"a=q,").count(),
        1
    );
    for command in String::from_utf8_lossy(&output).split("\x1b_G").skip(1) {
        let header = command.split(';').next().unwrap();
        for parameter in header.split(',') {
            if let Some(id) = parameter.strip_prefix("i=") {
                let consumer_id = id.parse::<i32>().expect("portable graphics image ID");
                assert!(consumer_id > 0, "graphics image IDs must remain positive");
            }
        }
    }
}

#[test]
fn rejected_or_missing_png_confirmation_never_transfers_initial_image() {
    for case in [
        "silent",
        "wrong-id",
        "error",
        "malformed",
        "da-only",
        "stale",
        "partial-timeout",
    ] {
        let directory = tempfile::tempdir().unwrap();
        let config = tempfile::tempdir().unwrap();
        image::RgbaImage::from_pixel(12, 12, image::Rgba([0, 255, 90, 255]))
            .save(directory.path().join("fixture.png"))
            .unwrap();
        let (mut master, mut child, _slave) = spawn_test_pty(directory.path(), config.path());
        let mut output = Vec::new();
        collect_until(
            &mut master,
            &mut child.0,
            &mut output,
            "PNG query",
            |bytes| probe_id(bytes).is_some(),
        );
        let id = probe_id(&output).unwrap();
        let response = match case {
            "wrong-id" => format!("\x1b_Gi={};OK\x1b\\", id - 1),
            "error" => format!("\x1b_Gi={id};EINVAL:q unsupported\x1b\\"),
            "malformed" => format!("\x1b_Gi={id},i={id};OK\x1b\\"),
            "da-only" => "\x1b[?1;2c".to_owned(),
            "partial-timeout" => format!("\x1b_Gi={id};O"),
            _ => String::new(),
        };
        master.write_all(response.as_bytes()).unwrap();
        collect_until(
            &mut master,
            &mut child.0,
            &mut output,
            "fallback picker",
            |bytes| screen(bytes).contains("fixture.png"),
        );
        if case == "stale" {
            master
                .write_all(format!("\x1b_Gi={id};OK\x1b\\").as_bytes())
                .unwrap();
        } else if case == "partial-timeout" {
            master.write_all(b"K\x1b\\").unwrap();
        }
        master.write_all(b"\x07").unwrap();
        let stdout = finish_pty(&mut master, &mut child.0, &mut output);
        assert_eq!(
            stdout,
            format!("{}\n", directory.path().display()).as_bytes(),
            "{case}"
        );
        assert!(
            !contains(&output, b"\x1b_Ga=t,"),
            "{case} enabled a PNG transfer"
        );
    }
}

#[test]
fn unconfirmed_markdown_keeps_queued_paste_cpr_and_resize_without_image_transfer() {
    let directory = tempfile::tempdir().unwrap();
    let config = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("fixture.md"), "# PASTE_FIFO_MARKER\n").unwrap();
    let (mut master, mut child, slave) = spawn_test_pty(directory.path(), config.path());
    let mut output = Vec::new();
    collect_until(
        &mut master,
        &mut child.0,
        &mut output,
        "PNG query",
        |bytes| probe_id(bytes).is_some(),
    );
    // These keys arrive during the PNG probe and must reach the same input FIFO.
    master.write_all(b"v/").unwrap();
    collect_until(
        &mut master,
        &mut child.0,
        &mut output,
        "viewer cursor query",
        |bytes| contains(bytes, b"\x1b[6n"),
    );
    master
        .write_all(b"\x1b[200~PASTE_FIFO_MARKER\x1b[201~\x1b[1;1R")
        .unwrap();
    collect_until(
        &mut master,
        &mut child.0,
        &mut output,
        "queued find paste",
        |bytes| screen(bytes).contains("/ PASTE_FIFO_MARKER"),
    );
    tcsetwinsize(
        &slave,
        Winsize {
            ws_row: 40,
            ws_col: 120,
            ws_xpixel: 960,
            ws_ypixel: 640,
        },
    )
    .unwrap();
    collect_until(
        &mut master,
        &mut child.0,
        &mut output,
        "resize cursor query",
        |bytes| {
            bytes
                .windows(4)
                .filter(|sequence| *sequence == b"\x1b[6n")
                .count()
                >= 2
        },
    );
    // Finish find, request rendered mode without support, then accept the directory.
    master.write_all(b"\x1b[1;1R\rm\x07").unwrap();
    let stdout = finish_pty(&mut master, &mut child.0, &mut output);
    assert_eq!(
        stdout,
        format!("{}\n", directory.path().display()).as_bytes()
    );
    assert!(!contains(&output, b"\x1b_Ga=t,"));
    assert!(
        contains(&output, b"\x1b[?1006h"),
        "valid CPR did not enable viewer mouse capture"
    );
    assert!(
        contains(&output, b"\x1b[?1006l"),
        "mouse capture was not restored"
    );
}
