# Cloudflare Solver

Solve Turnstile challenges and IUAM with real headless Chrome, raw CDP (no driver, no node),
zero Rust dependencies. One ~800 KB binary, hands you back a `1.` token or a `cf_clearance`
cookie.


## ⚠️ Disclaimer

This tool is provided for educational purposes only. Only use it on sites you own or have explicit permission to test. Bypassing CAPTCHAs may violate ToS and laws in your jurisdiction. The author assumes no liability for any misuse, damages, or legal consequences arising from the use of this software.

## Turnstile

```
$ curl -X POST http://localhost:407/turnstile \
    -H "content-type: application/json" \
    -d '{"url":"https://bypass.city","sitekey":"0x4AAAAAAAGzw6rXeQWJ_y2P"}'

{"token":"1.kMLfH4VM8kCMPXvMy-QcrmUMz4HA4cM44y6qmbYfCgDF16nE1Dh2ExaYPB...","elapsed":"2.94s","status":"completed"}
```

## IUAM

```
$ curl -X POST http://localhost:407/iuam \
    -H "content-type: application/json" \
    -d '{"url":"https://nowsecure.nl"}'

{"headers":{"Cookie":"cf_clearance=155jEz2BCC8oFRCOu0x8...","User-Agent":"Mozilla/5.0 (Linux; Android 16; K) AppleWebKit/537.36 ..."},"ip":"45.152.31.27","elapsed":"2.87s","status":"completed"}
```

## Run

It needs to run on linux (a windows chrome leaks its fingerprint and CF won't pass it). Docker
is the easy path: the image fetches its own chrome, so you only need Docker installed.

**Windows:** install [Docker Engine in WSL2](https://docs.docker.com/engine/install/) (not
Docker Desktop), then:

```
docker build -t turnstile-solver .
docker run -d -p 407:407 turnstile-solver
```

**Linux:** same two commands. If you'd rather run it bare, you need Rust 1.80+ and a chrome on
the box:

```
cargo build --release
CHROME_BIN=/usr/bin/google-chrome ./target/release/turnstile-solver
```

API is on `http://localhost:407` either way.

## API

Two endpoints, mode is in the path (no `mode` field needed). A third legacy endpoint
`POST /cloudflare` takes `"mode":"turnstile"|"iuam"` in the body if you prefer one route.

`GET /health` returns capacity: `{"status","dyno","capacity","available","active"}`.

### `POST /turnstile`

Solve a sitekey and get a token back.

```json
{"url":"https://the-site.com/","sitekey":"0x..."}
```

| Field | Required | Description |
|-------|----------|-------------|
| url | yes | Target URL (http or https) |
| sitekey | yes | Turnstile sitekey. Accepts an array for multiple widgets. |
| cdata | no | Turnstile cData |
| action | no | Turnstile action |
| proxy | no | `http://user:pass@host:port`, `socks5://...`, `socks4://...` |

### `POST /iuam`

Load the real page, pass the challenge, return the `cf_clearance` cookie plus the cookie jar,
the UA, and the egress IP so you can replay the session over plain HTTP.

```json
{"url":"https://the-site.com/"}
```

| Field | Required | Description |
|-------|----------|-------------|
| url | yes | Target URL |
| proxy | no | same format as above |

`cf_clearance` and the token are tied to the IP and UA they were earned on. If you used a proxy
to solve, use the same proxy when replaying. For iuam, send back the `user_agent` it returned.

## How it works

Two Chrome processes stay running. For each solve, a fresh isolated browser context (its own
cookie jar, no cross-solve leakage) is created on one of them. The solver loads a stub page that
renders the real Turnstile widget, clicks through it, and polls for the token. Once the token
lands, the context is thrown away. IUAM mode is the same idea but loads the real target page
instead of a stub and reads `cf_clearance` off the cookie jar when it's done.

## Settings

All optional, defaults work out of the box.

| Variable | Default | Description |
|----------|---------|-------------|
| PORT | 407 | HTTP port |
| BROWSERS | 2 | Chrome processes |
| TABS | 10 | Contexts per browser (BROWSERS x TABS = parallel solves) |
| timeOut | 29000 | ms before a solve is given up on |
| DEBUG | off | Set to anything for per-solve logs and timings |

## Scaling

One container does 20 solves in parallel. The challenge JS is CPU heavy, so to go faster you
scale horizontally:

```
docker compose up -d --scale solver=4
```

4 containers on ports 407-410, each its own 2x10. Round-robin requests across them and
throughput climbs ~linearly with cores.

20 solves in parallel per instance (2 browsers x 10 tabs). ~2-3 s per solve on a real linux box,
~5.7 s avg under full saturation (CPM 566 on a 24-core box, 200/200 success).

That's all I can explain

## License

MIT. See LICENSE.

---


