//! What differs between Linux and Windows for ognivo, kept in one place so that the rest
//! of the workspace says what it wants and not how each OS does it.
//!
//! - [`home`], [`private_dir`], [`private_file`], [`canonicalize`], [`path_bytes`] and
//!   [`path_from_bytes`]: where the user's files are, files only the user may read (modes
//!   0700 and 0600 on Unix; on Windows a new file takes its directory's ACL, the user's
//!   own in their profile), and paths as the OS spells them.
//! - [`program`], [`command`] and [`async_command`]: a program by its name, `npx` being
//!   `npx.cmd` on Windows; and children that open no console window there, with the
//!   environment a new terminal of the user gets ([`env`]), not the core's of when it
//!   started.
//! - [`env::decode_output`]: what a program printed, in the console's code page where it
//!   is not UTF-8.
//! - [`shell`]: bash, Git for Windows' on Windows, and paths as bash writes them.
//! - [`Tree`]: a child and all it starts, ended as a whole.
//! - [`signal`]: SIGINT, SIGTERM and SIGQUIT, and what stands for them on Windows.
//! - [`ipc`]: the core's socket, a Unix socket or a named pipe.
//!
//! Only Linux and Windows are built and tested; the Unix side is POSIX where it can be.

pub mod env;
pub mod ipc;
pub mod shell;
pub mod signal;
mod tree;

use std::borrow::Cow;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub use tree::{Stop, Tree};

/// Opens `url` in the user's browser, if there is one to open it in: `$BROWSER`, as other
/// tools take it, or the desktop's opener (`xdg-open`; on Windows the URL protocol's
/// handler, which no shell parses, so a `&` in the URL is the URL's). Whoever calls it
/// shows the URL too, for when there is none.
pub fn open_in_browser(url: &str) {
    let browser = std::env::var_os("BROWSER").filter(|b| !b.is_empty());
    let mut cmd = match browser {
        Some(b) => command(b),
        #[cfg(windows)]
        None => {
            let mut c = command("rundll32.exe");
            c.arg("url.dll,FileProtocolHandler");
            c
        }
        #[cfg(target_os = "macos")]
        None => command("open"),
        #[cfg(not(any(windows, target_os = "macos")))]
        None => command("xdg-open"),
    };
    let _ = cmd
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

/// A terminal's mode as it was, to give back when ognivo is stopped while a line editor
/// holds the terminal in a mode of its own: termios on Unix. Windows keeps nothing: what
/// stops ognivo there is its console closing.
#[derive(Debug)]
pub struct TerminalMode(#[cfg(unix)] rustix::termios::Termios);

impl TerminalMode {
    /// Stdin's mode, when stdin is a terminal.
    pub fn save() -> Option<Self> {
        #[cfg(unix)]
        let saved = rustix::termios::tcgetattr(std::io::stdin()).ok().map(Self);
        #[cfg(not(unix))]
        let saved = None;
        saved
    }

    pub fn restore(&self) {
        #[cfg(unix)]
        let _ = rustix::termios::tcsetattr(
            std::io::stdin(),
            rustix::termios::OptionalActions::Now,
            &self.0,
        );
    }
}

/// Whether the OS's file names match in any case, as Windows' do: `.GIT` is `.git` there.
pub const PATHS_IGNORE_CASE: bool = cfg!(windows);

/// A path with `/` between its parts, as globs and the window write paths: on Unix as it
/// is (where `\` may be in a name); on Windows each `\` a `/`.
pub fn slashed(path: &Path) -> String {
    let shown = path.to_string_lossy();
    #[cfg(windows)]
    let shown = std::borrow::Cow::<str>::Owned(shown.replace('\\', "/"));
    shown.into_owned()
}

/// `path` with `/` between its parts on Windows, exactly, for a path that stays a path:
/// the model reads and writes paths in JSON, where a `\` is an escape. On Unix as it is.
pub fn with_slashes(path: PathBuf) -> PathBuf {
    #[cfg(windows)]
    let path = {
        use std::os::windows::ffi::{OsStrExt as _, OsStringExt as _};
        let units: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .map(|u| {
                if u == u16::from(b'\\') {
                    u16::from(b'/')
                } else {
                    u
                }
            })
            .collect();
        PathBuf::from(OsString::from_wide(&units))
    };
    path
}

/// The home directory ognivo works in, where `.ognivo` and Claude Code's `.claude` are:
/// `$OGNIVO_HOME` when it is set (tests, a second profile), else the user's: `$HOME` on
/// Unix; on Windows `%USERPROFILE%`, as the window (Node's `os.homedir()`) and Claude Code
/// have it, or `$HOME` where that is not set. The window reads `OGNIVO_HOME` too.
pub fn home() -> Option<PathBuf> {
    let var = |k: &str| {
        std::env::var_os(k)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    #[cfg(windows)]
    let user = var("USERPROFILE").or_else(|| var("HOME"));
    #[cfg(not(windows))]
    let user = var("HOME");
    var("OGNIVO_HOME").or(user)
}

/// Creates `dir` and its parents; on Unix only the user may enter those it creates.
pub fn private_dir(dir: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)
}

/// Options that create a file only the user may read and write: mode 0600 on Unix; on
/// Windows a new file takes the ACL of its directory, which in the profile is the user's.
pub fn private_file() -> fs::OpenOptions {
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut options = fs::OpenOptions::new();
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options
}

/// A file to hold a lock on (`File::lock`, `File::try_lock`): created when missing, the
/// user's alone, and left as it is. It is opened for writing: Windows locks only through
/// a handle that may read or write, and one that only appends may not, so its lock would
/// fail as if another process held it.
pub fn lock_file(path: &Path) -> io::Result<fs::File> {
    private_file()
        .create(true)
        .write(true)
        .truncate(false)
        .open(path)
}

/// `fs::canonicalize`, without the `\\?\` that Windows puts in front of a path which
/// does not need it, and which neither a model nor git nor the user would know as theirs.
pub fn canonicalize(path: impl AsRef<Path>) -> io::Result<PathBuf> {
    let full = fs::canonicalize(path)?;
    #[cfg(windows)]
    let full = plain(full);
    Ok(full)
}

/// `\\?\C:\a` as `C:\a` and `\\?\UNC\host\share\a` as `\\host\share\a`, where the plain
/// path means the same: every part a name that needs no `\\?\` (none reserved like `NUL`
/// or `CON.txt`, none ending in a dot or a space, no character a plain path cannot have),
/// and shorter than the 260 characters of a plain path.
#[cfg(any(windows, test))]
fn plain(path: PathBuf) -> PathBuf {
    let Some(s) = path.to_str() else {
        return path;
    };
    let (lead, rest, named) = if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        (r"\\", rest, 0)
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        let b = rest.as_bytes();
        if !(b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\') {
            return path;
        }
        ("", rest, 1)
    } else {
        return path;
    };
    let parts: Vec<&str> = rest.split('\\').collect();
    // `C:\` is the drive's root: its one part after the drive is empty
    let root = named == 1 && parts.len() == 2 && parts[1].is_empty();
    let ok =
        lead.len() + rest.len() < 260 && (root || parts.iter().skip(named).all(|p| plain_name(p)));
    if ok {
        PathBuf::from(format!("{lead}{rest}"))
    } else {
        path
    }
}

#[cfg(any(windows, test))]
fn plain_name(part: &str) -> bool {
    let reserved = |stem: &str| {
        let stem = stem.trim_end_matches(' ').to_ascii_uppercase();
        matches!(
            stem.as_str(),
            "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
        ) || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes()[3].is_ascii_digit())
    };
    !part.is_empty()
        && !part.ends_with(['.', ' '])
        && !part.contains(|c: char| c < ' ' || "<>:\"/|?*".contains(c))
        && !reserved(part.split('.').next().unwrap_or(part))
}

/// A path as bytes to keep, which [`path_from_bytes`] makes the same path again: its bytes
/// on Unix; on Windows its UTF-16 as WTF-8, which is UTF-8 for every path that is valid
/// Unicode and keeps even a lone surrogate.
pub fn path_bytes(path: &Path) -> Cow<'_, [u8]> {
    #[cfg(unix)]
    let bytes = Cow::Borrowed(std::os::unix::ffi::OsStrExt::as_bytes(path.as_os_str()));
    #[cfg(windows)]
    let bytes = Cow::Owned(wtf8::encode(std::os::windows::ffi::OsStrExt::encode_wide(
        path.as_os_str(),
    )));
    bytes
}

/// The path of [`path_bytes`]; also a path a program printed, on Windows in UTF-8 (git
/// does so), where what is not UTF-8 becomes U+FFFD.
pub fn path_from_bytes(bytes: Vec<u8>) -> PathBuf {
    #[cfg(unix)]
    let os = <OsString as std::os::unix::ffi::OsStringExt>::from_vec(bytes);
    #[cfg(windows)]
    let os = <OsString as std::os::windows::ffi::OsStringExt>::from_wide(&wtf8::decode(&bytes));
    PathBuf::from(os)
}

/// WTF-8 (simonsapin.github.io/wtf-8): UTF-8 that also carries the lone surrogates a
/// Windows file name may have.
#[cfg(any(windows, test))]
mod wtf8 {
    pub fn encode(units: impl IntoIterator<Item = u16>) -> Vec<u8> {
        let mut out = Vec::new();
        for c in char::decode_utf16(units) {
            let c = c.map_or_else(|e| u32::from(e.unpaired_surrogate()), u32::from);
            let tail = |shift: u32| 0x80 | ((c >> shift) & 0x3f) as u8;
            match c {
                0..=0x7f => out.push(c as u8),
                0x80..=0x7ff => out.extend([0xc0 | (c >> 6) as u8, tail(0)]),
                0x800..=0xffff => out.extend([0xe0 | (c >> 12) as u8, tail(6), tail(0)]),
                _ => out.extend([0xf0 | (c >> 18) as u8, tail(12), tail(6), tail(0)]),
            }
        }
        out
    }

    /// The UTF-16 of `bytes`; what is not WTF-8, which `encode` never writes, comes back
    /// as U+FFFD, a byte at a time.
    pub fn decode(bytes: &[u8]) -> Vec<u16> {
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            let (len, min, lead) = match bytes[i] {
                b @ 0..=0x7f => (1, 0, u32::from(b)),
                b @ 0xc2..=0xdf => (2, 0x80, u32::from(b & 0x1f)),
                b @ 0xe0..=0xef => (3, 0x800, u32::from(b & 0x0f)),
                b @ 0xf0..=0xf4 => (4, 0x1_0000, u32::from(b & 0x07)),
                _ => (0, 0, 0),
            };
            let tail = bytes.get(i + 1..i + len.max(1)).unwrap_or_default();
            let c = (len > 0 && tail.len() + 1 == len && tail.iter().all(|b| b & 0xc0 == 0x80))
                .then(|| {
                    tail.iter()
                        .fold(lead, |c, b| (c << 6) | u32::from(b & 0x3f))
                })
                .filter(|&c| c >= min && c <= 0x10_ffff);
            match c {
                Some(c) if c >= 0x1_0000 => {
                    let c = c - 0x1_0000;
                    out.extend([0xd800 | (c >> 10) as u16, 0xdc00 | (c & 0x3ff) as u16]);
                }
                Some(c) => out.push(c as u16),
                None => out.push(0xfffd),
            }
            i += if c.is_some() { len } else { 1 };
        }
        out
    }
}

/// The program `name` names, for a command to start: `name` on Unix; on Windows, where a
/// bare name finds only an `.exe`, the first file of `name` with an extension of
/// `%PATHEXT%` that a command can start (`.exe`, `.com`, `.bat`, `.cmd`) in the
/// directories of `%PATH%`, so that `npx` is `npx.cmd`. A name with a directory or an
/// extension, or one not found, stays `name`, for the error to name it.
pub fn program(name: impl AsRef<OsStr>) -> PathBuf {
    let name = name.as_ref();
    // the PATH the program will have: a new terminal's on Windows (env.rs)
    #[cfg(windows)]
    if let Some(found) = on_path(
        name,
        &env::var("PATH").unwrap_or_default(),
        &env::var("PATHEXT")
            .map(|v| v.to_string_lossy().into_owned())
            .unwrap_or_default(),
    ) {
        return found;
    }
    PathBuf::from(name)
}

/// Whether `name` is a program on the PATH a command of the model gets.
pub fn on_users_path(name: &str) -> bool {
    #[cfg(windows)]
    {
        on_path(
            OsStr::new(name),
            &env::var("PATH").unwrap_or_default(),
            &env::var("PATHEXT")
                .map(|v| v.to_string_lossy().into_owned())
                .unwrap_or_default(),
        )
        .is_some()
    }
    #[cfg(not(windows))]
    {
        env::var("PATH")
            .is_some_and(|path| std::env::split_paths(&path).any(|dir| executable(&dir.join(name))))
    }
}

#[cfg(not(windows))]
fn executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// `name` with an extension of `pathext` that a command can start, in a directory of
/// `path`.
#[cfg(any(windows, test))]
fn on_path(name: &OsStr, path: &OsStr, pathext: &str) -> Option<PathBuf> {
    let bare = Path::new(name);
    if bare.components().count() != 1 || bare.extension().is_some() {
        return None;
    }
    let pathext = if pathext.is_empty() {
        ".COM;.EXE;.BAT;.CMD"
    } else {
        pathext
    };
    let exts: Vec<&str> = pathext
        .split(';')
        .filter(|e| {
            [".exe", ".com", ".bat", ".cmd"]
                .iter()
                .any(|x| e.eq_ignore_ascii_case(x))
        })
        .collect();
    std::env::split_paths(path).find_map(|dir| {
        exts.iter().find_map(|ext| {
            // Windows matches names in any case; the extension as a command spells it
            let mut file = name.to_owned();
            file.push(ext.to_ascii_lowercase());
            let candidate = dir.join(file);
            candidate.is_file().then_some(candidate)
        })
    })
}

/// `CREATE_NO_WINDOW`: a console program started by a process without a console (the core
/// that the window starts) would otherwise open a console window of its own.
#[cfg(windows)]
const NO_WINDOW: u32 = windows::Win32::System::Threading::CREATE_NO_WINDOW.0;

/// A command for [`program`]`(name)` that opens no window on Windows, with the
/// environment of a program the core starts ([`env::children`]).
pub fn command(name: impl AsRef<OsStr>) -> std::process::Command {
    let mut cmd = std::process::Command::new(program(name));
    cmd.env_clear().envs(env::children());
    #[cfg(windows)]
    std::os::windows::process::CommandExt::creation_flags(&mut cmd, NO_WINDOW);
    cmd
}

/// [`command`] for tokio.
pub fn async_command(name: impl AsRef<OsStr>) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(program(name));
    cmd.env_clear().envs(env::children());
    #[cfg(windows)]
    cmd.creation_flags(NO_WINDOW);
    cmd
}

/// Puts a command that `process_wrap` starts in a tree of its own, as [`Tree`] does: a
/// process group on Unix; on Windows a job, which ends with ognivo however ognivo ends,
/// and no console window.
pub fn tree_wrap(wrap: &mut process_wrap::tokio::CommandWrap) {
    #[cfg(unix)]
    wrap.wrap(process_wrap::tokio::ProcessGroup::leader());
    #[cfg(windows)]
    wrap.wrap(process_wrap::tokio::CreationFlags(
        windows::Win32::System::Threading::CREATE_NO_WINDOW,
    ))
    .wrap(process_wrap::tokio::JobObject)
    .wrap(process_wrap::tokio::KillOnDrop);
}

/// Whether the user or root owns the file of `meta`, and `path` itself if it is a link to
/// it: on Unix what keeps a file another user can write from passing as the user's own.
/// On Windows a file's owner says less (an administrator's files are the Administrators
/// group's), and a profile's files are the user's by their ACL: every file passes.
pub fn owned_by_user(path: &Path, meta: &fs::Metadata) -> bool {
    #[cfg(unix)]
    let owned = {
        use std::os::unix::fs::MetadataExt as _;
        let me = rustix::process::getuid().as_raw();
        let ok = |uid: u32| uid == me || uid == 0;
        ok(meta.uid()) && fs::symlink_metadata(path).is_ok_and(|link| ok(link.uid()))
    };
    #[cfg(not(unix))]
    let owned = {
        let _ = (path, meta);
        true
    };
    owned
}

/// Makes the kernel's out-of-memory killer take the process `pid` before others, a build
/// that eats the RAM before the llama-server it would take with it: on Linux its
/// `oom_score_adj` 500; elsewhere nothing.
pub fn oom_first(pid: u32) {
    #[cfg(target_os = "linux")]
    let _ = fs::write(format!("/proc/{pid}/oom_score_adj"), "500");
    #[cfg(not(target_os = "linux"))]
    let _ = pid;
}

/// Asks the tree whose root is the process `root`, started under [`tree_wrap`], to end:
/// SIGTERM to its process group on Unix. On Windows its job ends it, once its handle
/// closes with the child that holds it.
pub fn end_tree(root: u32) {
    #[cfg(unix)]
    if let Some(group) = i32::try_from(root)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
    {
        let _ = rustix::process::kill_process_group(group, rustix::process::Signal::TERM);
    }
    #[cfg(windows)]
    let _ = root;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wtf8_keeps_every_utf16_name() {
        for units in [
            "C:\\Users\\Пользователь\\Рабочий стол\\a.rs"
                .encode_utf16()
                .collect::<Vec<_>>(),
            "🔥 x".encode_utf16().collect(),
            // lone surrogates: a high one, a low one, a high one before a pair
            vec![0x61, 0xd800, 0x62],
            vec![0xdc00],
            vec![0xd83d, 0xd800, 0xdd25],
        ] {
            let bytes = wtf8::encode(units.iter().copied());
            assert_eq!(wtf8::decode(&bytes), units);
        }
        // valid Unicode is plain UTF-8
        assert_eq!(
            wtf8::encode("Рабочий стол".encode_utf16()),
            "Рабочий стол".as_bytes()
        );
        // what is not WTF-8 is U+FFFD, byte by byte
        assert_eq!(
            wtf8::decode(b"a\xffb\xe2\x82"),
            [0x61, 0xfffd, 0x62, 0xfffd, 0xfffd]
        );
    }

    #[test]
    fn paths_come_back_from_their_bytes() {
        for p in ["/home/me/Рабочий стол/a.rs", "relative/b", ""] {
            let p = Path::new(p);
            assert_eq!(path_from_bytes(path_bytes(p).into_owned()), p);
        }
    }

    #[test]
    fn verbatim_paths_are_plain_where_they_mean_the_same() {
        let plain = |s: &str| super::plain(PathBuf::from(s)).to_str().unwrap().to_owned();
        assert_eq!(plain(r"\\?\C:\Users\me\proj"), r"C:\Users\me\proj");
        assert_eq!(plain(r"\\?\C:\"), r"C:\");
        assert_eq!(plain(r"\\?\UNC\host\share\a"), r"\\host\share\a");
        // these need the prefix to mean what they mean, or are not drive paths
        for kept in [
            r"\\?\C:\a\NUL",
            r"\\?\C:\a\con.txt",
            r"\\?\C:\a\COM1",
            r"\\?\C:\a\b.",
            r"\\?\C:\a\b ",
            r"\\?\C:\a\b?c",
            r"\\?\Volume{0a1b}\x",
            r"C:\already",
        ] {
            assert_eq!(plain(kept), kept, "{kept}");
        }
        let long = format!(r"\\?\C:\{}", "a".repeat(300));
        assert_eq!(plain(&long), long);
        // a name that merely starts like a reserved one is fine
        assert_eq!(plain(r"\\?\C:\console\com10\lpt"), r"C:\console\com10\lpt");
    }

    #[test]
    fn a_lock_file_locks_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.lock");
        let first = lock_file(&path).unwrap();
        first.try_lock().unwrap();
        // another handle, as another process would have: it waits, it does not fail
        let second = lock_file(&path).unwrap();
        assert!(matches!(
            second.try_lock(),
            Err(fs::TryLockError::WouldBlock)
        ));
        drop(first);
        second.try_lock().unwrap();
    }

    #[test]
    fn a_private_dir_and_file_are_the_users_alone() {
        let dir = tempfile::tempdir().unwrap();
        let inner = dir.path().join("a/b");
        private_dir(&inner).unwrap();
        let file = inner.join("f");
        private_file()
            .write(true)
            .create_new(true)
            .open(&file)
            .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&inner), 0o700);
            assert_eq!(mode(&dir.path().join("a")), 0o700);
            assert_eq!(mode(&file), 0o600);
        }
    }

    #[test]
    fn a_program_by_its_bare_name_is_one_a_command_can_start() {
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        std::fs::write(a.path().join("tool.js"), "").unwrap();
        std::fs::write(b.path().join("tool.cmd"), "@echo off\r\n").unwrap();
        std::fs::write(b.path().join("other.exe"), "").unwrap();
        let path = std::env::join_paths([a.path(), b.path()]).unwrap();
        let find = |name: &str, ext: &str| on_path(name.as_ref(), &path, ext);
        // `.js` is in PATHEXT, but a command cannot start it
        assert_eq!(find("tool", ".JS;.CMD"), Some(b.path().join("tool.cmd")));
        assert_eq!(find("other", ""), Some(b.path().join("other.exe")));
        // a name with an extension or a directory is left to the OS
        assert_eq!(find("tool.cmd", ".CMD"), None);
        assert_eq!(find("x/tool", ".CMD"), None);
        assert_eq!(find("missing", ".CMD"), None);
        assert_eq!(program("/bin/sh"), Path::new("/bin/sh"));
    }
}
