use token_tracker::server::{ServerConfig, router};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let app = router(ServerConfig::from_env()?)?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000").await?;

    eprintln!("Listening on http://127.0.0.1:3000");

    axum::serve(listener, app).await?;
    Ok(())
}
