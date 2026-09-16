use axum::{Router, routing::get};

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let app = Router::new().route("/health", get(|| async { "ok\n" }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000").await?;

    eprintln!("Listening on http://127.0.0.1:3000");

    axum::serve(listener, app).await
}
