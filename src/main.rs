mod browser;
mod solver;
mod fingerprint;
mod server;
mod proxy;
mod ws;
mod tui;

use std::env;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

const TURNSTILE_UPDATE_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

fn main() {
    let port = env_usize("PORT", 407) as u16;
    let browsers = env_usize("BROWSERS", 2).clamp(1, 16);
    let tabs = env_usize("TABS", 10).clamp(1, 50);
    let timeout = Duration::from_millis(env_usize("timeOut", 29_000) as u64);
    let headless = should_run_headless();

    tui::banner(port, browsers, tabs);

    // empty fallback lets the service still boot if the fetch fails
    let turnstile_script = solver::fetch_turnstile_script().unwrap_or_else(|err| {
        eprintln!(
            "[Warning] Failed to dynamically fetch Turnstile script: {}. Falling back to empty script.",
            err
        );
        String::new()
    });
    let turnstile_script = Arc::<str>::from(turnstile_script.as_str());

    let service = Arc::new(solver::SolverPool::new(
        timeout,
        browsers,
        tabs,
        headless,
        turnstile_script,
    ));

    // ctrl-c / sigterm -> set the flag so worker loops tear down cleanly
    if let Err(err) = shutdown::install() {
        eprintln!("[Warning] Failed to install shutdown handler: {}", err);
    }

    start_browser_prewarm(Arc::clone(&service));
    start_turnstile_updater(Arc::clone(&service));

    if let Err(err) = server::serve(port, Arc::clone(&service)) {
        panic!("HTTP server failed: {}", err);
    }

    service.shutdown();
}

fn env_usize(name: &str, default_value: usize) -> usize {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(default_value)
}

fn env_bool(name: &str, default_value: bool) -> bool {
    match env::var(name) {
        Ok(value) => match value.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => true,
            "0" | "false" | "no" | "off" => false,
            _ => default_value,
        },
        Err(_) => default_value,
    }
}

fn should_run_headless() -> bool {
    // accept either casing of the var
    !matches!(
        env::var("headless")
            .ok()
            .or_else(|| env::var("HEADLESS").ok())
            .as_deref(),
        Some("0") | Some("false") | Some("no") | Some("off")
    )
}

fn start_browser_prewarm(service: Arc<solver::SolverPool>) {
    if !env_bool("PREWARM_BROWSER", true) {
        return;
    }

    thread::spawn(move || {
        if let Err(err) = service.prewarm(should_run_headless()) {
            eprintln!("[Warning] Browser prewarm failed: {}", err);
        }
    });
}

fn start_turnstile_updater(service: Arc<solver::SolverPool>) {
    thread::spawn(move || {
        while !shutdown::is_requested() {
            sleep_until_update_or_shutdown(TURNSTILE_UPDATE_INTERVAL);
            if shutdown::is_requested() {
                break;
            }

            match solver::fetch_turnstile_script() {
                Ok(script) => service.set_turnstile_script(Arc::<str>::from(script.as_str())),
                Err(err) => eprintln!("[Updater] Update failed: {}", err),
            }
        }
    });
}

// sleep in 1s chunks so a shutdown request doesn't wait out the full interval
fn sleep_until_update_or_shutdown(duration: Duration) {
    let mut slept = Duration::ZERO;
    while slept < duration && !shutdown::is_requested() {
        let remaining = duration.saturating_sub(slept);
        let step = remaining.min(Duration::from_secs(1));
        thread::sleep(step);
        slept += step;
    }
}

mod base64 {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    pub fn encode(input: &[u8]) -> String {
        let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
        let mut i = 0;

        while i < input.len() {
            let b0 = input[i];
            let b1 = if i + 1 < input.len() { input[i + 1] } else { 0 };
            let b2 = if i + 2 < input.len() { input[i + 2] } else { 0 };

            out.push(TABLE[(b0 >> 2) as usize] as char);
            out.push(TABLE[(((b0 & 0b0000_0011) << 4) | (b1 >> 4)) as usize] as char);

            if i + 1 < input.len() {
                out.push(TABLE[(((b1 & 0b0000_1111) << 2) | (b2 >> 6)) as usize] as char);
            } else {
                out.push('=');
            }

            if i + 2 < input.len() {
                out.push(TABLE[(b2 & 0b0011_1111) as usize] as char);
            } else {
                out.push('=');
            }

            i += 3;
        }

        out
    }
}

mod shutdown {
    use std::sync::atomic::{AtomicBool, Ordering};

    static SHUTDOWN: AtomicBool = AtomicBool::new(false);

    pub fn install() -> Result<(), String> {
        platform::install()
    }

    pub fn is_requested() -> bool {
        SHUTDOWN.load(Ordering::SeqCst)
    }

    fn request_shutdown() {
        SHUTDOWN.store(true, Ordering::SeqCst);
    }

    #[cfg(windows)]
    mod platform {
        use super::request_shutdown;

        const CTRL_C_EVENT: u32 = 0;
        const CTRL_BREAK_EVENT: u32 = 1;
        const CTRL_CLOSE_EVENT: u32 = 2;
        const CTRL_LOGOFF_EVENT: u32 = 5;
        const CTRL_SHUTDOWN_EVENT: u32 = 6;

        type HandlerRoutine = unsafe extern "system" fn(u32) -> i32;

        #[link(name = "Kernel32")]
        extern "system" {
            fn SetConsoleCtrlHandler(handler: Option<HandlerRoutine>, add: i32) -> i32;
        }

        pub fn install() -> Result<(), String> {
            let ok = unsafe { SetConsoleCtrlHandler(Some(handler), 1) };
            if ok == 0 {
                Err("SetConsoleCtrlHandler failed".to_string())
            } else {
                Ok(())
            }
        }

        unsafe extern "system" fn handler(ctrl_type: u32) -> i32 {
            match ctrl_type {
                // returning 1 = we handled it; otherwise the default handler kills us
                CTRL_C_EVENT | CTRL_BREAK_EVENT | CTRL_CLOSE_EVENT | CTRL_LOGOFF_EVENT
                | CTRL_SHUTDOWN_EVENT => {
                    request_shutdown();
                    1
                }
                _ => 0,
            }
        }
    }

    // unix: catch sigint/sigterm via raw signal()
    #[cfg(unix)]
    mod platform {
        use super::request_shutdown;

        const SIGINT: i32 = 2;
        const SIGTERM: i32 = 15;

        type SignalHandler = extern "C" fn(i32);

        extern "C" {
            fn signal(signum: i32, handler: SignalHandler) -> SignalHandler;
        }

        pub fn install() -> Result<(), String> {
            unsafe {
                signal(SIGINT, handler);
                signal(SIGTERM, handler);
            }
            Ok(())
        }

        extern "C" fn handler(_: i32) {
            request_shutdown();
        }
    }

    #[cfg(not(any(unix, windows)))]
    mod platform {
        pub fn install() -> Result<(), String> {
            Ok(())
        }
    }
}