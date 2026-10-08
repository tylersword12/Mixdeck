// SoundCloud without the official API, the same approach as NewPipe:
// 1. Load soundcloud.com, find the site's JS bundles, pull the public client_id out of them.
// 2. Call the website's own internal API (api-v2.soundcloud.com) with that client_id.
// 3. Resolve a track's "transcoding" URL into a short-lived CDN stream URL.
// Everything that depends on SoundCloud's internals is in this file.
use crate::app::App;
use crate::util::{num, now_ms, s, url_encode, ApiError, ApiResult};
use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::{json, Value};

pub const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36";

static RE_SCRIPT: Lazy<Regex> = Lazy::new(|| Regex::new(r#"<script[^>]+src="([^"]+/assets/[^"]+\.js)""#).unwrap());
static RE_CID1: Lazy<Regex> = Lazy::new(|| Regex::new(r#"[{,]\s*client_id\s*:\s*"([0-9a-zA-Z]{32})""#).unwrap());
static RE_CID2: Lazy<Regex> = Lazy::new(|| Regex::new(r"client_id=([0-9a-zA-Z]{32})").unwrap());
static RE_HYDRATION: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?s)window\.__sc_hydration\s*=\s*(\[.*?\]);\s*</script>").unwrap());
static RE_USERID: Lazy<Regex> = Lazy::new(|| Regex::new(r"soundcloud(?::|://)users:(\d+)").unwrap());

pub enum ProfileInput { Id(String), Slug(String), Short(String), Error(String) }

impl App {
    // ---- client_id discovery ------------------------------------------------
    pub async fn sc_client_id(&self, force: bool) -> ApiResult<String> {
        {
            let c = &self.config.lock().data.soundcloud;
            if !c.manual_client_id.is_empty() { return Ok(c.manual_client_id.clone()); }
            if !force && !c.client_id.is_empty() && now_ms() - c.client_id_at < 12 * 3600 * 1000 { return Ok(c.client_id.clone()); }
        }
        let _g = self.sc_cid_lock.lock().await;
        if !force {
            let c = &self.config.lock().data.soundcloud;
            if !c.client_id.is_empty() && now_ms() - c.client_id_at < 12 * 3600 * 1000 { return Ok(c.client_id.clone()); }
        }
        let base = self.opts.sc_web_base.clone();
        let html = self.sc_fetch_text(&format!("{base}/")).await?;
        let mut srcs: Vec<String> = RE_SCRIPT.captures_iter(&html)
            .filter_map(|c| url::Url::parse(&format!("{base}/")).ok()?.join(&c[1]).ok().map(|u| u.to_string()))
            .collect();
        if srcs.is_empty() { return Err(ApiError::new(502, "SoundCloud: could not find script bundles on soundcloud.com")); }
        srcs.reverse(); // the bundle with the id is usually one of the last ones
        for src in srcs {
            if let Ok(js) = self.sc_fetch_text(&src).await {
                if let Some(c) = RE_CID1.captures(&js).or_else(|| RE_CID2.captures(&js)) {
                    let id = c[1].to_string();
                    let mut cfg = self.config.lock();
                    cfg.data.soundcloud.client_id = id.clone();
                    cfg.data.soundcloud.client_id_at = now_ms();
                    cfg.save();
                    crate::logf!("[soundcloud] found client_id {}…", &id[..6]);
                    return Ok(id);
                }
            }
        }
        Err(ApiError::new(502, "SoundCloud: client_id not found in any script bundle (site layout may have changed)"))
    }

    fn sc_error(status: u16) -> ApiError {
        let msg = match status {
            404 => "Not found on SoundCloud (removed, private, or bad link)".to_string(),
            429 => "SoundCloud is rate-limiting requests, try again in a minute".to_string(),
            _ => format!("SoundCloud returned HTTP {status}"),
        };
        ApiError::new(if status == 401 || status == 403 { status } else if status == 404 { 404 } else { 502 }, msg)
    }

    async fn sc_get(&self, url: &str, auth: bool) -> ApiResult<reqwest::Response> {
        let mut req = self.http.get(url)
            .header("Accept-Language", "en-US,en;q=0.9")
            .header("Origin", "https://soundcloud.com")
            .header("Referer", "https://soundcloud.com/");
        if auth {
            let tok = self.config.lock().data.soundcloud.oauth_token.clone();
            if !tok.is_empty() { req = req.header("Authorization", format!("OAuth {tok}")); }
        }
        let res = req.send().await?;
        if !res.status().is_success() {
            let st = res.status().as_u16();
            crate::logf!("[soundcloud] {st} {}", url.split("client_id=").next().unwrap_or(url).chars().take(160).collect::<String>());
            return Err(Self::sc_error(st));
        }
        Ok(res)
    }

    pub async fn sc_fetch_text(&self, url: &str) -> ApiResult<String> { Ok(self.sc_get(url, false).await?.text().await?) }

    /// Plain download from SoundCloud's CDN (no client_id needed).
    pub async fn sc_download(&self, url: &str) -> ApiResult<reqwest::Response> { self.sc_get(url, false).await }

    pub async fn sc_api(&self, path_or_url: &str, params: &[(&str, String)]) -> ApiResult {
        for retried in [false, true] {
            let base = if path_or_url.starts_with("http") { path_or_url.to_string() } else { format!("{}{}", self.opts.sc_api_base, path_or_url) };
            let mut u = url::Url::parse(&base).map_err(|_| ApiError::bad("bad url"))?;
            let cid = self.sc_client_id(false).await?;
            {
                let existing: Vec<(String, String)> = u.query_pairs().filter(|(k, _)| k != "client_id").map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
                let mut qp = u.query_pairs_mut();
                qp.clear();
                for (k, v) in &existing { if !params.iter().any(|(pk, _)| pk == k) { qp.append_pair(k, v); } }
                for (k, v) in params { qp.append_pair(k, v); }
                qp.append_pair("client_id", &cid);
                if !existing.iter().any(|(k, _)| k == "app_locale") { qp.append_pair("app_locale", "en"); }
            }
            match self.sc_get(u.as_str(), true).await {
                Ok(res) => return Ok(res.json().await?),
                Err(e) => {
                    let st = e.status.as_u16();
                    let manual = !self.config.lock().data.soundcloud.manual_client_id.is_empty();
                    // A rotated client_id shows up as 401/403: grab a new one and retry once.
                    if !retried && (st == 401 || st == 403) && !manual {
                        self.sc_client_id(true).await?;
                        continue;
                    }
                    return Err(if st == 401 || st == 403 { e.with_status(502) } else { e });
                }
            }
        }
        Err(ApiError::new(502, "SoundCloud request failed"))
    }

    // ---- mapping ------------------------------------------------------------
    fn sc_artwork(t: &Value) -> String {
        let u = if t["artwork_url"].is_string() { s(&t["artwork_url"]) } else { s(&t["user"]["avatar_url"]) };
        u.replace("-large.", "-t300x300.")
    }

    fn sc_playability(t: &Value) -> &'static str {
        let tr = t["media"]["transcodings"].as_array().cloned().unwrap_or_default();
        let usable: Vec<&Value> = tr.iter().filter(|x| x["format"].is_object() && !s(&x["format"]["protocol"]).contains("encrypted")).collect();
        if t["streamable"] == json!(false) { return "unavailable"; }
        if usable.is_empty() { return if tr.is_empty() { "unavailable" } else { "drm" }; }
        if s(&t["policy"]) == "SNIP" || usable.iter().all(|x| x["snipped"] == json!(true)) { return "preview"; }
        "ok"
    }

    pub fn sc_map_track(t: &Value) -> Value {
        let artist = if t["publisher_metadata"]["artist"].is_string() && !s(&t["publisher_metadata"]["artist"]).is_empty() { s(&t["publisher_metadata"]["artist"]) } else { s(&t["user"]["username"]) };
        let title = s(&t["title"]);
        let dur = if t["full_duration"].is_number() { num(&t["full_duration"], 0.0) } else { num(&t["duration"], 0.0) };
        json!({
            "source": "soundcloud", "id": s(&t["id"]),
            "title": if title.is_empty() { "(untitled)".to_string() } else { title },
            "artist": artist, "duration": dur, "artwork": Self::sc_artwork(t),
            "url": t["permalink_url"].clone(),
            "playable": if t["media"].is_object() { Self::sc_playability(t) } else { "ok" },
        })
    }

    fn sc_map_playlist(p: &Value) -> Value {
        let count = if p["track_count"].is_number() { num(&p["track_count"], 0.0) } else { p["tracks"].as_array().map(|a| a.len()).unwrap_or(0) as f64 };
        json!({ "source": "soundcloud", "id": s(&p["id"]), "name": s(&p["title"]), "count": count, "artwork": Self::sc_artwork(p), "owner": p["user"]["username"].clone() })
    }

    // ---- catalog ------------------------------------------------------------
    pub async fn sc_search(&self, q: &str, offset: u64) -> ApiResult {
        let r = self.sc_api("/search/tracks", &[("q", q.to_string()), ("limit", "20".into()), ("offset", offset.to_string())]).await?;
        let items: Vec<Value> = r["collection"].as_array().cloned().unwrap_or_default().iter().map(Self::sc_map_track).collect();
        Ok(json!({ "items": items, "next": if r["next_href"].is_string() { json!(offset + 20) } else { Value::Null } }))
    }

    pub async fn sc_resolve(&self, link: &str) -> ApiResult {
        let mut link = link.to_string();
        if link.contains("on.soundcloud.com/") {
            let res = self.sc_get(&link, false).await?;
            link = res.url().to_string(); // short links redirect to the full URL
        }
        let r = self.sc_api("/resolve", &[("url", link)]).await?;
        match s(&r["kind"]).as_str() {
            "track" => Ok(json!({ "kind": "track", "tracks": [Self::sc_map_track(&r)] })),
            "playlist" => Ok(json!({ "kind": "playlist", "name": r["title"], "tracks": self.sc_playlist_tracks(&r).await? })),
            "user" => Ok(json!({ "kind": "user", "user": Self::sc_map_profile(&r) })),
            _ => Err(ApiError::bad("Unsupported SoundCloud link")),
        }
    }

    async fn sc_playlist_tracks(&self, p: &Value) -> ApiResult<Vec<Value>> {
        let tracks = p["tracks"].as_array().cloned().unwrap_or_default();
        // Playlists only include full data for the first few tracks; fetch the rest by id.
        let mut full: std::collections::HashMap<String, Value> = tracks.iter().filter(|t| t["title"].is_string()).map(|t| (s(&t["id"]), t.clone())).collect();
        let missing: Vec<String> = tracks.iter().filter(|t| !t["title"].is_string()).map(|t| s(&t["id"])).collect();
        for chunk in missing.chunks(50) {
            let batch = self.sc_api("/tracks", &[("ids", chunk.join(","))]).await?;
            for t in batch.as_array().cloned().unwrap_or_default() { full.insert(s(&t["id"]), t); }
        }
        Ok(tracks.iter().filter_map(|t| full.get(&s(&t["id"]))).map(Self::sc_map_track).collect())
    }

    pub async fn sc_playlist(&self, id: &str) -> ApiResult {
        let p = self.sc_api(&format!("/playlists/{id}"), &[("representation", "full".into())]).await?;
        Ok(json!({ "name": p["title"], "tracks": self.sc_playlist_tracks(&p).await? }))
    }

    // ---- profiles -------------------------------------------------------------
    pub fn sc_parse_profile_input(input: &str) -> Option<ProfileInput> {
        let mut st = input.trim().to_string();
        if st.is_empty() { return None; }
        if st.chars().all(|c| c.is_ascii_digit()) { return Some(ProfileInput::Id(st)); }
        let looks_like_link = Regex::new(r"(?i)(^|\.|/)(soundcloud\.com|snd\.sc)(/|$)").unwrap().is_match(&st);
        if looks_like_link {
            if !st.to_lowercase().starts_with("http") { st = format!("https://{st}"); }
            let Ok(u) = url::Url::parse(&st) else { return Some(ProfileInput::Error("That doesn't look like a SoundCloud link".into())) };
            let host = u.host_str().unwrap_or("").to_lowercase();
            if host == "on.soundcloud.com" || host.ends_with("snd.sc") { return Some(ProfileInput::Short(u.to_string())); }
            let slug = u.path_segments().and_then(|mut p| p.find(|x| !x.is_empty()).map(|x| x.to_string())).unwrap_or_default();
            if slug.is_empty() || slug == "you" {
                return Some(ProfileInput::Error("That link doesn't include a profile name. Open your profile on soundcloud.com and copy the address from there.".into()));
            }
            let slug = url::form_urlencoded::parse(format!("x={slug}").as_bytes()).next().map(|(_, v)| v.into_owned()).unwrap_or(slug);
            return Some(ProfileInput::Slug(slug.to_lowercase()));
        }
        let slug: String = st.trim_start_matches('@').split(|c: char| c.is_whitespace() || c == '/' || c == '?' || c == '#').next().unwrap_or("").to_lowercase();
        Some(ProfileInput::Slug(slug))
    }

    pub fn sc_map_profile(u: &Value) -> Value {
        let permalink = s(&u["permalink"]);
        json!({
            "id": u["id"], "username": s(&u["username"]), "permalink": permalink,
            "url": if u["permalink_url"].is_string() { s(&u["permalink_url"]) } else { format!("https://soundcloud.com/{permalink}") },
            "avatar": s(&u["avatar_url"]).replace("-large.", "-t200x200."),
            "likes": u["likes_count"], "playlists": u["playlist_count"],
        })
    }

    /// Find a profile from a link or username. Tries the internal API first, then reads
    /// the profile page itself, which embeds the user's id.
    pub async fn sc_find_profile(&self, input: &str) -> ApiResult {
        let parsed = Self::sc_parse_profile_input(input).ok_or_else(|| ApiError::bad("Paste your SoundCloud profile link"))?;
        let mut slug = match parsed {
            ProfileInput::Error(e) => return Err(ApiError::bad(e)),
            ProfileInput::Id(id) => {
                match self.sc_api(&format!("/users/{id}"), &[]).await {
                    Ok(u) => return Ok(Self::sc_map_profile(&u)),
                    Err(_) => id, // an all-digit username rather than a user id
                }
            }
            ProfileInput::Slug(sl) => sl,
            ProfileInput::Short(link) => {
                let res = self.sc_get(&link, false).await?;
                match Self::sc_parse_profile_input(res.url().as_str()) {
                    Some(ProfileInput::Slug(sl)) => sl,
                    _ => return Err(ApiError::bad("That short link didn't lead to a profile")),
                }
            }
        };
        slug = slug.trim().to_string();
        let page_url = format!("https://soundcloud.com/{slug}");
        match self.sc_api("/resolve", &[("url", page_url.clone())]).await {
            Ok(r) if s(&r["kind"]) == "user" => return Ok(Self::sc_map_profile(&r)),
            Ok(r) if r["user"].is_object() => return Ok(Self::sc_map_profile(&r["user"])), // a track/playlist link: use its owner
            Ok(_) => {}
            Err(e) => if e.status.as_u16() != 404 { crate::logf!("[soundcloud] resolve failed for {page_url}: {e}"); },
        }
        match self.sc_fetch_text(&format!("{}/{}", self.opts.sc_web_base, url_encode(&slug))).await {
            Ok(html) => {
                if let Some(c) = RE_HYDRATION.captures(&html) {
                    if let Ok(arr) = serde_json::from_str::<Value>(&c[1]) {
                        if let Some(u) = arr.as_array().and_then(|a| a.iter().find(|x| s(&x["hydratable"]) == "user")) {
                            if !u["data"]["id"].is_null() { return Ok(Self::sc_map_profile(&u["data"])); }
                        }
                    }
                }
                if let Some(c) = RE_USERID.captures(&html) {
                    let u = self.sc_api(&format!("/users/{}", &c[1]), &[]).await?;
                    return Ok(Self::sc_map_profile(&u));
                }
            }
            Err(e) => if e.status.as_u16() != 404 { crate::logf!("[soundcloud] profile page lookup failed: {e}"); },
        }
        Err(ApiError::bad(format!("Couldn't find a SoundCloud profile called \"{slug}\". Open your profile on soundcloud.com and paste the address from the browser bar.")))
    }

    async fn sc_user(&self) -> ApiResult {
        let (token, profile, username) = {
            let c = &self.config.lock().data.soundcloud;
            (c.oauth_token.clone(), c.profile.clone(), c.username.clone())
        };
        if !token.is_empty() {
            let me = self.sc_api("/me", &[]).await?;
            return Ok(json!({ "id": me["id"], "username": me["username"] }));
        }
        if let Some(p) = profile.filter(|p| !p["id"].is_null()) { return Ok(p); }
        if username.is_empty() { return Err(ApiError::bad("Paste your SoundCloud profile link in Settings")); }
        // Older settings stored only a username: look it up once and remember it.
        let p = self.sc_find_profile(&username).await?;
        let mut cfg = self.config.lock();
        cfg.data.soundcloud.profile = Some(p.clone());
        cfg.save();
        Ok(p)
    }

    pub async fn sc_likes(&self, next: Option<String>) -> ApiResult {
        let r = match next {
            Some(n) if !n.is_empty() => self.sc_api(&n, &[]).await?,
            _ => {
                let u = self.sc_user().await?;
                self.sc_api(&format!("/users/{}/track_likes", s(&u["id"])), &[("limit", "50".into())]).await?
            }
        };
        let items: Vec<Value> = r["collection"].as_array().cloned().unwrap_or_default().iter().filter(|x| x["track"].is_object()).map(|x| Self::sc_map_track(&x["track"])).collect();
        Ok(json!({ "items": items, "next": r["next_href"].clone() }))
    }

    pub async fn sc_playlists(&self) -> ApiResult {
        let u = self.sc_user().await?;
        let uid = s(&u["id"]);
        let r = self.sc_api(&format!("/users/{uid}/playlists"), &[("limit", "50".into())]).await?;
        let mut out: Vec<Value> = r["collection"].as_array().cloned().unwrap_or_default().iter().map(Self::sc_map_playlist).collect();
        // Playlists the profile has liked (other people's) come after its own.
        if let Ok(liked) = self.sc_api(&format!("/users/{uid}/playlist_likes"), &[("limit", "50".into())]).await {
            let seen: std::collections::HashSet<String> = out.iter().map(|p| s(&p["id"])).collect();
            for x in liked["collection"].as_array().cloned().unwrap_or_default() {
                if x["playlist"].is_object() && !seen.contains(&s(&x["playlist"]["id"])) {
                    let mut p = Self::sc_map_playlist(&x["playlist"]);
                    p["liked"] = json!(true);
                    out.push(p);
                }
            }
        }
        Ok(Value::Array(out))
    }

    // ---- streaming -------------------------------------------------------------
    /// Returns (protocol, mime, url, preview).
    pub async fn sc_stream_info(&self, id: &str) -> ApiResult<(String, String, String, bool)> {
        let t = self.sc_api(&format!("/tracks/{id}"), &[]).await?;
        let all: Vec<Value> = t["media"]["transcodings"].as_array().cloned().unwrap_or_default().into_iter()
            .filter(|x| x["url"].is_string() && x["format"].is_object() && !s(&x["format"]["protocol"]).contains("encrypted"))
            .collect();
        if all.is_empty() { return Err(ApiError::new(422, "No playable stream (track may be DRM-protected or region-blocked)")); }
        let full: Vec<Value> = all.iter().filter(|x| x["snipped"] != json!(true)).cloned().collect();
        let preview = full.is_empty();
        let mut pool = if preview { all } else { full };
        let rank = |x: &Value| -> u8 {
            let p = s(&x["preset"]);
            let proto = s(&x["format"]["protocol"]);
            let mime = s(&x["format"]["mime_type"]);
            if p.contains("aac_160") && proto == "hls" { 0 }
            else if p.contains("mp3") && proto == "progressive" { 1 }
            else if p.contains("mp3") && proto == "hls" { 2 }
            else if p.contains("aac") || mime.contains("mp4") || mime.contains("aac") { if proto == "hls" { 3 } else { 4 } }
            else { 9 } // opus/ogg can't be played through hls.js
        };
        pool.sort_by_key(|x| rank(x));
        let auth = s(&t["track_authorization"]);
        for tc in pool {
            if rank(&tc) == 9 { continue; }
            let params: Vec<(&str, String)> = if auth.is_empty() { vec![] } else { vec![("track_authorization", auth.clone())] };
            match self.sc_api(&s(&tc["url"]), &params).await {
                Ok(r) if r["url"].is_string() => return Ok((s(&tc["format"]["protocol"]), s(&tc["format"]["mime_type"]), s(&r["url"]), preview)),
                Ok(_) => {}
                Err(e) => crate::logf!("[soundcloud] transcoding failed {}: {e}", s(&tc["preset"])),
            }
        }
        Err(ApiError::new(422, "Could not resolve a stream URL"))
    }
}
