//! TTY abstraction: termios raw/restore + window size.
//!
//! Upstream delegates this whole layer to the Node host
//! (`process.stdin.setRawMode`, `stdout.columns`); the Rust port needs
//! it in-library, implemented over rustix (safe, no non-Rust transitive
//! deps — Phase A design ruling #4).

use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};

/// A saved termios snapshot returned by [`TtyDevice::raw_mode`] and
/// consumed by [`TtyDevice::restore`] — the counterpart of the Node
/// host's implicit cooked-mode restore.
#[derive(Clone, Debug)]
pub struct TermiosSave {
    pub(crate) attrs: rustix::termios::Termios,
}

impl TermiosSave {
    pub(crate) fn new(attrs: rustix::termios::Termios) -> Self {
        Self { attrs }
    }

    /// A synthetic snapshot for tests / non-termios devices. The mock
    /// is never fed back into a real `tcsetattr` — mock devices'
    /// `restore` is a stub.
    ///
    /// SAFETY-free note: rustix `Termios` derives no `Default`; a
    /// zeroed termios is a valid all-zero struct (every field raw
    /// integer/array), used only as a placeholder.
    #[must_use]
    pub fn mock() -> Self {
        Self {
            attrs: unsafe { std::mem::zeroed() },
        }
    }
}

/// The TTY seam (Phase A design: trait so PTY pairs (A3) and mocks can
/// substitute the real device).
pub trait TtyDevice {
    /// Enter raw mode, returning the snapshot [`TtyDevice::restore`]
    /// expects later. Mirrors Node's `setRawMode(true)`.
    fn raw_mode(&mut self) -> io::Result<TermiosSave>;
    /// Restore cooked mode from a snapshot (`setRawMode(false)`).
    fn restore(&mut self, save: &TermiosSave) -> io::Result<()>;
    /// The fd input pumping (A2) reads from.
    fn input_fd(&self) -> BorrowedFd<'_>;
    /// `TIOCGWINSZ` — (`columns`, `rows`), Node's `stdout.columns/rows`.
    fn window_size(&self) -> io::Result<(u16, u16)>;
    /// The fd a panic hook may restore cooked mode on, for the
    /// global panic-registry; None keeps the device out of it
    /// (mocks, virtual devices).
    fn panic_restore_fd(&self) -> Option<i32> {
        None
    }
}

/// The real Unix terminal: `/dev/tty` when openable (oc-tui's
/// crossterm equivalent does the same via its tty driver), falling
/// back to a dup of stdin (or stdout) so the fd we hold is owned and
/// its drop never closes the process-global descriptor.
pub struct UnixTty {
    fd: std::os::fd::OwnedFd,
}

impl UnixTty {
    #[allow(clippy::new_without_default)]
    pub fn new() -> io::Result<Self> {
        let fd = match rustix::fs::open(
            "/dev/tty",
            rustix::fs::OFlags::RDWR,
            rustix::fs::Mode::empty(),
        ) {
            Ok(owned) => owned,
            Err(_) => {
                let raw = if rustix::termios::isatty(unsafe {
                    BorrowedFd::borrow_raw(0)
                }) {
                    0
                } else {
                    1
                };
                // SAFETY: fd 0/1 are open for the process lifetime.
                rustix::io::dup(unsafe { BorrowedFd::borrow_raw(raw) })?
            }
        };
        Ok(Self { fd })
    }

    fn fd_ref(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
}

impl TtyDevice for UnixTty {
    fn raw_mode(&mut self) -> io::Result<TermiosSave> {
        let fd = self.fd_ref();
        let attrs = rustix::termios::tcgetattr(fd)?;
        let mut raw = attrs.clone();
        // cfmakeraw-equivalent, the exact set crossterm/Node setRawMode
        // apply: clear BRKINT|PARMRK|ISTRIP|INLCR|IGNCR|ICRNL|IXON,
        // clear OPOST, clear ECHO|ECHONL|ICANON|ISIG|IEXTEN, CS8.
        raw.input_modes &= !(rustix::termios::InputModes::BRKINT
            | rustix::termios::InputModes::PARMRK
            | rustix::termios::InputModes::ISTRIP
            | rustix::termios::InputModes::INLCR
            | rustix::termios::InputModes::IGNCR
            | rustix::termios::InputModes::ICRNL
            | rustix::termios::InputModes::IXON);
        raw.output_modes &= !rustix::termios::OutputModes::OPOST;
        raw.local_modes &= !(rustix::termios::LocalModes::ECHO
            | rustix::termios::LocalModes::ECHONL
            | rustix::termios::LocalModes::ICANON
            | rustix::termios::LocalModes::ISIG
            | rustix::termios::LocalModes::IEXTEN);
        raw.control_modes |= rustix::termios::ControlModes::CS8;
        rustix::termios::tcsetattr(
            fd,
            rustix::termios::OptionalActions::Now,
            &raw,
        )?;
        Ok(TermiosSave::new(attrs))
    }

    fn restore(&mut self, save: &TermiosSave) -> io::Result<()> {
        rustix::termios::tcsetattr(
            self.fd_ref(),
            rustix::termios::OptionalActions::Now,
            &save.attrs,
        )?;
        Ok(())
    }

    fn input_fd(&self) -> BorrowedFd<'_> {
        self.fd_ref()
    }

    fn window_size(&self) -> io::Result<(u16, u16)> {
        let ws = rustix::termios::tcgetwinsize(self.fd_ref())?;
        Ok((ws.ws_col, ws.ws_row))
    }

    fn panic_restore_fd(&self) -> Option<i32> {
        Some(self.fd_ref().as_raw_fd())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::fs::File;

    /// MockTty for byte-level lifecycle tests (real PTY pairs are A3).
    pub(crate) struct MockTty {
        pub file: File,
        pub raw_calls: u32,
        pub restore_calls: u32,
    }

    impl MockTty {
        pub(crate) fn new() -> Self {
            Self {
                file: File::create("/dev/null").unwrap(),
                raw_calls: 0,
                restore_calls: 0,
            }
        }
    }

    impl TtyDevice for MockTty {
        fn raw_mode(&mut self) -> io::Result<TermiosSave> {
            self.raw_calls += 1;
            Ok(TermiosSave::mock())
        }
        fn restore(&mut self, _save: &TermiosSave) -> io::Result<()> {
            self.restore_calls += 1;
            Ok(())
        }
        fn input_fd(&self) -> BorrowedFd<'_> {
            self.file.as_fd()
        }
        fn window_size(&self) -> io::Result<(u16, u16)> {
            Ok((80, 24))
        }
    }
}
