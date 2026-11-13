use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use futures_util::{Sink, SinkExt, Stream, StreamExt};
use tokio::sync::{Semaphore, mpsc, oneshot, watch};
use tokio_tungstenite::tungstenite::{Error, Message};

type Writer = Pin<Box<dyn Sink<Message, Error = Error> + Send>>;
type Reader = Pin<Box<dyn Stream<Item = Result<Message, Error>> + Send>>;
type Command = (Message, oneshot::Sender<Result<(), String>>);
/// An answer is one round trip away; the server also probes this often by itself.
const RESUME_PROBE: Duration = crate::protocol::GATEWAY_HEARTBEAT_INTERVAL;
const SLEPT: &str = "the connection did not outlive the time in the background";
const UNANSWERED: &str = "the host did not answer after the app returned";
const QUEUED_FRAMES: usize = 128;
const MAX_FRAME_BYTES: usize = discordia_server::gateway::MAX_FRAME_BYTES;
// A full image+preview send frame may be larger; keep room for one normal media
// response alongside it without doubling the largest new frame's memory bound.
const QUEUED_BYTES: usize =
    MAX_FRAME_BYTES + discordia_server::protocol::MAX_GUILD_UPLOAD_DATA_LEN + 16_384;

const GOODBYE: Duration = Duration::from_millis(300);
static OPEN: AtomicUsize = AtomicUsize::new(0);

fn ending() -> &'static watch::Sender<bool> {
    static ENDING: OnceLock<watch::Sender<bool>> = OnceLock::new();
    ENDING.get_or_init(|| watch::channel(false).0)
}

struct Open;

impl Open {
    fn new() -> Self {
        OPEN.fetch_add(1, Ordering::SeqCst);
        Self
    }
}

impl Drop for Open {
    fn drop(&mut self) {
        OPEN.fetch_sub(1, Ordering::SeqCst);
    }
}

/// A killed process sends nothing over QUIC, and the server would keep its seat
/// for a whole heartbeat deadline. Blocks at most `patience`; never call it on a runtime thread.
pub(crate) fn say_goodbye(patience: Duration) {
    ending().send_replace(true);
    if OPEN.load(Ordering::SeqCst) == 0 {
        return;
    }
    let deadline = std::time::Instant::now() + patience;
    while OPEN.load(Ordering::SeqCst) > 0 && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    // The close frame is queued by now; the QUIC driver still has to put it on the wire.
    std::thread::sleep(Duration::from_millis(50));
}

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
    start_with(
        sink,
        reader,
        crate::protocol::GATEWAY_HEARTBEAT_INTERVAL,
        Signals {
            clock: super::wake::now,
            resumes: super::wake::Resumes::subscribe(),
            ending: ending().subscribe(),
        },
    )
}

struct Signals {
    clock: super::wake::Clock,
    resumes: super::wake::Resumes,
    ending: watch::Receiver<bool>,
}

async fn process_ending(ending: &mut watch::Receiver<bool>) {
    if ending.wait_for(|ending| *ending).await.is_err() {
        std::future::pending::<()>().await;
    }
}

async fn until(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

fn start_with<S, R>(
    mut sink: S,
    mut reader: R,
    interval: Duration,
    signals: Signals,
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
    let Signals {
        clock,
        mut resumes,
        mut ending,
    } = signals;
    // Counted before the task first runs, so a goodbye in between still waits for it.
    let open = Open::new();
    // WebView edit acknowledgements can stop scoped tasks while Android hides the app.
    let task = tokio::spawn(async move {
        super::gateway_diagnostic(format_args!("native transport started"));
        let result: Result<(), String> = async {
            let mut heartbeat = tokio::time::interval(interval);
            heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            let timeout = crate::protocol::GATEWAY_HEARTBEAT_TIMEOUT;
            let mut received = clock();
            // The server hangs up on our silence, not on its own, and on a clock that never slept.
            let mut spoke = clock();
            let slept = |spoke| super::wake::overdue(clock, spoke, timeout);
            let mut probe = None::<(tokio::time::Instant, [u8; 8])>;
            let mut asked = 0_u64;
            let _open = open;
            loop {
                tokio::select! {
                    _ = inbound.closed() => return Ok(()),
                    _ = process_ending(&mut ending) => {
                        let close = sink.send(Message::Close(None));
                        if !matches!(tokio::time::timeout(GOODBYE, close).await, Ok(Ok(()))) {
                            tracing::debug!("gateway closed before the process could say so");
                        }
                        return Ok(());
                    }
                    _ = resumes.next() => {
                        if slept(spoke) {
                            return Err(SLEPT.into());
                        }
                        if probe.is_none() {
                            // Frames that queued up while the app slept say nothing about now;
                            // only the pong carrying this payload does.
                            asked += 1;
                            let question = asked.to_be_bytes();
                            super::gateway_send(&mut sink, &mut reader, Message::Ping(question.to_vec()), "resume probe").await?;
                            spoke = clock();
                            probe = Some((tokio::time::Instant::now() + RESUME_PROBE, question));
                        }
                    }
                    _ = until(probe.map(|(deadline, _)| deadline)) => return Err(UNANSWERED.into()),
                    command = commands.recv() => {
                        let Some((frame, reply)) = command else { return Ok(()) };
                        super::gateway_diagnostic(format_args!("native command bytes={}", frame.len()));
                        let result = super::gateway_send(&mut sink, &mut reader, frame, "background command").await;
                        let failed = result.as_ref().err().cloned();
                        if failed.is_none() {
                            spoke = clock();
                        }
                        if reply.send(result).is_err() {
                            return Ok(());
                        }
                        if let Some(error) = failed { return Err(error); }
                    }
                    _ = heartbeat.tick() => {
                        if slept(spoke) {
                            return Err(SLEPT.into());
                        }
                        if super::wake::overdue(clock, received, timeout) {
                            return Err("the host stopped answering heartbeat probes".into());
                        }
                        super::gateway_send(&mut sink, &mut reader, Message::Ping(Vec::new()), "heartbeat").await?;
                        spoke = clock();
                    }
                    frame = reader.next() => {
                        let Some(frame) = frame else { return Ok(()) };
                        let frame = frame.map_err(|error| format!("recv: {error}"))?;
                        super::gateway_diagnostic(format_args!("native receive bytes={} text={} ping={} pong={}", frame.len(), frame.is_text(), frame.is_ping(), frame.is_pong()));
                        // What queued up while the app slept must not pass for a server still listening.
                        if slept(spoke) && !frame.is_close() {
                            return Err(SLEPT.into());
                        }
                        received = clock();
                        match frame {
                            Message::Ping(payload) => {
                                super::gateway_send(&mut sink, &mut reader, Message::Pong(payload), "pong").await?;
                                spoke = clock();
                            }
                            Message::Pong(answer) => {
                                if probe.is_some_and(|(_, question)| answer[..] == question[..]) {
                                    probe = None;
                                }
                            }
                            frame => {
                                let closed = matches!(frame, Message::Close(_));
                                if frame.len() > MAX_FRAME_BYTES {
                                    return Err("gateway frame exceeded its byte limit".into());
                                }
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
        super::gateway_diagnostic(format_args!(
            "native transport stopped ok={}",
            result.is_ok()
        ));
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

    thread_local! {
        static NOW: std::cell::Cell<Duration> = const { std::cell::Cell::new(Duration::ZERO) };
    }

    fn fake_now() -> Duration {
        NOW.get()
    }

    fn never<T: Default>() -> watch::Receiver<T> {
        watch::channel(T::default()).1
    }

    fn quiet() -> Signals {
        Signals {
            clock: super::super::wake::now,
            resumes: super::super::wake::Resumes::from(never()),
            ending: never(),
        }
    }

    fn start_quiet<S, R>(sink: S, reader: R) -> (Writer, Reader, Transport)
    where
        S: Sink<Message> + Unpin + Send + 'static,
        S::Error: std::fmt::Display + Send,
        R: Stream<Item = Result<Message, Error>> + Unpin + Send + 'static,
    {
        start_with(
            sink,
            reader,
            crate::protocol::GATEWAY_HEARTBEAT_INTERVAL,
            quiet(),
        )
    }

    /// A transport whose heartbeat stays out of the way, a clock the test moves and
    /// the sender that says the app came back.
    fn resumable<S, R>(sink: S, reader: R) -> (Reader, (Writer, Transport), watch::Sender<u64>)
    where
        S: Sink<Message> + Unpin + Send + 'static,
        S::Error: std::fmt::Display + Send,
        R: Stream<Item = Result<Message, Error>> + Unpin + Send + 'static,
    {
        NOW.set(Duration::ZERO);
        let (resume, resumes) = watch::channel(0);
        let signals = Signals {
            clock: fake_now,
            resumes: super::super::wake::Resumes::from(resumes),
            ending: never(),
        };
        let (writer, ui, transport) = start_with(sink, reader, Duration::from_secs(3600), signals);
        (ui, (writer, transport), resume)
    }

    async fn stopped_with(ui: &mut Reader) -> String {
        loop {
            match ui.next().await {
                Some(Ok(_)) => {}
                Some(Err(error)) => return error.to_string(),
                None => return "ended without an error".into(),
            }
        }
    }

    #[tokio::test]
    async fn a_return_after_the_servers_deadline_redials_without_probing() {
        let (client, mut server) = sockets().await;
        let (sink, reader) = client.split();
        let (mut ui, _transport, resume) = resumable(sink, reader);
        // The interval's first tick is immediate; wait for its ping so the clock moves after it.
        assert!(server.next().await.unwrap().unwrap().is_ping());
        NOW.set(crate::protocol::GATEWAY_HEARTBEAT_TIMEOUT);
        resume.send_replace(1);
        let error = tokio::time::timeout(Duration::from_secs(1), stopped_with(&mut ui))
            .await
            .expect("a slept-through deadline needs no probe to be believed");
        assert!(error.contains(SLEPT), "{error}");
    }

    #[tokio::test]
    async fn what_queued_up_asleep_does_not_hide_the_sleep() {
        let (client, mut server) = sockets().await;
        let (sink, reader) = client.split();
        let (mut ui, _transport, _resume) = resumable(sink, reader);
        assert!(server.next().await.unwrap().unwrap().is_ping());
        NOW.set(crate::protocol::GATEWAY_HEARTBEAT_TIMEOUT);
        server.send(Message::Ping(Vec::new())).await.unwrap();
        server.send(Message::Text("old news".into())).await.unwrap();
        let error = tokio::time::timeout(Duration::from_secs(1), stopped_with(&mut ui))
            .await
            .expect("the sleep is noticed with no resume signal at all");
        assert!(error.contains(SLEPT), "{error}");
    }

    #[tokio::test]
    async fn a_close_that_waited_through_the_sleep_still_gives_its_reason() {
        let (client, mut server) = sockets().await;
        let (sink, reader) = client.split();
        let (mut ui, _transport, _resume) = resumable(sink, reader);
        assert!(server.next().await.unwrap().unwrap().is_ping());
        NOW.set(crate::protocol::GATEWAY_HEARTBEAT_TIMEOUT);
        let reason = tokio_tungstenite::tungstenite::protocol::CloseFrame {
            code: tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode::Away,
            reason: "host shutting down".into(),
        };
        server.send(Message::Close(Some(reason))).await.unwrap();
        let frame = tokio::time::timeout(Duration::from_secs(1), ui.next())
            .await
            .unwrap();
        assert!(
            matches!(frame, Some(Ok(Message::Close(Some(frame)))) if frame.reason == "host shutting down")
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_short_absence_probes_and_an_answer_keeps_the_session() {
        let (client, mut server) = sockets().await;
        let (sink, reader) = client.split();
        let (_ui, (_writer, transport), resume) = resumable(sink, reader);
        assert!(server.next().await.unwrap().unwrap().is_ping());
        NOW.set(crate::protocol::GATEWAY_HEARTBEAT_TIMEOUT - Duration::from_secs(1));
        resume.send_replace(1);
        assert!(server.next().await.unwrap().unwrap().is_ping());
        server.flush().await.unwrap();
        tokio::time::sleep(RESUME_PROBE * 3).await;
        assert!(!transport.0.is_finished(), "an answered probe is forgotten");
    }

    #[tokio::test(start_paused = true)]
    async fn a_probe_nobody_answers_ends_the_session_within_the_probe_deadline() {
        let (client, mut server) = sockets().await;
        let (sink, reader) = client.split();
        let (mut ui, _transport, resume) = resumable(sink, reader);
        assert!(server.next().await.unwrap().unwrap().is_ping());
        let asked = tokio::time::Instant::now();
        resume.send_replace(1);
        // Reading the probe sends the pong still owed for the first heartbeat, and
        // leaves the probe's own unsent: an old answer must not pass for a new one.
        assert!(server.next().await.unwrap().unwrap().is_ping());
        // A second return while the probe is out must not buy the dead socket more time.
        resume.send_replace(2);
        let error = stopped_with(&mut ui).await;
        assert!(error.contains(UNANSWERED), "{error}");
        assert!(asked.elapsed() <= RESUME_PROBE + Duration::from_millis(50));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_process_that_ends_on_purpose_closes_its_gateway_first() {
        let (client, mut server) = sockets().await;
        let (sink, reader) = client.split();
        let (_writer, _ui, _transport) = start(sink, reader);
        let goodbye = tokio::task::spawn_blocking(|| say_goodbye(Duration::from_secs(2)));
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Message::Close(_) = server.next().await.unwrap().unwrap() {
                    break;
                }
            }
        })
        .await
        .expect("the server hears the close before the process is gone");
        goodbye.await.unwrap();
    }

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
            start_with(sink, reader, Duration::from_millis(10), quiet());
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
        let (mut writer, mut ui, transport) = start_quiet(sink, reader);
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
    async fn legal_media_batches_larger_than_eight_mib_preserve_following_updates() {
        let data = format!(
            "data:image/png;base64,{}",
            "A".repeat(crate::protocol::MAX_IMAGE_BYTES / 3 * 4)
        );
        let blobs = (0..3)
            .map(|index| crate::protocol::EmojiBlob {
                image: format!("{index:064x}.png"),
                data_url: data.clone(),
            })
            .collect();
        let raw =
            serde_json::to_string(&crate::protocol::ServerMessage::EmojiBlobs { blobs }).unwrap();
        assert!(raw.len() > 8 * 1024 * 1024);
        assert!(raw.len() <= discordia_server::gateway::MAX_FRAME_BYTES);
        let frame = Message::Text(raw);
        let next = Message::Text("following chat update".into());
        let sink = futures_util::sink::drain::<Message>();
        let reader = futures_util::stream::iter([Ok(frame.clone()), Ok(next.clone())]);
        let (_writer, mut ui, _transport) = start_quiet(sink, reader);
        tokio::time::timeout(Duration::from_secs(5), async {
            assert_eq!(
                ui.next()
                    .await
                    .unwrap()
                    .expect("legal media batch must fit"),
                frame
            );
            assert_eq!(ui.next().await.unwrap().unwrap(), next);
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn queued_legal_frames_still_have_a_bounded_byte_budget() {
        let sink = futures_util::sink::drain::<Message>();
        let normal_frame_bytes = discordia_server::protocol::MAX_GUILD_UPLOAD_DATA_LEN + 16_384;
        let reader = futures_util::stream::iter([
            Ok(Message::Binary(vec![0; MAX_FRAME_BYTES])),
            Ok(Message::Binary(vec![0; normal_frame_bytes])),
            Ok(Message::Text("overflow".into())),
        ]);
        let (_writer, mut ui, mut transport) = start_quiet(sink, reader);
        tokio::time::timeout(Duration::from_secs(5), &mut transport.0)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(ui.next().await.unwrap().unwrap().len(), MAX_FRAME_BYTES);
        assert_eq!(ui.next().await.unwrap().unwrap().len(), normal_frame_bytes);
        assert!(
            ui.next()
                .await
                .unwrap()
                .unwrap_err()
                .to_string()
                .contains("background buffer exceeded its byte limit")
        );
    }

    #[tokio::test]
    async fn oversized_frames_cannot_escape_the_byte_budget() {
        let sink = futures_util::sink::drain::<Message>();
        let reader =
            futures_util::stream::iter([Ok(Message::Binary(vec![0; MAX_FRAME_BYTES + 1]))]);
        let (_writer, mut ui, _transport) = start_quiet(sink, reader);
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
        let (_writer, _ui, transport) = start_quiet(sink, reader);
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
        let (_writer, mut ui, mut transport) = start_quiet(sink, reader);
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
