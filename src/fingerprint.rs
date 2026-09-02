use crate::server::json;
use std::cell::Cell;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug)]
pub struct Profile {
    pub name: String,
    pub model: String,
    pub width: u32,
    pub height: u32,
    pub device_pixel_ratio: f64,
    pub video_renderer: String,
    pub video_vendor: String,
    pub oscpu: String,
    pub platform: String,
    pub not_brand_name: String,
    pub not_brand_version: String,
}

// single source of truth so headers and JS never disagree
#[derive(Clone, Debug)]
pub struct ActiveProfile {
    pub profile: Profile,
    pub ua_model: String,
    pub randomized: bool,
    pub chrome_major: u32,
    pub chrome_full: String,
    pub android_major: u32,
    pub platform_version: String,
    pub user_agent: String,
    pub sec_ch_ua: String,
    pub sec_ch_ua_full_version_list: String,
}

pub fn load_profiles() -> Result<Vec<Profile>, String> {
    // try a few spots since cwd differs between cargo run, the built binary, and docker
    let mut source_path = PathBuf::from("src/devices.json");
    if !source_path.exists() {
        source_path = PathBuf::from("../src/devices.json");
    }
    if !source_path.exists() {
        source_path = PathBuf::from("devices.json");
    }

    if source_path.exists() {
        let source = fs::read_to_string(&source_path).map_err(|err| {
            format!(
                "failed to read devices.json at {}: {}",
                source_path.display(),
                err
            )
        })?;
        let parsed = parse_profiles_from_json(&source);
        if !parsed.is_empty() {
            return Ok(parsed);
        }
    }

    // fall back to a small baked-in list so we never run dry
    Ok(fallback_profiles())
}

pub fn select_profile(requested: Option<&str>) -> Result<(Profile, usize), String> {
    let profiles = load_profiles()?;
    if profiles.is_empty() {
        return Err("no device profiles available".to_string());
    }

    if let Some(name) = requested {
        let wanted = name.trim().to_lowercase();
        if let Some(profile) = profiles
            .iter()
            .find(|profile| profile.name.to_lowercase() == wanted)
            .cloned()
        {
            return Ok((profile, profiles.len()));
        }
        if let Some(profile) = profiles
            .iter()
            .find(|profile| profile.name.to_lowercase().contains(&wanted))
            .cloned()
        {
            return Ok((profile, profiles.len()));
        }
        return Err(format!("unknown profile '{}'", name));
    }

    let index = pseudo_random_usize(profiles.len());
    Ok((profiles[index].clone(), profiles.len()))
}

// two modes: randomized (fresh phone + rolled versions per solve) or a fixed default.
// browser_product is chrome's own UA so the non-random path can match the real binary.
pub fn applied_for_mode(
    randomize: bool,
    browser_product: Option<&str>,
) -> Result<ActiveProfile, String> {
    let browser_chrome = browser_product.and_then(parse_chrome_version);
    if randomize {
        let (profile, _) = select_profile(None)?;
        let (chrome_major, chrome_full) = random_chrome_version();
        let android_major = 11 + pseudo_random_usize(5) as u32;
        let ua_model = profile.model.clone();
        Ok(apply_profile(
            profile,
            ua_model,
            true,
            &chrome_full,
            chrome_major,
            android_major,
        ))
    } else {
        let (chrome_major, chrome_full) =
            browser_chrome.unwrap_or_else(|| (148, "148.0.0.0".to_string()));
        Ok(apply_profile(
            default_fingerprint_profile(),
            "K".to_string(),
            false,
            &chrome_full,
            chrome_major,
            16,
        ))
    }
}

fn random_chrome_version() -> (u32, String) {
    let chrome_major = 138 + pseudo_random_usize(11) as u32;
    let chrome_full = format!(
        "{}.0.{}.{}",
        chrome_major,
        6000 + pseudo_random_usize(1000),
        pseudo_random_usize(200)
    );
    (chrome_major, chrome_full)
}

fn parse_chrome_version(product: &str) -> Option<(u32, String)> {
    let marker = product
        .find("Chrome/")
        .map(|index| index + "Chrome/".len())
        .or_else(|| {
            product
                .find("HeadlessChrome/")
                .map(|index| index + "HeadlessChrome/".len())
        })?;
    let version = product[marker..]
        .chars()
        .take_while(|ch| ch.is_ascii_digit() || *ch == '.')
        .collect::<String>();
    if version.is_empty() {
        return None;
    }
    let major = version.split('.').next()?.parse::<u32>().ok()?;
    Some((major, version))
}

fn apply_profile(
    profile: Profile,
    ua_model: String,
    randomized: bool,
    chrome_full: &str,
    chrome_major: u32,
    android_major: u32,
) -> ActiveProfile {
    let platform_version = format!("{}.0.0", android_major);
    let user_agent = format!(
        "Mozilla/5.0 (Linux; Android {}; {}) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/{} Mobile Safari/537.36",
        android_major, ua_model, chrome_full
    );

    let sec_ch_ua = format!(
        "\"Chromium\";v=\"{}\", \"Google Chrome\";v=\"{}\", \"{}\";v=\"{}\"",
        chrome_major, chrome_major, profile.not_brand_name, profile.not_brand_version
    );
    // full-version-list carries the complete a.b.c.d, gated on random mode
    let sec_ch_ua_full_version_list = format!(
        "\"Chromium\";v=\"{}\", \"Google Chrome\";v=\"{}\", \"{}\";v=\"{}.0.0.0\"",
        chrome_full, chrome_full, profile.not_brand_name, profile.not_brand_version
    );

    ActiveProfile {
        profile,
        ua_model,
        randomized,
        chrome_major,
        chrome_full: chrome_full.to_string(),
        android_major,
        platform_version,
        user_agent,
        sec_ch_ua,
        sec_ch_ua_full_version_list,
    }
}

fn default_fingerprint_profile() -> Profile {
    Profile {
        name: "Pixel 9 Pro".to_string(),
        model: "Pixel 9 Pro".to_string(),
        width: 412,
        height: 915,
        device_pixel_ratio: 2.625,
        video_renderer: "Adreno (TM) 740".to_string(),
        video_vendor: "Qualcomm".to_string(),
        oscpu: "Linux armv81".to_string(),
        platform: "Linux armv81".to_string(),
        not_brand_name: "Not(A:Brand".to_string(),
        not_brand_version: "99".to_string(),
    }
}

pub fn headers_json(applied: &ActiveProfile) -> String {
    let mut fields = vec![
        ("sec-ch-ua", json::string(&applied.sec_ch_ua)),
        ("sec-ch-ua-mobile", json::string("?1")),
        ("sec-ch-ua-platform", json::string("\"Android\"")),
        ("user-agent", json::string(&applied.user_agent)),
        ("accept-language", json::string("en-US,en;q=0.9")),
    ];

    if applied.randomized {
        fields.extend([
            (
                "sec-ch-ua-platform-version",
                json::string(&format!("\"{}\"", applied.platform_version)),
            ),
            (
                "sec-ch-ua-model",
                json::string(&format!("\"{}\"", applied.profile.model)),
            ),
            (
                "sec-ch-ua-full-version",
                json::string(&format!("\"{}\"", applied.chrome_full)),
            ),
            (
                "sec-ch-ua-full-version-list",
                json::string(&applied.sec_ch_ua_full_version_list),
            ),
        ]);
    }

    json::object(&fields)
}

pub fn user_agent_override_json(applied: &ActiveProfile) -> String {
    json::object(&[
        ("userAgent", json::string(&applied.user_agent)),
        ("acceptLanguage", json::string("en-US,en;q=0.9")),
        ("platform", json::string("Android")),
        ("userAgentMetadata", user_agent_metadata_json(applied)),
    ])
}

fn user_agent_metadata_json(applied: &ActiveProfile) -> String {
    json::object(&[
        ("brands", brands_json(applied)),
        ("fullVersionList", full_version_list_json(applied)),
        ("fullVersion", json::string(&applied.chrome_full)),
        ("platform", json::string("Android")),
        ("platformVersion", json::string(&applied.platform_version)),
        ("architecture", json::string("")),
        ("model", json::string(&applied.profile.model)),
        ("mobile", "true".to_string()),
        ("bitness", json::string("")),
        ("wow64", "false".to_string()),
    ])
}

fn brand_version_json(brand: &str, version: &str) -> String {
    json::object(&[
        ("brand", json::string(brand)),
        ("version", json::string(version)),
    ])
}

fn brands_json(applied: &ActiveProfile) -> String {
    format!(
        "[{},{},{}]",
        brand_version_json("Chromium", &applied.chrome_major.to_string()),
        brand_version_json("Google Chrome", &applied.chrome_major.to_string()),
        brand_version_json(
            &applied.profile.not_brand_name,
            &applied.profile.not_brand_version,
        )
    )
}

fn full_version_list_json(applied: &ActiveProfile) -> String {
    format!(
        "[{},{},{}]",
        brand_version_json("Chromium", &applied.chrome_full),
        brand_version_json("Google Chrome", &applied.chrome_full),
        brand_version_json(
            &applied.profile.not_brand_name,
            &format!("{}.0.0.0", applied.profile.not_brand_version),
        )
    )
}

fn navigator_user_agent_data_json(applied: &ActiveProfile) -> String {
    json::object(&[
        ("brands", brands_json(applied)),
        ("mobile", "true".to_string()),
        ("platform", json::string("Android")),
        ("platformVersion", json::string(&applied.platform_version)),
        ("architecture", json::string("")),
        ("bitness", json::string("")),
        ("model", json::string(&applied.profile.model)),
        ("uaFullVersion", json::string(&applied.chrome_full)),
        ("fullVersion", json::string(&applied.chrome_full)),
        ("fullVersionList", full_version_list_json(applied)),
        ("wow64", "false".to_string()),
    ])
}

pub fn fingerprint_script(applied: &ActiveProfile) -> String {
    let history_len = 2 + pseudo_random_usize(4);
    // navigator.appVersion is the UA minus the leading "Mozilla/"
    let app_version = format!(
        "5.0 (Linux; Android {}; {}) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/{} Mobile Safari/537.36",
        applied.android_major, applied.ua_model, applied.chrome_full
    );

    let js_template = include_str!("device.js");
    js_template
        .replace("__USER_AGENT__", &json::string(&applied.user_agent))
        .replace("__APP_VERSION__", &json::string(&app_version))
        .replace("__PLATFORM__", &json::string(&applied.profile.platform))
        .replace("__OSCPU__", &json::string(&applied.profile.oscpu))
        .replace("__USER_AGENT_DATA_METADATA__", &navigator_user_agent_data_json(applied))
        .replace("__WIDTH__", &applied.profile.width.to_string())
        .replace("__HEIGHT__", &applied.profile.height.to_string())
        .replace("__DPR__", &applied.profile.device_pixel_ratio.to_string())
        .replace("__HISTORY_LEN__", &history_len.to_string())
        .replace("__VIDEO_VENDOR__", &json::string(&applied.profile.video_vendor))
        .replace("__VIDEO_RENDERER__", &json::string(&applied.profile.video_renderer))
}

// hand-rolled parse (no serde dependency)
fn parse_profiles_from_json(source: &str) -> Vec<Profile> {
    let Some(array_start) = source.find('[') else {
        return Vec::new();
    };
    let Some(array_end) = source.rfind(']') else {
        return Vec::new();
    };
    let array_source = &source[array_start + 1..array_end];

    object_slices(array_source)
        .into_iter()
        .filter_map(parse_profile_object)
        .collect()
}

fn parse_profile_object(source: &str) -> Option<Profile> {
    let video_card = extract_block(source, "videoCard")?;
    let not_brand = extract_block(source, "notBrand")?;

    Some(Profile {
        name: json::find_string(source, "name")?,
        model: json::find_string(source, "model")?,
        width: json::find_number(source, "width")? as u32,
        height: json::find_number(source, "height")? as u32,
        device_pixel_ratio: json::find_number(source, "devicePixelRatio")?,
        video_renderer: json::find_string(video_card, "renderer")?,
        video_vendor: json::find_string(video_card, "vendor")?,
        oscpu: json::find_string(source, "oscpu")?,
        platform: json::find_string(source, "platform")?,
        not_brand_name: json::find_string(not_brand, "name")?,
        not_brand_version: json::find_string(not_brand, "version")?,
    })
}

// tracks string state so braces inside quoted values don't throw off the depth
fn object_slices(source: &str) -> Vec<&str> {
    let bytes = source.as_bytes();
    let mut slices = Vec::new();
    let mut depth = 0usize;
    let mut start = None;
    let mut in_string = false;
    let mut escaped = false;

    for index in 0..bytes.len() {
        let byte = bytes[index];
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            continue;
        }

        match byte {
            b'"' => in_string = true,
            b'{' => {
                if depth == 0 {
                    start = Some(index);
                }
                depth += 1;
            }
            b'}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    if let Some(start_index) = start.take() {
                        slices.push(&source[start_index..=index]);
                    }
                }
            }
            _ => {}
        }
    }

    slices
}

fn extract_block<'a>(source: &'a str, field: &str) -> Option<&'a str> {
    let field_start = source.find(field)?;
    let block_start = source[field_start..].find('{')? + field_start;
    let bytes = source.as_bytes();
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;

    for (index, byte) in bytes.iter().enumerate().skip(block_start) {
        if in_string {
            if escaped {
                escaped = false;
            } else if *byte == b'\\' {
                escaped = true;
            } else if *byte == b'"' {
                in_string = false;
            }
            continue;
        }

        match *byte {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return source.get(block_start..=index);
                }
            }
            _ => {}
        }
    }

    None
}

// per-thread PRNG (SplitMix64). seeded once per thread from the wall clock mixed
// with a global atomic bump, so two threads picking a profile at the same
// instant get different streams. advancing the state each call also
// decorrelates back-to-back picks within a solve.
thread_local! {
    static RNG_STATE: Cell<u64> = Cell::new(seed_rng_state());
}

// the atomic bump guarantees uniqueness even if two threads read the same nanos;
// mixing the clock keeps it unpredictable across restarts. never returns 0.
fn seed_rng_state() -> u64 {
    static COUNTER: AtomicU64 = AtomicU64::new(0x2545_F491_4F6C_DD1D);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or(0);
    let bump = COUNTER.fetch_add(0x9E37_79B9_7F4A_7C15, Ordering::Relaxed);
    let seed = nanos ^ bump.rotate_left(17);
    if seed == 0 {
        0x9E37_79B9_7F4A_7C15
    } else {
        seed
    }
}

fn next_rng_u64() -> u64 {
    RNG_STATE.with(|state| {
        let next = state.get().wrapping_add(0x9E37_79B9_7F4A_7C15);
        state.set(next);
        let mut z = next;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    })
}

// uniform-ish pick in [0, max); not crypto
fn pseudo_random_usize(max: usize) -> usize {
    if max <= 1 {
        return 0;
    }
    (next_rng_u64() % max as u64) as usize
}

fn fallback_profiles() -> Vec<Profile> {
    vec![
        Profile {
            name: "Pixel 9 Pro".to_string(),
            model: "Pixel 9 Pro".to_string(),
            width: 412,
            height: 892,
            device_pixel_ratio: 3.5,
            video_renderer: "Adreno (TM) 740".to_string(),
            video_vendor: "Qualcomm".to_string(),
            oscpu: "Linux armv81".to_string(),
            platform: "Linux armv81".to_string(),
            not_brand_name: "Not(A:Brand".to_string(),
            not_brand_version: "99".to_string(),
        },
        Profile {
            name: "Samsung Galaxy S24 Ultra".to_string(),
            model: "SM-S928B".to_string(),
            width: 384,
            height: 854,
            device_pixel_ratio: 3.0,
            video_renderer: "Adreno (TM) 750".to_string(),
            video_vendor: "Qualcomm".to_string(),
            oscpu: "Linux aarch64".to_string(),
            platform: "Linux aarch64".to_string(),
            not_brand_name: "Not-A.Brand".to_string(),
            not_brand_version: "99".to_string(),
        },
        Profile {
            name: "OnePlus 12".to_string(),
            model: "CPH2581".to_string(),
            width: 360,
            height: 800,
            device_pixel_ratio: 3.0,
            video_renderer: "Adreno (TM) 750".to_string(),
            video_vendor: "Qualcomm".to_string(),
            oscpu: "Linux aarch64".to_string(),
            platform: "Linux aarch64".to_string(),
            not_brand_name: "Not(A:Brand".to_string(),
            not_brand_version: "24".to_string(),
        },
    ]
}