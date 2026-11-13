//! The environment of the programs ognivo starts: the model's commands, the hooks and the
//! MCP servers ([`children`]); and what those programs print, as text ([`decode_output`]).
//!
//! The core outlives the windows that start it, so its own environment grows stale: a PATH
//! the user changed after it started, a compiler installed since (2026-11-13: `cl.exe` was
//! on the user's PATH and in a new terminal, and the agent's commands still did not find
//! it, because the core had started an hour before). On Windows a child gets what a new
//! terminal of the user gets now: the system's and the user's variables from the registry,
//! PATH the system's and then the user's, over the core's own for what the registry does
//! not hold. Everywhere, the variables of the editor that started the core stay out
//! (`VSCODE_*`, `ELECTRON_*`, Chromium's crash handler): `ELECTRON_RUN_AS_NODE` turns
//! `code` into node.
//!
//! Windows programs print in the console's code page (OEM: 866 in Russian, 437 in
//! English), not UTF-8: a line that is not UTF-8 is read in it, so `where`'s "not found"
//! reaches the model as words, not as replacement marks.

use std::collections::BTreeMap;
use std::ffi::OsString;

/// A variable of an editor's process, which a child must not get.
fn editors(name: &str) -> bool {
    let n = name.to_ascii_uppercase();
    n.starts_with("VSCODE_") || n.starts_with("ELECTRON_") || n == "CHROME_CRASHPAD_PIPE_NAME"
}

/// The environment of a program the core starts (see the module).
pub fn children() -> Vec<(OsString, OsString)> {
    let own = std::env::vars_os().filter(|(k, _)| !editors(&k.to_string_lossy()));
    #[cfg(windows)]
    if let Some(fresh) = registry::fresh() {
        return merge(own, fresh);
    }
    own.collect()
}

/// `fresh` over `own`, a variable of either in any case being the same one, as on Windows.
/// PATH is the fresh one, then the directories of the core's own that it lacks: what the
/// process that started the core put there for its children (CI's tool directories) stays.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
fn merge(
    own: impl Iterator<Item = (OsString, OsString)>,
    fresh: Vec<(String, String)>,
) -> Vec<(OsString, OsString)> {
    let mut by: BTreeMap<String, (OsString, OsString)> = BTreeMap::new();
    for (k, v) in own {
        by.insert(k.to_string_lossy().to_uppercase(), (k, v));
    }
    for (k, v) in fresh {
        let key = k.to_uppercase();
        let v = match by.get(&key) {
            Some((_, old)) if key == "PATH" => joined_path(&v, &old.to_string_lossy()),
            _ => v,
        };
        by.insert(key, (k.into(), v.into()));
    }
    by.into_values().collect()
}

/// `fresh`, then the directories of `old` that `fresh` lacks, in their order.
fn joined_path(fresh: &str, old: &str) -> String {
    let same = |d: &str| d.trim_end_matches(['\\', '/']).to_lowercase();
    let have: Vec<String> = fresh.split(';').map(same).collect();
    let mut out = fresh.trim_end_matches(';').to_owned();
    for dir in old.split(';').filter(|d| !d.is_empty()) {
        if !have.contains(&same(dir)) {
            out.push(';');
            out.push_str(dir);
        }
    }
    out
}

/// A variable of [`children`]'s environment.
pub fn var(name: &str) -> Option<OsString> {
    children()
        .into_iter()
        .find(|(k, _)| k.to_string_lossy().eq_ignore_ascii_case(name))
        .map(|(_, v)| v)
}

/// `%NAME%` replaced by the variable `vars` has under NAME in any case; one it does not
/// have stays as it is, as Windows leaves it.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
fn expand(text: &str, vars: &BTreeMap<String, String>) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('%') {
            Some(end) if end > 0 => {
                let name = &after[..end];
                match vars.get(&name.to_uppercase()) {
                    Some(v) => out.push_str(v),
                    None => {
                        out.push('%');
                        out.push_str(name);
                        out.push('%');
                    }
                }
                rest = &after[end + 1..];
            }
            _ => {
                out.push('%');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The variables of the system and of the user as the registry holds them, in the order
/// Windows gives a new process: plain values first, then the ones to expand, each of the
/// system then of the user; PATH is the system's and then the user's. `%VAR%` expands
/// against the core's own variables under the user's profile as Windows has it
/// (`profile`, the registry's Volatile Environment), as a new process's would: a core
/// whose USERPROFILE was set elsewhere (a test's) gives its children the real TEMP.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
fn assemble(
    own: &[(String, String)],
    profile: &[(String, String)],
    machine: &[(String, String, bool)],
    user: &[(String, String, bool)],
) -> Vec<(String, String)> {
    let mut vars: BTreeMap<String, String> = own
        .iter()
        .chain(profile)
        .map(|(k, v)| (k.to_uppercase(), v.clone()))
        .collect();
    let mut names: BTreeMap<String, String> = BTreeMap::new();
    let mut machine_path = None;
    for (scope, values) in [("machine", machine), ("user", user)] {
        for expanding in [false, true] {
            for (name, value, expand_it) in values {
                if *expand_it != expanding {
                    continue;
                }
                let value = if expanding {
                    expand(value, &vars)
                } else {
                    value.clone()
                };
                let key = name.to_uppercase();
                let value = if key == "PATH" && scope == "user" {
                    match &machine_path {
                        Some(m) if !value.is_empty() => format!("{m};{value}"),
                        Some(m) => String::clone(m),
                        None => value,
                    }
                } else {
                    value
                };
                if key == "PATH" && scope == "machine" {
                    machine_path = Some(value.clone());
                }
                vars.insert(key.clone(), value);
                names.insert(key, name.clone());
            }
        }
    }
    names
        .into_iter()
        .map(|(key, name)| {
            let value = vars.remove(&key).unwrap_or_default();
            (name, value)
        })
        .collect()
}

#[cfg(windows)]
mod registry {
    use windows_registry::{CURRENT_USER, Key, LOCAL_MACHINE, Type};

    const MACHINE: &str = r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment";
    const CODE_PAGES: &str = r"SYSTEM\CurrentControlSet\Control\Nls\CodePage";

    /// A key's text values: name, value, whether to expand it.
    fn read(key: &Key) -> Vec<(String, String, bool)> {
        let Ok(values) = key.values() else {
            return Vec::new();
        };
        values
            .filter_map(|(name, value)| {
                let expand = value.ty() == Type::ExpandString;
                if value.ty() != Type::String && !expand {
                    return None;
                }
                String::try_from(value).ok().map(|v| (name, v, expand))
            })
            .collect()
    }

    /// What a new terminal of the user gets now from the registry; `None` when the system's
    /// key cannot be read.
    pub fn fresh() -> Option<Vec<(String, String)>> {
        let machine = read(&LOCAL_MACHINE.open(MACHINE).ok()?);
        let user = CURRENT_USER
            .open("Environment")
            .map(|k| read(&k))
            .unwrap_or_default();
        let profile: Vec<(String, String)> = CURRENT_USER
            .open("Volatile Environment")
            .map(|k| read(&k).into_iter().map(|(n, v, _)| (n, v)).collect())
            .unwrap_or_default();
        let own: Vec<(String, String)> = std::env::vars().collect();
        Some(super::assemble(&own, &profile, &machine, &user))
    }

    /// The console's code page of the system, for programs that print in it.
    pub fn oem_code_page() -> Option<u32> {
        LOCAL_MACHINE
            .open(CODE_PAGES)
            .ok()?
            .get_string("OEMCP")
            .ok()?
            .trim()
            .parse()
            .ok()
    }
}

/// The encoding of a Windows code page, as far as encoding_rs knows one; a Western DOS
/// page it does not have reads as windows-1252, which agrees on letters and digits.
#[cfg(windows)]
fn encoding(page: u32) -> &'static encoding_rs::Encoding {
    match page {
        866 => encoding_rs::IBM866,
        874 => encoding_rs::WINDOWS_874,
        932 => encoding_rs::SHIFT_JIS,
        936 => encoding_rs::GBK,
        949 => encoding_rs::EUC_KR,
        950 => encoding_rs::BIG5,
        1250 => encoding_rs::WINDOWS_1250,
        1251 => encoding_rs::WINDOWS_1251,
        1253 => encoding_rs::WINDOWS_1253,
        1254 => encoding_rs::WINDOWS_1254,
        1255 => encoding_rs::WINDOWS_1255,
        1256 => encoding_rs::WINDOWS_1256,
        1257 => encoding_rs::WINDOWS_1257,
        1258 => encoding_rs::WINDOWS_1258,
        _ => encoding_rs::WINDOWS_1252,
    }
}

/// What a program printed, as text: UTF-8 as it is; on Windows, a line that is not UTF-8
/// in the console's code page; elsewhere with replacement marks.
pub fn decode_output(bytes: &[u8]) -> String {
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_owned();
    }
    #[cfg(windows)]
    {
        static OEM: std::sync::OnceLock<&'static encoding_rs::Encoding> =
            std::sync::OnceLock::new();
        let enc = *OEM.get_or_init(|| encoding(registry::oem_code_page().unwrap_or(437)));
        bytes
            .split_inclusive(|&b| b == b'\n')
            .map(|line| match std::str::from_utf8(line) {
                Ok(s) => s.to_owned(),
                Err(_) => enc.decode_without_bom_handling(line).0.into_owned(),
            })
            .collect()
    }
    #[cfg(not(windows))]
    String::from_utf8_lossy(bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_uppercase(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn variables_expand_in_any_case_and_unknown_ones_stay() {
        let vars = map(&[
            ("SystemRoot", "C:\\Windows"),
            ("USERPROFILE", "C:\\Users\\me"),
        ]);
        assert_eq!(
            expand("%SYSTEMROOT%\\system32;%userprofile%\\bin", &vars),
            "C:\\Windows\\system32;C:\\Users\\me\\bin"
        );
        assert_eq!(expand("%NOPE%\\x;100%;%", &vars), "%NOPE%\\x;100%;%");
    }

    #[test]
    fn a_new_terminal_gets_the_system_path_then_the_users_and_their_variables() {
        let own = vec![
            ("SystemRoot".to_owned(), "C:\\Windows".to_owned()),
            ("Path".to_owned(), "C:\\stale".to_owned()),
            // a test's core: its USERPROFILE is not the user's
            ("USERPROFILE".to_owned(), "C:\\tmp\\test".to_owned()),
        ];
        let profile = vec![("USERPROFILE".to_owned(), "C:\\Users\\me".to_owned())];
        let machine = vec![
            (
                "Path".to_owned(),
                "%SystemRoot%\\system32;%TOOLS%".to_owned(),
                true,
            ),
            ("TOOLS".to_owned(), "C:\\Tools".to_owned(), false),
        ];
        let user = vec![
            ("Path".to_owned(), "C:\\msvc\\bin".to_owned(), false),
            ("INCLUDE".to_owned(), "C:\\msvc\\include".to_owned(), false),
            (
                "TEMP".to_owned(),
                "%USERPROFILE%\\AppData\\Local\\Temp".to_owned(),
                true,
            ),
        ];
        let got: BTreeMap<String, String> = assemble(&own, &profile, &machine, &user)
            .into_iter()
            .collect();
        assert_eq!(got["Path"], "C:\\Windows\\system32;C:\\Tools;C:\\msvc\\bin");
        assert_eq!(got["INCLUDE"], "C:\\msvc\\include");
        assert_eq!(got["TOOLS"], "C:\\Tools");
        // the user's real profile, as a new process's
        assert_eq!(got["TEMP"], "C:\\Users\\me\\AppData\\Local\\Temp");
        // only what the registry's Environment keys hold: USERPROFILE stays the core's own
        assert!(!got.contains_key("USERPROFILE"));
    }

    #[test]
    fn the_registry_wins_over_the_cores_own_and_the_editors_variables_go() {
        let own = vec![
            (OsString::from("PATH"), OsString::from("C:\\stale")),
            (OsString::from("OGNIVO_HOME"), OsString::from("C:\\home")),
        ];
        let got = merge(
            own.into_iter(),
            vec![("Path".to_owned(), "C:\\fresh".to_owned())],
        );
        let got: BTreeMap<String, String> = got
            .into_iter()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.to_string_lossy().into_owned(),
                )
            })
            .collect();
        // the fresh PATH first; what only the core's own had goes on after it
        assert_eq!(
            got.get("Path").map(String::as_str),
            Some("C:\\fresh;C:\\stale")
        );
        assert!(!got.contains_key("PATH"));
        assert_eq!(got.get("OGNIVO_HOME").map(String::as_str), Some("C:\\home"));
        assert!(
            editors("VSCODE_PID")
                && editors("ELECTRON_RUN_AS_NODE")
                && editors("chrome_crashpad_pipe_name")
        );
        assert!(!editors("PATH"));
    }

    #[test]
    fn a_path_keeps_the_directories_only_the_cores_own_had() {
        assert_eq!(
            joined_path("C:\\a;C:\\b\\", "c:\\B;C:\\ci\\node;;C:\\a"),
            "C:\\a;C:\\b\\;C:\\ci\\node"
        );
    }

    #[test]
    fn utf8_output_stays_as_it_is() {
        assert_eq!(
            decode_output("файл не найден\n".as_bytes()),
            "файл не найден\n"
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_line_in_the_console_code_page_reads_as_words() {
        // "Привет" in code page 866, as a Russian console program prints it
        let cp866 = [0x8f, 0xe0, 0xa8, 0xa2, 0xa5, 0xe2, b'\n'];
        let page = registry::oem_code_page().unwrap_or(437);
        let got = decode_output(&cp866);
        if page == 866 {
            assert_eq!(got, "Привет\n");
        } else {
            assert!(!got.contains('\u{fffd}'), "{got:?}");
        }
    }
}
