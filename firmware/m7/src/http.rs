use embassy_net::Stack;
use embassy_time::Duration;
use heapless::String;
use m7::network_bus::Bus;
use picoserve::{
    Config, Router, Server, Timeouts,
    extract::{Json, Query},
    io::Write,
    response::{Content, IntoResponse, Json as JsonResponse, Response, StatusCode, fs::File},
    routing::{get, get_service, post},
};
use serde::Deserialize;
use shared::{
    usb::types::CommandRequest,
    wifi::{WifiControl, WifiRequest},
};

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

pub async fn serve(stack: Stack<'_>, bus: &Bus, wifi: &WifiControl) {
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
                "/api/wifi",
                get(move || async move {
                    JsonResponse(wifi.state())
                        .into_response()
                        .with_header("Cache-Control", "no-store")
                })
                .post(move |Json(request): Json<WifiRequest>| async move {
                    provision(wifi, request).await
                }),
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

async fn provision(wifi: &WifiControl, request: WifiRequest) -> impl IntoResponse {
    let (status, body) = if request.validate().is_err() {
        (
            StatusCode::BAD_REQUEST,
            "{\"error\":\"invalid SSID or password\"}",
        )
    } else if wifi.submit(request).is_err() {
        (
            StatusCode::CONFLICT,
            "{\"error\":\"Wi-Fi provisioning is only available in setup mode\"}",
        )
    } else {
        (StatusCode::ACCEPTED, "{\"accepted\":true}")
    };
    (
        status,
        ("Content-Type", "application/json"),
        ("Cache-Control", "no-store"),
        body,
    )
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
