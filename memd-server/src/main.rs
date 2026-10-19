//! Serves the memd HTTP API. Configure with MEMD_DB (database path, default `memd.db`) and
//! MEMD_ADDR (listen address, default `127.0.0.1:7878`).

use std::io::Read;
use std::process::ExitCode;

fn main() -> ExitCode {
    let db = std::env::var("MEMD_DB").unwrap_or_else(|_| "memd.db".to_string());
    let addr = std::env::var("MEMD_ADDR").unwrap_or_else(|_| "127.0.0.1:7878".to_string());

    let conn = match engine_api::open_db(&db) {
        Ok(conn) => conn,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let server = match tiny_http::Server::http(&addr) {
        Ok(server) => server,
        Err(e) => {
            eprintln!("error: cannot listen on {addr}: {e}");
            return ExitCode::FAILURE;
        }
    };
    eprintln!("memd-server listening on http://{addr} (database: {db})");

    // Requests are handled one at a time with one database connection, which is plenty for a
    // local tool. A local process that sends a body slowly can hold up other requests meanwhile.
    for mut request in server.incoming_requests() {
        let response = match read_body(&mut request) {
            Ok(body) => memd_server::handle(
                &conn,
                &memd_server::Request {
                    method: request.method().as_str(),
                    url: request.url(),
                    host: header(&request, "Host"),
                    content_type: header(&request, "Content-Type"),
                    body: &body,
                },
            ),
            Err(response) => response,
        };
        // Only fails for header bytes that aren't valid, and both parts here are constants.
        let content_type =
            tiny_http::Header::from_bytes("Content-Type", response.content_type).unwrap();
        let reply = tiny_http::Response::from_string(response.body)
            .with_status_code(response.status)
            .with_header(content_type);
        if let Err(e) = request.respond(reply) {
            eprintln!("warning: could not send response: {e}");
        }
    }
    ExitCode::SUCCESS
}

fn header<'r>(request: &'r tiny_http::Request, name: &'static str) -> Option<&'r str> {
    request
        .headers()
        .iter()
        .find(|header| header.field.equiv(name))
        .map(|header| header.value.as_str())
}

/// Request bodies are small JSON objects; anything bigger than this is refused.
const MAX_BODY_BYTES: u64 = 1024 * 1024;

fn read_body(request: &mut tiny_http::Request) -> Result<String, memd_server::Response> {
    let mut body = String::new();
    request
        .as_reader()
        .take(MAX_BODY_BYTES + 1)
        .read_to_string(&mut body)
        .map_err(|e| memd_server::Response::error(400, format!("unreadable request body: {e}")))?;
    if body.len() as u64 > MAX_BODY_BYTES {
        return Err(memd_server::Response::error(
            413,
            "request body is too large".to_string(),
        ));
    }
    Ok(body)
}
