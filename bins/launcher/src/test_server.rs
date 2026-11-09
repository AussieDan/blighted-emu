// SPDX-License-Identifier: GPL-3.0-or-later
//! A local HTTP server for the tests of the network calls.

use std::net::SocketAddr;

use axum::Router;

/// Serves `app` on a free loopback port in its own thread and returns its base URL
/// (`http://127.0.0.1:<port>`).
pub fn serve(app: Router) -> String {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        runtime.block_on(async move {
            let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
                .await
                .expect("bind");
            sender
                .send(listener.local_addr().expect("address"))
                .expect("send");
            axum::serve(listener, app).await.expect("serve");
        });
    });
    let address = receiver.recv().expect("address");
    format!("http://{address}")
}
