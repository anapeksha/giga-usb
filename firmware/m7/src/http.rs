use embassy_futures::select::{Either, select};
use embassy_net::Stack;
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, mutex::Mutex};
use embassy_time::{Duration, Instant, Timer, with_timeout};
use heapless::String;
use m7::{
    event_stream::{DRAIN_BATCH, Reset, StreamPages, StreamQuery},
    network_bus::{Bus, Subscriber},
};
use picoserve::{
    Config, Router, Server, Timeouts,
    extract::{Json, Query},
    io::Write,
    response::{
        Content, IntoResponse, Response, StatusCode,
        fs::File,
        sse::{EventSource, EventStream, EventWriter},
    },
    routing::{get, get_service, post},
};

use shared::usb::types::CommandRequest;

#[derive(Default)]
struct PageBuffers {
    events: String<4096>,
    trace: String<4096>,
}

impl PageBuffers {
    fn prepare(&mut self, pages: &mut StreamPages, bus: &Bus) -> (bool, bool, bool) {
        let next = pages.next(bus);
        let events = next.events.is_some();
        let trace = next.trace.is_some();
        self.events = next.events.unwrap_or_default();
        self.trace = next.trace.unwrap_or_default();
        (events, trace, next.pending)
    }
}

struct Stream<'a> {
    bus: &'a Bus,
    subscriber: Subscriber<'a>,
    pages: StreamPages,
    buffers: &'a Mutex<NoopRawMutex, PageBuffers>,
}

impl EventSource for Stream<'_> {
    async fn write_events<W: Write>(
        mut self,
        mut writer: EventWriter<'_, W>,
    ) -> Result<(), W::Error> {
        if !self.subscriber.resumed {
            let reset = picoserve::response::Json(Reset {
                resume: self.subscriber.proof.as_deref(),
            });
            match with_timeout(Duration::from_secs(5), writer.write_event("reset", reset)).await {
                Ok(result) => result?,
                Err(_) => return Ok(()),
            }
        }
        let mut buffers = self.buffers.lock().await;
        let mut heartbeat = Instant::now() + Duration::from_secs(2);
        loop {
            let mut pending = false;
            for _ in 0..DRAIN_BATCH {
                let (events, trace, more) = buffers.prepare(&mut self.pages, self.bus);
                if events {
                    match with_timeout(
                        Duration::from_secs(5),
                        writer.write_event("events", buffers.events.as_str()),
                    )
                    .await
                    {
                        Ok(result) => result?,
                        Err(_) => return Ok(()),
                    }
                }
                if trace {
                    match with_timeout(
                        Duration::from_secs(5),
                        writer.write_event("trace", buffers.trace.as_str()),
                    )
                    .await
                    {
                        Ok(result) => result?,
                        Err(_) => return Ok(()),
                    }
                }
                pending = more;
                if !pending || Instant::now() >= heartbeat {
                    break;
                }
            }
            if Instant::now() >= heartbeat {
                match with_timeout(Duration::from_secs(5), writer.write_keepalive()).await {
                    Ok(result) => result?,
                    Err(_) => return Ok(()),
                }
                heartbeat = Instant::now() + Duration::from_secs(2);
            }
            if pending {
                embassy_futures::yield_now().await;
            } else {
                match select(self.subscriber.changed(), Timer::at(heartbeat)).await {
                    Either::First(()) | Either::Second(()) => {}
                }
            }
        }
    }
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

#[embassy_executor::task(pool_size = 3)]
pub async fn serve(stack: Stack<'static>, bus: &'static Bus) {
    // Keep page storage outside picoserve's nested response futures.
    let buffers = Mutex::<NoopRawMutex, _>::new(PageBuffers::default());
    let buffers = &buffers;
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
                get(move |Query(mut query): Query<StreamQuery>| {
                    core::future::ready(
                        bus.subscribe_client(
                            query.client.as_deref(),
                            query.attempt.as_deref(),
                            query.resume.as_deref(),
                        )
                        .map(|subscriber| {
                            query.reset_unless_resumed(subscriber.resumed);
                            EventStream(Stream {
                                bus,
                                subscriber,
                                pages: StreamPages::new(query),
                                buffers,
                            })
                        })
                        .ok_or((
                            StatusCode::SERVICE_UNAVAILABLE,
                            "event stream limit reached",
                        )),
                    )
                }),
            )
            .route(
                "/api/trace",
                get(
                    move |Query(query): Query<m7::trace_query::TraceQuery>| async move {
                        Response::ok(Events(bus.trace(&query)))
                            .with_header("Cache-Control", "no-store")
                    },
                ),
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
