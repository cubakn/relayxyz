use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use std::sync::Arc;
use tokio::net::TcpListener;

use relayxyz::config::Config;
use relayxyz::connection;
use relayxyz::db::Db;
use relayxyz::relay::Relay;

#[tokio::main]
async fn main() {
    let _ = dotenvy::dotenv();
    let config = Config::from_env();
    let bind = config.bind;

    let db = Db::open(&config.db_path).expect("failed to open database");
    let relay = Arc::new(Relay::new(config, db));

    let listener = TcpListener::bind(bind).await.expect("failed to bind");
    eprintln!("listening on {bind}");

    let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);

    let shutdown_tx_clone = shutdown_tx.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        eprintln!("shutting down...");
        let _ = shutdown_tx_clone.send(true);
    });

    loop {
        tokio::select! {
            result = listener.accept() => {
                let (stream, addr) = match result {
                    Ok(v) => v,
                    Err(e) => {
                        eprintln!("accept error: {e}");
                        continue;
                    }
                };
                let relay = relay.clone();
                let mut shutdown_watch = shutdown_rx.clone();
                tokio::spawn(async move {
                    let io = TokioIo::new(stream);
                    let service = service_fn(move |req| {
                        let relay = relay.clone();
                        async move { connection::handle_request(req, relay).await }
                    });
                    let mut conn = std::pin::pin!(
                        http1::Builder::new()
                            .serve_connection(io, service)
                            .with_upgrades()
                    );
                    loop {
                        tokio::select! {
                            result = conn.as_mut() => {
                                if let Err(e) = result {
                                    let e_str = e.to_string();
                                    if !e_str.contains("early eof")
                                        && !e_str.contains("connection reset")
                                        && !e_str.contains("broken pipe") {
                                        eprintln!("connection error from {addr}: {e}");
                                    }
                                }
                                break;
                            }
                            _ = shutdown_watch.changed() => {
                                conn.as_mut().graceful_shutdown();
                            }
                        }
                    }
                });
            }
            _ = shutdown_rx.changed() => {
                eprintln!("stopped accepting connections");
                break;
            }
        }
    }
}
