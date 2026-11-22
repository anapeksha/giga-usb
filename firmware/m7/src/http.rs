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
    io::{Read, Write},
    response::{
        Content, IntoResponse, Response, ResponseWriter, StatusCode,
        sse::{EventSource, EventStream, EventWriter},
    },
    routing::{PathRouterService, RequestHandlerService},
};

use shared::usb::types::CommandRequest;

#[derive(Default)]
struct PageBuffers {
    events: String<4096>,
    trace: String<4096>,
}

impl PageBuffers {
    // Keep synchronous page construction out of the stream's movable future.
    #[inline(never)]
    fn prepare(&mut self, pages: &mut StreamPages, bus: &Bus) -> (bool, bool, bool) {
        pages.next_into(bus, &mut self.events, &mut self.trace)
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
            let mut bytes = [0; 256];
            let length = serde_json_core::to_slice(
                &Reset {
                    resume: self.subscriber.proof.as_deref(),
                },
                &mut bytes,
            )
            .expect("bounded reset proof");
            let reset = core::str::from_utf8(&bytes[..length]).expect("serialized reset JSON");
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

struct Events<'a>(&'a str);

impl Content for Events<'_> {
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

// One dispatch future avoids nesting a complete response future at every route.
// Large page bodies remain borrowed from this worker's storage.
struct HttpService<'a> {
    bus: &'a Bus,
    assets: &'static crate::assets::Assets,
    buffers: &'a Mutex<NoopRawMutex, PageBuffers>,
}

impl PathRouterService for HttpService<'_> {
    async fn call_path_router_service<R: Read, W: ResponseWriter<Error = R::Error>>(
        &self,
        state: &(),
        _parameters: (),
        path: picoserve::request::Path<'_>,
        mut request: picoserve::request::Request<'_, R>,
        response_writer: W,
    ) -> Result<picoserve::ResponseSent, W::Error> {
        let method = request.parts.method();
        if path == "/" && method == "GET" {
            return crate::assets::DashboardService(self.assets)
                .call_request_handler_service(state, (), request, response_writer)
                .await;
        }
        if path == "/api/events" && method == "GET" {
            let Query(mut query) =
                picoserve::from_request_parts!(state, request, response_writer, Query<StreamQuery>);
            let response = self
                .bus
                .subscribe_client(
                    query.client.as_deref(),
                    query.attempt.as_deref(),
                    query.resume.as_deref(),
                )
                .map(|subscriber| {
                    query.reset_unless_resumed(subscriber.resumed);
                    EventStream(Stream {
                        bus: self.bus,
                        subscriber,
                        pages: StreamPages::new(query),
                        buffers: self.buffers,
                    })
                })
                .ok_or((
                    StatusCode::SERVICE_UNAVAILABLE,
                    "event stream limit reached",
                ));
            return response
                .write_to(request.body_connection.finalize().await?, response_writer)
                .await;
        }
        if path == "/api/trace" && method == "GET" {
            let Query(query) = picoserve::from_request_parts!(
                state,
                request,
                response_writer,
                Query<m7::trace_query::TraceQuery>
            );
            let mut buffers = self.buffers.lock().await;
            self.bus.trace_into(&query, &mut buffers.trace);
            return Response::ok(Events(buffers.trace.as_str()))
                .with_header("Cache-Control", "no-store")
                .write_to(request.body_connection.finalize().await?, response_writer)
                .await;
        }
        if path == "/api/command" && method == "POST" {
            let Json(request_body) =
                picoserve::from_request!(state, request, response_writer, Json<CommandRequest>);
            return command(self.bus, request_body)
                .await
                .write_to(request.body_connection.finalize().await?, response_writer)
                .await;
        }
        let known =
            path == "/" || path == "/api/events" || path == "/api/trace" || path == "/api/command";
        let status = if known {
            StatusCode::METHOD_NOT_ALLOWED
        } else {
            StatusCode::NOT_FOUND
        };
        Response::empty(status)
            .write_to(request.body_connection.finalize().await?, response_writer)
            .await
    }
}

#[embassy_executor::task(pool_size = 3)]
pub async fn serve(
    stack: Stack<'static>,
    bus: &'static Bus,
    assets: &'static crate::assets::Assets,
) {
    // Keep page storage outside picoserve's nested response futures.
    let buffers = Mutex::<NoopRawMutex, _>::new(PageBuffers::default());
    let buffers = &buffers;
    let app = Router::from_service(HttpService {
        bus,
        assets,
        buffers,
    });
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
