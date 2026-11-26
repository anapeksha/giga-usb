//! Bounded HTTP admission only. Flash work belongs to the independent OTA task.
use crate::assets::Assets;
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, mutex::Mutex};
use embassy_time::{Duration, Instant, with_timeout};
use m7::ota_control::{self, Work};
use picoserve::{
    io::Read,
    response::{IntoResponse, Response, ResponseWriter, StatusCode},
    routing::RequestHandlerService,
};

pub struct UpdateService<'a> {
    pub assets: &'static Assets,
    pub buffer: &'a Mutex<NoopRawMutex, [u8; 4096]>,
}

fn number(headers: picoserve::request::Headers<'_>, name: &str) -> Option<u32> {
    let value = headers.get(name)?.as_str().ok()?;
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    value.parse().ok()
}

impl RequestHandlerService<(), picoserve::request::Path<'_>> for UpdateService<'_> {
    async fn call_request_handler_service<R: Read, W: ResponseWriter<Error = R::Error>>(
        &self,
        _state: &(),
        path: picoserve::request::Path<'_>,
        mut request: picoserve::request::Request<'_, R>,
        writer: W,
    ) -> Result<picoserve::ResponseSent, W::Error> {
        let control = &self.assets.control;
        let mut status = StatusCode::OK;
        let mut error = None;
        if path == "/api/update/status" {
            if request.parts.method() != "GET" {
                status = StatusCode::METHOD_NOT_ALLOWED;
                error = Some("GET required");
            }
        } else if request.parts.method() != "POST" {
            status = StatusCode::METHOD_NOT_ALLOWED;
            error = Some("POST required");
        } else {
            let headers = request.parts.headers();
            if !ota_control::csrf(
                headers.get("Origin").map(|v| v.as_raw()),
                headers.get("X-GigaUSB-Update").map(|v| v.as_raw()),
                headers.get("Content-Type").map(|v| v.as_raw()),
            ) {
                status = StatusCode::FORBIDDEN;
                error =
                    Some("trusted local Origin, binary type and explicit update header required");
            } else {
                let length = request.body_connection.content_length();
                let begin = path == "/api/update/begin";
                let chunk = path == "/api/update/chunk";
                if (begin && length != 256)
                    || (chunk && !(1..=4096).contains(&length))
                    || (!begin && !chunk && length != 0)
                {
                    status = StatusCode::BAD_REQUEST;
                    error = Some("invalid bounded request body length");
                } else {
                    let token = number(headers, "X-GigaUSB-Token");
                    let offset = number(headers, "X-GigaUSB-Offset");
                    let mut buffer = self.buffer.lock().await;
                    let body_ok = if length == 0 {
                        true
                    } else {
                        let mut reader = request.body_connection.body().reader();
                        let mut done = 0;
                        let deadline = Instant::now() + Duration::from_secs(5);
                        while done < length {
                            let remaining = deadline.saturating_duration_since(Instant::now());
                            if remaining.as_millis() == 0 {
                                break;
                            }
                            match with_timeout(remaining, reader.read(&mut buffer[done..length]))
                                .await
                            {
                                Ok(Ok(size)) if size != 0 => done += size,
                                _ => break,
                            }
                        }
                        done == length
                    };
                    if !body_ok {
                        status = StatusCode::BAD_REQUEST;
                        error = Some("body incomplete or deadline exceeded");
                    } else {
                        let now = Instant::now().as_millis();
                        let accepted = if path == "/api/update/authorize" {
                            control.authorize(now)
                        } else if let Some(token) = token {
                            let work = if begin {
                                Some(Work::Begin {
                                    token,
                                    manifest: buffer[..256].try_into().unwrap(),
                                })
                            } else if chunk {
                                offset.map(|offset| Work::Chunk {
                                    token,
                                    offset,
                                    bytes: heapless::Vec::from_slice(&buffer[..length]).unwrap(),
                                })
                            } else if path == "/api/update/commit" {
                                Some(Work::Commit { token })
                            } else if path == "/api/update/cancel" {
                                Some(Work::Cancel { token })
                            } else {
                                None
                            };
                            work.is_some_and(|work| control.admit(work, now))
                        } else {
                            false
                        };
                        if accepted {
                            status = StatusCode::ACCEPTED;
                        } else {
                            status = StatusCode::CONFLICT;
                            error = Some(
                                "update state, token, offset header or authorization window rejected",
                            );
                        }
                    }
                }
            }
        }
        let mut bytes = [0; 512];
        let length = if let Some(error) = error {
            #[derive(serde::Serialize)]
            struct Failure<'a> {
                error: &'a str,
            }
            serde_json_core::to_slice(&Failure { error }, &mut bytes).expect("bounded error")
        } else {
            serde_json_core::to_slice(&control.status(), &mut bytes).expect("bounded status")
        };
        let body = core::str::from_utf8(&bytes[..length]).expect("JSON UTF8");
        Response::new(status, crate::http::Events(body))
            .with_header("Cache-Control", "no-store")
            .write_to(request.body_connection.finalize().await?, writer)
            .await
    }
}
