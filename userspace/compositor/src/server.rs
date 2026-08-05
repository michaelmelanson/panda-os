//! The compositor process: client connections and the frame loop.

use alloc::vec::Vec;
use compositor_protocol::{Event, FORMAT_BGRA8888, MAX_FRAME_SIZE, Request};
use libpanda::scheme::SchemeProvider;
use libpanda::{buffer, environment, ipc::Channel};
use panda_abi::ErrorCode;
use panda_abi::scheme_protocol::Request as SchemeRequest;

use crate::display::Framebuffer;
use crate::manager::{Attachment, WindowManager};

/// The name clients connect to over `environment::connect` to reach this
/// compositor — see [`Compositor::serve_connects`].
pub const SCHEME_NAME: &str = "compositor";

/// Frame interval in milliseconds (~60 fps), as in the kernel compositor.
pub const REFRESH_INTERVAL_MS: u64 = 16;

/// A connected client and the windows it owns.
struct Client {
    channel: Channel,
    windows: Vec<u64>,
    /// Next attach sequence number per window.
    buffer_ids: Vec<(u64, u64)>,
}

impl Client {
    fn new(channel: Channel) -> Self {
        Self {
            channel,
            windows: Vec::new(),
            buffer_ids: Vec::new(),
        }
    }

    /// Attach sequence numbers are per window; the client counts the same
    /// sequence, which is how `BufferReleased` names a buffer.
    fn next_buffer_id(&mut self, window: u64) -> u64 {
        match self.buffer_ids.iter_mut().find(|(id, _)| *id == window) {
            Some((_, next)) => {
                let id = *next;
                *next += 1;
                id
            }
            None => {
                self.buffer_ids.push((window, 1));
                0
            }
        }
    }

    fn send(&self, event: Event<'_>) {
        let mut frame = [0u8; MAX_FRAME_SIZE];
        let Some(len) = event.encode(&mut frame) else {
            environment::log("compositor: could not encode an event");
            return;
        };
        // A client that has stopped reading must not stall the frame loop,
        // so events are dropped rather than blocking on a full queue.
        let _ = self.channel.try_send(&frame[..len]);
    }
}

/// The compositor service.
pub struct Compositor {
    manager: WindowManager<Framebuffer>,
    clients: Vec<Client>,
    /// This compositor's own endpoint of the `compositor:` scheme
    /// registration, if it managed to register one. `None` for a test
    /// compositor that only ever gets clients handed to it directly (e.g.
    /// via `add_client(Channel::parent())`) and doesn't care about scheme
    /// discovery — registration failure there shouldn't be fatal.
    provider: Option<SchemeProvider>,
}

impl Compositor {
    /// Claim the display, register the `compositor:` scheme, and start
    /// serving.
    ///
    /// If the display is unavailable the compositor still runs: it serves
    /// windows and skips presentation (Risk 1 of the plan). Until the
    /// kernel compositor is deleted in Phase 5 its permanent claim makes
    /// `Busy` the *normal* outcome here, so failing to open must not be
    /// fatal and must not be retried in a loop.
    ///
    /// Scheme registration failing (e.g. `AlreadyExists` if two compositors
    /// somehow start in the same kernel instance) is logged but likewise
    /// not fatal: a compositor with no scheme provider still works for
    /// clients handed to it directly, which is how every current test
    /// stands one up.
    pub fn new() -> Self {
        let target = match Framebuffer::open() {
            Ok(framebuffer) => {
                environment::log("compositor: claimed the display");
                Some(framebuffer)
            }
            Err(ErrorCode::Busy) => {
                environment::log("compositor: display is busy, running without output");
                None
            }
            Err(_) => {
                environment::log("compositor: could not open the display, running without output");
                None
            }
        };

        let provider = match SchemeProvider::register(SCHEME_NAME) {
            Ok(provider) => {
                environment::log("compositor: registered the compositor: scheme");
                Some(provider)
            }
            Err(_) => {
                environment::log("compositor: could not register the compositor: scheme");
                None
            }
        };

        Self {
            manager: WindowManager::new(target),
            clients: Vec::new(),
            provider,
        }
    }

    /// Serve pending `Request::Connect` calls against the `compositor:`
    /// scheme: each one gets a fresh channel pair, one half added as a new
    /// client, the other handed back to the caller as its `ConnectOk`
    /// attachment (see `libpanda::scheme::SchemeProvider::reply_connect_ok`
    /// and `panda_abi::scheme_protocol::Request::Connect`'s doc comment).
    ///
    /// This is how a process gets a channel to the compositor without being
    /// spawned by it — e.g. `init` spawns the compositor and the terminal as
    /// independent siblings, and the terminal reaches the compositor with
    /// `environment::connect("compositor:/connect")`. Any other
    /// scheme-provider request kind (`Open`/`Readdir`/`Read`/`Write`) has no
    /// meaning for this scheme and is answered with an error rather than
    /// left to hang the caller.
    fn serve_connects(&mut self) {
        // Taken out (not just borrowed) for the duration of the loop:
        // handling a `Connect` needs `&mut self` (to `add_client`) while
        // also needing the provider, and the two can't be borrowed at once
        // from `self.provider`. Put back before returning either way.
        let Some(provider) = self.provider.take() else {
            return;
        };

        let mut buf = [0u8; panda_abi::MAX_MESSAGE_SIZE];
        loop {
            let request = match provider.try_recv(&mut buf) {
                Ok(Some(request)) => request,
                Ok(None) => break,
                Err(_) => break,
            };

            match request {
                SchemeRequest::Connect { request_id, .. } => match libpanda::ipc::create_pair() {
                    Ok((server_handle, client_handle)) => {
                        let client_channel = Channel::from_typed(client_handle);
                        if provider.reply_connect_ok(request_id, &client_channel).is_err() {
                            environment::log("compositor: could not reply to a Connect request");
                            continue;
                        }
                        // `client_channel` drops here, closing this
                        // process's own copy of that endpoint — the
                        // connecting process keeps its own duplicate,
                        // installed by the kernel when it received the
                        // attachment (see docs/IPC.md "Handle transfer").
                        self.add_client(Channel::from_typed(server_handle));
                    }
                    Err(_) => {
                        environment::log(
                            "compositor: could not create a channel pair for a Connect request",
                        );
                        let _ = provider.reply_connect_err(request_id, ErrorCode::IoError);
                    }
                },
                SchemeRequest::Open { request_id, .. } => {
                    let _ = provider.reply_open_err(request_id, ErrorCode::NotSupported);
                }
                SchemeRequest::Readdir { request_id, .. } => {
                    let _ = provider.reply_readdir_err(request_id, ErrorCode::NotSupported);
                }
                SchemeRequest::Read { request_id, .. } => {
                    let _ = provider.reply_read_err(request_id, ErrorCode::NotSupported);
                }
                SchemeRequest::Write { request_id, .. } => {
                    let _ = provider.reply_write_err(request_id, ErrorCode::NotSupported);
                }
                SchemeRequest::Close { request_id, .. } => {
                    let _ = provider.reply_close_ok(request_id);
                }
            }
        }

        self.provider = Some(provider);
    }

    /// Accept a client connection and greet it with `DisplayFormats`.
    pub fn add_client(&mut self, channel: Channel) {
        let (width, height) = self.manager.screen_size();
        let client = Client::new(channel);
        client.send(Event::DisplayFormats {
            width,
            height,
            formats: &[FORMAT_BGRA8888],
        });
        self.clients.push(client);
    }

    /// Drain every pending request from every client.
    fn serve_clients(&mut self) {
        let mut frame = [0u8; MAX_FRAME_SIZE];

        for index in 0..self.clients.len() {
            loop {
                let received = self.clients[index].channel.try_recv_with_handle(&mut frame);
                let Ok(Some((len, attached))) = received else {
                    break;
                };

                let Some(request) = Request::decode(&frame[..len]) else {
                    environment::log("compositor: dropping a malformed request");
                    continue;
                };

                let attachment = match request {
                    Request::AttachBuffer {
                        window,
                        width,
                        height,
                        format,
                    } => map_attachment(&mut self.clients[index], window, width, height, format, attached),
                    _ => None,
                };

                let events = self.manager.handle_request(request, attachment);
                for event in events {
                    if let Event::WindowCreated { window } = event {
                        self.clients[index].windows.push(window);
                    }
                    if let Event::Closed { window } = event {
                        self.clients[index].windows.retain(|&w| w != window);
                    }
                    self.clients[index].send(event);
                }
            }
        }
    }

    /// Composite one frame and report the commits it consumed.
    fn tick(&mut self) {
        for event in self.manager.tick() {
            let window = match event {
                Event::FrameDone { window, .. } => window,
                _ => continue,
            };
            if let Some(client) = self
                .clients
                .iter()
                .find(|client| client.windows.contains(&window))
            {
                client.send(event);
            }
        }
    }

    /// Run the frame loop, ticking forever when `ticks` is `None`.
    pub fn run(&mut self, ticks: Option<u64>) {
        let mut remaining = ticks;
        loop {
            self.serve_connects();
            self.serve_clients();
            self.tick();

            if let Some(left) = remaining.as_mut() {
                if *left == 0 {
                    return;
                }
                *left -= 1;
            }

            libpanda::process::sleep(REFRESH_INTERVAL_MS);
        }
    }
}

/// Map a client's attached buffer handle into this process and validate it
/// against the geometry the client declared.
fn map_attachment(
    client: &mut Client,
    window: u64,
    width: u32,
    height: u32,
    format: u8,
    attached: Option<libpanda::Handle>,
) -> Option<Attachment> {
    let Some(handle) = attached else {
        environment::log("compositor: AttachBuffer arrived without a buffer handle");
        return None;
    };

    let Ok(address) = buffer::map(handle) else {
        environment::log("compositor: could not map an attached buffer");
        return None;
    };

    // The client declares the geometry; the kernel guarantees the mapping
    // is at least as large as the buffer, so validating against the
    // declared size is the check that matters (Risk 2 of the plan).
    let len = (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(4))?;

    let id = client.next_buffer_id(window);
    // SAFETY: `address` was just returned by a successful OP_BUFFER_MAP for
    // this process, and the compositor holds `handle` for as long as the
    // attachment lives, so the frames stay alive.
    let attachment = unsafe { Attachment::new(id, address as *mut u8, len, width, height, format) };
    if attachment.is_none() {
        environment::log("compositor: rejecting an invalid buffer attachment");
    }
    attachment
}

/// Run the compositor: claim the display, register the `compositor:`
/// scheme, add `Channel::parent()` as a client if this process has one, and
/// enter the frame loop.
///
/// The `Channel::parent()` client exists for tests that spawn a compositor
/// directly and want a channel to it without going through scheme discovery
/// (see `compositor_test_child` and `compositor_start_test`) — production
/// (`init` spawning the compositor) doesn't hand it any window client this
/// way; graphical clients like the terminal connect independently via
/// `environment::connect("compositor:/connect")`, served by
/// [`Compositor::serve_connects`].
///
/// `ticks` bounds the loop for tests; production passes `None`.
pub fn run(ticks: Option<u64>) {
    environment::log("compositor: starting");

    let mut compositor = Compositor::new();
    if let Some(parent) = Channel::parent() {
        compositor.add_client(parent);
    }

    environment::log("compositor: entering the frame loop");
    compositor.run(ticks);
    environment::log("compositor: frame loop finished");
}
