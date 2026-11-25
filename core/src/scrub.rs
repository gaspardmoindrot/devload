//! Removes secrets from process arguments before they are stored in the
//! database (and so before an agent reads them through the Analyze prompt).
//!
//! The `regex` crate has no lookaround, so the two patterns that would use
//! it are rewritten without; each rewrite shows the lookaround form it
//! stands for.
//!
//! Runs once per newly seen process, not per tick.

use regex::Regex;
use std::sync::LazyLock;

/// What a secret is replaced with.
pub const PLACEHOLDER: &str = "‹redacted›";

/// Max length, in chars, of the scrubbed arguments `scrub_args` returns
/// (the trailing "…" included).
pub const ARGS_MAX_CHARS: usize = 300;

/// Flags whose value is a secret: `--token X`, `--api-key=X`, `--password`,
/// `--secret`… (`-p X` is not one of them: too common).
const SECRET_FLAG_WORDS: &[&str] = &[
    "token",
    "secret",
    "password",
    "passwd",
    "pwd",
    "apikey",
    "api-key",
    "api_key",
    "auth",
    "credential",
    "private-key",
    "access-key",
    "client-secret",
    "session-key",
    "cookie",
    "bearer",
];

fn is_secret_flag(flag: &str) -> bool {
    if !flag.starts_with('-') {
        return false;
    }
    let name = flag.trim_start_matches('-').to_lowercase();
    if name.is_empty() {
        return false;
    }
    SECRET_FLAG_WORDS.iter().any(|w| {
        name == *w
            || name.starts_with(w)
            || (name.len() > w.len()
                && name.ends_with(w)
                && matches!(name.as_bytes()[name.len() - w.len() - 1], b'-' | b'_'))
    })
}

/// argv with secret values replaced, entry by entry. Catches what the text
/// patterns cannot see once argv is joined: a quoted value with a space in a
/// single entry (`--password`, `p@ss w0rd!`) and `--flag value` pairs over
/// two entries. argv[0] is scrubbed as text.
pub fn redact_args(argv: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(argv.len());
    let mut redact_next = false;
    for (i, arg) in argv.iter().enumerate() {
        if i == 0 {
            out.push(scrub(arg));
            continue;
        }
        if redact_next {
            redact_next = false;
            if arg.starts_with('-') {
                out.push(scrub(arg));
                redact_next = is_secret_flag(arg) && !arg.contains('=');
            } else {
                out.push(PLACEHOLDER.to_string());
            }
            continue;
        }
        if let Some(eq) = arg.find('=') {
            if is_secret_flag(&arg[..eq]) {
                out.push(format!("{}{PLACEHOLDER}", &arg[..=eq]));
                continue;
            }
        }
        if is_secret_flag(arg) {
            out.push(arg.clone());
            redact_next = true;
            continue;
        }
        // KEY=value assignments (`env TOKEN=x cmd`).
        if let Some(eq) = arg.find('=') {
            if !arg.starts_with('-') {
                let as_flag = format!("--{}", arg[..eq].to_lowercase().replace('_', "-"));
                if is_secret_flag(&as_flag) {
                    out.push(format!("{}{PLACEHOLDER}", &arg[..=eq]));
                    continue;
                }
            }
        }
        out.push(scrub(arg));
    }
    out
}

/// The arguments of a process as stored: argv[1..] redacted entry by entry,
/// joined with spaces, scrubbed once more as a whole, and cut to
/// `ARGS_MAX_CHARS` chars with a trailing "…" when longer.
pub fn scrub_args(argv: &[String]) -> String {
    if argv.len() < 2 {
        return String::new();
    }
    let redacted = redact_args(argv);
    let joined = scrub(&redacted[1..].join(" "));
    truncate_chars(joined, ARGS_MAX_CHARS)
}

fn truncate_chars(s: String, max: usize) -> String {
    match s.char_indices().nth(max) {
        None => s,
        Some(_) => {
            let cut = s.char_indices().nth(max - 1).map_or(s.len(), |(i, _)| i);
            let mut t = s[..cut].to_string();
            t.push('…');
            t
        }
    }
}

/// Known secret formats, credentials in URLs, secret flags and secret
/// assignments (`--token x`, `API_KEY=x`, `"password": "x"`), anywhere in a
/// string. Idempotent: scrubbing twice gives the same text.
pub fn scrub(s: &str) -> String {
    if s.chars().count() < 8 {
        return s.to_string();
    }
    let mut out = s.to_string();
    for (re, rep) in PATTERNS.iter() {
        // `replace_all` returns Cow::Borrowed when nothing matched: no copy.
        if let std::borrow::Cow::Owned(o) = re.replace_all(&out, rep.as_str()) {
            out = o;
        }
    }
    out
}

/// Names a secret hides behind, in flags (`--github-token`), assignments
/// (`DB_PASSWORD=`) and JSON keys (`"api_key":`).
const SECRET_NAMES: &str = r"(?:token|secret|password|passwd|api[-_]?key|private[-_]?key|access[-_]?key|client[-_]?secret|session[-_]?key|credentials?|cookie)";

static PATTERNS: LazyLock<Vec<(Regex, String)>> = LazyLock::new(|| {
    let p = PLACEHOLDER; // no `$` in it: safe in a replacement template
    let n = SECRET_NAMES;
    // A value: quoted, or up to the next blank or quote.
    //   (?:"[^"]*"|'[^']*'|[^\s"'`]+)
    // The variants below fold a lookahead into the value's first char.
    // A value that does not start with `-`: stands for `(?!-)` before it.
    let v_no_dash = r#"(?:"[^"]*"|'[^']*'|[^\s"'`\-][^\s"'`]*)"#;
    // A value that does not start with `&`: stands for `(?![\s&]|$)`.
    // The `\s` and `$` parts need nothing: a value never starts with a blank
    // and is never empty.
    let v_no_amp = r#"(?:"[^"]*"|'[^']*'|[^\s"'`&][^\s"'`]*)"#;
    let raw: Vec<(String, String)> = vec![
        // scheme://user:password@host
        (r"([A-Za-z][A-Za-z0-9+.-]*://[^/\s:@]*):[^@\s/]+@".into(), format!("${{1}}:{p}@")),
        (r"(?i)\bBearer\s+[A-Za-z0-9._~+/=-]{8,}".into(), format!("Bearer {p}")),
        (
            r"(?i)\b(Authorization|Proxy-Authorization)(\s*:\s*)(?:Basic|Token|Digest)\s+[A-Za-z0-9._~+/=:-]{6,}".into(),
            format!("${{1}}${{2}}{p}"),
        ),
        (r#"(?i)\b(X-Api-Key|Api-Key|X-Auth-Token|Cookie)(\s*:\s*)[^\s"']{6,}"#.into(), format!("${{1}}${{2}}{p}")),
        (r"\bsk-ant-[A-Za-z0-9_-]{4,}".into(), p.into()),
        (r"\bsk-[A-Za-z0-9_-]{16,}".into(), p.into()),
        (r"\b(?:sk|rk|pk)_(?:live|test)_[A-Za-z0-9]{10,}".into(), p.into()),
        (r"\b(?:ghp|gho|ghu|ghs|ghr)_[A-Za-z0-9]{8,}".into(), p.into()),
        (r"\bgithub_pat_[A-Za-z0-9_]{8,}".into(), p.into()),
        (r"\bxox[abposr]-[A-Za-z0-9-]{8,}".into(), p.into()),
        (r"hooks\.slack\.com/services/[A-Za-z0-9/_-]+".into(), format!("hooks.slack.com/services/{p}")),
        (r"\b(?:AKIA|ASIA)[A-Z0-9]{12,}".into(), p.into()),
        (r"\bAIza[0-9A-Za-z_-]{30,}".into(), p.into()),
        (r"\bglpat-[A-Za-z0-9_-]{8,}".into(), p.into()),
        (r"\bnpm_[A-Za-z0-9]{20,}".into(), p.into()),
        (r"\bpypi-[A-Za-z0-9_-]{16,}".into(), p.into()),
        (r"-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?(-----END [A-Z ]*PRIVATE KEY-----|$)".into(), p.into()),
        (r"\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}".into(), p.into()),
        // `--token x`, `--db-password=x`, `-api-key x` in a command line.
        //   with lookaround: (?i)((?<![\w-])--?[a-z0-9_-]*N(?:=|\s+))(?!-)V   → "$1‹›"
        // The lookbehind `(?<![\w-])` becomes a captured `(^|[^\w-])` put back
        // with `${1}` (the char before is consumed, but it is a separator, so
        // two flags in a row still both match). `(?!-)` becomes `v_no_dash`.
        (
            format!(r"(?i)(^|[^\w-])(--?[a-z0-9_-]*{n}(?:=|\s+)){v_no_dash}"),
            format!("${{1}}${{2}}{p}"),
        ),
        // `API_KEY=x`, `db_password=x`, `?access_token=x&` (env assignments,
        // query strings): the name ends with the secret word (`max_tokens=4096`
        // is not one).
        //   with lookaround: (?i)(\b[a-z0-9_]*(?:N|secret_key_base)=)(?![\s&]|$)V   → "$1‹›"
        // The lookahead becomes `v_no_amp`.
        (format!(r"(?i)(\b[a-z0-9_]*(?:{n}|secret_key_base)=){v_no_amp}"), format!("${{1}}{p}")),
        // `"password": "x"` (JSON, YAML with quotes).
        (format!(r#"(?i)("[a-z0-9_-]*{n}"\s*:\s*)"[^"]*""#), format!("${{1}}\"{p}\"")),
    ];
    raw.into_iter()
        .map(|(re, rep)| (Regex::new(&re).expect("scrub pattern"), rep))
        .collect()
});

#[cfg(test)]
mod tests {
    use super::*;

    /// Each value is a secret; none may survive.
    const SECRETS: &[&str] = &[
        concat!("sk-", "ant-api03-Zq9x7Yb2Lm4Np6Qr8St0Uv1Wx3Yz5Ab7Cd9Ef1Gh3Ij5Kl7Mn9Op1Qr3St5Uv7Wx9Yz1Ab3Cd5Ef7Gh9Ij1Kl3Mn5Op-AAAA"),
        concat!("sk-", "proj-AbCdEfGhIjKlMnOpQrStUvWxYz0123456789"),
        concat!("ghp", "_aB3dE5fG7hI9jK1lM3nO5pQ7rS9tU1vW3xY5"),
        concat!("git", "hub_pat_11ABCDEFG0123456789_abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGH"),
        concat!("gho", "_16C7e42F292c6912E7710c838347Ae178B4a"),
        concat!("xox", "b-2048-4096-AbCdEfGhIjKlMnOpQrStUvWx"),
        concat!("xox", "p-1234567890-1234567890-1234567890-abcdef0123456789abcdef0123456789"),
        concat!("AKI", "AIOSFODNN7EXAMPLE"),
        concat!("ASI", "AY34FZKBOKMUTVV7A"),
        concat!("AIz", "aSyD-9tSrke72PouQMnMX-a7eZSW0jkFMBWY"),
        concat!("glp", "at-xxxxxxxxxxxxxxxxxxxx"),
        concat!("npm", "_abcdefghijklmnopqrstuvwxyz0123456789"),
        concat!("sk_", "live_51H8abcdefghijklmnopqrstuvwx"),
        concat!("eyJ", "hbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U"),
    ];

    /// Secrets that only their context reveals: the scrubber must remove the value.
    const CONTEXTUAL: &[(&str, &str)] = &[
        ("postgres://app_user:Sup3rS3cret!@db.internal:5432/app", "Sup3rS3cret!"),
        ("mysql://root:hunter2hunter2@127.0.0.1/db", "hunter2hunter2"),
        ("redis://:r3d1sP4ss@cache:6379/0", "r3d1sP4ss"),
        ("https://deploy:gh0stT0ken@github.com/org/repo.git", "gh0stT0ken"),
        ("curl -H 'Authorization: Bearer abcDEF0123456789xyz' https://api", "abcDEF0123456789xyz"),
        ("curl -H \"Authorization: Basic dXNlcjpwYXNzd29yZA==\" https://api", "dXNlcjpwYXNzd29yZA=="),
        ("curl -H 'X-Api-Key: 9f8e7d6c5b4a39281706' https://api", "9f8e7d6c5b4a39281706"),
        ("node app.js --token tok_live_0123456789", "tok_live_0123456789"),
        ("node app.js --api-key=kk_0123456789abcdef", "kk_0123456789abcdef"),
        ("psql --password 'p@ss w0rd!'", "p@ss w0rd!"),
        ("deploy --github-token ghs_notaprefixbutsecret", "ghs_notaprefixbutsecret"),
        ("DATABASE_PASSWORD=Pl41nT3xtPw pnpm dev", "Pl41nT3xtPw"),
        ("export OPENAI_API_KEY=\"abc123def456ghi789\"", "abc123def456ghi789"),
        ("STRIPE_SECRET=whsec_0123456789abcdef node server.js", "whsec_0123456789abcdef"),
        ("GET /callback?code=1&access_token=ya29.a0AfH6SMBx&state=2", "ya29.a0AfH6SMBx"),
        ("{\"password\": \"json-secret-value\", \"user\": \"bob\"}", "json-secret-value"),
        ("https://hooks.slack.com/services/T000/B000/XXXXXXXXXXXXXXXXXXXXXXXX", "XXXXXXXXXXXXXXXXXXXXXXXX"),
        (
            "-----BEGIN RSA PRIVATE KEY-----\nMIIEpAIBAAKCAQEA0Z3VS5JJcds3xfn\n-----END RSA PRIVATE KEY-----",
            "MIIEpAIBAAKCAQEA0Z3VS5JJcds3xfn",
        ),
    ];

    fn count(s: &str) -> usize {
        s.matches(PLACEHOLDER).count()
    }

    fn argv(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn every_known_format_is_removed_from_free_text() {
        for secret in SECRETS {
            for text in [
                secret.to_string(),
                format!("x {secret} y"),
                format!("KEY={secret}"),
                format!("\"{secret}\""),
                format!("--flag={secret}"),
            ] {
                let out = scrub(&text);
                assert!(!out.contains(secret), "{text} → {out}");
            }
        }
        for (text, secret) in CONTEXTUAL {
            let out = scrub(text);
            assert!(!out.contains(secret), "{text} → {out}");
            assert!(out.contains(PLACEHOLDER), "{text}");
        }
    }

    #[test]
    fn what_is_not_a_secret_stays_readable() {
        for text in [
            "pnpm dev",
            "turbo run dev --filter=web",
            "node_modules/.bin/vite --port 5173",
            "git log --oneline -5",
            "jest --maxWorkers=10",
            "https://example.com/a/b?x=1",
            "cd /Users/me/git/app && pnpm test",
            "max-tokens 4096",
            "--max-tokens=4096",
        ] {
            assert_eq!(scrub(text), text);
        }
    }

    #[test]
    fn scrubbing_twice_changes_nothing() {
        for (text, _) in CONTEXTUAL {
            let once = scrub(text);
            assert_eq!(scrub(&once), once, "{text}");
        }
    }

    /// Secrets in process arguments, transcript lines and commit subjects:
    /// none of them survives `scrub_args` / `scrub`, and the harmless text
    /// around them does.
    #[test]
    fn args_transcripts_and_commits_lose_every_secret() {
        let all: Vec<&str> = SECRETS.iter().copied().chain(CONTEXTUAL.iter().map(|c| c.1)).collect();
        let a = argv(&[
            "node",
            "app.js",
            "--token",
            SECRETS[0],
            "--db",
            CONTEXTUAL[0].0,
            &format!("--api-key={}", SECRETS[2]),
        ]);
        let args = scrub_args(&a);
        assert!(args.starts_with("app.js --token ‹redacted› --db postgres://app_user:‹redacted›@"), "{args}");
        let mut text = vec![args];
        text.push(scrub(&format!("a1b2c3d fix: rotate {}", SECRETS[5])));
        text.push(scrub(&format!("b2c3d4e chore: {}", CONTEXTUAL[12].0)));
        text.extend(SECRETS.iter().map(|s| scrub(&format!("Bash : echo {s}"))));
        text.extend(CONTEXTUAL.iter().map(|c| scrub(c.0)));
        let joined = text.join("\n");
        for s in &all {
            assert!(!joined.contains(s), "leak: {s}");
        }
        assert!(count(&joined) >= SECRETS.len() + CONTEXTUAL.len());
        assert!(joined.contains("a1b2c3d fix: rotate"));
    }

    /// JSON text through `scrub`: its secrets go, and it stays valid JSON.
    #[test]
    fn json_text_loses_its_secrets_and_stays_json() {
        let raw = r#"{"note": "token=abc123secretvalue and \"quoted\" ghp_aB3dE5fG7hI9jK1lM3nO5pQ7rS9tU1vW3xY5", "n": 3, "list": ["AKIAIOSFODNN7EXAMPLE"]}"#;
        let out = scrub(raw);
        assert!(!out.contains("abc123secretvalue") && !out.contains("ghp_aB3") && !out.contains("AKIAIOSFODNN7EXAMPLE"), "{out}");
        assert!(out.contains(r#""n": 3"#));
        assert!(out.contains(r#"\"quoted\""#));
        let v: serde_json::Value = serde_json::from_str(&out).expect("still JSON");
        assert_eq!(v["n"], 3);
    }

    #[test]
    fn flag_value_pairs_over_two_argv_entries() {
        let a = argv(&["psql", "--password", "p@ss w0rd!", "-h", "localhost"]);
        assert_eq!(scrub_args(&a), "--password ‹redacted› -h localhost");
        // A secret flag followed by another secret flag: both values go.
        let a = argv(&["x", "--token", "--secret", "s3cr3t", "--port", "3000"]);
        assert_eq!(scrub_args(&a), "--token --secret ‹redacted› --port 3000");
        // env assignments and `--flag=value`.
        let a = argv(&["env", "DB_PASSWORD=hunter2", "max_tokens=4096", "--auth-token=abc", "node"]);
        assert_eq!(scrub_args(&a), "DB_PASSWORD=‹redacted› max_tokens=4096 --auth-token=‹redacted› node");
        // argv[0] is not part of the arguments.
        assert_eq!(scrub_args(&argv(&["node"])), "");
        assert_eq!(scrub_args(&[]), "");
        assert_eq!(scrub_args(&argv(&["pnpm", "dev"])), "dev");
    }

    #[test]
    fn secret_flag_words() {
        for f in ["--token", "-token", "--github-token", "--db_password", "--apikey", "--auth-file", "--PASSWORD"] {
            assert!(is_secret_flag(f), "{f}");
        }
        for f in ["token", "--", "--max-tokens", "--port", "-p", "--filter"] {
            assert!(!is_secret_flag(f), "{f}");
        }
    }

    #[test]
    fn long_args_are_cut_on_a_char_boundary() {
        let long = "é".repeat(400);
        let out = scrub_args(&argv(&["x", &long]));
        assert_eq!(out.chars().count(), ARGS_MAX_CHARS);
        assert!(out.ends_with('…'));
        assert!(out.starts_with("ééé"));
        let exact = "a".repeat(ARGS_MAX_CHARS);
        assert_eq!(scrub_args(&argv(&["x", &exact])), exact);
    }

    #[test]
    fn short_text_is_left_alone() {
        assert_eq!(scrub("a:b@c"), "a:b@c");
    }

    /// 1000 typical command lines. Prints the time; the budget only binds
    /// in release (`cargo test --release scrub`), debug regexes are slower.
    #[test]
    fn a_thousand_command_lines_are_cheap() {
        let lines: Vec<Vec<String>> = (0..1000)
            .map(|i| {
                argv(&[
                    "/usr/local/bin/node",
                    "/Users/me/git/app/node_modules/.bin/next",
                    "dev",
                    "--port",
                    &format!("{}", 3000 + i),
                    "--hostname",
                    "0.0.0.0",
                    &format!("--experimental-https-key=/Users/me/git/app-{i}/certs/key.pem"),
                    "DATABASE_URL=postgres://app:pw@localhost:5432/app",
                ])
            })
            .collect();
        let _ = scrub("warm up the patterns"); // compile once, outside the timing
        let t = std::time::Instant::now();
        let mut n = 0;
        for l in &lines {
            n += scrub_args(l).len();
        }
        let dt = t.elapsed();
        eprintln!("scrub_args x1000: {dt:?} ({n} bytes out)");
        if !cfg!(debug_assertions) {
            assert!(dt.as_millis() < 50, "{dt:?}");
        }
    }
}
