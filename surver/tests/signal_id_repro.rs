// Reproduces: a signal id past the signal table stalls the server's file.
// Run: cargo test --release -p surver --test signal_id_repro
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn out_of_range_signal_id_is_refused() {
    let port = std::net::TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut file = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    file.extend(["..", "examples", "with_1_bit.vcd"]);
    let file = file.canonicalize().unwrap();
    let token = "testtoken123456";

    let started = Arc::new(AtomicBool::new(false));
    let flag = started.clone();
    tokio::spawn(async move {
        let _ = surver::surver_main(
            port,
            "127.0.0.1".to_string(),
            Some(token.to_string()),
            &[file.to_string_lossy().to_string()],
            Some(flag),
        )
        .await;
    });
    while !started.load(Ordering::SeqCst) {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // Id 1_000_000 is past the signal table of with_1_bit.vcd. Expected: 404.
    // Before the fix: the loader panics, the request never completes, and the timeout fires.
    let resp = reqwest::Client::new()
        .get(format!("http://127.0.0.1:{port}/{token}/0/get_signals/1000000"))
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .expect("request completes (before the fix it hangs until the timeout)");
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
}
