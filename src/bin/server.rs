use token_tracker::{
    SqliteExportStore,
    server::{ServerConfig, router},
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = ServerConfig::from_env()?;
    let token = config.read_token()?;
    let sink = SqliteExportStore::open(&config.database_path)?;
    let app = router(token, sink, config.max_upload_bytes)?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000").await?;

    eprintln!("Listening on http://127.0.0.1:3000");

    axum::serve(listener, app).await?;
    Ok(())
}
