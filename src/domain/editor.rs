//! Building the "open in editor" command line.
//!
//! The configured command (`behavior.editor`, `$VISUAL` or `$EDITOR`) may use
//! `{file}` / `{line}` placeholders for full control. Without placeholders the
//! file path is appended as before, and a line argument is injected for editors
//! whose syntax is known.

use std::path::Path;

/// How an editor accepts a line number on its command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineArg {
    /// `path:LINE`
    PathSuffix,
    /// `FLAG path:LINE`
    GotoSuffix(&'static str),
    /// `+LINE path`
    PlusBefore,
    /// `FLAG LINE path`
    Flag(&'static str),
}

/// Line-argument syntax for a known editor, keyed by the executable's basename.
/// `None` means "unknown editor" — the path is passed on its own.
fn line_arg_for(basename: &str) -> Option<LineArg> {
    Some(match basename {
        "subl" | "sublime_text" | "zed" | "hx" | "helix" => LineArg::PathSuffix,
        "code" | "codium" | "code-insiders" | "cursor" => LineArg::GotoSuffix("-g"),
        "vim" | "nvim" | "gvim" | "vi" | "emacs" | "emacsclient" | "nano" | "kak" | "gedit" => {
            LineArg::PlusBefore
        }
        "kate" => LineArg::Flag("-l"),
        "idea" | "pycharm" | "goland" | "rustrover" | "clion" | "webstorm" => {
            LineArg::Flag("--line")
        }
        _ => return None,
    })
}

/// Build the argv for opening `file` at `line` (1-based) with `cmd`.
///
/// Returns an empty vec if `cmd` has no command word.
pub fn build_argv(cmd: &str, file: &Path, line: usize) -> Vec<String> {
    let mut parts = split_shell_words(cmd);
    if parts.is_empty() {
        return parts;
    }
    let file = file.to_string_lossy();

    // An explicit `{file}` template takes full control; substitution happens
    // per-word after splitting, so paths with spaces stay a single argument.
    if parts.iter().any(|p| p.contains("{file}")) {
        for part in &mut parts {
            if part.contains("{line}") {
                *part = part.replace("{line}", &line.to_string());
            }
            if part.contains("{file}") {
                *part = part.replace("{file}", &file);
            }
        }
        return parts;
    }

    let basename = Path::new(&parts[0])
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    match line_arg_for(&basename) {
        Some(LineArg::PathSuffix) => parts.push(format!("{file}:{line}")),
        Some(LineArg::GotoSuffix(flag)) => {
            parts.push(flag.to_string());
            parts.push(format!("{file}:{line}"));
        }
        Some(LineArg::PlusBefore) => {
            parts.push(format!("+{line}"));
            parts.push(file.into_owned());
        }
        Some(LineArg::Flag(flag)) => {
            parts.push(flag.to_string());
            parts.push(line.to_string());
            parts.push(file.into_owned());
        }
        None => parts.push(file.into_owned()),
    }
    parts
}

/// Split a command string into words, respecting single and double quotes
/// (e.g. `'/usr/bin/my editor' --wait`).
fn split_shell_words(s: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut in_single = false;
    let mut in_double = false;

    for ch in s.chars() {
        match ch {
            '\'' if !in_double => {
                in_single = !in_single;
            }
            '"' if !in_single => {
                in_double = !in_double;
            }
            c if c.is_whitespace() && !in_single && !in_double => {
                if !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(cmd: &str, line: usize) -> Vec<String> {
        build_argv(cmd, Path::new("/tmp/foo.rs"), line)
    }

    // ── split_shell_words ────────────────────────────────────────────

    #[test]
    fn shell_words_simple() {
        assert_eq!(split_shell_words("code --wait"), vec!["code", "--wait"]);
    }

    #[test]
    fn shell_words_single_quotes() {
        assert_eq!(
            split_shell_words("'/usr/bin/my editor' --wait"),
            vec!["/usr/bin/my editor", "--wait"]
        );
    }

    #[test]
    fn shell_words_double_quotes() {
        assert_eq!(
            split_shell_words(r#""my editor" arg1 arg2"#),
            vec!["my editor", "arg1", "arg2"]
        );
    }

    #[test]
    fn shell_words_empty() {
        assert!(split_shell_words("").is_empty());
        assert!(split_shell_words("   ").is_empty());
    }

    #[test]
    fn shell_words_extra_whitespace() {
        assert_eq!(split_shell_words("  vim   file  "), vec!["vim", "file"]);
    }

    // ── known editors ────────────────────────────────────────────────

    #[test]
    fn known_editor_path_suffix_keeps_user_args() {
        assert_eq!(argv("subl -w", 42), vec!["subl", "-w", "/tmp/foo.rs:42"]);
    }

    #[test]
    fn known_editor_goto_suffix() {
        assert_eq!(
            argv("code --wait", 42),
            vec!["code", "--wait", "-g", "/tmp/foo.rs:42"]
        );
    }

    #[test]
    fn known_editor_plus_before() {
        assert_eq!(argv("nvim", 42), vec!["nvim", "+42", "/tmp/foo.rs"]);
    }

    #[test]
    fn known_editor_flag() {
        assert_eq!(argv("kate", 42), vec!["kate", "-l", "42", "/tmp/foo.rs"]);
        assert_eq!(
            argv("idea", 42),
            vec!["idea", "--line", "42", "/tmp/foo.rs"]
        );
    }

    #[test]
    fn known_editor_matched_by_basename() {
        assert_eq!(
            argv("/usr/local/bin/subl -w", 7),
            vec!["/usr/local/bin/subl", "-w", "/tmp/foo.rs:7"]
        );
    }

    #[test]
    fn unknown_editor_appends_path_only() {
        assert_eq!(
            argv("my-editor --flag", 42),
            vec!["my-editor", "--flag", "/tmp/foo.rs"]
        );
    }

    #[test]
    fn empty_command_yields_no_argv() {
        assert!(argv("", 42).is_empty());
    }

    // ── templates ────────────────────────────────────────────────────

    #[test]
    fn template_substitutes_both_placeholders() {
        assert_eq!(
            argv("subl {file}:{line}", 42),
            vec!["subl", "/tmp/foo.rs:42"]
        );
    }

    #[test]
    fn template_without_line_omits_it() {
        // Explicit opt-out for known editors.
        assert_eq!(argv("subl {file}", 42), vec!["subl", "/tmp/foo.rs"]);
    }

    #[test]
    fn template_wraps_terminal_editor() {
        assert_eq!(
            argv("ghostty -e nvim +{line} {file}", 42),
            vec!["ghostty", "-e", "nvim", "+42", "/tmp/foo.rs"]
        );
    }

    #[test]
    fn template_placeholder_word_stays_one_arg_with_spaces() {
        let out = build_argv("subl {file}:{line}", Path::new("/tmp/my dir/foo.rs"), 9);
        assert_eq!(out, vec!["subl", "/tmp/my dir/foo.rs:9"]);
    }

    #[test]
    fn template_ignores_basename_table() {
        // `code` would normally get `-g`; the template wins.
        assert_eq!(argv("code {file}:{line}", 3), vec!["code", "/tmp/foo.rs:3"]);
    }
}
