//! Minimal HTTP/1.1 client for the Docker Engine API over a Unix socket.
//!
//! GET, and a body-less POST to stop containers. One connection is kept open and reused for every request (keep
//! alive); it is reopened once if the daemon closed it in the meantime.
//! Bodies are read with Content-Length, chunked transfer coding, or until EOF,
//! and never grow past a fixed bound so a bad peer cannot blow memory.

use std::fmt;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Read and write timeout on the socket.
pub const IO_TIMEOUT: Duration = Duration::from_secs(2);
/// Largest body we accept. A `containers/json` with 100 containers is ~200 KB.
pub const MAX_BODY: usize = 8 << 20;
/// Longest status, header, chunk-size or trailer line.
const MAX_LINE: usize = 8 << 10;
/// Most header (or trailer) lines in one response.
const MAX_HEADERS: usize = 128;

#[derive(Debug)]
pub enum HttpError {
    Io(io::Error),
    /// The peer sent something that is not a valid HTTP/1.x response.
    Malformed(&'static str),
    /// Body, line or header count above our bounds.
    TooLarge,
    /// A well-formed response with a status other than 200.
    Status(u16),
}

impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HttpError::Io(e) => write!(f, "io: {e}"),
            HttpError::Malformed(what) => write!(f, "malformed response: {what}"),
            HttpError::TooLarge => f.write_str("response too large"),
            HttpError::Status(s) => write!(f, "HTTP status {s}"),
        }
    }
}

impl From<io::Error> for HttpError {
    fn from(e: io::Error) -> Self {
        HttpError::Io(e)
    }
}

impl HttpError {
    /// The connection died under us (closed idle keep-alive, daemon restart):
    /// worth one retry on a fresh connection. Timeouts are not: a hung daemon
    /// would only cost another 2 s.
    fn is_stale_connection(&self) -> bool {
        match self {
            HttpError::Io(e) => matches!(
                e.kind(),
                io::ErrorKind::UnexpectedEof
                    | io::ErrorKind::BrokenPipe
                    | io::ErrorKind::ConnectionReset
                    | io::ErrorKind::ConnectionAborted
                    | io::ErrorKind::NotConnected
            ),
            _ => false,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub body: Vec<u8>,
    /// False when the connection cannot be reused (Connection: close, HTTP/1.0,
    /// or a body delimited by EOF).
    pub keep_alive: bool,
}

/// Reads one response from `r`. Never panics, whatever the bytes.
pub fn read_response<R: BufRead>(r: &mut R, max_body: usize) -> Result<Response, HttpError> {
    let mut line = Vec::new();
    // Skip interim 1xx responses (100 Continue): never sent for a GET, but legal.
    let (status, http10) = loop {
        read_line(r, &mut line)?;
        let (status, http10) = parse_status_line(&line)?;
        if (100..200).contains(&status) {
            skip_headers(r, &mut line)?;
            continue;
        }
        break (status, http10);
    };

    let mut chunked = false;
    let mut content_length: Option<u64> = None;
    let mut keep_alive = !http10;
    let mut n = 0;
    loop {
        read_line(r, &mut line)?;
        if line.is_empty() {
            break;
        }
        n += 1;
        if n > MAX_HEADERS {
            return Err(HttpError::TooLarge);
        }
        let colon = line
            .iter()
            .position(|&b| b == b':')
            .ok_or(HttpError::Malformed("header without colon"))?;
        let name = line[..colon].trim_ascii();
        let value = line[colon + 1..].trim_ascii();
        if name.eq_ignore_ascii_case(b"transfer-encoding") {
            // The last coding decides how the body is framed.
            let last = value.rsplit(|&b| b == b',').next().unwrap_or_default();
            chunked = last.trim_ascii().eq_ignore_ascii_case(b"chunked");
        } else if name.eq_ignore_ascii_case(b"content-length") {
            let v = parse_dec(value).ok_or(HttpError::Malformed("bad content-length"))?;
            if content_length.is_some_and(|old| old != v) {
                return Err(HttpError::Malformed("conflicting content-length"));
            }
            content_length = Some(v);
        } else if name.eq_ignore_ascii_case(b"connection") {
            for token in value.split(|&b| b == b',') {
                let t = token.trim_ascii();
                if t.eq_ignore_ascii_case(b"close") {
                    keep_alive = false;
                } else if t.eq_ignore_ascii_case(b"keep-alive") && http10 {
                    keep_alive = true;
                }
            }
        }
    }

    let mut body = Vec::new();
    if status == 204 || status == 304 {
        // No body by definition.
    } else if chunked {
        read_chunked(r, &mut body, &mut line, max_body)?;
    } else if let Some(len) = content_length {
        if len > max_body as u64 {
            return Err(HttpError::TooLarge);
        }
        body.resize(len as usize, 0);
        r.read_exact(&mut body)?;
    } else {
        // Delimited by the end of the connection.
        keep_alive = false;
        r.take(max_body as u64 + 1).read_to_end(&mut body)?;
        if body.len() > max_body {
            return Err(HttpError::TooLarge);
        }
    }
    Ok(Response { status, body, keep_alive })
}

fn read_chunked<R: BufRead>(
    r: &mut R,
    body: &mut Vec<u8>,
    line: &mut Vec<u8>,
    max_body: usize,
) -> Result<(), HttpError> {
    loop {
        read_line(r, line)?;
        // chunk-size [ ";" chunk-ext ]
        let size_part = match line.iter().position(|&b| b == b';') {
            Some(i) => &line[..i],
            None => &line[..],
        };
        let size = parse_hex(size_part.trim_ascii()).ok_or(HttpError::Malformed("bad chunk size"))?;
        if size == 0 {
            // Trailer section, then the final empty line.
            return skip_headers(r, line);
        }
        if size > (max_body - body.len()) as u64 {
            return Err(HttpError::TooLarge);
        }
        let start = body.len();
        body.resize(start + size as usize, 0);
        r.read_exact(&mut body[start..])?;
        read_line(r, line)?;
        if !line.is_empty() {
            return Err(HttpError::Malformed("chunk not followed by CRLF"));
        }
    }
}

/// Consumes header (or trailer) lines up to and including the empty line.
fn skip_headers<R: BufRead>(r: &mut R, line: &mut Vec<u8>) -> Result<(), HttpError> {
    for _ in 0..=MAX_HEADERS {
        read_line(r, line)?;
        if line.is_empty() {
            return Ok(());
        }
    }
    Err(HttpError::TooLarge)
}

/// "HTTP/1.1 200 OK" → (200, is HTTP/1.0).
fn parse_status_line(line: &[u8]) -> Result<(u16, bool), HttpError> {
    let rest = line
        .strip_prefix(b"HTTP/1.")
        .ok_or(HttpError::Malformed("not an HTTP/1.x status line"))?;
    let (minor, rest) = rest.split_first().ok_or(HttpError::Malformed("short status line"))?;
    if !minor.is_ascii_digit() || rest.first() != Some(&b' ') || rest.len() < 4 {
        return Err(HttpError::Malformed("bad status line"));
    }
    let code = &rest[1..4];
    if !code.iter().all(u8::is_ascii_digit) || rest.get(4).is_some_and(|&b| b != b' ') {
        return Err(HttpError::Malformed("bad status code"));
    }
    let status = code.iter().fold(0u16, |acc, &d| acc * 10 + (d - b'0') as u16);
    Ok((status, *minor == b'0'))
}

/// Reads one line into `out`, without its CRLF (a bare LF is accepted).
/// EOF before the end of the line is an `UnexpectedEof` error.
fn read_line<R: BufRead>(r: &mut R, out: &mut Vec<u8>) -> Result<(), HttpError> {
    out.clear();
    loop {
        let buf = match r.fill_buf() {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.into()),
        };
        if buf.is_empty() {
            return Err(io::Error::from(io::ErrorKind::UnexpectedEof).into());
        }
        let (take, done) = match buf.iter().position(|&b| b == b'\n') {
            Some(i) => (i + 1, true),
            None => (buf.len(), false),
        };
        if out.len() + take > MAX_LINE {
            return Err(HttpError::TooLarge);
        }
        out.extend_from_slice(&buf[..take]);
        r.consume(take);
        if done {
            out.pop(); // '\n'
            if out.last() == Some(&b'\r') {
                out.pop();
            }
            return Ok(());
        }
    }
}

fn parse_dec(s: &[u8]) -> Option<u64> {
    if s.is_empty() || s.len() > 19 || !s.iter().all(u8::is_ascii_digit) {
        return None;
    }
    Some(s.iter().fold(0u64, |acc, &d| acc * 10 + (d - b'0') as u64))
}

fn parse_hex(s: &[u8]) -> Option<u64> {
    if s.is_empty() || s.len() > 15 {
        return None;
    }
    s.iter().try_fold(0u64, |acc, &d| Some(acc * 16 + (d as char).to_digit(16)? as u64))
}

// ------------------------------------------------------------------ client

/// A keep-alive connection to one Unix socket.
pub struct Client {
    path: PathBuf,
    conn: Option<BufReader<UnixStream>>,
    /// Connections opened so far (tests and diagnostics).
    pub connects: u32,
}

impl Client {
    pub fn new(path: &Path) -> Self {
        Client { path: path.to_path_buf(), conn: None, connects: 0 }
    }

    /// GET `path` (already prefixed, e.g. "/v1.41/_ping"). Returns the body of
    /// a 200 response.
    pub fn get(&mut self, path: &str) -> Result<Vec<u8>, HttpError> {
        let r = self.send("GET", path, IO_TIMEOUT)?;
        if r.status != 200 {
            return Err(HttpError::Status(r.status));
        }
        Ok(r.body)
    }

    /// POST `path` with an empty body, waiting up to `timeout` for the answer
    /// (a container stop answers once the container is down). Any status.
    pub fn post(&mut self, path: &str, timeout: Duration) -> Result<Response, HttpError> {
        self.send("POST", path, timeout)
    }

    pub fn close(&mut self) {
        self.conn = None;
    }

    fn send(&mut self, method: &str, path: &str, timeout: Duration) -> Result<Response, HttpError> {
        let reused = self.conn.is_some();
        match self.try_send(method, path, timeout) {
            Err(e) if reused && e.is_stale_connection() => {
                // The kept-alive connection was closed by the daemon: once more
                // on a fresh one.
                self.conn = None;
                self.try_send(method, path, timeout)
            }
            other => other,
        }
    }

    fn try_send(&mut self, method: &str, path: &str, timeout: Duration) -> Result<Response, HttpError> {
        let res = self.exchange(method, path, timeout);
        match &res {
            Ok(r) if r.keep_alive => {}
            // Unknown state after an error, or the server asked to close.
            _ => self.conn = None,
        }
        res
    }

    fn exchange(&mut self, method: &str, path: &str, timeout: Duration) -> Result<Response, HttpError> {
        // setsockopt fails on a socket the peer closed (EINVAL on macOS): such
        // a connection is dropped and a fresh one opened.
        if let Some(c) = &self.conn
            && c.get_ref().read_timeout().ok().flatten() != Some(timeout)
            && c.get_ref().set_read_timeout(Some(timeout)).is_err()
        {
            self.conn = None;
        }
        let conn = match &mut self.conn {
            Some(c) => c,
            slot => {
                let s = UnixStream::connect(&self.path)?;
                s.set_read_timeout(Some(timeout))?;
                s.set_write_timeout(Some(IO_TIMEOUT))?;
                self.connects += 1;
                slot.insert(BufReader::with_capacity(64 << 10, s))
            }
        };
        conn.get_mut().write_all(request(method, path).as_bytes())?;
        read_response(conn, MAX_BODY)
    }
}

/// The request head. Bodies are never sent: a POST says so with a zero
/// Content-Length, which dockerd wants on a POST.
fn request(method: &str, path: &str) -> String {
    let len = if method == "GET" { "" } else { "Content-Length: 0\r\n" };
    format!("{method} {path} HTTP/1.1\r\nHost: docker\r\nUser-Agent: devload\r\nAccept: application/json\r\n{len}\r\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn parse(bytes: &[u8]) -> Result<Response, HttpError> {
        read_response(&mut Cursor::new(bytes), MAX_BODY)
    }

    /// A reader that hands out at most `n` bytes per read, to split every
    /// token across reads.
    struct Trickle<'a> {
        data: &'a [u8],
        n: usize,
    }
    impl Read for Trickle<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let k = self.n.min(buf.len()).min(self.data.len());
            buf[..k].copy_from_slice(&self.data[..k]);
            self.data = &self.data[k..];
            Ok(k)
        }
    }

    const CHUNKED: &[u8] = b"HTTP/1.1 200 OK\r\n\
Api-Version: 1.54\r\n\
Content-Type: application/json\r\n\
Transfer-Encoding: chunked\r\n\
\r\n\
5\r\n[{\"a\"\r\n\
7;name=value;x\r\n:1},{\"b\r\n\
A \r\n\":2},{\"c\":\r\n\
2\r\n3}\r\n\
1\r\n]\r\n\
0\r\n\
X-Trailer: yes\r\n\
\r\n";
    const CHUNKED_BODY: &[u8] = b"[{\"a\":1},{\"b\":2},{\"c\":3}]";

    #[test]
    fn chunked_with_extensions_and_trailers() {
        let r = parse(CHUNKED).unwrap();
        assert_eq!(r.status, 200);
        assert!(r.keep_alive);
        assert_eq!(r.body, CHUNKED_BODY);
    }

    #[test]
    fn content_length() {
        let r = parse(b"HTTP/1.1 200 OK\r\ncontent-length: 5\r\n\r\nhelloEXTRA").unwrap();
        assert_eq!(r.body, b"hello");
        assert!(r.keep_alive);
    }

    #[test]
    fn bare_lf_and_case_insensitive_headers() {
        let r = parse(b"HTTP/1.1 200 OK\nTRANSFER-ENCODING: Chunked\n\n3\nabc\n0\n\n").unwrap();
        assert_eq!(r.body, b"abc");
    }

    #[test]
    fn body_until_eof_disables_keep_alive() {
        let r = parse(b"HTTP/1.1 200 OK\r\n\r\nall of it").unwrap();
        assert_eq!(r.body, b"all of it");
        assert!(!r.keep_alive);
    }

    #[test]
    fn connection_close_and_http10() {
        let r = parse(b"HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 0\r\n\r\n").unwrap();
        assert!(!r.keep_alive);
        let r = parse(b"HTTP/1.0 200 OK\r\nContent-Length: 0\r\n\r\n").unwrap();
        assert!(!r.keep_alive);
        let r = parse(b"HTTP/1.0 200 OK\r\nConnection: Keep-Alive\r\nContent-Length: 0\r\n\r\n").unwrap();
        assert!(r.keep_alive);
    }

    #[test]
    fn interim_and_bodyless_statuses() {
        let r = parse(b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 204 No Content\r\n\r\n").unwrap();
        assert_eq!(r.status, 204);
        assert!(r.body.is_empty());
        let r = parse(b"HTTP/1.1 404 Not Found\r\nContent-Length: 2\r\n\r\n{}").unwrap();
        assert_eq!(r.status, 404);
    }

    #[test]
    fn status_line_variants() {
        assert_eq!(parse_status_line(b"HTTP/1.1 200").unwrap(), (200, false));
        assert_eq!(parse_status_line(b"HTTP/1.0 503 Service Unavailable").unwrap(), (503, true));
        for bad in [&b""[..], b"HTTP/2 200 OK", b"HTTP/1.1 20 OK", b"HTTP/1.1 2000 OK", b"HTTP/1.1  200", b"HTTP/1.x 200", b"http/1.1 200"] {
            assert!(parse_status_line(bad).is_err(), "{:?}", String::from_utf8_lossy(bad));
        }
    }

    #[test]
    fn split_reads_give_the_same_result() {
        for n in 1..=CHUNKED.len() {
            let mut r = BufReader::with_capacity(n.max(1), Trickle { data: CHUNKED, n });
            let resp = read_response(&mut r, MAX_BODY).unwrap();
            assert_eq!(resp.body, CHUNKED_BODY, "split {n}");
        }
    }

    #[test]
    fn keep_alive_reads_back_to_back_responses() {
        let mut both = CHUNKED.to_vec();
        both.extend_from_slice(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");
        let mut r = BufReader::with_capacity(7, Trickle { data: &both, n: 3 });
        assert_eq!(read_response(&mut r, MAX_BODY).unwrap().body, CHUNKED_BODY);
        assert_eq!(read_response(&mut r, MAX_BODY).unwrap().body, b"ok");
    }

    #[test]
    fn every_truncation_fails_without_panicking() {
        for cut in 0..CHUNKED.len() {
            assert!(parse(&CHUNKED[..cut]).is_err(), "prefix of {cut} bytes accepted");
        }
        let cl = b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello";
        for cut in 0..cl.len() {
            assert!(parse(&cl[..cut]).is_err(), "prefix of {cut} bytes accepted");
        }
    }

    #[test]
    fn random_mutations_never_panic() {
        let mut seed = 0x9e3779b97f4a7c15u64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let alphabet = b"0123456789abcdefABCDEF;: \r\n\t-xHTP/.";
        for _ in 0..20_000 {
            let mut v = CHUNKED.to_vec();
            for _ in 0..1 + next() % 4 {
                let i = (next() % v.len() as u64) as usize;
                match next() % 3 {
                    0 => v[i] = alphabet[(next() % alphabet.len() as u64) as usize],
                    1 => {
                        v.remove(i);
                    }
                    _ => v.insert(i, alphabet[(next() % alphabet.len() as u64) as usize]),
                }
            }
            let _ = read_response(&mut Cursor::new(&v), 64);
            let _ = read_response(&mut Cursor::new(&v), MAX_BODY);
        }
    }

    #[test]
    fn request_heads() {
        assert_eq!(
            request("GET", "/v1.41/containers/json"),
            "GET /v1.41/containers/json HTTP/1.1\r\nHost: docker\r\nUser-Agent: devload\r\nAccept: application/json\r\n\r\n"
        );
        assert_eq!(
            request("POST", "/v1.41/containers/ab12/stop?t=5"),
            "POST /v1.41/containers/ab12/stop?t=5 HTTP/1.1\r\nHost: docker\r\nUser-Agent: devload\r\nAccept: application/json\r\nContent-Length: 0\r\n\r\n"
        );
    }

    #[test]
    fn bad_input_is_rejected() {
        let cases: &[&[u8]] = &[
            b"garbage\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nNoColon\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nContent-Length: -1\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nContent-Length: 2\r\n\r\nab",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabcX\r\n0\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nffffffffffffffff\r\n",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n;ext\r\n",
        ];
        for c in cases {
            assert!(parse(c).is_err(), "{:?}", String::from_utf8_lossy(c));
        }
    }

    #[test]
    fn bounds_are_enforced() {
        let big = b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n";
        assert!(matches!(read_response(&mut Cursor::new(&big[..]), 10), Err(HttpError::TooLarge)));
        let chunks = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n8\r\n12345678\r\n8\r\n12345678\r\n0\r\n\r\n";
        assert!(matches!(read_response(&mut Cursor::new(&chunks[..]), 10), Err(HttpError::TooLarge)));
        assert_eq!(read_response(&mut Cursor::new(&chunks[..]), 16).unwrap().body.len(), 16);
        let eof = b"HTTP/1.1 200 OK\r\n\r\n0123456789A";
        assert!(matches!(read_response(&mut Cursor::new(&eof[..]), 10), Err(HttpError::TooLarge)));
        let mut long = b"HTTP/1.1 200 OK\r\nX: ".to_vec();
        long.resize(long.len() + MAX_LINE, b'a');
        assert!(matches!(parse(&long), Err(HttpError::TooLarge)));
        let mut many = b"HTTP/1.1 200 OK\r\n".to_vec();
        for _ in 0..=MAX_HEADERS {
            many.extend_from_slice(b"X: y\r\n");
        }
        many.extend_from_slice(b"\r\n");
        assert!(matches!(parse(&many), Err(HttpError::TooLarge)));
    }
}
