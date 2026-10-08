// Spotify: Authorization Code + PKCE login (no client secret), token refresh, and the
// Web API calls MixDeck uses. Written for the Feb 2026 Development Mode rules:
// search limit 10, /playlists/{id}/items, item.item.
use crate::app::App;
use crate::store::{SpotifyUser, Tokens};
use crate::util::{b64url, num, now_ms, s, url_encode, ApiError, ApiResult};
use rand::RngCore;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const SCOPES: &str = "streaming user-read-email user-read-private user-read-playback-state user-modify-playback-state playlist-read-private playlist-read-collaborative user-library-read";

impl App {
    pub fn spotify_redirect_uri(&self) -> String { format!("http://127.0.0.1:{}/auth/spotify/callback", self.port()) }

    pub fn spotify_logged_in(&self) -> bool {
        self.config.lock().data.spotify.tokens.as_ref().map(|t| !t.refresh_token.is_empty()).unwrap_or(false)
    }

    pub fn spotify_login_url(&self) -> ApiResult<String> {
        let client_id = self.config.lock().data.spotify.client_id.clone();
        if client_id.is_empty() { return Err(ApiError::bad("Add your Spotify Client ID first")); }
        let mut v = [0u8; 48];
        rand::thread_rng().fill_bytes(&mut v);
        let verifier = b64url(&v);
        let challenge = b64url(&Sha256::digest(verifier.as_bytes()));
        let mut st = [0u8; 12];
        rand::thread_rng().fill_bytes(&mut st);
        let state = b64url(&st);
        self.sp_pending.lock().insert(state.clone(), verifier);
        Ok(format!(
            "{}/authorize?client_id={}&response_type=code&redirect_uri={}&scope={}&code_challenge_method=S256&code_challenge={}&state={}",
            self.opts.spotify_accounts_base, url_encode(&client_id), url_encode(&self.spotify_redirect_uri()),
            url_encode(SCOPES), challenge, state
        ))
    }

    pub async fn spotify_callback(&self, params: &std::collections::HashMap<String, String>) -> ApiResult<()> {
        if let Some(e) = params.get("error") { return Err(ApiError::bad(format!("Spotify login was cancelled: {e}"))); }
        let state = params.get("state").cloned().unwrap_or_default();
        let verifier = self.sp_pending.lock().remove(&state).ok_or_else(|| ApiError::bad("Login session expired, please try again"))?;
        let client_id = self.config.lock().data.spotify.client_id.clone();
        self.spotify_token(&[
            ("grant_type", "authorization_code"),
            ("code", params.get("code").map(|s| s.as_str()).unwrap_or("")),
            ("redirect_uri", &self.spotify_redirect_uri()),
            ("client_id", &client_id),
            ("code_verifier", &verifier),
        ]).await?;
        let me = self.spotify_api("GET", "/me", None).await?;
        {
            let mut cfg = self.config.lock();
            cfg.data.spotify.user = Some(SpotifyUser {
                id: s(&me["id"]),
                name: if me["display_name"].is_string() { s(&me["display_name"]) } else { s(&me["id"]) },
            });
            cfg.save();
        }
        crate::logf!("[spotify] logged in");
        Ok(())
    }

    async fn spotify_token(&self, form: &[(&str, &str)]) -> ApiResult<()> {
        let res = self.http.post(format!("{}/api/token", self.opts.spotify_accounts_base)).form(form).send().await?;
        let status = res.status();
        let j: Value = res.json().await.unwrap_or(Value::Null);
        if !status.is_success() {
            let msg = if j["error_description"].is_string() { s(&j["error_description"]) } else if j["error"].is_string() { s(&j["error"]) } else { status.as_u16().to_string() };
            return Err(ApiError::new(status.as_u16(), format!("Spotify token error: {msg}")));
        }
        let mut cfg = self.config.lock();
        let prev_refresh = cfg.data.spotify.tokens.as_ref().map(|t| t.refresh_token.clone()).unwrap_or_default();
        let refresh = if j["refresh_token"].is_string() { s(&j["refresh_token"]) } else { prev_refresh };
        cfg.data.spotify.tokens = Some(Tokens {
            access_token: s(&j["access_token"]),
            refresh_token: refresh,
            expires_at: now_ms() + ((num(&j["expires_in"], 3600.0) - 60.0) * 1000.0) as i64,
        });
        cfg.save();
        Ok(())
    }

    pub async fn spotify_access_token(&self) -> ApiResult<(String, i64)> {
        let current = self.config.lock().data.spotify.tokens.clone();
        let Some(t) = current.filter(|t| !t.refresh_token.is_empty()) else {
            return Err(ApiError::new(401, "Not logged in to Spotify"));
        };
        if now_ms() < t.expires_at { return Ok((t.access_token, t.expires_at)); }
        // One refresh at a time; others wait and reuse the result.
        let _g = self.sp_refresh.lock().await;
        let again = self.config.lock().data.spotify.tokens.clone();
        if let Some(t2) = &again {
            if now_ms() < t2.expires_at { return Ok((t2.access_token.clone(), t2.expires_at)); }
        }
        let client_id = self.config.lock().data.spotify.client_id.clone();
        if let Err(e) = self.spotify_token(&[("grant_type", "refresh_token"), ("refresh_token", &t.refresh_token), ("client_id", &client_id)]).await {
            if e.status.as_u16() == 400 {
                // Refresh token expired or revoked: the user needs to log in again.
                let mut cfg = self.config.lock();
                cfg.data.spotify.tokens = None;
                cfg.save();
            }
            return Err(e);
        }
        let t3 = self.config.lock().data.spotify.tokens.clone().unwrap_or_default();
        Ok((t3.access_token, t3.expires_at))
    }

    pub fn spotify_logout(&self) {
        let mut cfg = self.config.lock();
        cfg.data.spotify.tokens = None;
        cfg.data.spotify.user = None;
        cfg.save();
    }

    pub async fn spotify_api(&self, method: &str, path: &str, body: Option<Value>) -> ApiResult {
        let url = if path.starts_with("http") { path.to_string() } else { format!("{}{}", self.opts.spotify_api_base, path) };
        for attempt in 0..3 {
            let (token, _) = self.spotify_access_token().await?;
            let mut req = match method {
                "PUT" => self.http.put(&url),
                "POST" => self.http.post(&url),
                _ => self.http.get(&url),
            }.bearer_auth(&token);
            if let Some(b) = &body { req = req.json(b); }
            let res = req.send().await?;
            let status = res.status().as_u16();
            if status == 429 && attempt < 2 {
                let wait = res.headers().get("retry-after").and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok()).unwrap_or(1).min(10);
                let j: Value = res.json().await.unwrap_or(Value::Null);
                if s(&j["reason"]) == "QUOTA_EXCEEDED" {
                    return Err(ApiError::new(429, "Spotify Development Mode quota exceeded, try again later"));
                }
                tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
                continue;
            }
            if status == 204 { return Ok(Value::Null); }
            let j: Value = res.json().await.unwrap_or(Value::Null);
            if !(200..300).contains(&status) {
                let msg = if j["error"]["message"].is_string() { s(&j["error"]["message"]) } else { status.to_string() };
                return Err(ApiError::new(status, format!("Spotify: {msg}")));
            }
            return Ok(j);
        }
        Err(ApiError::new(429, "Spotify is rate-limiting requests"))
    }

    pub fn spotify_map_track(t: &Value) -> Option<Value> {
        if s(&t["type"]) != "track" || s(&t["id"]).is_empty() { return None; } // skip episodes, local files
        let mut imgs: Vec<&Value> = t["album"]["images"].as_array().map(|a| a.iter().collect()).unwrap_or_default();
        imgs.sort_by_key(|i| i["width"].as_i64().unwrap_or(0));
        let img = imgs.iter().find(|i| i["width"].as_i64().unwrap_or(300) >= 200).or(imgs.first());
        let artists: Vec<String> = t["artists"].as_array().map(|a| a.iter().map(|x| s(&x["name"])).collect()).unwrap_or_default();
        Some(json!({
            "source": "spotify", "id": s(&t["id"]), "uri": s(&t["uri"]),
            "title": s(&t["name"]), "artist": artists.join(", "),
            "duration": num(&t["duration_ms"], 0.0), "artwork": img.map(|i| s(&i["url"])).unwrap_or_default(),
            "url": t["external_urls"]["spotify"].clone(),
            "playable": if t["is_playable"] == json!(false) { "unavailable" } else { "ok" },
        }))
    }

    pub async fn spotify_search(&self, q: &str, offset: u64) -> ApiResult {
        // Dev Mode caps search at 10 per request, so grab two pages at once.
        let page = |o: u64| format!("/search?q={}&type=track&limit=10&offset={o}", url_encode(q));
        let (p1, p2) = (page(offset), page(offset + 10));
        let (a, b) = tokio::join!(self.spotify_api("GET", &p1, None), self.spotify_api("GET", &p2, None));
        let a = a?;
        let b = b.ok();
        let mut items: Vec<Value> = a["tracks"]["items"].as_array().cloned().unwrap_or_default();
        if let Some(b) = &b { items.extend(b["tracks"]["items"].as_array().cloned().unwrap_or_default()); }
        let items: Vec<Value> = items.iter().filter_map(Self::spotify_map_track).collect();
        let more = match &b { Some(b) => !b["tracks"]["next"].is_null(), None => !a["tracks"]["next"].is_null() };
        Ok(json!({ "items": items, "next": if more { json!(offset + 20) } else { Value::Null } }))
    }

    pub async fn spotify_playlists(&self) -> ApiResult {
        let me = self.config.lock().data.spotify.user.as_ref().map(|u| u.id.clone()).unwrap_or_default();
        let mut out = Vec::new();
        let mut url = Some("/me/playlists?limit=50".to_string());
        while let Some(u) = url.take() {
            if out.len() >= 500 { break; }
            let r = self.spotify_api("GET", &u, None).await?;
            for p in r["items"].as_array().cloned().unwrap_or_default() {
                if p.is_null() { continue; }
                let own = !me.is_empty() && s(&p["owner"]["id"]) == me;
                let count = if p["items"]["total"].is_number() { num(&p["items"]["total"], 0.0) } else { num(&p["tracks"]["total"], 0.0) };
                out.push(json!({
                    "source": "spotify", "id": s(&p["id"]), "name": s(&p["name"]), "count": count,
                    "artwork": p["images"][0]["url"].as_str().unwrap_or(""),
                    "owner": p["owner"]["display_name"].clone(),
                    "readable": own || p["collaborative"] == json!(true),
                }));
            }
            url = r["next"].as_str().map(|s| s.to_string());
        }
        Ok(Value::Array(out))
    }

    pub async fn spotify_playlist_tracks(&self, id: &str) -> ApiResult {
        let name = self.spotify_api("GET", &format!("/playlists/{id}?fields=name"), None).await.ok().map(|j| s(&j["name"])).unwrap_or_default();
        let mut out = Vec::new();
        let mut url = Some(format!("/playlists/{id}/items?limit=50"));
        while let Some(u) = url.take() {
            if out.len() >= 2000 { break; }
            let r = self.spotify_api("GET", &u, None).await?;
            for it in r["items"].as_array().cloned().unwrap_or_default() {
                let t = if it["item"].is_object() { &it["item"] } else { &it["track"] };
                if let Some(m) = Self::spotify_map_track(t) { out.push(m); }
            }
            url = r["next"].as_str().map(|s| s.to_string());
        }
        Ok(json!({ "name": name, "tracks": out }))
    }

    pub async fn spotify_liked(&self, offset: u64) -> ApiResult {
        let r = self.spotify_api("GET", &format!("/me/tracks?limit=50&offset={offset}"), None).await?;
        let items: Vec<Value> = r["items"].as_array().cloned().unwrap_or_default().iter().filter_map(|x| Self::spotify_map_track(&x["track"])).collect();
        Ok(json!({ "items": items, "next": if r["next"].is_null() { Value::Null } else { json!(offset + 50) } }))
    }

    pub async fn spotify_resolve(&self, link: &str) -> ApiResult {
        let re = regex::Regex::new(r"(?:open\.spotify\.com/(?:intl-[a-z-]+/)?|spotify:)(track|playlist|album)[/:]([A-Za-z0-9]+)").unwrap();
        let caps = re.captures(link).ok_or_else(|| ApiError::bad("Unrecognised Spotify link"))?;
        let kind = caps[1].to_string();
        let id = caps[2].to_string();
        match kind.as_str() {
            "track" => {
                let t = self.spotify_api("GET", &format!("/tracks/{id}"), None).await?;
                Ok(json!({ "kind": "track", "tracks": Self::spotify_map_track(&t).into_iter().collect::<Vec<_>>() }))
            }
            "playlist" => {
                let r = self.spotify_playlist_tracks(&id).await?;
                if r["tracks"].as_array().map(|a| a.is_empty()).unwrap_or(true) {
                    return Err(ApiError::bad("Spotify only shares the contents of playlists you own or collaborate on"));
                }
                Ok(json!({ "kind": "playlist", "name": r["name"], "tracks": r["tracks"] }))
            }
            _ => {
                let al = self.spotify_api("GET", &format!("/albums/{id}"), None).await?;
                let tracks: Vec<Value> = al["tracks"]["items"].as_array().cloned().unwrap_or_default().into_iter().filter_map(|mut t| {
                    t["album"] = json!({ "images": al["images"] });
                    Self::spotify_map_track(&t)
                }).collect();
                Ok(json!({ "kind": "album", "name": al["name"], "tracks": tracks }))
            }
        }
    }
}
