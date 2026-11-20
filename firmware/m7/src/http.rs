use embassy_net::Stack;
use embassy_time::Duration;
use heapless::String;
use m7::network_bus::Bus;
use picoserve::{
    Config, Router, Server, Timeouts,
    extract::{Json, Query},
    io::Write,
    response::{Content, IntoResponse, Response, StatusCode, fs::File},
    routing::{get, get_service, post},
};
use serde::Deserialize;
use shared::usb::types::CommandRequest;

#[derive(Deserialize)]
struct Cursor {
    #[serde(default)]
    after: u32,
}

struct Events(String<4096>);

impl Content for Events {
    fn content_type(&self) -> &'static str {
        "application/json"
    }

    fn content_length(&self) -> usize {
        self.0.len()
    }

    async fn write_content<W: Write>(self, mut writer: W) -> Result<(), W::Error> {
        writer.write_all(self.0.as_bytes()).await
    }
}

pub async fn serve(stack: Stack<'_>, bus: &Bus) {
    let app =
        Router::new()
            .route(
                "/",
                get_service(File::with_content_type_and_headers(
                    File::MIME_HTML,
                    include_bytes!("../../../dist/index.html.gz"),
                    &[("Content-Encoding", "gzip"), ("Cache-Control", "no-cache")],
                )),
            )
            .route(
                "/api/events",
                get(move |Query(cursor): Query<Cursor>| async move {
                    Response::ok(Events(bus.events(cursor.after)))
                        .with_header("Cache-Control", "no-store")
                }),
            )
            .route(
                "/api/command",
                post(move |Json(request): Json<CommandRequest>| async move {
                    command(bus, request).await
                }),
            );
    let config = Config::new(Timeouts {
        start_read_request: Duration::from_secs(5),
        read_request: Duration::from_secs(3),
        persistent_start_read_request: Duration::from_secs(1),
        write: Duration::from_secs(5),
    })
    .keep_connection_alive();
    let mut rx = [0; 1024];
    let mut tx = [0; 1024];
    let mut http = [0; 2048];
    Server::new(&app, &config, &mut http)
        .listen_and_serve(0, stack, 80, &mut rx, &mut tx)
        .await;
}

async fn command(bus: &Bus, request: CommandRequest) -> impl IntoResponse {
    let (status, body) = if bus.command(request.command) {
        (StatusCode::ACCEPTED, "{\"accepted\":true}")
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "{\"error\":\"command queue full\"}",
        )
    };
    (status, ("Content-Type", "application/json"), body)
}
