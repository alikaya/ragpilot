//! `ragpilot dashboard` — a local window onto the project fleet and the brain.
//!
//! Bound to the loopback interface and gated by a token minted at startup: the
//! page can act (close a thread, start a compile), and a page in the user's
//! browser must not be able to do that just by knowing the port.
//!
//! Nothing here talks to the enterprise server, and nothing here is reported to
//! it. The brain in particular is the user's own content — the same boundary
//! the observation seam enforces holds here by construction.

mod http;
mod state;

use std::sync::Arc;

use anyhow::{Context, Result};
use colored::Colorize;
use tokio::net::TcpListener;

use http::{escape, Request, Response};

const PAGE: &str = include_str!("page.html");

struct Server {
    token: String,
}

pub async fn cmd_dashboard(port: u16, open: bool) -> Result<()> {
    // Loopback only. This is a personal dashboard, not a service.
    let addr = format!("127.0.0.1:{port}");
    let listener = TcpListener::bind(&addr)
        .await
        .with_context(|| format!("Cannot bind {addr} — is a dashboard already running?"))?;

    let token = uuid::Uuid::new_v4().simple().to_string();
    let url = format!("http://{addr}/?t={token}");
    let server = Arc::new(Server { token });

    println!("{} RagPilot dashboard", "→".cyan());
    println!("  {}", url.bold());
    println!("  {}", "Ctrl-C to stop. The token is new each run.".dimmed());

    if open {
        let _ = std::process::Command::new("xdg-open").arg(&url).status();
    }

    loop {
        let Ok((mut socket, _)) = listener.accept().await else { continue };
        let server = Arc::clone(&server);
        tokio::spawn(async move {
            if let Some(request) = http::read_request(&mut socket).await {
                let response = server.route(request).await;
                let _ = http::write_response(&mut socket, response).await;
            }
        });
    }
}

impl Server {
    /// Every route is behind the token. It arrives once in the URL and is kept
    /// in a cookie afterwards, so a link cannot leak it through the referer of
    /// a later request.
    fn authorised(&self, req: &Request) -> bool {
        req.query.get("t").is_some_and(|t| t == &self.token)
            || req.cookies.get("ragpilot_dash").is_some_and(|t| t == &self.token)
    }

    async fn route(&self, req: Request) -> Response {
        if !self.authorised(&req) {
            return Response::text(401, "Unauthorized. Open the URL printed by `ragpilot dashboard`.");
        }

        match (req.method.as_str(), req.path.as_str()) {
            ("GET", "/") => Response::html(PAGE.replace("{{TOKEN}}", &escape(&self.token))).with_header(
                format!("Set-Cookie: ragpilot_dash={}; Path=/; HttpOnly; SameSite=Strict", self.token),
            ),
            ("GET", "/api/state") => match serde_json::to_string(&state::collect()) {
                Ok(json) => Response::json(json),
                Err(e) => Response::text(500, &format!("state error: {e}")),
            },
            ("GET", "/api/points") => {
                let projects = state::collect().projects;
                let points = state::collection_points(&projects).await;
                Response::json(serde_json::to_string(&points).unwrap_or_else(|_| "{}".into()))
            }
            ("GET", "/api/note") => self.note(&req),
            ("POST", "/api/thread/close") => self.close_thread(&req),
            ("POST", "/api/compile") => self.compile(),
            ("GET", _) | ("POST", _) => Response::text(404, "Not found"),
            _ => Response::text(405, "Method not allowed"),
        }
    }

    /// The body of one knowledge note or skill, for reading in place.
    fn note(&self, req: &Request) -> Response {
        let Some(slug) = req.query.get("slug") else {
            return Response::text(400, "missing slug");
        };
        // The slug names a file: anything that is not a plain name is refused
        // rather than resolved.
        if !slug.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            return Response::text(400, "bad slug");
        }
        let area = req.query.get("area").map(String::as_str).unwrap_or("knowledge");
        let dir = match area {
            "skills" => crate::brain::skills_dir(),
            "knowledge" => crate::brain::knowledge_dir(),
            _ => return Response::text(400, "bad area"),
        };
        match std::fs::read_to_string(dir.join(format!("{slug}.md"))) {
            Ok(text) => Response::json(serde_json::json!({ "slug": slug, "text": text }).to_string()),
            Err(e) => Response::text(404, &format!("no such note: {e}")),
        }
    }

    fn close_thread(&self, req: &Request) -> Response {
        let Ok(payload) = serde_json::from_str::<serde_json::Value>(&req.body) else {
            return Response::text(400, "bad json");
        };
        let Some(text) = payload.get("text").and_then(|t| t.as_str()) else {
            return Response::text(400, "missing text");
        };
        match crate::brain::vault::update_threads(&[], &[text.to_string()]) {
            Ok(_) => Response::json(r#"{"ok":true}"#.to_string()),
            Err(e) => Response::text(500, &format!("close failed: {e}")),
        }
    }

    /// Start a compile in the background. It takes minutes, so the request
    /// returns immediately and the page picks the result up on its next poll.
    fn compile(&self) -> Response {
        let binary = std::env::current_exe().unwrap_or_else(|_| "ragpilot".into());
        match std::process::Command::new(binary)
            .args(["brain", "compile"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(_) => Response::json(r#"{"ok":true,"started":true}"#.to_string()),
            Err(e) => Response::text(500, &format!("could not start compile: {e}")),
        }
    }
}
