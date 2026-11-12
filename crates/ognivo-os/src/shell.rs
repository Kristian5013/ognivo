//! Bash, in which the bash tool and hooks run what the model and the user write: the
//! system's on Unix; on Windows Git for Windows' bash ("Git Bash"), as Claude Code has it
//! there. Models write bash, and Git Bash runs it, with a Windows path spelled
//! `/c/Users/me` or `C:/Users/me`.

use std::ffi::OsString;
#[cfg(any(windows, test))]
use std::path::Path;
use std::path::PathBuf;

/// Bash is not where ognivo looks for it.
#[derive(Debug, thiserror::Error)]
pub enum NoBash {
    #[error("{var}={}: no such file", path.display())]
    Named { var: &'static str, path: PathBuf },
    #[error(
        "Git Bash not found: on Windows ognivo runs commands in Git for Windows' bash; install Git for Windows (git-scm.com), or set OGNIVO_GIT_BASH to its bin\\bash.exe"
    )]
    NotFound,
}

/// The bash to run: on Unix `bash`, which the OS finds on `PATH`; on Windows the file
/// `%OGNIVO_GIT_BASH%` (or Claude Code's `%CLAUDE_CODE_GIT_BASH_PATH%`) names, else the
/// `bin\bash.exe` of the Git whose `git.exe` is on `PATH`, else of Git where its installer
/// puts it. Never `System32\bash.exe`, which is WSL's: another system, with other paths.
pub fn bash() -> Result<PathBuf, NoBash> {
    #[cfg(windows)]
    let bash = git_bash(|k| std::env::var_os(k));
    #[cfg(not(windows))]
    let bash = Ok(PathBuf::from("bash"));
    bash
}

#[cfg(any(windows, test))]
fn git_bash(env: impl Fn(&str) -> Option<OsString>) -> Result<PathBuf, NoBash> {
    let env = |k: &str| env(k).filter(|v| !v.is_empty());
    for var in ["OGNIVO_GIT_BASH", "CLAUDE_CODE_GIT_BASH_PATH"] {
        if let Some(path) = env(var).map(PathBuf::from) {
            return if path.is_file() {
                Ok(path)
            } else {
                Err(NoBash::Named { var, path })
            };
        }
    }
    // Git's root from the git.exe on PATH: <root>\cmd, <root>\bin or <root>\mingw64\bin
    let dirs: Vec<PathBuf> = env("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    let roots = dirs
        .iter()
        .map(|d| d.join("git.exe"))
        .filter(|git| git.is_file())
        .flat_map(|git| {
            git.ancestors()
                .skip(2)
                .take(2)
                .map(Path::to_path_buf)
                .collect::<Vec<_>>()
        });
    let installed = [
        ("ProgramFiles", "Git"),
        ("ProgramW6432", "Git"),
        ("LOCALAPPDATA", "Programs/Git"),
    ]
    .into_iter()
    .filter_map(|(var, sub)| env(var).map(|dir| PathBuf::from(dir).join(sub)));
    roots
        .chain(installed)
        .map(|root| root.join("bin").join("bash.exe"))
        .find(|bash| bash.is_file())
        .ok_or(NoBash::NotFound)
}

/// The bash command that prints the directory the shell is in, as the OS spells it:
/// `pwd -P`; on Windows Git Bash's `pwd -W`, `C:/Users/me` for its `/c/Users/me`.
#[cfg(not(windows))]
pub const PWD: &str = "pwd -P";
#[cfg(windows)]
pub const PWD: &str = "pwd -W";

/// `path` for bash to read from a variable: as it is; on Windows with `/` for each `\`,
/// the spelling Git Bash reads surely as the same file.
pub fn bash_path(path: &std::path::Path) -> OsString {
    crate::with_slashes(path.to_owned()).into_os_string()
}

/// A path as a model may have copied it from bash, as the OS spells it: on Windows Git
/// Bash's `/c/Users/me` and `C:/Users/me` are `C:\Users\me`; any other path, and any path
/// on Unix, is as it is.
pub fn native(path: &str) -> PathBuf {
    #[cfg(windows)]
    let path = windows_path(path);
    PathBuf::from(path)
}

#[cfg(any(windows, test))]
fn windows_path(path: &str) -> String {
    let b = path.as_bytes();
    if b.len() >= 2 && b[0] == b'/' && b[1].is_ascii_alphabetic() && (b.len() == 2 || b[2] == b'/')
    {
        let drive = char::from(b[1]).to_ascii_uppercase();
        return format!(
            "{drive}:\\{}",
            path.get(3..).unwrap_or("").replace('/', "\\")
        );
    }
    if b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'/' {
        return path.replace('/', "\\");
    }
    path.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_bash_paths_are_windows_paths() {
        for (bash, windows) in [
            ("/c/Users/me/proj/a.rs", r"C:\Users\me\proj\a.rs"),
            ("/d", r"D:\"),
            ("/c/", r"C:\"),
            ("C:/Users/me", r"C:\Users\me"),
            (r"C:\Users\me", r"C:\Users\me"),
            ("src/a.rs", "src/a.rs"),
            ("/tmp/x", "/tmp/x"),
            ("/usr", "/usr"),
        ] {
            assert_eq!(windows_path(bash), windows, "{bash}");
        }
    }

    #[test]
    fn git_bash_is_found_where_git_is() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("Git");
        for d in ["cmd", "bin", "usr/bin"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        std::fs::write(root.join("cmd/git.exe"), "").unwrap();
        std::fs::write(root.join("bin/bash.exe"), "").unwrap();
        // WSL's bash on PATH is not it
        let system = dir.path().join("System32");
        std::fs::create_dir_all(&system).unwrap();
        std::fs::write(system.join("bash.exe"), "").unwrap();
        let path = std::env::join_paths([system.clone(), root.join("cmd")]).unwrap();
        let env = |vars: Vec<(&'static str, OsString)>| {
            move |k: &str| vars.iter().find(|(v, _)| *v == k).map(|(_, x)| x.clone())
        };
        let found = git_bash(env(vec![("PATH", path.clone())])).unwrap();
        assert_eq!(found, root.join("bin/bash.exe"));
        // where the installer puts it, with no git on PATH
        let programs = dir.path().to_owned();
        let found = git_bash(env(vec![("ProgramFiles", programs.into())])).unwrap();
        assert_eq!(found, root.join("bin/bash.exe"));
        // a named one must be there
        let named = git_bash(env(vec![
            ("OGNIVO_GIT_BASH", dir.path().join("nope.exe").into()),
            ("PATH", path),
        ]));
        assert!(matches!(
            named,
            Err(NoBash::Named {
                var: "OGNIVO_GIT_BASH",
                ..
            })
        ));
        assert!(matches!(
            git_bash(env(vec![("PATH", system.into())])),
            Err(NoBash::NotFound)
        ));
    }

    #[test]
    fn a_path_for_bash_has_no_backslash_on_windows() {
        let p = std::path::Path::new("a/b");
        assert_eq!(bash_path(p), OsString::from("a/b"));
    }
}
