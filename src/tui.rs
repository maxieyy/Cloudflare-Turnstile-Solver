use std::io::Write;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

const RESET: &str = "\x1b[0m";
const BOLD_CYAN: &str = "\x1b[1;36m";
const DIM: &str = "\x1b[2m";
const GRAY: &str = "\x1b[90m";
const CYAN: &str = "\x1b[96m";
const WHITE: &str = "\x1b[97m";
const YELLOW: &str = "\x1b[93m";
const GREEN: &str = "\x1b[92m";
const RED: &str = "\x1b[91m";

const BANNER: &[&str] = &[
    " ██████╗███████╗    ████████╗██╗   ██╗██████╗ ███╗   ██╗███████╗████████╗██╗██╗     ███████╗",
    "██╔════╝██╔════╝    ╚══██╔══╝██║   ██║██╔══██╗████╗  ██║██╔════╝╚══██╔══╝██║██║     ██╔════╝",
    "██║     █████╗         ██║   ██║   ██║██████╔╝██╔██╗ ██║███████╗   ██║   ██║██║     █████╗",
    "██║     ██╔══╝         ██║   ██║   ██║██╔══██╗██║╚██╗██║╚════██║   ██║   ██║██║     ██╔══╝",
    "╚██████╗██║            ██║   ╚██████╔╝██║  ██║██║ ╚████║███████║   ██║   ██║███████╗███████╗",
    " ╚═════╝╚═╝            ╚═╝    ╚═════╝ ╚═╝  ╚═╝╚═╝  ╚═══╝╚══════╝   ╚═╝   ╚═╝╚══════╝╚══════╝",
];
const HEADER: &str = "Cf Turnstile Solver  ";
const SEP: &str = "─────────────────────────────";
const DIVIDER: &str =
    "───────────────────────────────────────────────────────────────────────────────────────";

// fixed-width middle column so POST / 4.5s / FAIL all line up under each other
const COL: usize = 8;
static LOG_LOCK: Mutex<()> = Mutex::new(());

pub fn banner(port: u16, browsers: usize, tabs: usize) {
    enable_vt();
    let w = term_width();
    let debug = std::env::var("DEBUG").is_ok();
    let mut out = String::from("\n\n");

    let art_w = BANNER
        .iter()
        .map(|l| l.chars().count())
        .max()
        .unwrap_or(0)
        .min(w);
    let pad = w.saturating_sub(art_w) / 2;
    for line in BANNER {
        let l: String = line.trim_end().chars().take(art_w).collect();
        out.push_str(&" ".repeat(pad));
        out.push_str(BOLD_CYAN);
        out.push_str(&l);
        out.push_str(RESET);
        out.push('\n');
    }

    out.push('\n');
    out.push_str(&centered(HEADER, BOLD_CYAN, w));
    out.push_str("\n\n");
    if debug {
        let cfg = format!("Browsers: {}   |   Tabs: {}   |   Port: {}", browsers, tabs, port);
        out.push_str(&centered(&cfg, CYAN, w));
        out.push_str("\n\n");
    }
    let server = format!("Server: http://localhost:{}", port);
    out.push_str(&centered(&server, WHITE, w));
    out.push('\n');
    out.push_str(DIM);
    out.push_str(&center_plain(SEP, w));
    out.push_str(RESET);
    out.push_str("\n\n");

    print!("{}", out);
    let _ = std::io::stdout().flush();
}

// the POST line. printed together with the result (not at request time) so
// concurrent solves never get their two lines split apart in the log.
fn print_post(start_ts: &str, url: &str, sitekey: &str) {
    println!(
        "{GRAY}[{}]{RESET} {DIM}|{RESET}{CYAN}{}{RESET}{DIM}|{RESET} {WHITE}{}{RESET} {DIM}|{RESET} {GRAY}{}{RESET}",
        start_ts,
        cell("POST"),
        url,
        sitekey
    );
}

pub fn log_done(start_ts: &str, url: &str, sitekey: &str, elapsed_ms: u128, token: &str) {
    let _g = LOG_LOCK.lock();
    print_post(start_ts, url, sitekey);
    println!(
        "{GRAY}[{}]{RESET} {DIM}|{RESET}{YELLOW}{}{RESET}{DIM}|{RESET} {GREEN}{}{RESET}",
        now_hms(),
        cell(&secs(elapsed_ms)),
        short_token(token)
    );
    println!("{DIM}{}{RESET}", DIVIDER);
}

pub fn log_fail(start_ts: &str, url: &str, sitekey: &str, error: &str) {
    let _g = LOG_LOCK.lock();
    print_post(start_ts, url, sitekey);
    println!(
        "{GRAY}[{}]{RESET} {DIM}|{RESET}{RED}{}{RESET}{DIM}|{RESET} {RED}{}{RESET}",
        now_hms(),
        cell("FAIL"),
        error
    );
    println!("{DIM}{}{RESET}", DIVIDER);
}

fn secs(ms: u128) -> String {
    format!("{:.1}s", ms as f64 / 1000.0)
}

fn cell(s: &str) -> String {
    let n = s.chars().count();
    if n >= COL {
        return s.to_string();
    }
    let l = (COL - n) / 2;
    format!("{}{}{}", " ".repeat(l), s, " ".repeat(COL - n - l))
}

// long head + ellipsis so the useful prefix of the token stays visible
fn short_token(t: &str) -> String {
    let n = t.chars().count();
    if n <= 60 {
        return t.to_string();
    }
    let head: String = t.chars().take(57).collect();
    format!("{}...", head)
}

pub fn now_hms() -> String {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let t = d.as_secs() % 86_400;
    format!("{:02}:{:02}:{:02}", t / 3600, (t % 3600) / 60, t % 60)
}

fn center_plain(s: &str, width: usize) -> String {
    let n = s.chars().count();
    if n >= width {
        return s.to_string();
    }
    format!("{}{}", " ".repeat((width - n) / 2), s)
}

fn centered(s: &str, color: &str, width: usize) -> String {
    let n = s.chars().count();
    let pad = if n >= width { 0 } else { (width - n) / 2 };
    format!("{}{}{}{}", " ".repeat(pad), color, s, RESET)
}

// raw kernel32 on windows, no deps

#[cfg(windows)]
fn enable_vt() {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetStdHandle(which: u32) -> isize;
        fn GetConsoleMode(handle: isize, mode: *mut u32) -> i32;
        fn SetConsoleMode(handle: isize, mode: u32) -> i32;
    }
    unsafe {
        let h = GetStdHandle(0xFFFF_FFF5); // STD_OUTPUT_HANDLE (-11)
        let mut mode: u32 = 0;
        if GetConsoleMode(h, &mut mode) != 0 {
            SetConsoleMode(h, mode | 0x0004); // ENABLE_VIRTUAL_TERMINAL_PROCESSING
        }
    }
}

#[cfg(not(windows))]
fn enable_vt() {}

#[cfg(windows)]
fn term_width() -> usize {
    #[repr(C)]
    struct Coord {
        x: i16,
        y: i16,
    }
    #[repr(C)]
    struct SmallRect {
        left: i16,
        top: i16,
        right: i16,
        bottom: i16,
    }
    #[repr(C)]
    struct Csbi {
        size: Coord,
        cursor: Coord,
        attrs: u16,
        window: SmallRect,
        max_size: Coord,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetStdHandle(which: u32) -> isize;
        fn GetConsoleScreenBufferInfo(handle: isize, info: *mut Csbi) -> i32;
    }
    unsafe {
        let h = GetStdHandle(0xFFFF_FFF5);
        let mut info: Csbi = std::mem::zeroed();
        if GetConsoleScreenBufferInfo(h, &mut info) != 0 {
            let w = (info.window.right - info.window.left + 1) as i32;
            if w > 20 {
                return w as usize;
            }
        }
    }
    100
}

#[cfg(not(windows))]
fn term_width() -> usize {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|c| c.parse().ok())
        .filter(|&w: &usize| w > 20)
        .unwrap_or(100)
}
