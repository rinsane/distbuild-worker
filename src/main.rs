use axum::{
    body::Body,
    extract::{Query, Request},
    http::StatusCode,
    response::Response,
    routing::post,
    Router,
};
use http_body_util::BodyExt;
use serde::Deserialize;
use std::{fs, net::SocketAddr, process::Command};
use tar::Archive;
use tempfile::tempdir;
use tokio::net::TcpListener;

#[derive(Deserialize)]
struct CompileParams {
    crate_name: String,
}

#[tokio::main]
async fn main() {
    let app = Router::new().route("/compile", post(compile_handler));

    let port = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(5000);

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    println!("🔧 Worker listening on http://{}", addr);

    axum::serve(TcpListener::bind(addr).await.unwrap(), app)
        .await
        .unwrap();
}

async fn compile_handler(
    Query(params): Query<CompileParams>,
    req: Request<Body>
) -> Response<Body> {
    println!("📥 Received /compile request for crate: {}", params.crate_name);

    let bytes = match req.into_body().collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(e) => {
            eprintln!("❌ Failed to collect request body: {:?}", e);
            return error_response(StatusCode::BAD_REQUEST, "Failed to collect body");
        }
    };

    // Create temp directory
    let temp_dir = match tempdir() {
        Ok(dir) => dir,
        Err(_) => return error_response(StatusCode::INTERNAL_SERVER_ERROR, "Temp dir error"),
    };

    // Unpack tarball
    if let Err(e) = Archive::new(bytes.as_ref()).unpack(&temp_dir) {
        eprintln!("❌ Failed to unpack archive: {:?}", e);
        return error_response(StatusCode::BAD_REQUEST, "Unpack failed");
    }

    // Compile specific crate
    let output = Command::new("cargo")
        .arg("build")
        .arg("-p")
        .arg(&params.crate_name)
        .arg("--offline")
        .current_dir(temp_dir.path())
        .output();

    match output {
        Ok(o) if o.status.success() => {
            let target_dir = temp_dir.path().join("target/debug");
            
            // First try looking for .rlib (library crates)
            if let Ok(entries) = fs::read_dir(target_dir.join("deps")) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().map(|ext| ext == "rlib").unwrap_or(false)
                        && path.file_name().map_or(false, |f| f.to_string_lossy().contains(&format!("lib{}", params.crate_name)))
                    {
                        let filename = path.file_name().unwrap().to_string_lossy();
                        if let Ok(binary) = fs::read(&path) {
                            return Response::builder()
                                .status(StatusCode::OK)
                                .header("Content-Type", "application/octet-stream")
                                .header("X-Rlib-File", filename.as_ref())
                                .body(Body::from(binary))
                                .unwrap();
                        }
                    }
                }
            }

            // If no .rlib found, look for executable (binary crates)
            let exe_path = target_dir.join(&params.crate_name);
            if exe_path.exists() {
                if let Ok(binary) = fs::read(&exe_path) {
                    return Response::builder()
                        .status(StatusCode::OK)
                        .header("Content-Type", "application/octet-stream")
                        .header("X-Binary-File", params.crate_name)
                        .body(Body::from(binary))
                        .unwrap();
                }
            }

            eprintln!("❌ No output file found for {}", params.crate_name);
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "No output file found")
        }

        Ok(o) => {
            let err = String::from_utf8_lossy(&o.stderr);
            eprintln!("❌ Compilation failed:\n{}", err);
            error_response(StatusCode::INTERNAL_SERVER_ERROR, &err)
        }

        Err(e) => {
            eprintln!("❌ Failed to run cargo: {:?}", e);
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Cargo execution failed")
        }
    }
}

fn extract_crate_name(toml: &str) -> String {
    toml.lines()
        .find(|line| line.trim_start().starts_with("name ="))
        .and_then(|line| line.split('=').nth(1))
        .map(|s| s.trim().trim_matches('"').to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

fn error_response(status: StatusCode, message: &str) -> Response<Body> {
    Response::builder()
        .status(status)
        .body(Body::from(message.to_string()))
        .unwrap()
}
