use super::*;

use futures_util::FutureExt;

use norte_core::{ProviderId, WireFamily};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

const HEADERS: &[u8] =
    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n";

async fn loopback(policy: TransportPolicy) -> (HttpClient, VendorProfile, TcpListener) {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("bind loopback");
    let profile = VendorProfile::new(
        ProviderId::new("local").expect("provider"),
        WireFamily::new("test").expect("wire"),
        &format!("http://{}", listener.local_addr().expect("address")),
    )
    .expect("profile");
    let client = HttpClient {
        inner: reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("client"),
        policy,
    };
    (client, profile, listener)
}

fn policy() -> TransportPolicy {
    TransportPolicy {
        request_timeout: Some(Duration::from_secs(10)),
        stream_idle_timeout: Some(Duration::from_secs(30)),
        ..TransportPolicy::default()
    }
}

fn open(client: HttpClient, profile: VendorProfile) -> JoinHandle<Result<SseStream>> {
    tokio::spawn(async move { client.send_sse(&profile, "events", &Value::Null).await })
}

async fn accept_request(listener: &TcpListener) -> TcpStream {
    let (mut socket, _) = listener.accept().await.expect("accept");
    let mut request = Vec::new();
    loop {
        let mut byte = [0];
        socket.read_exact(&mut byte).await.expect("request headers");
        request.push(byte[0]);
        if request.ends_with(b"\r\n\r\n") {
            break;
        }
        assert!(request.len() < 16 * 1024, "bounded test headers");
    }
    let headers = String::from_utf8(request).expect("headers");
    let length = headers.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("content-length")
            .then(|| value.trim().parse::<usize>().expect("content length"))
    });
    if let Some(length) = length {
        socket
            .read_exact(&mut vec![0; length])
            .await
            .expect("request body");
    }
    socket
}

async fn disconnected(socket: &mut TcpStream) {
    let mut byte = [0];
    let read = tokio::time::timeout(Duration::from_secs(5), socket.read(&mut byte))
        .await
        .expect("client must release socket");
    assert!(matches!(read, Ok(0) | Err(_)), "expected EOF or reset");
}

fn assert_timeout(error: &Error) {
    assert_eq!(error.provider_kind(), Some(ProviderErrorKind::Transport));
    assert!(!never_reached_provider(error), "timeouts must not retry");
}

#[tokio::test]
async fn opening_deadline_cancels_a_request_without_headers() {
    let (client, profile, listener) = loopback(policy()).await;
    let opening = open(client, profile);
    let mut socket = accept_request(&listener).await;
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(11)).await;
    let error = opening
        .await
        .expect("task")
        .err()
        .expect("opening times out");
    assert_timeout(&error);
    disconnected(&mut socket).await;
    assert!(
        listener.accept().now_or_never().is_none(),
        "no duplicate request"
    );
}

#[tokio::test]
async fn active_body_outlives_opening_deadline_then_idle_closes_socket() {
    let (client, profile, listener) = loopback(policy()).await;
    let opening = open(client, profile);
    let mut socket = accept_request(&listener).await;
    socket.write_all(HEADERS).await.expect("headers");
    let mut stream = opening.await.expect("task").expect("stream");
    tokio::time::pause();
    for _ in 0..4 {
        tokio::time::advance(Duration::from_secs(6)).await;
        tokio::time::resume();
        socket.write_all(b"data: active\n\n").await.expect("event");
        assert_eq!(
            stream.next().await.expect("item").expect("event").data,
            "active"
        );
        tokio::time::pause();
    }
    // Poll once to start the idle wait before advancing the clock.
    let waiting = tokio::spawn(async move {
        let item = stream.next().await.expect("idle error");
        assert!(stream.next().await.is_none(), "terminal error");
        item
    });
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(31)).await;
    assert_timeout(&waiting.await.expect("task").expect_err("idle timeout"));
    disconnected(&mut socket).await;
    assert!(
        listener.accept().now_or_never().is_none(),
        "no retry after headers/body"
    );
}

#[tokio::test]
async fn dropping_stream_or_cancelling_open_releases_socket() {
    for before_headers in [true, false] {
        let (client, profile, listener) = loopback(policy()).await;
        let opening = open(client, profile);
        let mut socket = accept_request(&listener).await;
        if before_headers {
            opening.abort();
            assert!(opening.await.err().expect("cancelled").is_cancelled());
        } else {
            socket.write_all(HEADERS).await.expect("headers");
            let mut stream = opening.await.expect("task").expect("stream");
            socket.write_all(b"data: first\n\n").await.expect("event");
            stream.next().await.expect("item").expect("event");
            drop(stream);
        }
        disconnected(&mut socket).await;
    }
}

#[tokio::test]
async fn explicit_none_disables_opening_deadline() {
    let (client, profile, listener) = loopback(TransportPolicy {
        request_timeout: None,
        ..policy()
    })
    .await;
    let opening = open(client, profile);
    let mut socket = accept_request(&listener).await;
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(600)).await;
    assert!(
        !opening.is_finished(),
        "None must not inherit a default deadline"
    );
    tokio::time::resume();
    socket.write_all(HEADERS).await.expect("headers");
    socket.write_all(b"data: late\n\n").await.expect("event");
    let mut stream = opening.await.expect("task").expect("stream");
    assert_eq!(
        stream.next().await.expect("item").expect("event").data,
        "late"
    );
    drop(stream);
    disconnected(&mut socket).await;
}

#[tokio::test]
async fn non_streaming_deadlines_include_response_body_and_none_disables_them() {
    for mode in 0..3 {
        for timeout in [Some(Duration::from_secs(10)), None] {
            let (client, profile, listener) = loopback(TransportPolicy {
                request_timeout: timeout,
                ..policy()
            })
            .await;
            let request = tokio::spawn(async move {
                match mode {
                    0 => client
                        .send_post_json(&profile, "json", &Value::Null)
                        .await
                        .map(|_| ()),
                    1 => client
                        .send_post_multipart(
                            &profile,
                            "upload",
                            vec![MultipartField::Text {
                                name: "field".to_owned(),
                                value: "value".to_owned(),
                            }],
                        )
                        .await
                        .map(|_| ()),
                    _ => client.send_get_bytes(&profile, "bytes").await.map(|_| ()),
                }
            });
            let mut socket = accept_request(&listener).await;
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n")
                .await
                .expect("headers");
            tokio::time::pause();
            tokio::time::advance(Duration::from_secs(600)).await;
            if timeout.is_some() {
                assert_timeout(&request.await.expect("task").expect_err("body deadline"));
            } else {
                assert!(!request.is_finished(), "None has no body deadline");
                tokio::time::resume();
                socket.write_all(b"{}").await.expect("body");
                request.await.expect("task").expect("response");
            }
            disconnected(&mut socket).await;
            if timeout.is_some() {
                tokio::time::resume();
            }
        }
    }
}

#[tokio::test]
async fn oversized_body_is_a_terminal_decode_error_including_at_eof() {
    for at_eof in [false, true] {
        let (client, profile, listener) = loopback(policy()).await;
        let opening = open(client, profile);
        let mut socket = accept_request(&listener).await;
        socket.write_all(HEADERS).await.expect("headers");
        let mut stream = opening.await.expect("task").expect("stream");
        let writer = tokio::spawn(async move {
            let mut frame = b"data: preserved\n\n".to_vec();
            frame.resize(frame.len() + 8 * 1024 * 1024, b'x');
            // At EOF this trailing CR is no longer a possible delimiter; without
            // EOF an ordinary byte already proves the frame is oversized.
            frame.push(if at_eof { b'\r' } else { b'x' });
            socket.write_all(&frame).await.expect("oversized frame");
            if at_eof {
                socket.shutdown().await.expect("EOF");
            }
            disconnected(&mut socket).await;
        });
        assert_eq!(
            stream
                .next()
                .await
                .expect("prior item")
                .expect("valid event")
                .data,
            "preserved"
        );
        let error = stream
            .next()
            .await
            .expect("item")
            .expect_err("oversized frame");
        assert_eq!(error.provider_kind(), Some(ProviderErrorKind::Decode));
        assert!(stream.next().await.is_none(), "decode failure ends stream");
        writer.await.expect("writer");
    }
}

#[tokio::test]
async fn provider_status_and_broken_body_are_not_retried() {
    for response in [
        b"HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".as_slice(),
        b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\ndata: partial"
            .as_slice(),
    ] {
        let (client, profile, listener) = loopback(policy()).await;
        let opening = open(client, profile);
        let mut socket = accept_request(&listener).await;
        socket.write_all(response).await.expect("response");
        socket.shutdown().await.expect("close response");
        match opening.await.expect("task") {
            Ok(mut stream) => {
                stream.next().await.expect("item").expect_err("broken body");
                assert!(stream.next().await.is_none());
            }
            Err(error) => assert_eq!(error.provider_kind(), Some(ProviderErrorKind::ServerError)),
        }
        assert!(
            listener.accept().now_or_never().is_none(),
            "provider may have done billable work"
        );
    }
}
