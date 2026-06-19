/// NOTE: This is untested!
use std::io;

use axum::{
    body::{Body, Bytes},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Router,
};
use base64::prelude::*;
use tokio::net::TcpListener;
use tracing::info;

const BIND_ADDR: &'static str = "127.0.0.1:3000";

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error("there was an IO error: {0}")]
    Io(#[from] io::Error),
    #[error("invalid base64 data recieved: {0}")]
    Base64(#[from] base64::DecodeError),
}
impl Error {
    fn status_code(&self) -> StatusCode {
        match self {
            Error::Io(_) => StatusCode::INTERNAL_SERVER_ERROR,
            Error::Base64(_) => StatusCode::BAD_REQUEST,
        }
    }
}
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        Response::builder()
            .status(self.status_code())
            .body(Body::from(format!("{self}")))
            .expect("failed to build body")
    }
}

#[tokio::main]
async fn main() -> Result<(), io::Error> {
    // Setup the logger
    let subscriber = tracing_subscriber::fmt()
        .compact()
        .with_file(true)
        .with_line_number(true)
        .with_thread_ids(true)
        .with_target(false)
        .finish();
    tracing::subscriber::set_global_default(subscriber)
        .expect("failed to set global tracing subscriber");

    // Build our application with a route
    let app = Router::new().route("/decompile", post(decompile));

    // Run the web server
    let listener = TcpListener::bind(BIND_ADDR).await?;
    info!("🚀 Listening on {}", listener.local_addr()?);
    axum::serve(listener, app).await
}

async fn decompile(headers: HeaderMap, body: Bytes) -> Result<String, Error> {
    let mut bytecode = Vec::new();
    BASE64_STANDARD.decode_vec(body, &mut bytecode)?;
    let script_name = headers
        .get("x-script-name")
        .and_then(|value| value.to_str().ok());
    let decompiled = luau_lifter::decompile_bytecode_with_script_name(&bytecode, 203, script_name);
    info!("Successfully decompiled bytecode.");
    Ok(decompiled)
}
