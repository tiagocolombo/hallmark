//! `hall web`: the web app (ADR 0052).
//!
//! A small HTTP server on 127.0.0.1 serves one page (`assets/`, embedded in
//! the binary) and an API that runs hall's own commands the way a person
//! does in a terminal: each call is `hall --json <args>` in a child
//! process, in one of the repos the app shows, and its reply is the
//! command's JSON. So the CLI stays the API, and the page can do only what
//! [`crate::cli::allowed_on_web`] lets it.
//!
//! Only this machine can reach it, and only with the token in the address
//! it prints. The page reads the token from the address's fragment, which
//! browsers never send, and sends it in a header with every API call; a
//! page on another site can't set that header without a preflight, which
//! is never answered. A request whose `Host` isn't this address is
//! refused, so a DNS-rebinding page can't reach it either.

use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use schemars::JsonSchema;
use serde::Serialize;
use serde_json::{Value, json};
use tiny_http::{Header, Method, Request, Server};

use crate::cli::WebArgs;
use crate::error::{ErrorCode, HallError, Result};
use crate::output::{Mode, Render, Response};
use crate::paths;
use crate::repo::user_dir;

/// The port `hall web` asks for first.
pub const DEFAULT_PORT: u16 = 4747;
/// The longest one command may run for the page: `hall update` builds hall.
const RUN_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// How long a restarted server keeps trying for its port.
const REBIND: Duration = Duration::from_secs(10);
/// The largest request body read.
const MAX_BODY: u64 = 1 << 20;
/// The most lines a diff keeps.
const DIFF_LINES: usize = 20_000;
/// Hands the token to the server that replaces this one after an update,
/// so the open page keeps working.
const TOKEN_ENV: &str = "HALL_WEB_TOKEN";

/// Session variables a command run for the page must not see: what it
/// does is the person's doing, not an agent's or a hook's.
const SCRUB: &[&str] = &[
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_SESSION_ID",
    "CLAUDE_CODE_SESSION_ID",
    "HALL_ACTOR",
    "HALL_HEADLESS",
];

/// The page, by path: (path, content type, body).
const ASSETS: &[(&str, &str, &str)] = &[
    (
        "/",
        "text/html; charset=utf-8",
        include_str!("assets/index.html"),
    ),
    (
        "/app.js",
        "text/javascript; charset=utf-8",
        include_str!("assets/app.js"),
    ),
    (
        "/app.css",
        "text/css; charset=utf-8",
        include_str!("assets/app.css"),
    ),
    (
        "/icon.svg",
        "image/svg+xml",
        include_str!("assets/icon.svg"),
    ),
];

/// A repo the app shows: the registry's, plus the one `hall web` started in.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct WebRepo {
    pub name: String,
    pub path: String,
}

/// The line `hall web` prints once it is listening.
#[derive(Debug, Serialize, JsonSchema)]
pub struct WebStart {
    /// The address to open, with the token in its fragment.
    pub url: String,
    pub port: u16,
    pub repos: Vec<WebRepo>,
}

/// The reply, once the server stops on its own (an update restarted it).
#[derive(Debug, Serialize, JsonSchema)]
pub struct WebOut {
    pub url: String,
    pub port: u16,
    /// restarted.
    pub stopped: String,
}

impl Render for WebOut {
    fn render(&self) -> String {
        format!("hall web restarted at {}", self.url)
    }
}

struct Ctx {
    token: String,
    port: u16,
    /// This hall, found at start: after an update, Linux names a replaced
    /// running binary "… (deleted)", and the new one is at this path.
    exe: PathBuf,
    cwd: PathBuf,
    restart: AtomicBool,
}

pub fn web(cwd: &Path, a: WebArgs, mode: Mode) -> Result<Response> {
    let inherited = std::env::var(TOKEN_ENV).ok().filter(|t| !t.is_empty());
    // Keep the credential out of every child: only a restart passes it on.
    // SAFETY: set at start, before this process starts a thread.
    unsafe { std::env::remove_var(TOKEN_ENV) };
    let token = match &inherited {
        Some(t) => t.clone(),
        None => new_token()?,
    };
    let server = bind(a.port, inherited.is_some())?;
    let port = server
        .server_addr()
        .to_ip()
        .map(|s| s.port())
        .unwrap_or(DEFAULT_PORT);
    let url = format!("http://127.0.0.1:{port}/#token={token}");
    let ctx = Arc::new(Ctx {
        token,
        port,
        exe: std::env::current_exe()?,
        cwd: cwd.to_path_buf(),
        restart: AtomicBool::new(false),
    });
    print_start(
        mode,
        &WebStart {
            url: url.clone(),
            port,
            repos: repos(&ctx.cwd).0,
        },
    );
    if !a.no_open {
        // The token must not be in any process's arguments, which other
        // users can read: open a private file that redirects to the address.
        if let Some(page) = launch_page(&url) {
            let _ = crate::tui::open(&page);
        }
    }
    let server = Arc::new(server);
    for req in server.incoming_requests() {
        let c = ctx.clone();
        let s = server.clone();
        std::thread::spawn(move || {
            if handle(&c, req) {
                c.restart.store(true, Ordering::SeqCst);
                s.unblock();
            }
        });
        if ctx.restart.load(Ordering::SeqCst) {
            break;
        }
    }
    if !ctx.restart.load(Ordering::SeqCst) {
        return Err(HallError::internal("the web server stopped").next(["hall web"]));
    }
    // Free the port before the new server asks for it.
    drop(server);
    restart(&ctx, mode)?;
    Ok(Response::new(
        WebOut {
            url,
            port,
            stopped: "restarted".into(),
        },
        vec![format!("hall web --port {port}")],
    ))
}

/// Writes a page only you can read that sends the browser to `url`, and
/// returns its path.
fn launch_page(url: &str) -> Option<String> {
    let dir = launch_dir();
    std::fs::create_dir_all(&dir).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    let file = dir.join("web-open.html");
    let html = format!(
        "<!doctype html><meta charset=utf-8><meta http-equiv=refresh content=\"0;url={url}\">\
         <title>Hallmark</title><script>location.replace(\"{url}\")</script>\n"
    );
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let _ = std::fs::remove_file(&file);
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&file)
            .ok()?;
        f.write_all(html.as_bytes()).ok()?;
    }
    #[cfg(not(unix))]
    std::fs::write(&file, html).ok()?;
    // The native path: Windows' explorer reads a `/` as a switch.
    Some(file.to_string_lossy().into_owned())
}

/// Where the redirect page goes. On Linux a snap-packaged browser can't read
/// hidden directories under the home, so it goes in a visible, private one.
fn launch_dir() -> std::path::PathBuf {
    let home = std::env::var_os("HOME").filter(|h| !h.is_empty());
    match home {
        Some(home) if cfg!(all(unix, not(target_os = "macos"))) => {
            std::path::PathBuf::from(home).join("hallmark-web")
        }
        _ => user_dir(),
    }
}

/// A random token, 32 hex digits.
fn new_token() -> Result<String> {
    let mut buf = [0u8; 16];
    getrandom::fill(&mut buf).map_err(|e| HallError::internal(format!("no randomness: {e}")))?;
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}

/// Listens on 127.0.0.1: the port asked for, else the default, else any
/// free one. A restarted server waits for its old port to come free.
fn bind(port: Option<u16>, restarted: bool) -> Result<Server> {
    let start = Instant::now();
    loop {
        match Server::http(("127.0.0.1", port.unwrap_or(DEFAULT_PORT))) {
            Ok(s) => return Ok(s),
            Err(_) if restarted && start.elapsed() < REBIND => {
                crate::daemon::nap(Duration::from_millis(200));
            }
            Err(e) if port.is_some() => {
                return Err(HallError::usage(format!(
                    "can't listen on 127.0.0.1:{}: {e}",
                    port.unwrap_or_default()
                ))
                .next(["hall web"]));
            }
            Err(_) => {
                return Server::http(("127.0.0.1", 0))
                    .map_err(|e| HallError::internal(format!("can't listen on 127.0.0.1: {e}")));
            }
        }
    }
}

fn print_start(mode: Mode, start: &WebStart) {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    match mode {
        Mode::Json => {
            let _ = writeln!(out, "{}", json!(start));
        }
        Mode::Text => {
            let _ = writeln!(out, "Hallmark is open at {}", start.url);
            let _ = writeln!(out, "Only this machine can reach it. Ctrl-C stops it.");
        }
    }
    let _ = out.flush();
}

/// Starts the new hall in this one's place, on the same port and token:
/// on Unix it replaces this process, elsewhere this one waits for it.
fn restart(ctx: &Ctx, mode: Mode) -> Result<()> {
    let mut c = crate::process::command(&ctx.exe);
    c.args(["web", "--port", &ctx.port.to_string(), "--no-open"])
        .current_dir(&ctx.cwd)
        .env(TOKEN_ENV, &ctx.token);
    if mode == Mode::Json {
        c.arg("--json");
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let e = c.exec();
        Err(HallError::internal(format!("could not restart hall web: {e}")).next(["hall web"]))
    }
    #[cfg(not(unix))]
    {
        c.status()
            .map(|_| ())
            .map_err(|e| HallError::internal(format!("could not restart hall web: {e}")))
    }
}

type Reply = tiny_http::Response<Cursor<Vec<u8>>>;

/// Answers one request; true when it asked for a restart.
fn handle(ctx: &Ctx, mut req: Request) -> bool {
    let (reply, restart) = route(ctx, &mut req);
    let _ = req.respond(reply);
    restart
}

fn route(ctx: &Ctx, req: &mut Request) -> (Reply, bool) {
    if !host_ok(req, ctx.port) {
        return (
            error(403, &HallError::usage("this address is not served")),
            false,
        );
    }
    let url = req.url().to_string();
    let (path, query) = url.split_once('?').unwrap_or((&url, ""));
    let method = req.method().clone();
    if method == Method::Get
        && let Some((_, ctype, body)) = ASSETS.iter().find(|(p, _, _)| *p == path)
    {
        return (respond(200, ctype, body.as_bytes().to_vec()), false);
    }
    if !path.starts_with("/api/") {
        return (
            error(404, &HallError::usage(format!("no page at {path}"))),
            false,
        );
    }
    if path == "/api/ping" {
        let v = json!({ "ok": true, "version": env!("HALL_VERSION") });
        return (json_reply(200, &v), false);
    }
    if !token_ok(req, &ctx.token) {
        let e = HallError::usage("missing or wrong token: open the address `hall web` printed")
            .next(["hall web"]);
        return (error(401, &e), false);
    }
    let r = match (method, path) {
        (Method::Get, "/api/repos") => {
            let (repos, here) = repos(&ctx.cwd);
            Ok(json!({ "repos": repos, "here": here }))
        }
        (Method::Post, "/api/run") => read_json(req).and_then(|body| api_run(ctx, &body)),
        (Method::Get, "/api/diff") => api_diff(ctx, query),
        (Method::Post, "/api/restart") => {
            return (json_reply(200, &json!({ "ok": true })), true);
        }
        _ => Err(HallError::usage(format!("no API at {path}"))),
    };
    match r {
        Ok(v) => (json_reply(200, &v), false),
        Err(e) => (error(400, &e), false),
    }
}

/// The `Host` header names this server: 127.0.0.1, localhost or [::1], on
/// its port.
fn host_ok(req: &Request, port: u16) -> bool {
    let Some(host) = header(req, "Host") else {
        return false;
    };
    ["127.0.0.1", "localhost", "[::1]"]
        .iter()
        .any(|h| host.eq_ignore_ascii_case(&format!("{h}:{port}")))
}

fn token_ok(req: &Request, token: &str) -> bool {
    let Some(given) = header(req, "X-Hall-Token") else {
        return false;
    };
    // The same time for every wrong token of the right length.
    given.len() == token.len()
        && given
            .bytes()
            .zip(token.bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
}

fn header(req: &Request, name: &'static str) -> Option<String> {
    req.headers()
        .iter()
        .find(|h| h.field.equiv(name))
        .map(|h| h.value.as_str().to_string())
}

fn read_json(req: &mut Request) -> Result<Value> {
    let mut body = String::new();
    req.as_reader()
        .take(MAX_BODY)
        .read_to_string(&mut body)
        .map_err(|e| HallError::usage(format!("unreadable request: {e}")))?;
    serde_json::from_str(&body).map_err(|e| HallError::usage(format!("not JSON: {e}")))
}

/// The repos to show, and which one `hall web` started in.
fn repos(cwd: &Path) -> (Vec<WebRepo>, Option<usize>) {
    let (found, here) = crate::tui::data::discover(cwd, &user_dir().join("repos.json"));
    let repos = found
        .into_iter()
        .map(|(name, path)| WebRepo {
            name,
            path: paths::display(&path),
        })
        .collect();
    (repos, here)
}

/// The directory of the repo a call names; only a repo the app shows.
fn repo_dir(ctx: &Ctx, named: Option<&str>) -> Result<PathBuf> {
    let named = named.ok_or_else(|| HallError::usage("no repo given"))?;
    repos(&ctx.cwd)
        .0
        .into_iter()
        .find(|r| r.path == named)
        .map(|r| PathBuf::from(r.path))
        .ok_or_else(|| {
            HallError::new(
                ErrorCode::NotARepo,
                format!("{named} isn't a repo hall knows"),
            )
        })
}

/// `POST /api/run {"repo", "args"}`: runs `hall --json <args>` in the repo.
/// The reply is the command's last line; the lines before it (`hall
/// trace`'s steps) are in `lines`.
fn api_run(ctx: &Ctx, body: &Value) -> Result<Value> {
    let dir = repo_dir(ctx, body["repo"].as_str())?;
    let args: Vec<String> = body["args"]
        .as_array()
        .ok_or_else(|| HallError::usage("args must be a list of strings"))?
        .iter()
        .map(|a| a.as_str().map(str::to_string))
        .collect::<Option<_>>()
        .ok_or_else(|| HallError::usage("args must be a list of strings"))?;
    crate::cli::allowed_on_web(&args)?;
    run_hall(&ctx.exe, &dir, &args)
}

/// Runs `hall --json <args>` in `dir` and reads its JSON lines. `--json`
/// goes first: after a `--` in `args` it would be one more value.
fn run_hall(exe: &Path, dir: &Path, args: &[String]) -> Result<Value> {
    let mut c = crate::process::command(exe);
    c.arg("--json")
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for v in SCRUB {
        c.env_remove(v);
    }
    let mut child = c
        .spawn()
        .map_err(|e| HallError::internal(format!("could not run hall: {e}")))?;
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let out = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(o) = stdout.as_mut() {
            let _ = o.read_to_string(&mut s);
        }
        s
    });
    let err = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(e) = stderr.as_mut() {
            let _ = e.read_to_string(&mut s);
        }
        s
    });
    let code = wait(&mut child, RUN_TIMEOUT);
    let stdout = out.join().unwrap_or_default();
    let stderr = err.join().unwrap_or_default();
    let Some(code) = code else {
        return Err(HallError::new(
            ErrorCode::CommandFailed,
            format!(
                "`hall {}` ran past {}m and was stopped",
                args.join(" "),
                RUN_TIMEOUT.as_secs() / 60
            ),
        ));
    };
    let mut lines: Vec<Value> = stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    let reply = lines.pop().unwrap_or_else(|| {
        let tail: Vec<&str> = stderr.lines().rev().take(5).collect();
        let msg = tail.into_iter().rev().collect::<Vec<_>>().join("\n");
        envelope(&HallError::internal(if msg.is_empty() {
            format!("`hall {}` printed nothing", args.join(" "))
        } else {
            msg
        }))
    });
    Ok(json!({ "exit": code, "reply": reply, "lines": lines }))
}

/// The exit code, or None when the deadline passed and the child was
/// killed. `wait_timeout` may return early without an exit, so it waits
/// again until the deadline.
fn wait(child: &mut std::process::Child, limit: Duration) -> Option<i32> {
    use wait_timeout::ChildExt;
    let deadline = Instant::now() + limit;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match child.wait_timeout(left) {
            Ok(Some(s)) => return Some(s.code().unwrap_or(1)),
            Ok(None) if Instant::now() < deadline => continue,
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

/// `GET /api/diff?repo=…&id=…`: `git diff <base>...<commit>` for the item,
/// as the terminal app's diff view shows it: its frozen commit, else its
/// branch.
fn api_diff(ctx: &Ctx, query: &str) -> Result<Value> {
    let param = |k: &str| {
        query
            .split('&')
            .filter_map(|p| p.split_once('='))
            .find(|(key, _)| *key == k)
            .map(|(_, v)| decode(v))
    };
    let dir = repo_dir(ctx, param("repo").as_deref())?;
    let id = param("id").ok_or_else(|| HallError::usage("no item given"))?;
    let status = run_hall(&ctx.exe, &dir, &["status".into(), id.clone()])?;
    let item = &status["reply"];
    if item.get("error").is_some() {
        return Ok(json!({ "error": item["error"] }));
    }
    let base = item["base"].as_str().unwrap_or("main");
    let rev = item["sha"]
        .as_str()
        .or(item["branch"].as_str())
        .unwrap_or("HEAD");
    let range = format!("{base}...{rev}");
    let text = crate::git::run(&dir, &["diff", "--no-color", "--no-ext-diff", &range])?;
    let mut lines: Vec<&str> = text.lines().collect();
    let more = lines.len().saturating_sub(DIFF_LINES);
    lines.truncate(DIFF_LINES);
    Ok(json!({ "id": id, "range": range, "lines": lines, "more": more }))
}

/// Decodes `%XX` and `+` in a query value.
fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                let hex = std::str::from_utf8(&b[i + 1..i + 3]).ok();
                match hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    Some(v) => {
                        out.push(v);
                        i += 2;
                    }
                    None => out.push(b'%'),
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn respond(status: u16, ctype: &str, body: Vec<u8>) -> Reply {
    let mut r = tiny_http::Response::from_data(body).with_status_code(status);
    for (k, v) in [
        ("Content-Type", ctype),
        ("Cache-Control", "no-store"),
        ("X-Content-Type-Options", "nosniff"),
        ("Referrer-Policy", "no-referrer"),
        (
            "Content-Security-Policy",
            // Inline styles carry the page's sizes and colours; scripts
            // only ever come from /app.js.
            "default-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; frame-ancestors 'none'; base-uri 'none'; form-action 'none'",
        ),
    ] {
        if let Ok(h) = Header::from_bytes(k.as_bytes(), v.as_bytes()) {
            r.add_header(h);
        }
    }
    r
}

fn json_reply(status: u16, v: &Value) -> Reply {
    respond(status, "application/json", v.to_string().into_bytes())
}

fn error(status: u16, e: &HallError) -> Reply {
    json_reply(status, &envelope(e))
}

/// The error as every command prints it: `{"error":{"code","message","next"}}`.
fn envelope(e: &HallError) -> Value {
    serde_json::to_value(e.envelope()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_query_values() {
        assert_eq!(decode("H-0001"), "H-0001");
        assert_eq!(decode("%2FUsers%2Fme%2Fapp"), "/Users/me/app");
        assert_eq!(decode("a+b%20c"), "a b c");
        assert_eq!(decode("50%"), "50%");
        assert_eq!(decode("%zz"), "%zz");
    }

    #[test]
    fn tokens_are_32_hex_digits_and_differ() {
        let a = new_token().unwrap();
        let b = new_token().unwrap();
        assert_eq!(a.len(), 32);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn every_asset_is_served_with_its_type() {
        for path in ["/", "/app.js", "/app.css", "/icon.svg"] {
            assert!(ASSETS.iter().any(|(p, _, b)| *p == path && !b.is_empty()));
        }
    }
}
