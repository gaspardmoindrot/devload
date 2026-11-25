//! The Analyze prompt: the text the Analyze button copies. It points an AI
//! coding agent (Claude Code, Codex…) at devload's SQLite database and lets
//! it find what went wrong on its own. devload launches nothing.

/// The prompt for the period shown by the graph (`period`: "hour",
/// "6 hours"…) and the database at `db_path` (absolute, or starting with `~/`).
pub fn build(period: &str, db_path: &str) -> String {
    format!(
        "Look at what happened on my Mac over the last {period}.\n\n\
         devload records its memory and CPU in a SQLite database:\n  {}\n\n\
         Open it read-only (sqlite3 -readonly) and start with .schema, every column is commented.\n\n\
         Tell me what looks problematic and why. Don't change anything without asking me first.\n",
        shell_quote(db_path)
    )
}

/// `path` with the home folder written `~/`: shorter, and the user name
/// stays out of what gets pasted. Unchanged outside `home`.
pub fn home_relative(path: &str, home: Option<&str>) -> String {
    let home = home.map(|h| h.trim_end_matches('/')).filter(|h| !h.is_empty());
    match home.and_then(|h| path.strip_prefix(h)).and_then(|r| r.strip_prefix('/')) {
        Some(rest) if !rest.is_empty() => format!("~/{rest}"),
        _ => path.to_string(),
    }
}

/// A path that survives a copy-paste into a shell: left bare when it only has
/// safe characters, single-quoted otherwise (`'` written as `'\''`). A leading
/// `~/` stays outside the quotes so the shell still expands it.
pub fn shell_quote(path: &str) -> String {
    let (home, rest) = match path.strip_prefix("~/") {
        Some(r) => ("~/", r),
        None => ("", path),
    };
    let safe = !rest.is_empty()
        && rest
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/._-+@%:,=".contains(&b));
    if safe {
        return format!("{home}{rest}");
    }
    format!("{home}'{}'", rest.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOLDEN: &str = "\
Look at what happened on my Mac over the last 6 hours.

devload records its memory and CPU in a SQLite database:
  ~/'Library/Application Support/devload/devload.sqlite'

Open it read-only (sqlite3 -readonly) and start with .schema, every column is commented.

Tell me what looks problematic and why. Don't change anything without asking me first.
";

    #[test]
    fn the_whole_prompt() {
        assert_eq!(build("6 hours", "~/Library/Application Support/devload/devload.sqlite"), GOLDEN);
        let p = build("hour", "/tmp/it's here/devload.sqlite");
        assert!(p.starts_with("Look at what happened on my Mac over the last hour.\n"), "{p}");
        assert!(p.contains("\n  '/tmp/it'\\''s here/devload.sqlite'\n"), "{p}");
    }

    #[test]
    fn home_is_written_with_a_tilde() {
        let db = "/Users/me/Library/Application Support/devload/devload.sqlite";
        assert_eq!(home_relative(db, Some("/Users/me")), "~/Library/Application Support/devload/devload.sqlite");
        assert_eq!(home_relative(db, Some("/Users/me/")), "~/Library/Application Support/devload/devload.sqlite");
        assert_eq!(home_relative(db, Some("/Users/m")), db, "a prefix of a folder name is not the home");
        assert_eq!(home_relative(db, None), db);
        assert_eq!(home_relative(db, Some("")), db);
        assert_eq!(home_relative("/Users/me", Some("/Users/me")), "/Users/me");
        assert_eq!(home_relative("/tmp/devload.sqlite", Some("/Users/me")), "/tmp/devload.sqlite");
    }

    #[test]
    fn paths_are_shell_safe() {
        assert_eq!(shell_quote("/Users/me/.devload/devload.sqlite"), "/Users/me/.devload/devload.sqlite");
        assert_eq!(
            shell_quote("/Users/me/Library/Application Support/devload/devload.sqlite"),
            "'/Users/me/Library/Application Support/devload/devload.sqlite'"
        );
        assert_eq!(shell_quote("/tmp/it's here/db"), r"'/tmp/it'\''s here/db'");
        assert_eq!(shell_quote("~/Library/Application Support/x.db"), "~/'Library/Application Support/x.db'");
        assert_eq!(shell_quote("~/x.db"), "~/x.db");
        assert_eq!(shell_quote("/a/$HOME/`b`"), "'/a/$HOME/`b`'");
        assert_eq!(shell_quote(""), "''");
    }
}
