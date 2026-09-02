use crate::browser::{ChromeWorker, DevTools};
use crate::server::json;
use crate::proxy::ProxyConfig;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone)]
pub struct RouteState {
    pub mode: String,
    pub url: String,
    pub turnstile_script: Arc<str>,
    pub page_html: Arc<str>,
    pub proxy_username: Option<Arc<str>>,
    pub proxy_password: Option<Arc<str>>,
}

pub struct SolveJob {
    pub mode: String,
    pub url: String,
    pub proxy: Option<ProxyConfig>,
    pub sitekey: Vec<String>,
    pub cdata: Option<String>,
    pub action: Option<String>,
}

impl SolveJob {
    // if the route already picked the mode (POST /turnstile or /iuam) we take that and ignore
    // any "mode" in the body; otherwise (POST /cloudflare) the mode must come from the body.
    pub fn from_json_with_mode(body: &str, mode_override: Option<&str>) -> Result<Self, String> {
        let mode = match mode_override {
            Some(mode) => mode.to_string(),
            None => match json::find_string(body, "mode") {
                Some(mode) if mode == "iuam" || mode == "turnstile" => mode,
                _ => {
                    return Err(
                        "missing or invalid mode (must be iuam or turnstile)".to_string(),
                    )
                }
            },
        };

        let url = match json::find_string(body, "url") {
            Some(value) if value.starts_with("http://") || value.starts_with("https://") => value,
            Some(_) => return Err("url must start with http:// or https://".to_string()),
            None => return Err("missing url parameter".to_string()),
        };

        let proxy = crate::proxy::parse_from_request_body(body)?;
        let sitekey = json::find_string_array(body, "sitekey").unwrap_or_default();
        if mode == "turnstile" && sitekey.is_empty() {
            return Err("missing or empty sitekey parameter".to_string());
        }
        let cdata = json::find_string(body, "cdata");
        let action = json::find_string(body, "action");

        Ok(Self {
            mode,
            url,
            proxy,
            sitekey,
            cdata,
            action,
        })
    }
}

pub struct SolveOutcome {
    pub success: bool,
    pub token: Option<String>,
    pub tokens: Option<Vec<String>>,
    pub cf_clearance: Option<String>,
    pub cookies: Option<Vec<(String, String)>>,
    pub user_agent: Option<String>,
    pub ip: Option<String>,
    pub elapsed: String,
    pub elapsed_ms: u128,
    pub timings: Option<String>,
}

impl SolveOutcome {
    pub fn to_json(&self) -> String {
        if self.cf_clearance.is_some() {
            return self.to_clearance_json();
        }
        if self.token.is_some() || self.tokens.is_some() {
            return self.to_token_json();
        }

        let mut fields = vec![
            (
                "success",
                if self.success { "true" } else { "false" }.to_string(),
            ),
            ("elapsed", json::string(&self.elapsed)),
            ("elapsed_ms", self.elapsed_ms.to_string()),
        ];
        if let Some(ref timings) = self.timings {
            fields.push(("timings", timings.clone()));
        }
        if let Some(ref token) = self.token {
            fields.push(("token", json::string(token)));
        }
        if let Some(ref tokens) = self.tokens {
            let tokens_json = format!(
                "[{}]",
                tokens
                    .iter()
                    .map(|token| json::string(token))
                    .collect::<Vec<_>>()
                    .join(",")
            );
            fields.push(("tokens", tokens_json));
        }
        if let Some(ref cookies) = self.cookies {
            let mut headers = vec![("Cookie", json::string(&cookie_header_value(cookies)))];
            if let Some(ref ua) = self.user_agent {
                headers.push(("User-Agent", json::string(ua)));
            }
            fields.push(("headers", json::object(&headers)));
        } else if let Some(ref clearance) = self.cf_clearance {
            let mut headers = vec![(
                "Cookie",
                json::string(&format!("cf_clearance={};", clearance)),
            )];
            if let Some(ref ua) = self.user_agent {
                headers.push(("User-Agent", json::string(ua)));
            }
            fields.push(("headers", json::object(&headers)));
        }
        json::object(&fields)
    }

    fn to_token_json(&self) -> String {
        let mut fields = Vec::new();
        if let Some(ref token) = self.token {
            fields.push(("token", json::string(token)));
        }
        if let Some(ref tokens) = self.tokens {
            let tokens_json = format!(
                "[{}]",
                tokens
                    .iter()
                    .map(|token| json::string(token))
                    .collect::<Vec<_>>()
                    .join(",")
            );
            fields.push(("tokens", tokens_json));
        }
        fields.push(("elapsed", json::string(&self.elapsed)));
        // per-stage timings exposed only with DEBUG so the normal response stays clean
        if std::env::var("DEBUG").is_ok() {
            if let Some(ref timings) = self.timings {
                fields.push(("timings", timings.clone()));
            }
        }
        fields.push(("status", json::string("completed")));
        json::object(&fields)
    }

    fn to_clearance_json(&self) -> String {
        let mut fields = Vec::new();
        if let Some(ref cookies) = self.cookies {
            let mut headers = vec![("Cookie", json::string(&cookie_header_value(cookies)))];
            if let Some(ref ua) = self.user_agent {
                headers.push(("User-Agent", json::string(ua)));
            }
            fields.push(("headers", json::object(&headers)));
        } else if let Some(ref clearance) = self.cf_clearance {
            let mut headers = vec![(
                "Cookie",
                json::string(&format!("cf_clearance={};", clearance)),
            )];
            if let Some(ref ua) = self.user_agent {
                headers.push(("User-Agent", json::string(ua)));
            }
            fields.push(("headers", json::object(&headers)));
        }
        fields.push((
            "ip",
            json::string(self.ip.as_deref().unwrap_or("127.0.0.1")),
        ));
        fields.push(("elapsed", json::string(&self.elapsed)));
        fields.push(("elapsed_ms", self.elapsed_ms.to_string()));
        if std::env::var("DEBUG").is_ok() {
            if let Some(ref timings) = self.timings {
                fields.push(("timings", timings.clone()));
            }
        }
        fields.push(("status", json::string("completed")));
        json::object(&fields)
    }
}

fn cookie_header_value(cookies: &[(String, String)]) -> String {
    let mut header = cookies
        .iter()
        .map(|(name, value)| format!("{}={}", name, value))
        .collect::<Vec<_>>()
        .join("; ");
    if !header.is_empty() {
        header.push(';');
    }
    header
}

fn response_ip_placeholder() -> String {
    "127.0.0.1".to_string()
}

fn ip_fetch_timeout(total_timeout: Duration) -> Duration {
    let millis = env::var("IP_FETCH_TIMEOUT_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(2_500);
    Duration::from_millis(millis).min(total_timeout)
}

fn device_timezone_id() -> String {
    env::var("DEVICE_TIMEZONE")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "Asia/Kolkata".to_string())
}

fn fetch_browser_ip(cdp: &mut DevTools, timeout: Duration) -> Option<String> {
    if timeout.is_zero() {
        return None;
    }

    let deadline = Instant::now() + timeout;
    for url in [
        "https://api.ipify.org?format=json",
        "https://api64.ipify.org?format=json",
        "https://icanhazip.com",
        "https://checkip.amazonaws.com",
    ] {
        if crate::shutdown::is_requested() {
            return None;
        }

        let remaining = deadline.checked_duration_since(Instant::now())?;
        let navigate_timeout = remaining.min(Duration::from_millis(1_500));
        let navigate_params = format!("{{\"url\":{}}}", json::string(url));
        if cdp
            .call("Page.navigate", &navigate_params, navigate_timeout)
            .is_err()
        {
            continue;
        }

        if let Some(ip) = read_ip_from_current_page(cdp, deadline) {
            return Some(ip);
        }
    }

    None
}

fn read_ip_from_current_page(cdp: &mut DevTools, deadline: Instant) -> Option<String> {
    let expression = "document.body ? document.body.innerText : ''";
    loop {
        if crate::shutdown::is_requested() {
            return None;
        }

        let remaining = deadline.checked_duration_since(Instant::now())?;
        let timeout = remaining.min(Duration::from_millis(300));
        match cdp.evaluate_string_with_timeout(expression, timeout) {
            Ok(text) => {
                if let Some(ip) = extract_ip_from_text(&text) {
                    return Some(ip);
                }
            }
            Err(err) if is_transient_navigation_error(&err) => {}
            Err(_) => {}
        }

        let remaining = deadline.checked_duration_since(Instant::now())?;
        std::thread::sleep(Duration::from_millis(75).min(remaining));
    }
}

fn extract_ip_from_text(text: &str) -> Option<String> {
    if let Some(ip) = json::find_string(text, "ip") {
        if ip.parse::<std::net::IpAddr>().is_ok() {
            return Some(ip);
        }
    }

    for part in
        text.split(|ch: char| !(ch.is_ascii_hexdigit() || ch == '.' || ch == ':' || ch == '%'))
    {
        let candidate = part.trim_matches(|ch| ch == '[' || ch == ']');
        if !candidate.contains('.') && !candidate.contains(':') {
            continue;
        }
        if let Ok(ip) = candidate.parse::<std::net::IpAddr>() {
            return Some(ip.to_string());
        }
    }

    None
}

pub struct SolverPool {
    browsers: Vec<WorkerEntry>,
    tabs_each: usize,
    timeout: Duration,
    headless: bool,
    next: AtomicUsize,
    turnstile_script: Mutex<Arc<str>>,
}

struct WorkerEntry {
    manager: Mutex<ChromeWorker>,
    available: AtomicUsize,
}

// TooManyRequests = no free tab before the queue timeout; Internal = anything that actually broke
pub enum SolveError {
    TooManyRequests,
    Internal(String),
}

impl SolverPool {
    pub fn new(
        timeout: Duration,
        browsers_count: usize,
        tabs_each: usize,
        headless: bool,
        turnstile_script: Arc<str>,
    ) -> Self {
        let mut browsers = Vec::with_capacity(browsers_count);
        for _ in 0..browsers_count {
            browsers.push(WorkerEntry {
                manager: Mutex::new(ChromeWorker::new(timeout)),
                available: AtomicUsize::new(tabs_each),
            });
        }

        Self {
            browsers,
            tabs_each,
            timeout,
            headless,
            next: AtomicUsize::new(0),
            turnstile_script: Mutex::new(turnstile_script),
        }
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    pub fn capacity_snapshot(&self) -> (usize, usize, usize) {
        let capacity = self.browsers.len() * self.tabs_each;
        let available = self
            .browsers
            .iter()
            .map(|entry| entry.available.load(Ordering::SeqCst))
            .sum::<usize>()
            .min(capacity);
        let active = capacity.saturating_sub(available);
        (capacity, available, active)
    }

    pub fn solve(
        &self,
        request: SolveJob,
    ) -> Result<SolveOutcome, SolveError> {
        let started_at = Instant::now();
        let slot = self.acquire_tab()?;
        let turnstile_script = match self.turnstile_script.lock() {
            Ok(guard) => Arc::clone(&guard),
            Err(err) => return Err(SolveError::Internal(err.to_string())),
        };
        let entry = &self.browsers[slot.browser_index];

        let (context_page, product) = {
            let mut manager = entry
                .manager
                .lock()
                .map_err(|err| SolveError::Internal(err.to_string()))?;
            manager
                .ensure_started(self.headless, None)
                .map_err(SolveError::Internal)?;
            let product = manager.browser_product_cached();
            let page = manager
                .create_context_page(request.proxy.as_ref())
                .map_err(SolveError::Internal)?;
            (page, product)
        };

        // nothing locked here so tabs go in parallel; the free-tab count is only
        // restored when `slot` drops at end of fn
        let result = (|| -> Result<SolveOutcome, String> {
            let mut page_cdp =
                DevTools::connect(&context_page.ws_url, self.timeout, self.headless)?;
            run_solve_on_cdp(
                &mut page_cdp,
                &request,
                &turnstile_script,
                product.as_deref(),
                self.timeout,
                self.headless,
                started_at,
            )
        })();

        if let Ok(mut manager) = entry.manager.lock() {
            manager.dispose_context_page(&context_page);
        }

        result.map_err(SolveError::Internal)
    }

    fn acquire_tab(&self) -> Result<PoolSlot<'_>, SolveError> {
        let count = self.browsers.len();
        if count == 0 {
            return Err(SolveError::TooManyRequests);
        }
        let deadline = Instant::now() + queue_timeout();
        loop {
            if crate::shutdown::is_requested() {
                return Err(SolveError::TooManyRequests);
            }
            for _ in 0..count {
                let index = self.next.fetch_add(1, Ordering::SeqCst) % count;
                let available = &self.browsers[index].available;
                let current = available.load(Ordering::SeqCst);
                // CAS the count down so two threads can't both grab the last slot
                if current > 0
                    && available
                        .compare_exchange(
                            current,
                            current - 1,
                            Ordering::SeqCst,
                            Ordering::SeqCst,
                        )
                        .is_ok()
                {
                    return Ok(PoolSlot {
                        available,
                        browser_index: index,
                    });
                }
            }
            if Instant::now() >= deadline {
                return Err(SolveError::TooManyRequests);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    pub fn set_turnstile_script(&self, script: Arc<str>) {
        if let Ok(mut current) = self.turnstile_script.lock() {
            *current = script;
        }
    }

    // open every browser + its CDP up front so the first real solve isn't paying startup cost
    pub fn prewarm(&self, headless: bool) -> Result<(), String> {
        for entry in &self.browsers {
            let mut manager = entry
                .manager
                .lock()
                .map_err(|err| format!("failed to lock prewarm browser: {}", err))?;
            manager.ensure_started(headless, None)?;
            manager.ensure_browser_cdp()?;
            let _ = manager.browser_product_cached();
        }
        Ok(())
    }

    pub fn shutdown(&self) {
        for entry in &self.browsers {
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                match entry.manager.try_lock() {
                    Ok(mut manager) => {
                        manager.shutdown();
                        break;
                    }
                    Err(std::sync::TryLockError::Poisoned(err)) => {
                        err.into_inner().shutdown();
                        break;
                    }
                    Err(std::sync::TryLockError::WouldBlock) if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(25));
                    }
                    Err(std::sync::TryLockError::WouldBlock) => break,
                }
            }
        }
    }
}

struct PoolSlot<'a> {
    available: &'a AtomicUsize,
    browser_index: usize,
}

impl Drop for PoolSlot<'_> {
    fn drop(&mut self) {
        self.available.fetch_add(1, Ordering::SeqCst);
    }
}

fn queue_timeout() -> Duration {
    let millis = env::var("QUEUE_TIMEOUT_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(20_000);
    Duration::from_millis(millis)
}

pub fn fetch_turnstile_script() -> Result<String, String> {
    if let Some(script) = load_local_turnstile_script()? {
        return Ok(script);
    }

    if std::env::var("DEBUG").is_ok() {
        println!("[Updater] Fetching Turnstile script via curl...");
    }
    let output = std::process::Command::new("curl")
        .args([
            "-sSL",
            "--retry",
            "4",
            "--retry-delay",
            "2",
            "--retry-all-errors",
            "--connect-timeout",
            "15",
            "https://challenges.cloudflare.com/turnstile/v0/g/825e783f7fae/api.js?onload=GHkYU6&render=explicit",
        ])
        .output()
        .map_err(|err| format!("failed to execute curl: {}", err))?;

    if !output.status.success() {
        return Err(format!(
            "curl failed with status: {}, stderr: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let script = String::from_utf8(output.stdout)
        .map_err(|err| format!("invalid UTF-8 in fetched script: {}", err))?;

    if script.trim().is_empty() {
        return Err("fetched empty Turnstile script".to_string());
    }

    if std::env::var("DEBUG").is_ok() {
        println!(
            "[Updater] Turnstile script successfully fetched ({} bytes).",
            script.len()
        );
    }
    Ok(script)
}

fn load_local_turnstile_script() -> Result<Option<String>, String> {
    for path in local_turnstile_script_paths() {
        if !path.exists() {
            continue;
        }
        let script = fs::read_to_string(&path)
            .map_err(|err| format!("failed to read {}: {}", path.display(), err))?;
        if script.trim().is_empty() {
            return Err(format!(
                "local Turnstile script is empty: {}",
                path.display()
            ));
        }
        if std::env::var("DEBUG").is_ok() {
            println!(
                "[Updater] Turnstile script loaded from {} ({} bytes).",
                path.display(),
                script.len()
            );
        }
        return Ok(Some(script));
    }

    Ok(None)
}

fn local_turnstile_script_paths() -> Vec<PathBuf> {
    vec![PathBuf::from("src/api.txt"), PathBuf::from("api.txt")]
}

#[derive(Clone)]
struct StageTimings {
    browser_ms: u128,
    device_ms: u128,
    cdp_ms: u128,
    setup_ms: u128,
    navigation_ms: u128,
}

impl StageTimings {
    fn to_json(&self, token_wait_ms: u128, total_ms: u128) -> String {
        json::object(&[
            ("browser_ms", self.browser_ms.to_string()),
            ("device_ms", self.device_ms.to_string()),
            ("cdp_ms", self.cdp_ms.to_string()),
            ("setup_ms", self.setup_ms.to_string()),
            ("navigation_ms", self.navigation_ms.to_string()),
            ("token_wait_ms", token_wait_ms.to_string()),
            ("total_ms", total_ms.to_string()),
        ])
    }
}

pub fn run_solve_on_cdp(
    cdp: &mut DevTools,
    request: &SolveJob,
    turnstile_script: &Arc<str>,
    browser_product: Option<&str>,
    timeout: Duration,
    headless: bool,
    started_at: Instant,
) -> Result<SolveOutcome, String> {
    let device_started = Instant::now();
    let applied = crate::fingerprint::applied_for_mode(request.mode == "turnstile", browser_product)?;
    let after_device = Instant::now();

    if std::env::var("DEBUG").is_ok() {
        println!(
            "[Cloudflare Solver] Applied device profile: {} (Chrome {}, Android {})",
            applied.profile.name, applied.chrome_major, applied.android_major
        );
    }

    cdp.headless = headless;
    cdp.attempted_authentications.clear();

    let setup_started = Instant::now();
    let page_html = build_challenge_html(
        request.mode == "turnstile",
        &request.sitekey,
        request.cdata.as_deref(),
        request.action.as_deref(),
    );

    cdp.interception_state = Some(RouteState {
        mode: request.mode.clone(),
        url: request.url.clone(),
        turnstile_script: Arc::clone(turnstile_script),
        page_html: Arc::clone(&page_html),
        proxy_username: request
            .proxy
            .as_ref()
            .and_then(|proxy| proxy.username.clone())
            .map(|s| Arc::from(s.as_str())),
        proxy_password: request
            .proxy
            .as_ref()
            .and_then(|proxy| proxy.password.clone())
            .map(|s| Arc::from(s.as_str())),
    });

    let fingerprint = crate::fingerprint::fingerprint_script(&applied);
    let headers = crate::fingerprint::headers_json(&applied);
    // screen orientation flips with aspect ratio: landscape (90deg) if wider than tall, else portrait
    let device_metrics = format!(
        "{{\"mobile\":true,\"width\":{},\"height\":{},\"deviceScaleFactor\":{},\"screenWidth\":{},\"screenHeight\":{},\"positionX\":0,\"positionY\":0,\"dontSetVisibleSize\":false,\"screenOrientation\":{{\"angle\":{},\"type\":{}}}}}",
        applied.profile.width,
        applied.profile.height,
        applied.profile.device_pixel_ratio,
        applied.profile.width,
        applied.profile.height,
        if applied.profile.width > applied.profile.height { 90 } else { 0 },
        if applied.profile.width > applied.profile.height {
            json::string("landscapePrimary")
        } else {
            json::string("portraitPrimary")
        }
    );

    let user_agent_params = crate::fingerprint::user_agent_override_json(&applied);
    let touch_params = "{\"enabled\":true,\"maxTouchPoints\":5}".to_string();
    let locale_params = "{\"locale\":\"en-US\"}".to_string();
    let timezone_params = format!("{{\"timezoneId\":{}}}", json::string(&device_timezone_id()));
    let emulated_media_params =
        "{\"features\":[{\"name\":\"prefers-color-scheme\",\"value\":\"dark\"}]}".to_string();
    let fingerprint_params = format!("{{\"source\":{}}}", json::string(&fingerprint));
    let extra_headers_params = format!("{{\"headers\":{}}}", headers);
    let cache_params = "{\"cacheDisabled\":false}".to_string();
    let host_pattern = host_pattern_from_url(&request.url);
    let fetch_params = fetch_enable_params(request, &host_pattern);
    let enable_dom = should_enable_dom(request, timeout);
    let mut setup_calls = vec![
        ("Page.enable", "{}"),
        ("Network.enable", "{}"),
        ("Network.setCacheDisabled", &cache_params),
        ("Emulation.setTouchEmulationEnabled", &touch_params),
        ("Emulation.setLocaleOverride", &locale_params),
        ("Emulation.setTimezoneOverride", &timezone_params),
        ("Emulation.setEmulatedMedia", &emulated_media_params),
        ("Network.setUserAgentOverride", &user_agent_params),
        ("Network.setExtraHTTPHeaders", &extra_headers_params),
        ("Emulation.setDeviceMetricsOverride", &device_metrics),
        ("Page.addScriptToEvaluateOnNewDocument", &fingerprint_params),
        ("Fetch.enable", &fetch_params),
    ];
    if enable_dom {
        setup_calls.insert(3, ("DOM.enable", "{}"));
    }

    // fire the whole batch pipelined rather than one round-trip at a time
    cdp.call_burst(setup_calls, timeout)?;
    let after_setup = Instant::now();

    // fresh context every time, so no cookies to clear here
    let navigation_started = Instant::now();
    let navigate_params = format!("{{\"url\":{}}}", json::string(&request.url));
    cdp.call("Page.navigate", &navigate_params, timeout)?;

    if request.mode == "turnstile" {
        let navigation_wait = turnstile_navigation_wait_timeout(timeout);
        if !navigation_wait.is_zero() {
            let _ = cdp.wait_for_event("Page.domContentEventFired", navigation_wait);
        }
    }
    let after_navigation = Instant::now();

    let timings = StageTimings {
        browser_ms: 0,
        device_ms: after_device.duration_since(device_started).as_millis(),
        cdp_ms: 0,
        setup_ms: after_setup.duration_since(setup_started).as_millis(),
        navigation_ms: after_navigation
            .duration_since(navigation_started)
            .as_millis(),
    };
    let solve_started = Instant::now();

    let result = if request.mode == "turnstile" {
        solve_turnstile(
            cdp,
            &applied,
            &request.sitekey,
            timeout,
            started_at,
            timings.clone(),
            solve_started,
        )
    } else {
        solve_iuam(
            cdp,
            &applied,
            &request.url,
            timeout,
            started_at,
            timings,
            solve_started,
        )
    };

    if result.is_ok() {
        let _ = cdp.call("Fetch.disable", "{}", Duration::from_secs(2));
        let _ = cdp.call(
            "Page.navigate",
            "{\"url\":\"about:blank\"}",
            Duration::from_secs(2),
        );
    }

    result
}

fn solve_turnstile(
    cdp: &mut DevTools,
    _applied: &crate::fingerprint::ActiveProfile,
    sitekeys: &[String],
    timeout: Duration,
    started_at: Instant,
    timings: StageTimings,
    solve_started: Instant,
) -> Result<SolveOutcome, String> {
    let sitekeys_json = string_array_json(sitekeys);
    let wait_expression = format!(
        r#"
        new Promise(function(resolve) {{
            var expected = {sitekeys_json}.length;
            var done = false;
            function readInputs() {{
                var inputs = document.querySelectorAll('input[name="ts-response"]');
                if (inputs.length < expected) return null;

                var results = new Array(expected).fill(null);
                for (var i = 0; i < inputs.length; i++) {{
                    var idx = parseInt(inputs[i].getAttribute('data-index') || '0', 10);
                    if (isNaN(idx) || idx >= expected) continue;
                    results[idx] = inputs[i].value;
                }}

                return results.every(function(value) {{
                    return value && value.length > 10;
                }}) ? results : null;
            }}
            function check() {{
                if (done) return;
                if (window.__tsTokenPromise && typeof window.__tsTokenPromise.then === 'function') {{
                    window.__tsTokenPromise.then(function(value) {{
                        done = true;
                        resolve(value);
                    }});
                    return;
                }}
                var results = readInputs();
                if (results) {{
                    done = true;
                    resolve(JSON.stringify(results));
                    return;
                }}
                setTimeout(check, 0);
            }}
            check();
        }})
        "#
    );

    let click_probe_timeout = turnstile_click_probe_timeout(timeout);
    if !click_probe_timeout.is_zero() {
        if let Ok(Some((x, y))) = cdp.find_turnstile_click_target(click_probe_timeout) {
            let _ = cdp.click(x, y, click_probe_timeout);
        }
    }

    let value = if turnstile_auto_click_enabled() {
        wait_turnstile_with_clicker(cdp, sitekeys.len(), &wait_expression, timeout)?
    } else {
        evaluate_value_with_navigation_retry(cdp, &wait_expression, timeout)?
    };
    let tokens: Vec<String> = match value {
        json::Value::String(s) => parse_string_tokens(&s)?,
        json::Value::Array(arr) => arr
            .into_iter()
            .map(|item| match item {
                json::Value::String(s) => Ok(s),
                other => Ok(other.to_string()),
            })
            .collect::<Result<Vec<_>, String>>()?,
        other => {
            return Err(format!(
                "Turnstile wait promise returned unexpected value: {}",
                other
            ));
        }
    };

    // single sitekey reports as `token`, multiple as `tokens`
    let token = if tokens.len() == 1 {
        Some(tokens[0].clone())
    } else {
        None
    };
    let tokens_list = if tokens.len() > 1 { Some(tokens) } else { None };

    let elapsed = started_at.elapsed();
    let elapsed_ms = elapsed.as_millis();
    let token_wait_ms = solve_started.elapsed().as_millis();

    Ok(SolveOutcome {
        success: true,
        token,
        tokens: tokens_list,
        cf_clearance: None,
        cookies: None,
        user_agent: None,
        ip: None,
        elapsed: format!("{:.2}s", elapsed.as_secs_f64()),
        elapsed_ms,
        timings: Some(timings.to_json(token_wait_ms, elapsed_ms)),
    })
}

fn turnstile_click_probe_timeout(total_timeout: Duration) -> Duration {
    let millis = env::var("TURNSTILE_CLICK_TIMEOUT_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    Duration::from_millis(millis).min(total_timeout)
}

fn string_array_json(values: &[String]) -> String {
    format!(
        "[{}]",
        values
            .iter()
            .map(|value| json::string(value))
            .collect::<Vec<_>>()
            .join(",")
    )
}

fn parse_string_tokens(value: &str) -> Result<Vec<String>, String> {
    match json::parse(value).map_err(|err| format!("invalid tokens JSON: {}", err))? {
        json::Value::Array(values) => values
            .into_iter()
            .map(|item| match item {
                json::Value::String(value) => Ok(value),
                other => Ok(other.to_string()),
            })
            .collect(),
        other => Err(format!("invalid tokens JSON: {}", other)),
    }
}

fn turnstile_navigation_wait_timeout(total_timeout: Duration) -> Duration {
    let millis = env::var("TURNSTILE_NAV_WAIT_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    Duration::from_millis(millis).min(total_timeout)
}

fn should_enable_dom(request: &SolveJob, timeout: Duration) -> bool {
    (request.mode == "turnstile" && !turnstile_click_probe_timeout(timeout).is_zero())
        || (request.mode == "iuam" && iuam_auto_click_enabled())
}

fn wait_turnstile_with_clicker(
    cdp: &mut DevTools,
    expected_count: usize,
    wait_expression: &str,
    timeout: Duration,
) -> Result<json::Value, String> {
    let deadline = Instant::now() + timeout;
    let click_interval = Duration::from_millis(turnstile_click_interval_ms());
    let click_timeout = turnstile_click_action_timeout(timeout);
    let max_attempts = turnstile_click_max_attempts();
    let mut attempts = 0usize;
    let mut last_click = Instant::now()
        .checked_sub(click_interval)
        .unwrap_or_else(Instant::now);

    loop {
        if crate::shutdown::is_requested() {
            return Err("shutdown requested".to_string());
        }

        if let Some(value) = read_ready_turnstile_tokens(cdp, expected_count, timeout) {
            return Ok(value);
        }

        if Instant::now() >= deadline {
            break;
        }

        if attempts < max_attempts && last_click.elapsed() >= click_interval {
            attempts += 1;
            if click_turnstile_like_puppeteer(cdp, click_timeout) {
                last_click = Instant::now();
            }
        }

        std::thread::sleep(Duration::from_millis(25));
    }

    evaluate_value_with_navigation_retry(cdp, wait_expression, Duration::from_millis(1))
}

fn read_ready_turnstile_tokens(
    cdp: &mut DevTools,
    expected_count: usize,
    timeout: Duration,
) -> Option<json::Value> {
    let expression = format!(
        r#"
        (function() {{
            try {{
                var expected = {};
                if (window.__tsTokens && window.__tsTokens.length >= expected) {{
                    var tokens = Array.prototype.slice.call(window.__tsTokens, 0, expected);
                    if (tokens.every(function(value) {{ return value && value.length > 10; }})) {{
                        return JSON.stringify(tokens);
                    }}
                }}

                var inputs = document.querySelectorAll('input[name="ts-response"]');
                if (inputs.length >= expected) {{
                    var results = new Array(expected).fill(null);
                    for (var i = 0; i < inputs.length; i++) {{
                        var idx = parseInt(inputs[i].getAttribute('data-index') || '0', 10);
                        if (isNaN(idx) || idx >= expected) continue;
                        results[idx] = inputs[i].value;
                    }}
                    if (results.every(function(value) {{ return value && value.length > 10; }})) {{
                        return JSON.stringify(results);
                    }}
                }}
            }} catch (e) {{}}
            return null;
        }})()
        "#,
        expected_count
    );

    match cdp.evaluate_value(&expression, false, timeout.min(Duration::from_millis(250))) {
        Ok(json::Value::String(value)) if !value.is_empty() => Some(json::Value::String(value)),
        _ => None,
    }
}

fn click_turnstile_like_puppeteer(cdp: &mut DevTools, timeout: Duration) -> bool {
    let Ok(targets) = cdp.find_turnstile_click_targets(timeout) else {
        return false;
    };
    if targets.is_empty() {
        return false;
    }

    let mut clicked = false;
    for (x, y) in targets {
        if cdp.click(x, y, timeout).is_ok() {
            clicked = true;
        }
    }
    clicked
}

fn turnstile_auto_click_enabled() -> bool {
    env_bool("TURNSTILE_AUTO_CLICK", true)
}

fn iuam_auto_click_enabled() -> bool {
    env_bool("IUAM_AUTO_CLICK", turnstile_auto_click_enabled())
}

fn turnstile_click_interval_ms() -> u64 {
    env::var("TURNSTILE_CLICK_INTERVAL_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(30)
        .clamp(10, 1000)
}

fn turnstile_click_action_timeout(total_timeout: Duration) -> Duration {
    let millis = env::var("TURNSTILE_CLICK_ACTION_TIMEOUT_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(250);
    Duration::from_millis(millis).min(total_timeout)
}

fn turnstile_click_max_attempts() -> usize {
    env::var("TURNSTILE_CLICK_MAX_ATTEMPTS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(240)
        .clamp(1, 1000)
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

// evaluate JS, retrying briefly when the page navigates out from under us (the
// context dies mid-eval during a CF redirect/reload); any non-navigation error
// bubbles up right away
fn evaluate_value_with_navigation_retry(
    cdp: &mut DevTools,
    expression: &str,
    timeout: Duration,
) -> Result<json::Value, String> {
    let deadline = Instant::now() + timeout;
    loop {
        if crate::shutdown::is_requested() {
            return Err("shutdown requested".to_string());
        }

        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| "Runtime.evaluate timed out".to_string())?;
        match cdp.evaluate_value(expression, true, remaining) {
            Ok(value) => return Ok(value),
            Err(err) if is_transient_navigation_error(&err) => {
                if Instant::now() >= deadline {
                    return Err(err);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(err) => return Err(err),
        }
    }
}

// CDP errors that just mean "the page navigated, retry" rather than a real failure
fn is_transient_navigation_error(error: &str) -> bool {
    error.contains("Execution context was destroyed")
        || error.contains("Cannot find context")
        || error.contains("Cannot find default execution context")
        || error.contains("Inspected target navigated")
}

fn solve_iuam(
    cdp: &mut DevTools,
    applied: &crate::fingerprint::ActiveProfile,
    url: &str,
    timeout: Duration,
    started_at: Instant,
    timings: StageTimings,
    solve_started: Instant,
) -> Result<SolveOutcome, String> {
    let cookies = wait_for_iuam_cookies(cdp, url, timeout)?;
    let clearance = cookies
        .iter()
        .find_map(|(name, value)| {
            if name == "cf_clearance" {
                Some(value.clone())
            } else {
                None
            }
        })
        .ok_or_else(|| "IUAM solved without cf_clearance cookie".to_string())?;

    let browser_ip =
        fetch_browser_ip(cdp, ip_fetch_timeout(timeout)).unwrap_or_else(response_ip_placeholder);

    let reported_ua = cdp
        .evaluate_string_with_timeout("navigator.userAgent", Duration::from_secs(2))
        .unwrap_or_else(|_| applied.user_agent.clone());

    let elapsed = started_at.elapsed();
    let elapsed_ms = elapsed.as_millis();
    let token_wait_ms = solve_started.elapsed().as_millis();

    Ok(SolveOutcome {
        success: true,
        token: None,
        tokens: None,
        cf_clearance: Some(clearance),
        cookies: Some(cookies),
        user_agent: Some(reported_ua),
        ip: Some(browser_ip),
        elapsed: format!("{:.2}s", elapsed.as_secs_f64()),
        elapsed_ms,
        timings: Some(timings.to_json(token_wait_ms, elapsed_ms)),
    })
}

fn wait_for_iuam_cookies(
    cdp: &mut DevTools,
    url: &str,
    timeout: Duration,
) -> Result<Vec<(String, String)>, String> {
    let deadline = Instant::now() + timeout;
    let params = format!("{{\"urls\":[{}]}}", json::string(url));
    let click_enabled = iuam_auto_click_enabled();
    let click_interval = Duration::from_millis(turnstile_click_interval_ms());
    let click_timeout = turnstile_click_action_timeout(timeout);
    let max_attempts = turnstile_click_max_attempts();
    let mut attempts = 0usize;
    let mut last_click = Instant::now()
        .checked_sub(click_interval)
        .unwrap_or_else(Instant::now);

    loop {
        if crate::shutdown::is_requested() {
            return Err("shutdown requested".to_string());
        }

        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| "IUAM timed out waiting for cf_clearance".to_string())?;
        let call_timeout = remaining.min(Duration::from_millis(250));

        match cdp.call("Network.getCookies", &params, call_timeout) {
            Ok(response) => {
                let cookies = find_cookie_values(&response, &["cf_clearance", "__ts_bm"]);
                if cookies.iter().any(|(name, _)| name == "cf_clearance") {
                    return Ok(cookies);
                }
            }
            Err(err) if is_transient_navigation_error(&err) => {}
            Err(err) => return Err(format!("failed to read IUAM cookies: {}", err)),
        }

        if click_enabled && attempts < max_attempts && last_click.elapsed() >= click_interval {
            attempts += 1;
            if click_turnstile_like_puppeteer(cdp, click_timeout) {
                last_click = Instant::now();
            }
        }

        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err("IUAM timed out waiting for cf_clearance".to_string());
        }
        std::thread::sleep(Duration::from_millis(75).min(remaining));
    }
}

fn find_cookie_values(response: &str, expected_names: &[&str]) -> Vec<(String, String)> {
    let value = match json::parse(response) {
        Ok(value) => value,
        Err(_) => return Vec::new(),
    };
    let browser_cookies = value
        .get("result")
        .and_then(|result| result.get("cookies"))
        .and_then(|cookies| cookies.as_array());

    let Some(browser_cookies) = browser_cookies else {
        return Vec::new();
    };

    let mut found = Vec::new();
    for expected_name in expected_names {
        for cookie in browser_cookies {
            if cookie.get("name").and_then(|value| value.as_str()) == Some(*expected_name) {
                if let Some(value) = cookie.get("value").and_then(value_to_string) {
                    found.push(((*expected_name).to_string(), value));
                }
                break;
            }
        }
    }

    found
}

fn build_challenge_html(
    is_turnstile: bool,
    sitekeys: &[String],
    cdata: Option<&str>,
    action: Option<&str>,
) -> Arc<str> {
    if !is_turnstile {
        return Arc::from("");
    }
    // parallelism = render N widgets per sitekey and race them; stagger spaces their render calls
    let parallelism = turnstile_parallelism();
    let stagger_ms = turnstile_stagger_ms();
    let mut widget_divs = String::new();
    let mut render_calls = String::new();
    for (i, key) in sitekeys.iter().enumerate() {
        for attempt in 0..parallelism {
            let mut options = format!(
                "{{sitekey:{},callback:function(token){{window.__tsSetToken({}, token);}}",
                script_json_string(key),
                i
            );
            if let Some(cd) = cdata {
                if !cd.is_empty() {
                    options.push_str(&format!(",cData:{}", script_json_string(cd)));
                }
            }
            if let Some(act) = action {
                if !act.is_empty() {
                    options.push_str(&format!(",action:{}", script_json_string(act)));
                }
            }
            options.push('}');

            widget_divs.push_str(&format!(
                "<div id=\"cf-turnstile-{}-{}\" class=\"cf-turnstile\"></div>\n",
                i, attempt
            ));
            let render_call = format!(
                "window.__tsWidgetIds[{}].push(window.turnstile.render('#cf-turnstile-{}-{}', {}));",
                i, i, attempt, options
            );
            if stagger_ms == 0 {
                render_calls.push_str(&render_call);
                render_calls.push('\n');
            } else {
                render_calls.push_str(&format!(
                    "setTimeout(function(){{ {} }}, {});\n",
                    render_call,
                    attempt * stagger_ms
                ));
            }
        }
    }
    let html = format!(
        "<!DOCTYPE html>\n<html>\n<head>\n    <title>Challenge</title>\n    <script>\nwindow.__tsExpected = {};\nwindow.__tsTokens = new Array(window.__tsExpected).fill(null);\nwindow.__tsWidgetIds = new Array(window.__tsExpected).fill(null).map(function() {{ return []; }});\nwindow.__tsTokenPromise = new Promise(function(resolve) {{ window.__tsResolveTokens = resolve; }});\nwindow.__tsRemoveWidgets = function(index) {{\n  var ids = window.__tsWidgetIds[index] || [];\n  for (var i = 0; i < ids.length; i++) {{\n    try {{ window.turnstile.remove(ids[i]); }} catch (e) {{}}\n  }}\n  window.__tsWidgetIds[index] = [];\n}};\nwindow.__tsSetToken = function(index, token) {{\n  if (window.__tsTokens[index]) return;\n  window.__tsTokens[index] = token;\n  window.__tsRemoveWidgets(index);\n  var input = document.querySelector('input[name=\"ts-response\"][data-index=\"' + index + '\"]');\n  if (!input) {{\n    input = document.createElement('input');\n    input.type = 'hidden';\n    input.name = 'ts-response';\n    input.setAttribute('data-index', index);\n    document.body.appendChild(input);\n  }}\n  input.value = token;\n  if (window.__tsTokens.every(function(value) {{ return value && value.length > 10; }})) {{\n    window.__tsResolveTokens(JSON.stringify(window.__tsTokens));\n  }}\n}};\nwindow.__tsRenderTurnstile = function() {{\n  if (!window.turnstile || typeof window.turnstile.render !== 'function') {{\n    setTimeout(window.__tsRenderTurnstile, 0);\n    return;\n  }}\n{}\n}};\n    </script>\n</head>\n<body>\n{}\n<script src=\"https://challenges.cloudflare.com/turnstile/v0/api.js?onload=__tsRenderTurnstile&render=explicit\" async defer></script>\n</body>\n</html>",
        sitekeys.len(),
        render_calls,
        widget_divs
    );
    Arc::from(html.as_str())
}

fn turnstile_parallelism() -> usize {
    env::var("TURNSTILE_PARALLELISM")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(1)
        .clamp(1, 8)
}

fn turnstile_stagger_ms() -> usize {
    env::var("TURNSTILE_STAGGER_MS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0)
        .min(500)
}

// JSON-escape for embedding inside a <script>; also escape `<` so it can't break out of the tag
fn script_json_string(value: &str) -> String {
    json::string(value).replace('<', "\\u003c")
}

fn host_pattern_from_url(url: &str) -> String {
    let rest = match url.find("://") {
        Some(index) => &url[index + 3..],
        None => return "*".to_string(),
    };
    let host_end = rest
        .find(|c: char| c == '/' || c == '?' || c == '#')
        .unwrap_or(rest.len());
    let host = &rest[..host_end];
    if host.is_empty() {
        "*".to_string()
    } else {
        format!("*{}*", host)
    }
}

// normally just the target host + cloudflare/turnstile urls, but iuam-with-auth-proxy
// needs "*" so the auth challenge on every request reaches our handler
fn fetch_enable_params(request: &SolveJob, host_pattern: &str) -> String {
    let patterns = if request.mode == "iuam" && proxy_needs_cdp_auth(request.proxy.as_ref()) {
        vec!["*".to_string()]
    } else {
        vec![
            host_pattern.to_string(),
            "*challenges.cloudflare.com*".to_string(),
            "*turnstile*".to_string(),
        ]
    };

    let patterns_json = patterns
        .iter()
        .map(|pattern| format!("{{\"urlPattern\":{}}}", json::string(pattern)))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{{\"patterns\":[{}],\"handleAuthRequests\":true}}",
        patterns_json
    )
}

// only http proxies with a username need CDP-level auth; socks/no-auth handle it themselves
fn proxy_needs_cdp_auth(proxy: Option<&ProxyConfig>) -> bool {
    proxy
        .filter(|proxy| proxy.scheme == "http")
        .and_then(|proxy| proxy.username.as_ref())
        .is_some()
}

pub fn handle_paused_request(
    cdp: &mut DevTools,
    msg: &str,
    state: &RouteState,
) -> Result<(), String> {
    let event = parse_fetch_event(msg)?;
    let request_id = event.request_id;

    if event.has_auth_challenge {
        if cdp
            .attempted_authentications
            .iter()
            .any(|attempted| attempted == &request_id)
        {
            let params = format!(
                "{{\"requestId\":{},\"authChallengeResponse\":{{\"response\":\"CancelAuth\"}}}}",
                json::string(&request_id)
            );
            return cdp.send_cdp_event("Fetch.continueWithAuth", &params);
        }

        if let (Some(ref username), Some(ref password)) =
            (&state.proxy_username, &state.proxy_password)
        {
            cdp.attempted_authentications.push(request_id.clone());
            let params = format!(
                "{{\"requestId\":{},\"authChallengeResponse\":{{\"response\":\"ProvideCredentials\",\"username\":{},\"password\":{}}}}}",
                json::string(&request_id),
                json::string(username),
                json::string(password)
            );
            return cdp.send_cdp_event("Fetch.continueWithAuth", &params);
        } else {
            let params = format!(
                "{{\"requestId\":{},\"authChallengeResponse\":{{\"response\":\"Default\"}}}}",
                json::string(&request_id)
            );
            return cdp.send_cdp_event("Fetch.continueWithAuth", &params);
        }
    }

    let req_url = event
        .request_url
        .ok_or_else(|| "request.url not found in Fetch.requestPaused event".to_string())?;
    let resource_type = event.resource_type;

    if state.mode == "turnstile" {
        // compare urls with query + trailing slash stripped, and accept either being a
        // suffix of the other to absorb http/https + with/without-www mismatches
        let target_normalized = state.url.trim_end_matches('/');
        let req_normalized = req_url
            .split('?')
            .next()
            .unwrap_or(&req_url)
            .trim_end_matches('/');
        let is_main_doc = resource_type.eq_ignore_ascii_case("document")
            && (req_normalized == target_normalized
                || target_normalized.ends_with(req_normalized)
                || req_normalized.ends_with(target_normalized));

        if is_main_doc {
            let params =
                fulfill_request_params(&request_id, "text/html", state.page_html.as_bytes(), &[]);
            cdp.send_cdp_event("Fetch.fulfillRequest", &params)?;
        } else if req_url.contains("challenges.cloudflare.com/reports/v0/post") {
            let params = format!(
                "{{\"requestId\":{},\"errorReason\":\"Failed\"}}",
                json::string(&request_id)
            );
            cdp.send_cdp_event("Fetch.failRequest", &params)?;
        } else if req_url.contains("challenges.cloudflare.com/turnstile")
            && req_url.contains("/api.js")
        {
            let params = fulfill_request_params(
                &request_id,
                "application/javascript",
                state.turnstile_script.as_bytes(),
                &[],
            );
            cdp.send_cdp_event("Fetch.fulfillRequest", &params)?;
        } else if req_url.contains("challenges.cloudflare.com")
            || req_url.contains("/cdn-cgi/challenge-platform/")
            || req_url.starts_with("chrome-extension://")
        {
            let params = format!("{{\"requestId\":{}}}", json::string(&request_id));
            cdp.send_cdp_event("Fetch.continueRequest", &params)?;
        } else {
            let params = format!(
                "{{\"requestId\":{},\"errorReason\":\"Failed\"}}",
                json::string(&request_id)
            );
            cdp.send_cdp_event("Fetch.failRequest", &params)?;
        }
    } else if state.mode == "iuam" {
        if req_url.contains("challenges.cloudflare.com/turnstile") && req_url.contains("/api.js") {
            let params = fulfill_request_params(
                &request_id,
                "application/javascript",
                state.turnstile_script.as_bytes(),
                &[("access-control-allow-origin", "*")],
            );
            cdp.send_cdp_event("Fetch.fulfillRequest", &params)?;
        } else {
            let params = format!("{{\"requestId\":{}}}", json::string(&request_id));
            cdp.send_cdp_event("Fetch.continueRequest", &params)?;
        }
    }

    Ok(())
}

struct RouteEvent {
    request_id: String,
    request_url: Option<String>,
    resource_type: String,
    has_auth_challenge: bool,
}

// tolerates both wrapped ({params:{...}}) and flat shapes
fn parse_fetch_event(msg: &str) -> Result<RouteEvent, String> {
    let value = json::parse(msg).map_err(|err| format!("invalid Fetch event JSON: {}", err))?;
    let params = value.get("params").unwrap_or(&value);

    let request_id = params
        .get("requestId")
        .or_else(|| value.get("requestId"))
        .and_then(value_to_string)
        .ok_or_else(|| "requestId not found in Fetch event".to_string())?;

    let request_url = params
        .get("request")
        .and_then(|request| request.get("url"))
        .or_else(|| params.get("url"))
        .or_else(|| value.get("url"))
        .and_then(value_to_string);

    let resource_type = params
        .get("resourceType")
        .or_else(|| value.get("resourceType"))
        .and_then(value_to_string)
        .unwrap_or_default();

    Ok(RouteEvent {
        request_id,
        request_url,
        resource_type,
        has_auth_challenge: params.get("authChallenge").is_some()
            || value.get("authChallenge").is_some(),
    })
}

fn value_to_string(value: &json::Value) -> Option<String> {
    match value {
        json::Value::String(s) => Some(s.clone()),
        json::Value::Number(n) => {
            if n.fract() == 0.0 {
                Some(format!("{:.0}", n))
            } else {
                Some(n.to_string())
            }
        }
        json::Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn fulfill_request_params(
    request_id: &str,
    content_type: &str,
    body: &[u8],
    extra_headers: &[(&str, &str)],
) -> String {
    let mut headers: Vec<(&str, String)> = vec![
        ("content-type", content_type.to_string()),
        ("content-length", body.len().to_string()),
    ];
    headers.extend(
        extra_headers
            .iter()
            .map(|(name, value)| (*name, (*value).to_string())),
    );

    let mut response_headers = String::from("[");
    for (index, (name, value)) in headers.iter().enumerate() {
        if index > 0 {
            response_headers.push(',');
        }
        response_headers.push_str(&format!(
            "{{\"name\":{},\"value\":{}}}",
            json::string(name),
            json::string(value)
        ));
    }
    response_headers.push(']');

    format!(
        "{{\"requestId\":{},\"responseCode\":200,\"responsePhrase\":\"OK\",\"responseHeaders\":{},\"body\":{}}}",
        json::string(request_id),
        response_headers,
        json::string(&crate::base64::encode(body))
    )
}