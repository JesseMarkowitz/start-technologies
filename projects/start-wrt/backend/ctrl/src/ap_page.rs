//! The access point's own page (design D23): what the router can say about
//! itself and nothing the main router controls. Served in place of the full UI
//! on an adopted access point; recovery mode (D22) reuses it.

use std::collections::BTreeMap;
use std::fmt::Write;

use axum::body::Body;
use axum::http::{header, Response};

use crate::access_points;

pub fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// What the page shows, gathered once per request.
pub struct View {
    pub name: String,
    pub tunnel_live: bool,
    pub dark: Option<bool>,
    pub generation: u64,
    pub main_generation: u64,
    pub version: String,
    pub main_version: String,
    pub inventory: access_points::Inventory,
    pub clients: Vec<access_points::ClientInfo>,
    /// VLAN → profile name, from the payload applied last.
    pub profiles: BTreeMap<u16, String>,
    pub log: String,
}

async fn gather() -> View {
    let role = access_points::load_role();
    let (tunnel_live, dark) = crate::liveness::ap_status();
    let uplink = role.uplink.clone().unwrap_or_default();
    let log = tokio::process::Command::new("logread")
        .args(["-e", "startwrt"])
        .output()
        .await
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    let log = log
        .lines()
        .rev()
        .take(40)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n");
    View {
        name: role
            .name
            .clone()
            .unwrap_or_else(|| "access-point".to_string()),
        tunnel_live,
        dark,
        generation: role.generation,
        main_generation: crate::control::main_generation(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        main_version: crate::control::main_version(),
        inventory: crate::control::inventory(&uplink),
        clients: crate::control::clients(&uplink).await,
        profiles: crate::ap::load_applied()
            .map(|s| {
                s.profiles
                    .into_iter()
                    .map(|p| (p.vlan_tag, p.fullname))
                    .collect()
            })
            .unwrap_or_default(),
        log,
    }
}

/// The page for a view; pure, so it can be checked.
pub fn render(v: &View) -> String {
    let mut h = String::new();
    let name = esc(&v.name);
    let state = match (v.tunnel_live, v.dark) {
        (true, Some(false)) => "serving",
        (true, _) => "in contact, catching up",
        (false, _) => "out of contact: Wi-Fi off and ports cut until the main router is back",
    };
    let _ = write!(
        h,
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>Access point {name}</title>\
         <style>body{{font:16px/1.5 system-ui,sans-serif;max-width:40rem;margin:2rem auto;padding:0 1rem;color:#222}}\
         h1{{font-size:1.5rem}}h2{{font-size:1.1rem;margin-top:1.5rem}}table{{border-collapse:collapse;width:100%}}\
         td,th{{text-align:left;padding:.25rem .5rem;border-bottom:1px solid #ddd}}pre{{font-size:.8rem;overflow:auto;background:#f4f4f4;padding:.5rem}}\
         .muted{{color:#666}}</style></head><body>\
         <h1>Access point {name}</h1>\
         <p>Everything about your network is managed at the main router: <a href=\"http://router.lan\">router.lan</a>.</p>\
         <h2>Main router link</h2><table>\
         <tr><th>State</th><td>{state}</td></tr>\
         <tr><th>Configuration</th><td>generation {gen}{main_gen}</td></tr>\
         <tr><th>Firmware</th><td>{ver}{main_ver}</td></tr></table>",
        gen = v.generation,
        main_gen = if v.main_generation > 0 && v.main_generation != v.generation {
            format!(" <span class=\"muted\">(the main router is at {})</span>", v.main_generation)
        } else {
            String::new()
        },
        ver = esc(&v.version),
        main_ver = if !v.main_version.is_empty() && v.main_version != v.version {
            format!(" <span class=\"muted\">(the main router runs {})</span>", esc(&v.main_version))
        } else {
            String::new()
        },
    );
    let _ = write!(
        h,
        "<h2>Ports</h2><table><tr><th>Port</th><th>Role</th><th>Link</th></tr>"
    );
    for p in &v.inventory.ports {
        let _ = write!(
            h,
            "<tr><td>{}</td><td>{}</td><td>{}</td></tr>",
            esc(&p.name),
            if p.uplink {
                "uplink to the main router"
            } else {
                "access port"
            },
            if p.link { "up" } else { "down" }
        );
    }
    let _ = write!(
        h,
        "</table><h2>Radios</h2><table><tr><th>Radio</th><th>Bands</th></tr>"
    );
    for r in &v.inventory.radios {
        let _ = write!(
            h,
            "<tr><td>{}</td><td>{}</td></tr>",
            esc(&r.name),
            esc(&r.bands.join(", "))
        );
    }
    let _ = write!(h, "</table><h2>Devices</h2>");
    if v.clients.is_empty() {
        let _ = write!(h, "<p class=\"muted\">None connected.</p>");
    } else {
        let _ = write!(
            h,
            "<table><tr><th>Device</th><th>Via</th><th>Profile</th></tr>"
        );
        for c in &v.clients {
            let profile = c
                .vlan
                .and_then(|v2| v.profiles.get(&v2).cloned())
                .or_else(|| c.vlan.map(|v2| format!("VLAN {v2}")))
                .unwrap_or_else(|| "none".to_string());
            let _ = write!(
                h,
                "<tr><td>{}</td><td>{}</td><td>{}</td></tr>",
                esc(&c.mac),
                esc(&c.via),
                esc(&profile)
            );
        }
        let _ = write!(h, "</table>");
    }
    let _ = write!(h, "<h2>Log</h2><pre>{}</pre></body></html>", esc(&v.log));
    h
}

/// Serves the page for every path on an access point.
pub async fn serve() -> Response<Body> {
    let html = render(&gather().await);
    Response::builder()
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from(html))
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_names_the_access_point_and_escapes_what_it_shows() {
        let v = View {
            name: "garage<b>".into(),
            tunnel_live: true,
            dark: Some(false),
            generation: 3,
            main_generation: 4,
            version: "1.3.0".into(),
            main_version: "1.3.1".into(),
            inventory: access_points::Inventory {
                ports: vec![access_points::PortInfo {
                    name: "eth1".into(),
                    uplink: true,
                    link: true,
                }],
                radios: vec![],
            },
            clients: vec![access_points::ClientInfo {
                mac: "AA:BB".into(),
                via: "Wi-Fi 2.4GHz".into(),
                vlan: Some(101),
            }],
            profiles: BTreeMap::from([(101, "guest".to_string())]),
            log: "<script>".into(),
        };
        let html = render(&v);
        assert!(html.contains("<h1>Access point garage&lt;b&gt;</h1>"));
        assert!(html.contains("router.lan"));
        assert!(html.contains("the main router is at 4"));
        assert!(html.contains("the main router runs 1.3.1"));
        assert!(html.contains("uplink to the main router"));
        assert!(html.contains("<td>guest</td>"));
        assert!(html.contains("&lt;script&gt;"));
        assert!(!html.contains("<script>"));
    }
}

/// The page on an adopted access point, the embedded UI anywhere else.
pub async fn serve_or_embedded(req: axum::http::Request<Body>) -> Response<Body> {
    let role = access_points::load_role();
    if role.role == access_points::RouterRole::AccessPoint && role.main_key.is_some() {
        serve().await
    } else {
        crate::embedded_web::serve_embedded(req).await
    }
}
