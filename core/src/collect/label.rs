//! Short program labels shown in a row's detail ("terminal", "next", "vite").

use std::sync::LazyLock;

use regex::Regex;

use super::repo::basename;

const SHELLS: [&str; 6] = ["zsh", "bash", "fish", "sh", "dash", "nu"];
const INTERPRETERS: [&str; 10] = ["node", "bun", "deno", "python", "python3", "ruby", "php", "perl", "uv", "npx"];

/// A label for a process, from its kernel name and argv.
pub fn label(name: &str, argv: &[String]) -> String {
    let arg0 = argv.first().map(String::as_str).unwrap_or(name);
    let prog = basename(arg0.trim_start_matches('-'));
    let prog = if prog.is_empty() { name } else { prog };
    let base = strip_version(prog);

    if SHELLS.contains(&base) {
        // A login or interactive shell is a terminal; `sh -c "cmd …"` is cmd.
        if let Some(i) = argv.iter().position(|a| a == "-c" || a == "-lc" || a == "-ic") {
            return match argv.get(i + 1).and_then(|c| command_of(c)) {
                Some(cmd) => basename(&cmd).to_string(),
                None => "shell".to_string(),
            };
        }
        return "terminal".to_string();
    }
    if INTERPRETERS.contains(&base) {
        let mut it = argv.iter().skip(1);
        while let Some(a) = it.next() {
            match a.as_str() {
                // python -m http.server
                "-m" => {
                    if let Some(m) = it.next() {
                        return m.split('.').next().unwrap_or(m).to_string();
                    }
                }
                // Inline code (node -e, python -c, perl -e): just the program.
                "-e" | "-c" | "-p" | "--eval" | "--print" => break,
                // Options that take a value.
                "-r" | "--require" | "--import" | "--loader" => {
                    it.next();
                }
                _ if a.starts_with('-') => {}
                _ => return script_label(a),
            }
        }
    }
    prog.to_string()
}

/// Words that only prefix the real command (`exec pnpm dev`).
const PREFIXES: [&str; 6] = ["builtin", "command", "exec", "nohup", "time", "env"];
/// Commands that don't say what a `sh -c` is for: their segment is skipped.
const NOISE: [&str; 17] =
    ["source", ".", "cd", "export", "set", "setopt", "unsetopt", "unalias", "unset", "alias", "shopt", "true", "false", "pwd", "which", "trap", "ulimit"];

/// Redirections (`2>/dev/null`, `>| /tmp/x`, `2>&1`, `< /dev/null`).
static REDIRECT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\d*(?:>>|>\||>&|<&|&>|>|<)\s*[^\s;|&]+").expect("valid regex"));

/// The program a `sh -c` command string runs: the first word of the first
/// segment (between `;`, `&&`, `||`, `|`, braces) that isn't noise. `eval
/// 'cmd'` is looked into. Agent tool shells (`source snapshot; setopt …;
/// eval '…'`) are cut at 1 KiB, so the real command may be gone: None.
fn command_of(cmd: &str) -> Option<String> {
    let cmd = REDIRECT.replace_all(cmd, " ");
    for seg in cmd.split(['\n', ';', '&', '|', '{', '}', '(', ')']) {
        let mut words = seg.split_whitespace().map(|w| w.trim_start_matches('\\'));
        while let Some(w) = words.next() {
            if w.contains('=') || PREFIXES.contains(&w) || w.starts_with('-') {
                continue;
            }
            if w == "eval" {
                let rest: Vec<&str> = words.collect();
                let inner = rest.join(" ");
                let inner = inner.trim_matches(|c| c == '"' || c == '\'');
                if let Some(c) = command_of(inner) {
                    return Some(c);
                }
                break;
            }
            if NOISE.contains(&w) {
                break;
            }
            let w = w.trim_matches(|c| c == '"' || c == '\'');
            return (!w.is_empty()).then(|| w.to_string());
        }
    }
    None
}

/// `node_modules/.bin/vite` → vite, `node_modules/next/dist/bin/next` → next,
/// `node_modules/@nestjs/cli/bin/nest.js` → @nestjs/cli, `server.js` → server.js.
fn script_label(path: &str) -> String {
    if let Some(i) = path.rfind("node_modules/") {
        let rest = &path[i + "node_modules/".len()..];
        if let Some(bin) = rest.strip_prefix(".bin/") {
            return basename(bin).to_string();
        }
        let mut parts = rest.split('/');
        let first = parts.next().unwrap_or(rest);
        if first.starts_with('@')
            && let Some(second) = parts.next()
        {
            return format!("{first}/{second}");
        }
        return first.to_string();
    }
    basename(path).to_string()
}

/// "python3.12" → "python3", "node22" stays (rare).
fn strip_version(prog: &str) -> &str {
    match prog.find('.') {
        Some(i) if prog[..i].starts_with("python") => &prog[..i],
        _ => prog,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn l(name: &str, argv: &[&str]) -> String {
        label(name, &argv.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn shells() {
        assert_eq!(l("zsh", &["-zsh"]), "terminal");
        assert_eq!(l("zsh", &["/bin/zsh", "-l"]), "terminal");
        assert_eq!(l("zsh", &["/bin/zsh", "-c", "npm run dev"]), "npm");
        assert_eq!(l("bash", &["bash", "-lc", "/usr/local/bin/cargo build"]), "cargo");
        assert_eq!(l("zsh", &["/bin/zsh", "-c", "source /Users/me/.zshrc && cd /w/app && NODE_ENV=dev exec pnpm dev"]), "pnpm");
        assert_eq!(l("zsh", &["/bin/zsh", "-c", "source /Users/me/.zshrc"]), "shell");
        assert_eq!(l("sh", &["sh", "-c", "(cd /w && make)"]), "make");
        assert_eq!(l("sh", &["sh", "-c", "cargo test 2>&1 | tail -5"]), "cargo");
    }

    #[test]
    fn agent_tool_shells() {
        let head = "source /Users/me/.claude/shell-snapshots/snapshot-zsh-1.sh 2>/dev/null || true && \\builtin setopt NO_EXTENDED_GLOB NO_EQUALS 2>/dev/null || true && { \\builtin unalias -- 'which'; \\builtin unset -f -- 'which'; } >/dev/null 2>&1 || true";
        let full = format!("{head} && eval 'cargo build --release' \\< /dev/null && pwd -P >| /tmp/claude-cwd");
        assert_eq!(l("zsh", &["/bin/zsh", "-c", &full]), "cargo");
        // Cut before the eval (argv is capped at 1 KiB): no guess.
        assert_eq!(l("zsh", &["/bin/zsh", "-c", head]), "shell");
    }

    #[test]
    fn inline_code_is_the_program() {
        assert_eq!(l("perl", &["perl", "-e", "select(undef,undef,undef,60)"]), "perl");
        assert_eq!(l("node", &["node", "-e", "require('x')"]), "node");
        assert_eq!(l("python3", &["python3", "-c", "import time"]), "python3");
        assert_eq!(l("node", &["node", "-r", "ts-node/register", "src/main.ts"]), "main.ts");
    }

    #[test]
    fn interpreters_show_their_script() {
        assert_eq!(l("node", &["node", "/w/app/node_modules/.bin/vite", "--port", "5173"]), "vite");
        assert_eq!(l("node", &["node", "/w/app/node_modules/next/dist/server/lib/start-server.js"]), "next");
        assert_eq!(l("node", &["node", "--max-old-space-size=4096", "/w/node_modules/@nestjs/cli/bin/nest.js", "start"]), "@nestjs/cli");
        assert_eq!(l("node", &["node", "server.js"]), "server.js");
        assert_eq!(l("python3.12", &["/opt/homebrew/bin/python3.12", "-m", "http.server", "8000"]), "http");
        assert_eq!(l("python3", &["python3", "-u", "manage.py", "runserver"]), "manage.py");
        assert_eq!(l("node", &["node"]), "node");
    }

    #[test]
    fn plain_programs() {
        assert_eq!(l("postgres", &["postgres: checkpointer"]), "postgres: checkpointer");
        assert_eq!(l("cargo", &["/Users/me/.cargo/bin/cargo", "build"]), "cargo");
        assert_eq!(l("Google Chrome He", &[]), "Google Chrome He");
    }
}
