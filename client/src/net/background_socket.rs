use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use futures_util::{Sink, Stream, StreamExt};
use tokio::sync::{Semaphore, mpsc, oneshot};
use tokio_tungstenite::tungstenite::{Error, Message};

type Writer = Pin<Box<dyn Sink<Message, Error = Error> + Send>>;
type Reader = Pin<Box<dyn Stream<Item = Result<Message, Error>> + Send>>;
type Command = (Message, oneshot::Sender<Result<(), String>>);
const QUEUED_FRAMES: usize = 128;
const QUEUED_BYTES: usize = 8 * 1024 * 1024;

pub(super) struct Transport(tokio::task::JoinHandle<()>);

impl Drop for Transport {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub(super) fn start<S, R>(sink: S, reader: R) -> (Writer, Reader, Transport)
where
    S: Sink<Message> + Unpin + Send + 'static,
    S::Error: std::fmt::Display + Send,
    R: Stream<Item = Result<Message, Error>> + Unpin + Send + 'static,
{
    start_with_interval(sink, reader, crate::protocol::GATEWAY_HEARTBEAT_INTERVAL)
}

fn start_with_interval<S, R>(
    mut sink: S,
    mut reader: R,
    interval: Duration,
) -> (Writer, Reader, Transport)
where
    S: Sink<Message> + Unpin + Send + 'static,
    S::Error: std::fmt::Display + Send,
    R: Stream<Item = Result<Message, Error>> + Unpin + Send + 'static,
{
    let (outbound, mut commands) = mpsc::channel::<Command>(1);
    let (inbound, frames) = mpsc::channel(QUEUED_FRAMES);
    let (finished, completion) = oneshot::channel();
    let budget = Arc::new(Semaphore::new(QUEUED_BYTES));
    // WebView edit acknowledgements can stop scoped tasks while Android hides the app.
    let task = tokio::spawn(async move {
        let result: Result<(), String> = async {
            let mut heartbeat = tokio::time::interval(interval);
            heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            let mut received = tokio::time::Instant::now();
            loop {
                tokio::select! {
                    _ = inbound.closed() => return Ok(()),
                    command = commands.recv() => {
                        let Some((frame, reply)) = command else { return Ok(()) };
                        let result = super::gateway_send(&mut sink, &mut reader, frame, "background command").await;
                        let failed = result.as_ref().err().cloned();
                        if reply.send(result).is_err() {
                            return Ok(());
                        }
                        if let Some(error) = failed { return Err(error); }
                    }
                    _ = heartbeat.tick() => {
                        if received.elapsed() >= crate::protocol::GATEWAY_HEARTBEAT_TIMEOUT {
                            return Err("the host stopped answering heartbeat probes".into());
                        }
                        super::gateway_send(&mut sink, &mut reader, Message::Ping(Vec::new()), "heartbeat").await?;
                    }
                    frame = reader.next() => {
                        let Some(frame) = frame else { return Ok(()) };
                        let frame = frame.map_err(|error| format!("recv: {error}"))?;
                        received = tokio::time::Instant::now();
                        match frame {
                            Message::Ping(payload) => {
                                super::gateway_send(&mut sink, &mut reader, Message::Pong(payload), "pong").await?;
                            }
                            Message::Pong(_) => {}
                            frame => {
                                let closed = matches!(frame, Message::Close(_));
                                let bytes = u32::try_from(frame.len()).map_err(|_| "gateway frame too large")?;
                                let permit = budget.clone().try_acquire_many_owned(bytes)
                                    .map_err(|_| "gateway background buffer exceeded its byte limit")?;
                                inbound.try_send((frame, permit))
                                    .map_err(|_| "gateway background buffer exceeded its frame limit")?;
                                if closed { return Ok(()); }
                            }
                        }
                    }
                }
            }
        }.await;
        if let Err(error) = &result {
            tracing::warn!(%error, "background gateway transport stopped");
        }
        if finished.send(result).is_err() {
            tracing::debug!("background gateway consumer already closed");
        }
    });
    let writer = futures_util::sink::unfold(outbound, |outbound, frame| async move {
        let (reply, done) = oneshot::channel();
        outbound
            .send((frame, reply))
            .await
            .map_err(|_| io_error("gateway transport closed"))?;
        done.await
            .map_err(|_| io_error("gateway transport stopped"))?
            .map_err(io_error)?;
        Ok::<_, Error>(outbound)
    });
    let reader = futures_util::stream::unfold(
        (frames, Some(completion)),
        |(mut frames, mut completion)| async move {
            if let Some((frame, _permit)) = frames.recv().await {
                return Some((Ok(frame), (frames, completion)));
            }
            if let Some(done) = completion.take()
                && let Ok(Err(error)) = done.await
            {
                return Some((Err(io_error(error)), (frames, completion)));
            }
            None
        },
    );
    (Box::pin(writer), Box::pin(reader), Transport(task))
}

fn io_error(error: impl Into<String>) -> Error {
    Error::Io(std::io::Error::other(error.into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::SinkExt;
    use tokio_tungstenite::{WebSocketStream, tungstenite::protocol::Role};

    async fn sockets() -> (
        WebSocketStream<tokio::io::DuplexStream>,
        WebSocketStream<tokio::io::DuplexStream>,
    ) {
        let (client, server) = tokio::io::duplex(4096);
        let client = WebSocketStream::from_raw_socket(client, Role::Client, None).await;
        let server = WebSocketStream::from_raw_socket(server, Role::Server, None).await;
        (client, server)
    }

    #[tokio::test]
    async fn heartbeats_and_pongs_continue_without_polling_the_ui_reader() {
        let (client, mut server) = sockets().await;
        let (sink, reader) = client.split();
        let (_writer, mut ui, _transport) =
            start_with_interval(sink, reader, Duration::from_millis(10));
        server
            .send(Message::Text("queued update".into()))
            .await
            .unwrap();
        server.send(Message::Ping(vec![42])).await.unwrap();
        let mut probes = 0;
        let mut pong = false;
        tokio::time::timeout(Duration::from_secs(1), async {
            while probes < 4 || !pong {
                match server.next().await.unwrap().unwrap() {
                    Message::Ping(payload) => {
                        probes += 1;
                        server.send(Message::Pong(payload)).await.unwrap();
                    }
                    Message::Pong(payload) => pong |= payload == [42],
                    _ => {}
                }
            }
        })
        .await
        .expect("native heartbeat runs while the UI is idle");
        assert_eq!(
            ui.next().await.unwrap().unwrap(),
            Message::Text("queued update".into())
        );
    }

    #[tokio::test]
    async fn preserves_commands_and_close_reasons_and_releases_the_socket_on_drop() {
        let (client, mut server) = sockets().await;
        let (sink, reader) = client.split();
        let (mut writer, mut ui, transport) = start(sink, reader);
        writer
            .send(Message::Text("LeaveVoice".into()))
            .await
            .unwrap();
        loop {
            if let Message::Text(text) = server.next().await.unwrap().unwrap() {
                assert_eq!(text, "LeaveVoice");
                break;
            }
        }
        server
            .send(Message::Close(Some(
                tokio_tungstenite::tungstenite::protocol::CloseFrame {
                    code:
                        tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode::Normal,
                    reason: "host shutting down".into(),
                },
            )))
            .await
            .unwrap();
        assert!(
            matches!(ui.next().await.unwrap().unwrap(), Message::Close(Some(frame)) if frame.reason == "host shutting down")
        );
        drop(transport);
        tokio::time::timeout(
            Duration::from_secs(1),
            writer.send(Message::Text("late".into())),
        )
        .await
        .unwrap()
        .expect_err("commands are not replayed after closure");
    }

    #[tokio::test]
    async fn oversized_frames_cannot_escape_the_byte_budget() {
        let sink = futures_util::sink::drain::<Message>();
        let reader = futures_util::stream::iter([Ok(Message::Binary(vec![0; QUEUED_BYTES + 1]))]);
        let (_writer, mut ui, _transport) = start(sink, reader);
        let error = tokio::time::timeout(Duration::from_secs(1), ui.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert!(error.to_string().contains("byte limit"));
    }

    #[tokio::test]
    async fn dropping_an_active_transport_releases_its_socket() {
        let (client, mut server) = sockets().await;
        let (sink, reader) = client.split();
        let (_writer, _ui, transport) = start(sink, reader);
        drop(transport);
        tokio::time::timeout(Duration::from_secs(1), async {
            while let Some(Ok(_)) = server.next().await {}
        })
        .await
        .expect("socket is released without a UI poll or a close response");
    }

    #[tokio::test]
    async fn a_suspended_consumer_has_a_bounded_frame_buffer() {
        let (client, mut server) = sockets().await;
        let (sink, reader) = client.split();
        let (_writer, mut ui, mut transport) = start(sink, reader);
        let producer = tokio::spawn(async move {
            for _ in 0..QUEUED_FRAMES + 1 {
                server.send(Message::Text("update".into())).await.unwrap();
            }
            std::future::pending::<()>().await;
        });
        tokio::time::timeout(Duration::from_secs(1), &mut transport.0)
            .await
            .unwrap()
            .unwrap();
        let mut count = 0;
        tokio::time::timeout(Duration::from_secs(1), async {
            while let Some(frame) = ui.next().await {
                match frame {
                    Ok(_) => count += 1,
                    Err(error) => {
                        assert!(error.to_string().contains("frame limit"));
                        break;
                    }
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(count, QUEUED_FRAMES);
        producer.abort();
    }
}
