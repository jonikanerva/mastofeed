//! Test helpers: a fake Mastodon server and fixture paths.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::{Request, State};
use axum::http::header::AUTHORIZATION;
use axum::response::Response;

/// One request that the fake server received.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct Recorded {
    pub method: String,
    pub path: String,
    pub authorization: Option<String>,
}

type Log = Arc<Mutex<Vec<Recorded>>>;

/// A fake server. `respond` gets each request and builds the response.
pub struct FakeServer {
    pub addr: SocketAddr,
    pub requests: Log,
    task: tokio::task::JoinHandle<()>,
}

impl FakeServer {
    pub async fn start<F>(respond: F) -> Self
    where
        F: Fn(&Request) -> Response + Clone + Send + Sync + 'static,
    {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let state = (requests.clone(), Arc::new(respond));
        let app = Router::new().fallback(
            |State((requests, respond)): State<(Log, Arc<F>)>, request: Request| async move {
                requests.lock().unwrap().push(Recorded {
                    method: request.method().to_string(),
                    path: request.uri().to_string(),
                    authorization: request
                        .headers()
                        .get(AUTHORIZATION)
                        .map(|value| value.to_str().unwrap().to_owned()),
                });
                respond(&request)
            },
        );
        let app = app.with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            addr,
            requests,
            task,
        }
    }

    pub fn base_url(&self) -> reqwest::Url {
        format!("http://{}/", self.addr).parse().unwrap()
    }

    pub fn recorded(&self) -> Vec<Recorded> {
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for FakeServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// The fake home timeline of the golden files: `recent.json` without
/// `since_id` and `fresh.json` with it. The TypeScript service made no
/// request with `max_id`; the fake answers it with an empty page, the end
/// of the newer statuses.
pub fn timeline_fixture(request: &Request) -> Response {
    let query = request.uri().query().unwrap_or_default();
    if query.contains("max_id=") {
        return json_response("[]");
    }
    let file = if query.contains("since_id=") {
        "fresh.json"
    } else {
        "recent.json"
    };
    json_response(std::fs::read(fixture(&format!("mastodon/{file}"))).unwrap())
}

pub fn json_response(body: impl Into<axum::body::Body>) -> Response {
    Response::builder()
        .header("content-type", "application/json; charset=utf-8")
        .body(body.into())
        .unwrap()
}

pub fn fixture(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(relative)
}

pub fn read_json(relative: &str) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(fixture(relative)).unwrap()).unwrap()
}

pub fn public_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../public")
}
