//! The Mastodon adapter against a fake server: success, failures, and
//! the outbound limits.

use std::time::Duration;

use axum::http::StatusCode;
use axum::response::{IntoResponse as _, Response};
use mastofeed_domain::status::StatusId;
use mastofeed_server::config::Secret;
use mastofeed_server::mastodon::{ClientSettings, MastodonClient, MastodonError};
use serde_json::json;

use crate::common::{FakeServer, json_response};

fn client(server: &FakeServer, settings: ClientSettings) -> MastodonClient {
    MastodonClient::new(
        &server.base_url(),
        &Secret::new("test-token".to_owned()),
        settings,
    )
    .unwrap()
}

async fn fetch(
    respond: fn(&axum::extract::Request) -> Response,
) -> Result<Vec<serde_json::Value>, MastodonError> {
    let server = FakeServer::start(respond).await;
    client(&server, ClientSettings::default())
        .home_timeline(None, None)
        .await
}

#[tokio::test]
async fn sends_the_token_the_query_and_a_user_agent() {
    let server = FakeServer::start(|request| {
        let user_agent = request
            .headers()
            .get("user-agent")
            .unwrap()
            .to_str()
            .unwrap();
        assert!(user_agent.starts_with("mastofeed/"));
        assert_eq!(request.headers().get("accept").unwrap(), "application/json");
        json_response("[]")
    })
    .await;
    let since = StatusId::new("42".to_owned()).unwrap();
    let entries = client(&server, ClientSettings::default())
        .home_timeline(Some(&since), None)
        .await
        .unwrap();
    assert_eq!(entries, Vec::<serde_json::Value>::new());
    let recorded = server.recorded();
    assert_eq!(
        recorded[0].path,
        "/api/v1/timelines/home?limit=40&since_id=42"
    );
    assert_eq!(
        recorded[0].authorization.as_deref(),
        Some("Bearer test-token")
    );
}

#[tokio::test]
async fn sends_max_id_after_since_id() {
    let server = FakeServer::start(|_| json_response("[]")).await;
    let since = StatusId::new("42".to_owned()).unwrap();
    let max = StatusId::new("90".to_owned()).unwrap();
    client(&server, ClientSettings::default())
        .home_timeline(Some(&since), Some(&max))
        .await
        .unwrap();
    assert_eq!(
        server.recorded()[0].path,
        "/api/v1/timelines/home?limit=40&since_id=42&max_id=90"
    );
}

#[tokio::test]
async fn converts_keys_and_keeps_the_last_repeated_key() {
    let entries = fetch(|_| {
        json_response(r#"[{"media_attachments": [], "spoiler_text": "a", "spoiler_text": "b"}]"#)
    })
    .await
    .unwrap();
    assert_eq!(
        entries,
        [json!({"mediaAttachments": [], "spoilerText": "b"})]
    );
}

#[tokio::test]
async fn a_base_url_path_is_kept() {
    let server = FakeServer::start(|_| json_response("[]")).await;
    let base = format!("http://{}/prefix", server.addr).parse().unwrap();
    let client = MastodonClient::new(
        &base,
        &Secret::new("t".to_owned()),
        ClientSettings::default(),
    )
    .unwrap();
    client.home_timeline(None, None).await.unwrap();
    assert_eq!(
        server.recorded()[0].path,
        "/prefix/api/v1/timelines/home?limit=40"
    );
}

#[tokio::test]
async fn an_error_status_is_an_error() {
    let result = fetch(|_| {
        (
            StatusCode::UNAUTHORIZED,
            r#"{"error":"The access token is invalid"}"#,
        )
            .into_response()
    })
    .await;
    assert_eq!(result, Err(MastodonError::Status(401)));
}

#[tokio::test]
async fn a_body_that_is_not_an_array_is_malformed() {
    for body in [r#"{"error":"x"}"#, "not json", "", "[1,"] {
        let server = FakeServer::start(move |_| json_response(body)).await;
        let result = client(&server, ClientSettings::default())
            .home_timeline(None, None)
            .await;
        assert_eq!(result, Err(MastodonError::MalformedBody), "{body:?}");
    }
}

#[tokio::test]
async fn a_redirect_is_not_followed() {
    let target = FakeServer::start(|_| json_response("[]")).await;
    let location = format!("http://{}/elsewhere", target.addr);
    let server = FakeServer::start(move |_| {
        Response::builder()
            .status(StatusCode::FOUND)
            .header("location", location.as_str())
            .body(axum::body::Body::empty())
            .unwrap()
    })
    .await;
    let result = client(&server, ClientSettings::default())
        .home_timeline(None, None)
        .await;
    assert_eq!(result, Err(MastodonError::Status(302)));
    assert!(
        target.recorded().is_empty(),
        "the redirect target received the token"
    );
}

#[tokio::test]
async fn a_large_body_is_rejected() {
    let server =
        FakeServer::start(|_| json_response(format!("[{}]", "1,".repeat(2000) + "1"))).await;
    let settings = ClientSettings {
        max_body_bytes: 1024,
        ..ClientSettings::default()
    };
    let result = client(&server, settings).home_timeline(None, None).await;
    assert_eq!(result, Err(MastodonError::BodyTooLarge));
}

#[tokio::test]
async fn a_slow_body_times_out() {
    let server = FakeServer::start(|_| {
        let stream = async_stream_body();
        Response::builder()
            .header("content-type", "application/json")
            .body(axum::body::Body::from_stream(stream))
            .unwrap()
    })
    .await;
    let settings = ClientSettings {
        request_timeout: Duration::from_millis(300),
        ..ClientSettings::default()
    };
    let started = std::time::Instant::now();
    let result = client(&server, settings).home_timeline(None, None).await;
    assert_eq!(result, Err(MastodonError::Timeout));
    assert!(started.elapsed() < Duration::from_secs(5));
}

/// A body that sends `[` and then stalls.
fn async_stream_body() -> impl futures_util::Stream<Item = Result<Vec<u8>, std::io::Error>> {
    futures_util::stream::unfold(0, |state| async move {
        if state == 0 {
            Some((Ok(b"[".to_vec()), 1))
        } else {
            tokio::time::sleep(Duration::from_secs(60)).await;
            None
        }
    })
}

#[tokio::test]
async fn a_closed_port_is_a_connection_error() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let base = format!("http://{addr}/").parse().unwrap();
    let client = MastodonClient::new(
        &base,
        &Secret::new("t".to_owned()),
        ClientSettings::default(),
    )
    .unwrap();
    assert_eq!(
        client.home_timeline(None, None).await,
        Err(MastodonError::Connect)
    );
}

#[tokio::test]
async fn floats_are_parsed_to_the_nearest_double() {
    let entries =
        fetch(|_| json_response(r#"[{"x": 0.36035866877676836, "y": -1.8600795271598733}]"#))
            .await
            .unwrap();
    for (key, text) in [("x", "0.36035866877676836"), ("y", "-1.8600795271598733")] {
        let expected: f64 = text.parse().unwrap();
        assert_eq!(
            entries[0][key].as_f64().unwrap().to_bits(),
            expected.to_bits(),
            "{key}"
        );
    }
}
