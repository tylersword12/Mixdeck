// Several linked Spotify and SoundCloud accounts, one active per service. The active
// account lives in the original config fields (so everything else is unchanged); the
// others wait in `others` until you switch to them.
use crate::app::App;
use crate::store::{SpotifyAccount, SpotifyUser};
use crate::util::{ApiError, ApiResult};
use serde_json::{json, Value};

impl App {
    pub fn accounts_json(c: &crate::store::Config) -> (Value, Value) {
        let mut sp: Vec<Value> = vec![];
        if let (Some(u), Some(t)) = (&c.spotify.user, &c.spotify.tokens) {
            if !t.refresh_token.is_empty() { sp.push(json!({ "id": u.id, "name": u.name, "active": true })); }
        }
        for a in &c.spotify.others { sp.push(json!({ "id": a.user.id, "name": a.user.name, "active": false })); }
        let mut sc: Vec<Value> = vec![];
        let act = c.soundcloud.active();
        let avatar = |a: &crate::store::ScAccount| a.profile.as_ref().map(|p| p["avatar"].clone()).unwrap_or(Value::Null);
        if !act.is_empty() { sc.push(json!({ "id": act.id(), "name": act.name(), "avatar": avatar(&act), "active": true, "hasToken": !act.oauth_token.is_empty() })); }
        for a in &c.soundcloud.others { sc.push(json!({ "id": a.id(), "name": a.name(), "avatar": avatar(a), "active": false, "hasToken": !a.oauth_token.is_empty() })); }
        (Value::Array(sp), Value::Array(sc))
    }

    /// The Spotify account that's active right now, so a new login can be stored beside it.
    pub(crate) fn spotify_snapshot(&self) -> Option<SpotifyAccount> {
        let c = &self.config.lock().data.spotify;
        match (&c.user, &c.tokens) {
            (Some(u), Some(t)) if !t.refresh_token.is_empty() => Some(SpotifyAccount { user: u.clone(), tokens: t.clone() }),
            _ => None,
        }
    }

    /// After logging in as `user`: keep the previously active account (if it was someone
    /// else) in the list of others.
    pub(crate) fn spotify_after_login(&self, prev: Option<SpotifyAccount>, user: SpotifyUser) {
        let mut cfg = self.config.lock();
        let sp = &mut cfg.data.spotify;
        sp.others.retain(|a| a.user.id != user.id);
        if let Some(p) = prev.filter(|p| p.user.id != user.id && !p.user.id.is_empty()) {
            sp.others.insert(0, p);
        }
        sp.user = Some(user);
        cfg.save();
    }

    pub(crate) fn spotify_restore(&self, prev: Option<SpotifyAccount>) {
        let mut cfg = self.config.lock();
        let sp = &mut cfg.data.spotify;
        match prev {
            Some(p) => { sp.user = Some(p.user); sp.tokens = Some(p.tokens); }
            None => { sp.user = None; sp.tokens = None; }
        }
        cfg.save();
    }

    pub fn account_switch(&self, service: &str, id: &str) -> ApiResult<()> {
        let mut cfg = self.config.lock();
        match service {
            "spotify" => {
                let sp = &mut cfg.data.spotify;
                if sp.user.as_ref().map(|u| u.id == id).unwrap_or(false) { return Ok(()); }
                let i = sp.others.iter().position(|a| a.user.id == id).ok_or_else(|| ApiError::bad("That Spotify account isn't linked"))?;
                let next = sp.others.remove(i);
                if let (Some(u), Some(t)) = (sp.user.take(), sp.tokens.take()) {
                    if !t.refresh_token.is_empty() { sp.others.insert(0, SpotifyAccount { user: u, tokens: t }); }
                }
                sp.user = Some(next.user);
                sp.tokens = Some(next.tokens);
            }
            "soundcloud" => {
                let sc = &mut cfg.data.soundcloud;
                if sc.active().id() == id { return Ok(()); }
                let i = sc.others.iter().position(|a| a.id() == id).ok_or_else(|| ApiError::bad("That SoundCloud account isn't linked"))?;
                let next = sc.others.remove(i);
                let cur = sc.active();
                if !cur.is_empty() { sc.others.insert(0, cur); }
                sc.set_active(next);
            }
            _ => return Err(ApiError::bad("Unknown service")),
        }
        cfg.save();
        crate::logf!("[accounts] switched {service} account");
        Ok(())
    }

    /// Unlink an account. Removing the active one makes the next linked account active.
    pub fn account_remove(&self, service: &str, id: &str) -> ApiResult<()> {
        let mut cfg = self.config.lock();
        match service {
            "spotify" => {
                let sp = &mut cfg.data.spotify;
                if sp.user.as_ref().map(|u| u.id == id).unwrap_or(false) || id.is_empty() {
                    sp.user = None; sp.tokens = None;
                    if !sp.others.is_empty() {
                        let next = sp.others.remove(0);
                        sp.user = Some(next.user); sp.tokens = Some(next.tokens);
                    }
                } else {
                    sp.others.retain(|a| a.user.id != id);
                }
            }
            "soundcloud" => {
                let sc = &mut cfg.data.soundcloud;
                if sc.active().id() == id || id.is_empty() {
                    let next = if sc.others.is_empty() { Default::default() } else { sc.others.remove(0) };
                    sc.set_active(next);
                } else {
                    sc.others.retain(|a| a.id() != id);
                }
            }
            _ => return Err(ApiError::bad("Unknown service")),
        }
        cfg.save();
        Ok(())
    }
}

