use crate::server::json;
use crate::server::json::Value;
use crate::proxy::{LocalProxyBridge, ProxyConfig};
use crate::ws::WebSocket;
use std::env;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub struct ChromeWorker {
    pub(crate) process: Option<Child>,
    pub(crate) port: Option<u16>,
    pub(crate) user_data_dir: Option<PathBuf>,
    pub(crate) cdp: Option<DevTools>,
    // connection scoped to the browser target, not a page; used for Target.* calls
    pub(crate) browser_cdp: Option<DevTools>,
    pub(crate) browser_product: Option<String>,
    pub(crate) timeout: Duration,
    pub(crate) headless: bool,
    pub current_proxy: Option<ProxyConfig>,
    // local listener that fronts proxies chrome can't speak to directly (auth/socks)
    proxy_bridge: Option<LocalProxyBridge>,
}

pub(crate) struct TabHandle {
    pub(crate) browser_context_id: String,
    pub(crate) target_id: String,
    pub(crate) ws_url: String,
}

impl ChromeWorker {
    pub fn new(timeout: Duration) -> Self {
        Self {
            process: None,
            port: None,
            user_data_dir: None,
            cdp: None,
            browser_cdp: None,
            browser_product: None,
            timeout,
            headless: false,
            current_proxy: None,
            proxy_bridge: None,
        }
    }

    pub(crate) fn ensure_started(
        &mut self,
        headless: bool,
        proxy: Option<ProxyConfig>,
    ) -> Result<(), String> {
        // proxy/headless are baked into launch flags, so a change means full restart
        let headless_changed = self.process.is_some() && self.headless != headless;
        let proxy_changed = self.current_proxy != proxy;
        if self.current_proxy != proxy {
            self.current_proxy = proxy;
        }
        self.headless = headless;

        if let Some(process) = self.process.as_mut() {
            if proxy_changed || headless_changed {
                if env::var("DEBUG").is_ok() {
                    println!("[Browser] Browser config changed. Restarting Chrome...");
                }
                let _ = process.kill();
                let _ = process.wait();
                self.process = None;
                self.port = None;
                self.cdp = None;
                self.browser_cdp = None;
                self.browser_product = None;
                self.proxy_bridge = None;
                if let Some(dir) = self.user_data_dir.take() {
                    let _ = fs::remove_dir_all(dir);
                }
            } else {
                match process.try_wait() {
                    Ok(None) => return Ok(()),
                    Ok(Some(status)) => {
                        eprintln!("[Browser] previous Chrome exited: {}", status);
                        self.process = None;
                        self.port = None;
                        self.cdp = None;
                self.browser_cdp = None;
                self.browser_product = None;
                        self.proxy_bridge = None;
                    }
                    Err(err) => {
                        eprintln!("[Browser] failed to inspect Chrome process: {}", err);
                        self.process = None;
                        self.port = None;
                        self.cdp = None;
                self.browser_cdp = None;
                self.browser_product = None;
                        self.proxy_bridge = None;
                    }
                }
            }
        }

        self.proxy_bridge = None;

        let chrome = find_chrome_executable()?;
        let port = free_port()?;
        // unique profile dir per process+port so parallel workers don't collide
        let user_data_dir =
            env::temp_dir().join(format!("tsolver-{}-{}", std::process::id(), port));
        fs::create_dir_all(&user_data_dir)
            .map_err(|err| format!("failed to create user data dir: {}", err))?;

        let disabled_features = concat!(
            "Translate,",
            "BackForwardCache,",
            "AcceptCHFrame,",
            "MediaRouter,",
            "OptimizationHints,",
            "RenderDocument,",
            "PartitionAllocSchedulerLoopQuarantineTaskControlledPurge,",
            "ProcessPerSiteUpToMainFrameThreshold,",
            "IsolateSandboxedIframes,",
            "IsolateOrigins,",
            "site-per-process"
        );
        let enabled_features =
            "PdfOopif,NetworkService,NetworkServiceInProcess,WebRtcHideLocalIpsWithMdns";

        let mut args = vec![
            "--allow-pre-commit-input".to_string(),
            "--disable-background-networking".to_string(),
            "--disable-background-timer-throttling".to_string(),
            "--disable-backgrounding-occluded-windows".to_string(),
            "--disable-breakpad".to_string(),
            "--disable-client-side-phishing-detection".to_string(),
            "--disable-component-extensions-with-background-pages".to_string(),
            "--disable-crash-reporter".to_string(),
            "--disable-default-apps".to_string(),
            "--disable-dev-shm-usage".to_string(),
            "--disable-hang-monitor".to_string(),
            "--disable-infobars".to_string(),
            "--disable-ipc-flooding-protection".to_string(),
            "--disable-popup-blocking".to_string(),
            "--disable-prompt-on-repost".to_string(),
            "--disable-renderer-backgrounding".to_string(),
            "--disable-renderer-accessibility".to_string(),
            "--disable-search-engine-choice-screen".to_string(),
            "--disable-sync".to_string(),
            "--export-tagged-pdf".to_string(),
            "--force-color-profile=srgb".to_string(),
            "--generate-pdf-document-outline".to_string(),
            "--metrics-recording-only".to_string(),
            "--no-default-browser-check".to_string(),
            "--no-first-run".to_string(),
            "--no-pings".to_string(),
            "--disable-domain-reliability".to_string(),
            "--password-store=basic".to_string(),
            "--use-mock-keychain".to_string(),
            format!("--disable-features={}", disabled_features),
            format!("--enable-features={}", enabled_features),
        ];

        if headless {
            args.push("--headless=new".to_string());
            args.push("--hide-scrollbars".to_string());
            args.push("--mute-audio".to_string());
        }

        args.push("--disable-extensions".to_string());
        args.push("about:blank".to_string());

        args.extend([
            "--disable-blink-features=AutomationControlled".to_string(),
            "--disable-site-isolation-trials".to_string(),
            "--disable-gpu".to_string(),
            "--no-sandbox".to_string(),
            "--disable-setuid-sandbox".to_string(),
            "--ignore-certificate-errors".to_string(),
            "--ignore-certificate-errors-spki-list".to_string(),
            "--disable-accelerated-2d-canvas".to_string(),
            "--hide-scrollbars".to_string(),
            "--disable-notifications".to_string(),
            "--force-webrtc-ip-handling-policy=disable_non_proxied_udp".to_string(),
            "--webrtc-ip-handling-policy=disable_non_proxied_udp".to_string(),
            "--enforce-webrtc-ip-permission-check".to_string(),
        ]);

        if cfg!(target_os = "linux") {
            args.push("--no-zygote".to_string());
        }

        // proxies chrome can't do natively (auth'd / socks) get fronted by a local
        // bridge; chrome then just points at 127.0.0.1 and the bridge forwards
        let mut proxy_bridge = None;
        if let Some(ref proxy) = self.current_proxy {
            let proxy_server = if proxy.requires_bridge() {
                let bridge = LocalProxyBridge::start(proxy.clone(), self.timeout)?;
                let server = bridge.chrome_proxy_server();
                proxy_bridge = Some(bridge);
                server
            } else {
                proxy.chrome_proxy_server()
            };
            args.push(format!("--proxy-server={}", proxy_server));
        }

        args.push(format!("--remote-debugging-port={}", port));
        args.push(format!("--user-data-dir={}", user_data_dir.display()));

        if std::env::var("DEBUG").is_ok() {
            println!("[Browser] Launching {}", chrome.display());
        }
        let mut child = Command::new(&chrome)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|err| format!("failed to launch Chrome '{}': {}", chrome.display(), err))?;

        match wait_for_version(port, self.timeout) {
            Ok(()) => {}
            Err(err) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = fs::remove_dir_all(&user_data_dir);
                return Err(err);
            }
        };
        self.proxy_bridge = proxy_bridge;
        self.process = Some(child);
        self.port = Some(port);
        self.user_data_dir = Some(user_data_dir);
        self.cdp = None;
        self.browser_cdp = None;
        self.browser_product = None;
        Ok(())
    }

    pub(crate) fn shutdown(&mut self) {
        if let Some(process) = self.process.as_mut() {
            let _ = process.kill();
            let _ = process.wait();
        }
        self.process = None;
        self.port = None;
        self.cdp = None;
        self.browser_cdp = None;
        self.browser_product = None;
        self.proxy_bridge = None;
        self.current_proxy = None;

        if let Some(dir) = self.user_data_dir.take() {
            let _ = fs::remove_dir_all(dir);
        }
    }

    pub(crate) fn browser_product_cached(&mut self) -> Option<String> {
        if self.browser_product.is_none() {
            if let Some(port) = self.port {
                self.browser_product = browser_product(port, self.timeout).ok();
            }
        }
        self.browser_product.clone()
    }

    pub(crate) fn ensure_browser_cdp(&mut self) -> Result<(), String> {
        if self.browser_cdp.is_some() {
            return Ok(());
        }
        let port = self
            .port
            .ok_or_else(|| "browser not started (no port)".to_string())?;
        let ws = browser_ws_url(port, self.timeout)?;
        self.browser_cdp = Some(DevTools::connect(&ws, self.timeout, self.headless)?);
        Ok(())
    }

    pub(crate) fn create_context_page(
        &mut self,
        proxy: Option<&ProxyConfig>,
    ) -> Result<TabHandle, String> {
        self.ensure_browser_cdp()?;
        let port = self
            .port
            .ok_or_else(|| "browser not started (no port)".to_string())?;
        let timeout = self.timeout;
        let cdp = self
            .browser_cdp
            .as_mut()
            .ok_or_else(|| "browser CDP not available".to_string())?;

        let ctx_params = match proxy {
            Some(proxy) => format!(
                "{{\"proxyServer\":{}}}",
                json::string(&proxy.chrome_proxy_server())
            ),
            None => "{}".to_string(),
        };
        let resp = cdp.call("Target.createBrowserContext", &ctx_params, timeout)?;
        let browser_context_id = json::find_string(&resp, "result.browserContextId")
            .ok_or_else(|| format!("createBrowserContext returned no id: {}", resp))?;

        let target_params = format!(
            "{{\"url\":\"about:blank\",\"browserContextId\":{}}}",
            json::string(&browser_context_id)
        );
        let resp = cdp.call("Target.createTarget", &target_params, timeout)?;
        let target_id = json::find_string(&resp, "result.targetId")
            .ok_or_else(|| format!("createTarget returned no targetId: {}", resp))?;

        let ws_url = format!("ws://127.0.0.1:{}/devtools/page/{}", port, target_id);
        Ok(TabHandle {
            browser_context_id,
            target_id,
            ws_url,
        })
    }

    pub(crate) fn dispose_context_page(&mut self, page: &TabHandle) {
        if let Some(cdp) = self.browser_cdp.as_mut() {
            let timeout = Duration::from_secs(3);
            let _ = cdp.call(
                "Target.closeTarget",
                &format!("{{\"targetId\":{}}}", json::string(&page.target_id)),
                timeout,
            );
            let _ = cdp.call(
                "Target.disposeBrowserContext",
                &format!(
                    "{{\"browserContextId\":{}}}",
                    json::string(&page.browser_context_id)
                ),
                timeout,
            );
        }
    }
}

impl Drop for ChromeWorker {
    fn drop(&mut self) {
        self.shutdown();
    }
}

// hand-rolled CDP client over one websocket. sends commands with a monotonic id
// and matches replies by that id; fetch-interception events that arrive mid-wait
// are handled inline instead of being treated as our response.
pub(crate) struct DevTools {
    pub(crate) ws: WebSocket,
    pub(crate) next_id: u64,
    // set while Fetch is enabled; lets the read loop dispatch paused requests
    pub(crate) interception_state: Option<crate::solver::RouteState>,
    // proxies we've already sent creds for, so we don't loop on authRequired
    pub(crate) attempted_authentications: Vec<String>,
    pub(crate) headless: bool,
}

impl DevTools {
    pub(crate) fn connect(url: &str, timeout: Duration, headless: bool) -> Result<Self, String> {
        Ok(Self {
            ws: WebSocket::connect(url, timeout)?,
            next_id: 1,
            interception_state: None,
            attempted_authentications: Vec::new(),
            headless,
        })
    }

    // swallow fetch-interception events: when Fetch is on they fly by constantly,
    // so handle them here and keep reading until we get something the caller
    // actually wants. caps each read at 60s so a long overall deadline still
    // wakes up periodically.
    fn read_message(&mut self, deadline: Instant) -> Result<String, String> {
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .ok_or_else(|| "CDP read timed out".to_string())?;

            let timeout = if remaining > Duration::from_secs(60) {
                Duration::from_secs(60)
            } else {
                remaining
            };
            self.ws.set_read_timeout(timeout)?;

            let msg = self.ws.read_text()?;

            if is_fetch_interception_event(&msg) {
                let state_opt = self.interception_state.clone();
                if let Some(state) = state_opt {
                    let _ = crate::solver::handle_paused_request(self, &msg, &state);
                }
                continue;
            }
            return Ok(msg);
        }
    }

    pub(crate) fn send_cdp_event(&mut self, method: &str, params: &str) -> Result<(), String> {
        let id = self.next_id;
        self.next_id += 1;
        let message = format!(
            "{{\"id\":{},\"method\":{},\"params\":{}}}",
            id,
            json::string(method),
            params
        );
        self.ws.send_text(&message)
    }

    pub(crate) fn call(
        &mut self,
        method: &str,
        params: &str,
        timeout: Duration,
    ) -> Result<String, String> {
        let id = self.next_id;
        self.next_id += 1;
        let message = format!(
            "{{\"id\":{},\"method\":{},\"params\":{}}}",
            id,
            json::string(method),
            params
        );
        self.ws.send_text(&message)?;

        let deadline = Instant::now() + timeout;
        loop {
            let response = self.read_message(deadline)?;
            if json::has_id(&response, id) {
                // a real CDP error is a top-level {"id":N,"error":{...}} object; match
                // the key exactly (not a bare "error" substring, which can appear
                // inside a result value)
                if response.contains("\"error\":") {
                    return Err(format!("CDP call failed for {}: {}", method, response));
                }
                return Ok(response);
            }
        }
    }

    // blast all commands out first, then count replies back. we don't care which
    // is which, only that each one answered.
    pub(crate) fn call_burst<'a, I>(&mut self, calls: I, timeout: Duration) -> Result<(), String>
    where
        I: IntoIterator<Item = (&'a str, &'a str)>,
    {
        // remember exactly which ids we sent, so a stray event with a nested "id"
        // field can't be miscounted as one of our replies
        let mut pending: Vec<u64> = Vec::new();
        for (method, params) in calls {
            let id = self.next_id;
            self.next_id += 1;
            let message = format!(
                "{{\"id\":{},\"method\":{},\"params\":{}}}",
                id,
                json::string(method),
                params
            );
            self.ws.send_text(&message)?;
            pending.push(id);
        }

        let deadline = Instant::now() + timeout;
        while !pending.is_empty() {
            let response = self.read_message(deadline)?;
            if let Ok(value) = json::parse(&response) {
                if let Some(id) = value.get("id").and_then(|v| v.as_u64()) {
                    if let Some(pos) = pending.iter().position(|p| *p == id) {
                        pending.swap_remove(pos);
                    }
                }
            }
        }
        Ok(())
    }

    pub(crate) fn wait_for_event(&mut self, method: &str, timeout: Duration) -> Result<(), String> {
        let deadline = Instant::now() + timeout;
        loop {
            let message = self.read_message(deadline)?;
            let Ok(value) = json::parse(&message) else {
                continue;
            };
            if value.get("method").and_then(|value| value.as_str()) == Some(method) {
                return Ok(());
            }
        }
    }

    pub(crate) fn evaluate_string_with_timeout(
        &mut self,
        expression: &str,
        timeout: Duration,
    ) -> Result<String, String> {
        let response = self.call(
            "Runtime.evaluate",
            &format!(
                "{{\"expression\":{},\"returnByValue\":true,\"awaitPromise\":true}}",
                json::string(expression)
            ),
            timeout,
        )?;
        json::find_string(&response, "result.result.value")
            .ok_or_else(|| format!("Runtime.evaluate did not return string value: {}", response))
    }

    pub(crate) fn evaluate_value(
        &mut self,
        expression: &str,
        await_promise: bool,
        timeout: Duration,
    ) -> Result<Value, String> {
        let params = format!(
            "{{\"expression\":{},\"returnByValue\":true,\"awaitPromise\":{}}}",
            json::string(expression),
            if await_promise { "true" } else { "false" }
        );
        let response = self.call("Runtime.evaluate", &params, timeout)?;
        let value: Value =
            json::parse(&response).map_err(|err| format!("invalid CDP response JSON: {}", err))?;
        let result = value
            .get("result")
            .ok_or_else(|| "Runtime.evaluate response missing result".to_string())?;
        if let Some(exception) = result.get("exceptionDetails") {
            return Err(format!("Runtime.evaluate threw: {}", exception));
        }
        result
            .get("result")
            .ok_or_else(|| "Runtime.evaluate result missing value".to_string())?
            .get("value")
            .cloned()
            .ok_or_else(|| "Runtime.evaluate value missing".to_string())
    }

    // three separate Input.dispatchMouseEvent calls like a human pointer would
    // generate (move, press, hold, release)
    pub(crate) fn click(&mut self, x: f64, y: f64, timeout: Duration) -> Result<(), String> {
        let move_params = format!(
            "{{\"type\":\"mouseMoved\",\"modifiers\":0,\"buttons\":0,\"button\":\"none\",\"x\":{},\"y\":{}}}",
            x, y
        );
        self.call("Input.dispatchMouseEvent", &move_params, timeout)?;

        let press_params = format!(
            "{{\"type\":\"mousePressed\",\"modifiers\":0,\"clickCount\":1,\"buttons\":1,\"button\":\"left\",\"x\":{},\"y\":{}}}",
            x, y
        );
        self.call("Input.dispatchMouseEvent", &press_params, timeout)?;

        // jittery press-to-release hold, 50-99ms, so the dwell time isn't a constant
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.subsec_nanos())
            .unwrap_or(0);
        std::thread::sleep(Duration::from_millis(50 + (nanos % 50) as u64));

        let release_params = format!(
            "{{\"type\":\"mouseReleased\",\"modifiers\":0,\"clickCount\":1,\"buttons\":0,\"button\":\"left\",\"x\":{},\"y\":{}}}",
            x, y
        );
        self.call("Input.dispatchMouseEvent", &release_params, timeout)?;

        Ok(())
    }

    pub(crate) fn find_turnstile_click_target(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<(f64, f64)>, String> {
        let root_resp = self.call("DOM.getDocument", "{}", timeout)?;
        let root_id = json::find_number(&root_resp, "nodeId")
            .ok_or_else(|| format!("DOM.getDocument did not return nodeId: {}", root_resp))?
            as i64;

        let iframe_query = format!(
            "{{\"nodeId\":{},\"selector\":{}}}",
            root_id,
            json::string("iframe[src*=\"challenges.cloudflare.com\"]")
        );
        let iframe_resp = self.call("DOM.querySelector", &iframe_query, timeout)?;
        let iframe_id = match json::find_number(&iframe_resp, "nodeId") {
            Some(value) => value as i64,
            None => {
                let alt_query = format!(
                    "{{\"nodeId\":{},\"selector\":{}}}",
                    root_id,
                    json::string("input[name=\"cf-turnstile-response\"]")
                );
                let alt_resp = self.call("DOM.querySelector", &alt_query, timeout)?;
                match json::find_number(&alt_resp, "nodeId") {
                    Some(value) => value as i64,
                    None => return Ok(None),
                }
            }
        };

        let model_params = format!("{{\"nodeId\":{}}}", iframe_id);
        let model_resp = self.call("DOM.getBoxModel", &model_params, timeout)?;
        let model: Value = json::parse(&model_resp)
            .map_err(|err| format!("invalid DOM.getBoxModel JSON: {}", err))?;
        // box model gives a quad as a flat [x1,y1,x2,y2,x3,y3,x4,y4]; we only
        // need the first 3 corners to recover the bounding min/max in each axis
        let content = model
            .get("model")
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_array())
            .ok_or_else(|| "DOM.getBoxModel did not return content quad".to_string())?;
        if content.len() < 6 {
            return Ok(None);
        }
        let mut xs = [0f64; 3];
        let mut ys = [0f64; 3];
        for (i, item) in content[..6].iter().enumerate() {
            let value = item
                .as_f64()
                .ok_or_else(|| "DOM.getBoxModel content quad value is not a number".to_string())?;
            if i % 2 == 0 {
                xs[i / 2] = value;
            } else {
                ys[i / 2] = value;
            }
        }
        let (min_x, max_x) = xs
            .iter()
            .copied()
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), v| {
                (lo.min(v), hi.max(v))
            });
        let (min_y, max_y) = ys
            .iter()
            .copied()
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), v| {
                (lo.min(v), hi.max(v))
            });
        let cx = (min_x + max_x) / 2.0;
        let cy = (min_y + max_y) / 2.0;
        if !cx.is_finite() || !cy.is_finite() {
            return Ok(None);
        }
        Ok(Some((cx + 30.0, cy)))
    }

    pub(crate) fn find_turnstile_click_targets(
        &mut self,
        timeout: Duration,
    ) -> Result<Vec<(f64, f64)>, String> {
        let expression = r#"
            (function() {
                var coords = [];
                function pushRect(rect) {
                    if (!rect || rect.width <= 0 || rect.height <= 0) return;
                    coords.push({
                        x: rect.x + 30,
                        y: rect.y + rect.height / 2
                    });
                }

                var responseElements = document.querySelectorAll('[name="cf-turnstile-response"]');
                if (responseElements.length <= 0) {
                    document.querySelectorAll('div').forEach(function(item) {
                        try {
                            var rect = item.getBoundingClientRect();
                            var css = window.getComputedStyle(item);
                            if (
                                css.margin === '0px' &&
                                css.padding === '0px' &&
                                rect.width > 290 &&
                                rect.width <= 310
                            ) {
                                pushRect(rect);
                            }
                        } catch (e) {}
                    });

                    if (coords.length <= 0) {
                        document.querySelectorAll('div').forEach(function(item) {
                            try {
                                var rect = item.getBoundingClientRect();
                                if (rect.width > 290 && rect.width <= 310) {
                                    pushRect(rect);
                                }
                            } catch (e) {}
                        });
                    }

                    if (coords.length <= 0) {
                        document.querySelectorAll('iframe[src*="challenges.cloudflare.com"]').forEach(function(item) {
                            try {
                                pushRect(item.getBoundingClientRect());
                            } catch (e) {}
                        });
                    }
                } else {
                    responseElements.forEach(function(item) {
                        try {
                            var parent = item.parentElement;
                            if (!parent) return;
                            pushRect(parent.getBoundingClientRect());
                        } catch (e) {}
                    });
                }

                return coords;
            })()
        "#;
        let value = self.evaluate_value(expression, false, timeout)?;
        let Some(items) = value.as_array() else {
            return Ok(Vec::new());
        };

        let mut targets = Vec::new();
        for item in items {
            let Some(x) = item.get("x").and_then(|value| value.as_f64()) else {
                continue;
            };
            let Some(y) = item.get("y").and_then(|value| value.as_f64()) else {
                continue;
            };
            if x.is_finite() && y.is_finite() {
                targets.push((x, y));
            }
        }
        Ok(targets)
    }
}

fn is_fetch_interception_event(input: &str) -> bool {
    let Ok(value) = json::parse(input) else {
        return false;
    };
    matches!(
        value.get("method").and_then(|method| method.as_str()),
        Some("Fetch.requestPaused" | "Fetch.authRequired")
    )
}

pub(crate) fn browser_product(port: u16, timeout: Duration) -> Result<String, String> {
    let body = devtools_http_request("GET", port, "/json/version", timeout)?;
    parse_browser_product(&body)
}

pub(crate) fn browser_ws_url(port: u16, timeout: Duration) -> Result<String, String> {
    let body = devtools_http_request("GET", port, "/json/version", timeout)?;
    let value: Value =
        json::parse(&body).map_err(|err| format!("DevTools /json/version not JSON: {}", err))?;
    value
        .get("webSocketDebuggerUrl")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| format!("DevTools version missing webSocketDebuggerUrl: {}", body))
}

fn wait_for_version(port: u16, timeout: Duration) -> Result<(), String> {
    let deadline = Instant::now() + timeout;

    loop {
        match devtools_http_request("GET", port, "/json/version", Duration::from_secs(2)) {
            Ok(body) => return parse_version(&body),
            Err(err) => {
                if Instant::now() >= deadline {
                    return Err(format!("Chrome DevTools did not become ready: {}", err));
                }
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn parse_version(body: &str) -> Result<(), String> {
    parse_browser_product(body).map(|_| ())
}

fn parse_browser_product(body: &str) -> Result<String, String> {
    let value: Value = json::parse(body)
        .map_err(|err| format!("DevTools /json/version response not JSON: {}", err))?;
    let browser = value
        .get("Browser")
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("DevTools version response missing Browser: {}", body))?;
    if browser.trim().is_empty() {
        Err(format!(
            "DevTools version response missing browser name: {}",
            body
        ))
    } else {
        Ok(browser.to_string())
    }
}

// tiny blocking HTTP/1.1 client just for the devtools /json/* endpoints; copes
// with both content-length and chunked bodies
fn devtools_http_request(
    method: &str,
    port: u16,
    path: &str,
    timeout: Duration,
) -> Result<String, String> {
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .map_err(|err| format!("DevTools HTTP connect failed: {}", err))?;
    stream
        .set_nodelay(true)
        .map_err(|err| format!("failed to set DevTools TCP_NODELAY: {}", err))?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|err| format!("failed to set DevTools read timeout: {}", err))?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|err| format!("failed to set DevTools write timeout: {}", err))?;

    let request = format!(
        "{} {} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\nContent-Length: 0\r\n\r\n",
        method, path, port
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|err| format!("DevTools HTTP write failed: {}", err))?;

    let (head, body) = read_http_response(&mut stream)?;

    if !head.starts_with("HTTP/1.1 200")
        && !head.starts_with("HTTP/1.0 200")
        && !head.starts_with("HTTP/1.1 201")
        && !head.starts_with("HTTP/1.0 201")
    {
        return Err(format!(
            "DevTools HTTP request failed: {}",
            head.lines().next().unwrap_or(&head)
        ));
    }

    Ok(body)
}

fn read_http_response(stream: &mut TcpStream) -> Result<(String, String), String> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end;

    loop {
        let read = stream
            .read(&mut chunk)
            .map_err(|err| format!("DevTools HTTP read failed: {}", err))?;
        if read == 0 {
            return Err("DevTools HTTP closed before headers".to_string());
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(index) = find_bytes(&buffer, b"\r\n\r\n") {
            header_end = index + 4;
            break;
        }
        if buffer.len() > 1024 * 1024 {
            return Err("DevTools HTTP headers too large".to_string());
        }
    }

    let head = String::from_utf8_lossy(&buffer[..header_end - 4]).to_string();
    let lower_head = head.to_lowercase();

    // body framing: chunked, or content-length, or read-till-close as a last resort
    if lower_head.contains("transfer-encoding: chunked") {
        loop {
            if let Some(decoded) = try_decode_chunked(&buffer[header_end..])? {
                return String::from_utf8(decoded)
                    .map(|body| (head, body))
                    .map_err(|err| format!("DevTools HTTP body was not utf-8: {}", err));
            }
            let read = stream
                .read(&mut chunk)
                .map_err(|err| format!("DevTools HTTP chunked read failed: {}", err))?;
            if read == 0 {
                return Err("DevTools HTTP chunked body ended early".to_string());
            }
            buffer.extend_from_slice(&chunk[..read]);
        }
    }

    if let Some(content_length) = content_length(&head) {
        while buffer.len() < header_end + content_length {
            let read = stream
                .read(&mut chunk)
                .map_err(|err| format!("DevTools HTTP body read failed: {}", err))?;
            if read == 0 {
                break;
            }
            buffer.extend_from_slice(&chunk[..read]);
        }
        let end = buffer.len().min(header_end + content_length);
        return String::from_utf8(buffer[header_end..end].to_vec())
            .map(|body| (head, body))
            .map_err(|err| format!("DevTools HTTP body was not utf-8: {}", err));
    }

    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => buffer.extend_from_slice(&chunk[..read]),
            Err(_) => break,
        }
    }

    String::from_utf8(buffer[header_end..].to_vec())
        .map(|body| (head, body))
        .map_err(|err| format!("DevTools HTTP body was not utf-8: {}", err))
}

fn content_length(head: &str) -> Option<usize> {
    for line in head.lines() {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.trim().eq_ignore_ascii_case("content-length") {
            return value.trim().parse::<usize>().ok();
        }
    }
    None
}

// returns Ok(None) when we don't have all the bytes yet, so the caller knows to
// read more off the socket and try again
fn try_decode_chunked(input: &[u8]) -> Result<Option<Vec<u8>>, String> {
    let mut cursor = 0usize;
    let mut out = Vec::new();

    loop {
        let Some(line_end) = find_bytes(&input[cursor..], b"\r\n") else {
            return Ok(None);
        };
        let size_line = String::from_utf8_lossy(&input[cursor..cursor + line_end]);
        let size_text = size_line.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_text, 16)
            .map_err(|err| format!("bad chunk size '{}': {}", size_text, err))?;
        cursor += line_end + 2;

        if size == 0 {
            if input.len() >= cursor + 2 {
                return Ok(Some(out));
            }
            return Ok(None);
        }

        if input.len() < cursor + size + 2 {
            return Ok(None);
        }
        out.extend_from_slice(&input[cursor..cursor + size]);
        cursor += size + 2;
    }
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

// search order for a chrome/chromium/edge binary:
//   1. CHROME_BIN / CHROME_PATH env override
//   2. per-os install locations (gathered now, checked last)
//   3. a chrome/ folder shipped next to the binary
//   4. puppeteer's download cache under HOME
//   5. finally the os candidates from step 2
fn find_chrome_executable() -> Result<PathBuf, String> {
    for name in ["CHROME_BIN", "CHROME_PATH"] {
        if let Ok(value) = env::var(name) {
            let path = PathBuf::from(value);
            if path.exists() {
                return Ok(path);
            }
        }
    }

    // collect the usual install paths for this os; checked after bundled/cache
    // lookups below so a local copy takes precedence
    let mut candidates = Vec::new();

    if cfg!(target_os = "windows") {
        if let Ok(program_files) = env::var("ProgramFiles") {
            candidates
                .push(PathBuf::from(&program_files).join("Google/Chrome/Application/chrome.exe"));
            candidates.push(PathBuf::from(&program_files).join("Chromium/Application/chrome.exe"));
            candidates
                .push(PathBuf::from(&program_files).join("Microsoft/Edge/Application/msedge.exe"));
        }
        if let Ok(program_files_x86) = env::var("ProgramFiles(x86)") {
            candidates.push(
                PathBuf::from(&program_files_x86).join("Google/Chrome/Application/chrome.exe"),
            );
        }
        if let Ok(local_app_data) = env::var("LOCALAPPDATA") {
            candidates
                .push(PathBuf::from(&local_app_data).join("Google/Chrome/Application/chrome.exe"));
        }
    } else if cfg!(target_os = "macos") {
        candidates.push(PathBuf::from(
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        ));
        candidates.push(PathBuf::from(
            "/Applications/Chromium.app/Contents/MacOS/Chromium",
        ));
    } else {
        for path in [
            "/usr/bin/google-chrome",
            "/usr/bin/google-chrome-stable",
            "/usr/bin/chromium",
            "/usr/bin/chromium-browser",
            "/snap/bin/chromium",
        ] {
            candidates.push(PathBuf::from(path));
        }
    }

    for local in ["chrome", "chromium", "rust/chrome", "../rust/chrome"] {
        let path = PathBuf::from(local);
        if path.exists() {
            if let Some(found) = find_named_executable(&path, 0) {
                return Ok(found);
            }
        }
    }

    if let Some(home) = env::var("USERPROFILE")
        .or_else(|_| env::var("HOME"))
        .ok()
        .map(PathBuf::from)
    {
        let puppeteer_cache = home.join(".cache").join("puppeteer");
        if puppeteer_cache.exists() {
            if let Some(found) = find_named_executable(&puppeteer_cache, 0) {
                return Ok(found);
            }
        }
    }

    for candidate in candidates {
        if candidate.exists() {
            return Ok(candidate);
        }
    }

    Err("No Chrome or Chromium executable found. Set CHROME_BIN to the browser path.".to_string())
}

// checks files in each dir before recursing so a top-level hit wins
fn find_named_executable(dir: &Path, depth: usize) -> Option<PathBuf> {
    if depth > 8 {
        return None;
    }
    let entries = fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            let name = path.file_name()?.to_string_lossy().to_lowercase();
            if name == "chrome" || name == "chrome.exe" || name == "chromium" {
                return Some(path);
            }
        }
    }

    let entries = fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_named_executable(&path, depth + 1) {
                return Some(found);
            }
        }
    }
    None
}

// (small race: it's freed when the listener drops, before chrome claims it)
fn free_port() -> Result<u16, String> {
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .map_err(|err| format!("failed to find free port: {}", err))?;
    listener
        .local_addr()
        .map(|addr| addr.port())
        .map_err(|err| format!("failed to read free port: {}", err))
}