use std::{
    ffi::CStr,
    fs::File,
    io::{self, stdout, BufRead, BufReader, Read, Stdout, Write},
    mem::ManuallyDrop,
    os::unix::prelude::FromRawFd,
    process,
    sync::{Arc, Mutex, Weak},
    thread::{self, JoinHandle},
};

use config::RlwrapConfig;
use libc::{STDERR_FILENO as STDERR, STDIN_FILENO as STDIN, STDOUT_FILENO as STDOUT};
use termion::{
    event::{Event, Key},
    raw::{IntoRawMode, RawTerminal},
};

pub mod config;

#[cfg(target_family = "windows")]
compile_error!("Not implemented on windows");

/// Previous terminal state.
/// This is static so the application can try revert it when a panic ocurs.
pub static RAW_TERMINAL_STATE: Mutex<Option<RawTerminal<Stdout>>> = Mutex::new(None);

/// Readline prompt struct.
/// This struct will be shared across two threads,
/// one that will read stdin and one that will write to stdout.
pub struct Rlwrap {
    is_running: bool,

    /// Original stdin file descriptor.
    original_stdin: i32,
    /// Original stdout file descriptor.
    original_stdout: i32,
    /// Original stderr file descriptor.
    original_stderr: i32,

    /// Terminal created.
    pty: i32,

    /// Original output.
    /// This is a file struct used to write data to the original terminal
    /// and wrapped in ManuallyDrop to avoid closing the original fd.
    original_output_file: Option<ManuallyDrop<File>>,

    pub out_thread: Option<JoinHandle<()>>,

    /// Configuration for rlwrap.
    pub config: RlwrapConfig,

    /// The current buffer being edited.
    pub buffer: String,

    /// Cursor position in the buffer
    pub cursor: u16,

    /// Terminal size (rows, cols).
    pub terminal_size: (u16, u16),
}

impl Rlwrap {
    /// Sets up the pseudo terminal and make the dup/dup2 syscalls.
    pub fn setup(config: RlwrapConfig) -> Result<Arc<Mutex<Self>>, io::Error> {
        // Turn raw mode
        let raw_term = stdout().into_raw_mode()?;

        if let Ok(mut guard) = RAW_TERMINAL_STATE.lock() {
            *guard = Some(raw_term);
        } else {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "Failed to aquire RAW_TERMINAL_STATE lock",
            ));
        }

        let original_stdin = dup(STDIN)?;
        let original_stdout = dup(STDOUT)?;
        let original_stderr = dup(STDERR)?;

        let original_output_file = ManuallyDrop::new(unsafe { File::from_raw_fd(original_stdout) });

        let pty = open_pty(libc::O_RDWR)?;
        grantpt(pty)?;
        unlockpt(pty)?;
        let pty_name = pty_name(pty)
            .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "Failed to get pty name"))?;
        let pty_child = open_file(&pty_name, libc::O_RDWR)?;

        let rlwrap = Arc::new(Mutex::new(Self {
            is_running: true,
            pty,
            original_output_file: Some(original_output_file),
            original_stdin,
            original_stdout,
            original_stderr,
            config,
            out_thread: None,
            buffer: String::new(),
            cursor: 0,
            terminal_size: termion::terminal_size()?,
        }));

        let out_thread = output_pipe_thread(Arc::downgrade(&rlwrap), pty);
        rlwrap.lock().unwrap().out_thread = Some(out_thread);
        readline_thread(Arc::downgrade(&rlwrap), original_stdin, pty);

        dup2(pty_child, STDIN)?;
        dup2(pty_child, STDOUT)?;
        dup2(pty_child, STDERR)?;

        close_file(pty_child)?;

        rlwrap.lock().unwrap().redraw();

        Ok(rlwrap)
    }
    pub fn print(&mut self, s: &str) {
        if let Some(out) = &mut self.original_output_file {
            write!(out, "{}\r{s}\r\n", termion::clear::CurrentLine).ok();
            self.redraw();
        } else {
            // Readline probably has already been stopped, so just
            // print it to stdout.
            println!("{s}");
        }
    }
    pub fn redraw(&mut self) {
        if let Some(out) = &mut self.original_output_file {
            let cursor_x = (self.config.prefix.len() as u16) + self.cursor + 1;
            write!(
                out,
                "{}{}\r{}{}{}",
                termion::cursor::Goto(0, self.terminal_size.1),
                termion::clear::CurrentLine,
                &self.config.prefix,
                &self.buffer,
                termion::cursor::Goto(cursor_x, self.terminal_size.1),
            )
            .ok();
        }
    }
    /// Closes all the pipes created by rlwrap and restores stdin, stdout and stderr.
    /// Some messages may be still being processed by the output thread.
    /// If you want to wait for all messages to be printed, use Rlwrap::stop_gracefully.
    pub fn stop(&mut self) -> Result<(), io::Error> {
        if self.is_running {
            self.original_output_file.take();
            dup2(self.original_stdin, STDIN)?;
            dup2(self.original_stdout, STDOUT)?;
            dup2(self.original_stderr, STDERR)?;
            close_file(self.pty)?;
            close_file(self.original_stdin)?;
            close_file(self.original_stdout)?;
            close_file(self.original_stderr)?;
            if let Ok(mut guard) = RAW_TERMINAL_STATE.lock() {
                guard.take();
            }
            self.is_running = false;
            println!();
            Ok(())
        } else {
            Err(io::Error::new(io::ErrorKind::Other, "Not running"))
        }
    }

    /// Tries to gracefully stop the rlwrap prompt by waiting for the output thread.
    /// This function takes a Mutex instead of Self to be able to unlock it and make the
    /// thread lock it again.
    /// TODO: I should find a better way to do this :(
    pub fn stop_gracefully(rlwrap: &Mutex<Self>) -> Result<(), io::Error> {
        let mut lock = rlwrap.lock().unwrap();
        let out_thread = lock.out_thread.take();
        lock.stop()?;
        drop(lock);
        if let Some(t) = out_thread {
            t.join()
                .map_err(|_| io::Error::new(io::ErrorKind::Other, "Output thread failed"))?;
        }
        Ok(())
    }
}

impl Drop for Rlwrap {
    fn drop(&mut self) {
        self.stop().ok();
    }
}

fn readline_thread(rlwrap: Weak<Mutex<Rlwrap>>, from: i32, to: i32) -> JoinHandle<()> {
    thread::spawn(move || {
        let mut from = ManuallyDrop::new(unsafe { File::from_raw_fd(from) });
        let from_ref: &mut File = &mut from;
        let mut to = ManuallyDrop::new(unsafe { File::from_raw_fd(to) });
        let mut bytes = from_ref.bytes();
        while let Some(byte) = bytes.next() {
            if let Some(rlwrap) = rlwrap.upgrade() {
                if let Ok(byte) = byte {
                    let event = termion::event::parse_event(byte, &mut bytes);
                    if let Ok(event) = event {
                        if let Event::Key(k) = event {
                            let mut guard = rlwrap.lock().unwrap();
                            match k {
                                Key::Char(c) => {
                                    let cpos = guard.cursor as usize;
                                    guard.buffer.insert(cpos, c);
                                    guard.cursor += 1;
                                    if c == '\n' {
                                        if to.write_all(guard.buffer.as_bytes()).is_err() {
                                            break;
                                        }
                                        guard.buffer.clear();
                                        guard.cursor = 0;
                                    }
                                }
                                Key::Ctrl(c) => {
                                    if c == 'd' {
                                        guard.buffer.push(4u8 as char);
                                        if to.write_all(guard.buffer.as_bytes()).is_err() {
                                            break;
                                        }
                                        guard.buffer.clear();
                                        guard.cursor = 0;
                                    }
                                    if c == 'c' {
                                        if guard.config.stop_on_ctrl_c {
                                            guard.stop().unwrap();
                                        }
                                        if let Err(e) = kill(process::id() as i32, libc::SIGINT) {
                                            eprintln!("Failed to send interrupt signal: {e}");
                                        }
                                    }
                                }
                                Key::Backspace => {
                                    let cur = guard.cursor as usize;
                                    let blen = guard.buffer.len();
                                    if blen > 0 && cur <= blen {
                                        guard.buffer.remove(cur as usize - 1);
                                        guard.cursor -= 1;
                                    }
                                },
                                Key::Left => {
                                    if guard.cursor > 0 {
                                        guard.cursor -= 1;
                                    }
                                },
                                Key::Right => {
                                    if (guard.cursor as usize) < guard.buffer.len() {
                                        guard.cursor += 1;
                                    }
                                }
                                _ => {}
                            }
                            guard.redraw();
                        }
                    } else {
                        break;
                    }
                } else {
                    break;
                }
            } else {
                break;
            }
        }
    })
}

fn output_pipe_thread(rlwrap: Weak<Mutex<Rlwrap>>, from: i32) -> JoinHandle<()> {
    thread::spawn(move || {
        let mut from = ManuallyDrop::new(unsafe { File::from_raw_fd(from) });
        let file: &mut File = &mut from;
        for line in BufReader::new(file).lines() {
            if let Some(rlwrap) = rlwrap.upgrade() {
                if let Ok(line) = line {
                    let mut guard = rlwrap.lock().unwrap();
                    guard.print(&line);
                } else {
                    break;
                }
            } else {
                break;
            }
        }
    })
}

/// Wrapper around libc::dup
fn dup(fd: i32) -> Result<i32, io::Error> {
    let result = unsafe { libc::dup(fd) };
    if result == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(result)
    }
}

/// Wrapper around libc::dup2
fn dup2(src: i32, dest: i32) -> Result<i32, io::Error> {
    let result = unsafe { libc::dup2(src, dest) };
    if result == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(result)
    }
}

/// Wrapper around libc::kill
fn kill(pid: i32, sig: i32) -> Result<(), io::Error> {
    if unsafe { libc::kill(pid, sig) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Wrapper around libc::posix_openpt
fn open_pty(flags: i32) -> Result<i32, io::Error> {
    let result = unsafe { libc::posix_openpt(flags) };
    if result == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(result)
    }
}

/// Wrapper around libc::ptsname
fn pty_name(fd: i32) -> Option<String> {
    let result = unsafe { libc::ptsname(fd) };
    if result.is_null() {
        None
    } else {
        let string = unsafe { CStr::from_ptr(result) };
        Some(string.to_str().ok()?.to_string())
    }
}

/// Wrapper around libc::open
fn open_file(path: &str, flags: i32) -> Result<i32, io::Error> {
    let result = unsafe { libc::open(path.as_ptr() as *const i8, flags) };
    if result == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(result)
    }
}

/// Wrapper around libc::close
fn close_file(fd: i32) -> Result<(), io::Error> {
    let result = unsafe { libc::close(fd) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn grantpt(pty: i32) -> Result<(), io::Error> {
    if unsafe { libc::grantpt(pty) } != 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn unlockpt(pty: i32) -> Result<(), io::Error> {
    if unsafe { libc::unlockpt(pty) } != 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
