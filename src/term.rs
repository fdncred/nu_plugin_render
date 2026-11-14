//! Asking the terminal what it can draw and how big its cells are.
//!
//! The plugin runs in its own process group, so it first asks nushell for
//! the terminal's foreground, then queries `/dev/tty` in raw mode and reads
//! the answers until the device attributes arrive or a timeout passes. Any
//! failure means "draw nothing", and the command falls back to `table`.

use nu_plugin::EngineInterface;

/// What the terminal answered.
#[derive(Clone, Debug, Default)]
pub struct Caps {
    /// It took a kitty graphics query.
    pub kitty: bool,
    /// Its device attributes include sixel (extension 4).
    pub sixel: bool,
    /// It read a kitty image from a temporary file, so it runs on this
    /// machine and images can skip base64.
    pub temp_files: bool,
    /// One cell, width and height in pixels.
    pub cell: Option<(u32, u32)>,
    /// The screen, in cells.
    pub columns: u32,
    pub rows: u32,
    /// Its background color, when it reports one.
    pub background: Option<[u8; 3]>,
}

/// Probe the terminal nushell runs in. With `known` answers from an earlier
/// probe, only the window size is read; nothing is read from the terminal.
///
/// # Errors
///
/// Fails when there is no terminal, the plugin can't take the foreground,
/// or the terminal doesn't answer in time.
#[cfg(unix)]
pub fn probe(engine: &EngineInterface, known: Option<&Caps>) -> Result<Caps, String> {
    unix::probe(engine, known)
}

/// Probe the console nushell runs in, as on Unix but through the console
/// API: Windows Terminal answers the same queries.
///
/// # Errors
///
/// Fails when there is no console or the terminal doesn't answer in time.
#[cfg(windows)]
pub fn probe(engine: &EngineInterface, known: Option<&Caps>) -> Result<Caps, String> {
    windows::probe(engine, known)
}

/// Terminal probing isn't written for this platform yet.
///
/// # Errors
///
/// Always.
#[cfg(not(any(unix, windows)))]
pub fn probe(_engine: &EngineInterface, _known: Option<&Caps>) -> Result<Caps, String> {
    Err("terminal probing isn't supported on this platform yet".into())
}

#[cfg(unix)]
mod unix {
    use std::fs::{File, OpenOptions};
    use std::io::{IsTerminal as _, Read as _, Write as _};
    use std::os::fd::AsRawFd as _;
    use std::time::{Duration, Instant};

    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD as BASE64;
    use nu_plugin::EngineInterface;

    use super::{Caps, parse};
    use crate::kitty;

    /// How long to wait for answers. Terminals answer in a few
    /// milliseconds; only one that ignores the device-attributes query waits
    /// this long.
    const TIMEOUT: Duration = Duration::from_millis(1000);

    /// Kitty graphics query, cell size, text area size, and background
    /// color. The temporary-file query and device attributes, which every
    /// terminal answers, go after it.
    const QUERY: &[u8] =
        b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[16t\x1b[14t\x1b]11;?\x1b\\";

    /// Device attributes, the last query.
    const DEVICE_ATTRIBUTES: &[u8] = b"\x1b[c";

    pub fn probe(engine: &EngineInterface, known: Option<&Caps>) -> Result<Caps, String> {
        // In local-socket mode the plugin's stdin is nushell's. When that
        // isn't a terminal, nushell can't hand the terminal over either.
        if !engine.is_using_stdio() && !std::io::stdin().is_terminal() {
            return Err("stdin is not a terminal".into());
        }
        let mut tty = OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/tty")
            .map_err(|err| format!("no terminal: {err}"))?;
        if let Some(known) = known {
            let size = window_size(&tty)?;
            return Ok(Caps {
                columns: u32::from(size.ws_col),
                rows: u32::from(size.ws_row),
                ..known.clone()
            });
        }
        let _foreground = engine
            .enter_foreground()
            .map_err(|err| format!("couldn't take the terminal's foreground: {err}"))?;
        let fd = tty.as_raw_fd();
        // SAFETY: plain libc calls on a descriptor this function holds open.
        let (owner, us) = unsafe { (libc::tcgetpgrp(fd), libc::getpgrp()) };
        if owner != us {
            // Reading now would stop the plugin with SIGTTIN.
            return Err("the plugin isn't in the terminal's foreground".into());
        }

        let size = window_size(&tty)?;

        // A three-byte image in a temporary file: a terminal on this
        // machine reads it and deletes it.
        let probe_file = kitty::temp_path();
        let mut query = QUERY.to_vec();
        if std::fs::write(&probe_file, [0u8; 3]).is_ok() {
            query.extend_from_slice(b"\x1b_Gi=33,s=1,v=1,a=q,t=t,f=24;");
            query.extend_from_slice(
                BASE64
                    .encode(probe_file.as_os_str().as_encoded_bytes())
                    .as_bytes(),
            );
            query.extend_from_slice(b"\x1b\\");
        }
        query.extend_from_slice(DEVICE_ATTRIBUTES);
        let replies = {
            let _raw = RawMode::enter(&tty)?;
            tty.write_all(&query)
                .and_then(|()| tty.flush())
                .map_err(|err| format!("writing to the terminal: {err}"))?;
            read_replies(&mut tty)?
        };
        if probe_file.exists() {
            // The terminal didn't take it. It's our own file from above.
            let _ = std::fs::remove_file(&probe_file);
        }
        let mut caps = parse(&replies);
        if std::env::var_os("TMUX").is_some() {
            // tmux answers for itself: it may report sixel when the terminal
            // it draws on has none, and then shows nothing. It doesn't pass
            // kitty graphics on.
            caps.kitty = false;
            caps.temp_files = false;
            caps.sixel = caps.sixel && tmux_terminal_has_sixel();
        }
        caps.columns = u32::from(size.ws_col);
        caps.rows = u32::from(size.ws_row);
        if caps.cell.is_none() && size.ws_xpixel > 0 && size.ws_col > 0 && size.ws_row > 0 {
            caps.cell = Some((
                u32::from(size.ws_xpixel) / u32::from(size.ws_col),
                u32::from(size.ws_ypixel) / u32::from(size.ws_row),
            ));
        }
        if super::device_attributes(&replies).is_none() {
            return Err("the terminal didn't answer".into());
        }
        Ok(caps)
    }

    /// Whether the terminal tmux draws on takes sixel images, from tmux's
    /// own record of that terminal's features.
    fn tmux_terminal_has_sixel() -> bool {
        std::process::Command::new("tmux")
            .args(["display-message", "-p", "#{client_termfeatures}"])
            .output()
            .is_ok_and(|output| {
                output.status.success()
                    && String::from_utf8_lossy(&output.stdout)
                        .split(',')
                        .any(|feature| feature.trim() == "sixel")
            })
    }

    /// The window's size, in cells and, when the terminal says, pixels.
    fn window_size(tty: &File) -> Result<libc::winsize, String> {
        // SAFETY: `winsize` is plain data that TIOCGWINSZ fills in, on a
        // descriptor the caller holds open.
        unsafe {
            let mut size: libc::winsize = std::mem::zeroed();
            (libc::ioctl(tty.as_raw_fd(), libc::TIOCGWINSZ, &mut size) == 0).then_some(size)
        }
        .ok_or_else(|| "couldn't read the window size".to_string())
    }

    /// The terminal in raw mode until dropped.
    struct RawMode {
        fd: i32,
        saved: libc::termios,
    }

    impl RawMode {
        fn enter(tty: &File) -> Result<RawMode, String> {
            let fd = tty.as_raw_fd();
            // SAFETY: `termios` is plain data that tcgetattr fills in.
            unsafe {
                let mut saved: libc::termios = std::mem::zeroed();
                if libc::tcgetattr(fd, &mut saved) != 0 {
                    return Err("couldn't read the terminal's mode".into());
                }
                let mut raw = saved;
                libc::cfmakeraw(&mut raw);
                if libc::tcsetattr(fd, libc::TCSANOW, &raw) != 0 {
                    return Err("couldn't put the terminal in raw mode".into());
                }
                Ok(RawMode { fd, saved })
            }
        }
    }

    impl Drop for RawMode {
        fn drop(&mut self) {
            // SAFETY: restores the mode read in `enter` on the same descriptor.
            unsafe {
                libc::tcsetattr(self.fd, libc::TCSADRAIN, &self.saved);
            }
        }
    }

    /// Read until the device attributes arrive or the time is up. Uses
    /// `select`, since macOS can't `poll` a terminal.
    fn read_replies(tty: &mut File) -> Result<Vec<u8>, String> {
        let fd = tty.as_raw_fd();
        let deadline = Instant::now() + TIMEOUT;
        let mut replies = Vec::new();
        let mut chunk = [0u8; 1024];
        while super::device_attributes(&replies).is_none() {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            let mut wait = libc::timeval {
                tv_sec: left.as_secs() as libc::time_t,
                tv_usec: left.subsec_micros() as libc::suseconds_t,
            };
            // SAFETY: `fd_set` is plain data set up by FD_ZERO and FD_SET
            // for one open descriptor below FD_SETSIZE.
            let ready = unsafe {
                let mut readable: libc::fd_set = std::mem::zeroed();
                libc::FD_ZERO(&mut readable);
                libc::FD_SET(fd, &mut readable);
                libc::select(
                    fd + 1,
                    &mut readable,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    &mut wait,
                )
            };
            if ready < 0 {
                let err = std::io::Error::last_os_error();
                if err.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(format!("waiting for the terminal: {err}"));
            }
            if ready == 0 {
                break;
            }
            let read = tty
                .read(&mut chunk)
                .map_err(|err| format!("reading from the terminal: {err}"))?;
            if read == 0 {
                break;
            }
            replies.extend_from_slice(&chunk[..read]);
        }
        Ok(replies)
    }
}

/// The capabilities in the terminal's answers.
#[cfg(windows)]
mod windows {
    use std::fs::{File, OpenOptions};
    use std::io::{IsTerminal as _, Write as _};
    use std::os::windows::io::AsRawHandle as _;
    use std::time::{Duration, Instant};

    use nu_plugin::EngineInterface;
    use windows_sys::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Console::{
        CONSOLE_MODE, CONSOLE_SCREEN_BUFFER_INFO, ENABLE_VIRTUAL_TERMINAL_INPUT, GetConsoleMode,
        GetConsoleScreenBufferInfo, INPUT_RECORD, KEY_EVENT, ReadConsoleInputW, SetConsoleMode,
    };
    use windows_sys::Win32::System::Threading::WaitForSingleObject;

    use super::{Caps, parse};

    /// How long to wait for answers, as on Unix.
    const TIMEOUT: Duration = Duration::from_millis(1000);

    /// Kitty graphics query (WezTerm has them), cell size, text area size,
    /// background color, and device attributes, which every terminal
    /// answers. There is no temporary-file query: Windows terminals don't
    /// read images from files.
    const QUERY: &[u8] =
        b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[16t\x1b[14t\x1b]11;?\x1b\\\x1b[c";

    pub fn probe(_engine: &EngineInterface, known: Option<&Caps>) -> Result<Caps, String> {
        if !std::io::stdin().is_terminal() {
            return Err("stdin is not a console".into());
        }
        let input = OpenOptions::new()
            .read(true)
            .write(true)
            .open("CONIN$")
            .map_err(|err| format!("no console input: {err}"))?;
        let mut output = OpenOptions::new()
            .read(true)
            .write(true)
            .open("CONOUT$")
            .map_err(|err| format!("no console output: {err}"))?;
        let (columns, rows) = window_size(&output)?;
        if let Some(known) = known {
            return Ok(Caps {
                columns,
                rows,
                ..known.clone()
            });
        }
        let replies = {
            let _vt = VtInput::enter(&input)?;
            output
                .write_all(QUERY)
                .and_then(|()| output.flush())
                .map_err(|err| format!("writing to the console: {err}"))?;
            read_replies(&input)?
        };
        if super::device_attributes(&replies).is_none() {
            return Err("the terminal didn't answer".into());
        }
        let mut caps = parse(&replies);
        caps.temp_files = false;
        caps.columns = columns;
        caps.rows = rows;
        Ok(caps)
    }

    /// The visible window, in cells.
    fn window_size(output: &File) -> Result<(u32, u32), String> {
        let mut info: CONSOLE_SCREEN_BUFFER_INFO = unsafe { std::mem::zeroed() };
        // SAFETY: `info` is plain data the call fills in, and the handle is
        // the caller's open console output.
        let ok = unsafe { GetConsoleScreenBufferInfo(output.as_raw_handle() as HANDLE, &mut info) };
        if ok == 0 {
            return Err("couldn't read the console's size".into());
        }
        let window = info.srWindow;
        let span =
            |from: i16, to: i16| u32::try_from(i32::from(to) - i32::from(from) + 1).unwrap_or(0);
        Ok((
            span(window.Left, window.Right),
            span(window.Top, window.Bottom),
        ))
    }

    /// Console input that delivers terminal replies as text, until dropped:
    /// no line editing, no echo, and escape sequences passed through.
    struct VtInput {
        handle: HANDLE,
        saved: CONSOLE_MODE,
    }

    impl VtInput {
        fn enter(input: &File) -> Result<VtInput, String> {
            let handle = input.as_raw_handle() as HANDLE;
            let mut saved: CONSOLE_MODE = 0;
            // SAFETY: plain console calls on the caller's open input handle.
            unsafe {
                if GetConsoleMode(handle, &mut saved) == 0 {
                    return Err("couldn't read the console's mode".into());
                }
                if SetConsoleMode(handle, ENABLE_VIRTUAL_TERMINAL_INPUT) == 0 {
                    return Err("couldn't switch the console to terminal input".into());
                }
            }
            Ok(VtInput { handle, saved })
        }
    }

    impl Drop for VtInput {
        fn drop(&mut self) {
            // SAFETY: restores the mode read in `enter` on the same handle.
            unsafe {
                SetConsoleMode(self.handle, self.saved);
            }
        }
    }

    /// Read until the device attributes arrive or the time is up. Replies
    /// come in as key events, one character each.
    fn read_replies(input: &File) -> Result<Vec<u8>, String> {
        let handle = input.as_raw_handle() as HANDLE;
        let deadline = Instant::now() + TIMEOUT;
        let mut replies = Vec::new();
        let mut records = [INPUT_RECORD::default(); 64];
        while super::device_attributes(&replies).is_none() {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            let wait = u32::try_from(left.as_millis()).unwrap_or(u32::MAX);
            // SAFETY: waits on the caller's open console input handle.
            if unsafe { WaitForSingleObject(handle, wait) } != WAIT_OBJECT_0 {
                break;
            }
            let mut read = 0u32;
            // SAFETY: `records` has room for as many records as it asks for.
            let ok = unsafe {
                ReadConsoleInputW(
                    handle,
                    records.as_mut_ptr(),
                    records.len() as u32,
                    &mut read,
                )
            };
            if ok == 0 {
                return Err("couldn't read the console's input".into());
            }
            let mut units = Vec::new();
            for record in records.iter().take(read as usize) {
                if u32::from(record.EventType) != KEY_EVENT {
                    continue;
                }
                // SAFETY: a KEY_EVENT record holds a key event.
                let key = unsafe { record.Event.KeyEvent };
                // SAFETY: with ReadConsoleInputW, the character is UTF-16.
                let unit = unsafe { key.uChar.UnicodeChar };
                if key.bKeyDown != 0 && unit != 0 {
                    units.push(unit);
                }
            }
            replies.extend_from_slice(String::from_utf16_lossy(&units).as_bytes());
        }
        Ok(replies)
    }
}

#[cfg(any(unix, windows, test))]
fn parse(replies: &[u8]) -> Caps {
    let text = String::from_utf8_lossy(replies);
    Caps {
        kitty: text.contains("\x1b_Gi=31;OK"),
        temp_files: text.contains("\x1b_Gi=33;OK"),
        sixel: device_attributes(replies).is_some_and(|params| {
            params
                .split(|byte| *byte == b';')
                .any(|param| param == b"4")
        }),
        cell: report(&text, "\x1b[6;").map(|(height, width)| (width, height)),
        background: background(&text),
        ..Caps::default()
    }
}

/// The two numbers in an XTWINOPS answer, `CSI <prefix> a ; b t`.
#[cfg(any(unix, windows, test))]
fn report(text: &str, prefix: &str) -> Option<(u32, u32)> {
    let start = text.find(prefix)? + prefix.len();
    let rest = &text[start..];
    let (a, b) = rest[..rest.find('t')?].split_once(';')?;
    let (a, b) = (a.parse().ok()?, b.parse().ok()?);
    (a > 0 && b > 0).then_some((a, b))
}

/// The color in an OSC 11 answer, `OSC 11 ; rgb:RRRR/GGGG/BBBB`.
#[cfg(any(unix, windows, test))]
fn background(text: &str) -> Option<[u8; 3]> {
    let start = text.find("\x1b]11;rgb:")? + "\x1b]11;rgb:".len();
    let rest = &text[start..];
    let end = rest.find(['\x1b', '\x07'])?;
    let mut channels = rest[..end].split('/').map(|hex| {
        let value = u32::from_str_radix(hex, 16).ok()?;
        let max = (1u32 << (4 * hex.len() as u32)) - 1;
        Some((value * 255 / max.max(1)) as u8)
    });
    Some([channels.next()??, channels.next()??, channels.next()??])
}

/// The parameters of the primary device attributes, `CSI ? … c`.
#[cfg(any(unix, windows, test))]
fn device_attributes(replies: &[u8]) -> Option<&[u8]> {
    replies.windows(3).enumerate().find_map(|(start, window)| {
        if window != b"\x1b[?" {
            return None;
        }
        let params = &replies[start + 3..];
        let end = params
            .iter()
            .position(|byte| !(byte.is_ascii_digit() || *byte == b';'))?;
        (params[end] == b'c').then_some(&params[..end])
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_rex_answers() {
        let caps = parse(b"\x1b_Gi=31;OK\x1b\\\x1b[6;16;8t\x1b[4;384;640t\x1b[?62;22c");
        assert!(caps.kitty);
        assert!(!caps.sixel);
        assert_eq!(caps.cell, Some((8, 16)));
        assert_eq!(caps.background, None);
    }

    #[test]
    fn reads_sixel_and_background() {
        let caps = parse(b"\x1b]11;rgb:1e1e/1e1e/2e2e\x1b\\\x1b[?62;4;22c");
        assert!(!caps.kitty);
        assert!(caps.sixel);
        assert_eq!(caps.background, Some([0x1e, 0x1e, 0x2e]));
    }
}
