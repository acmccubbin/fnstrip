//! Production built-in renderer for the first private Touch Bar cutover.

use std::collections::BTreeMap;
use std::fmt;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::path::Path;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use t1_platform::diagnostics::{Component, Outcome, Record, Stage, emit};
use t1_platform::frame_memfd::RendererFrame;
use t1_platform::seqpacket::{self, SeqPacketClient, SeqPacketError};
use t1_touchbar_hw::digitizer::DisplayDimensions;
use t1_touchbar_hw::packet::MAX_PACKET_LENGTH;
use t1_touchbar_hw::wire::{
    self, AncillaryMetadata, ClientMessage, Envelope, ErrorCode, FeatureBits, Hello, HelloAck,
    InputContact, KeyCode, RegisterBuffer, ServiceMessage, StepDirection as WireStepDirection,
    SubmitFrame, TapKeys, WireInputFrame,
};

use crate::brightness::{self, Light};
use crate::config::Config;
use crate::desktop_provider::{DesktopAction, DesktopCapabilities, DesktopProvider, DesktopState};
use crate::extras::{Extras, LiveFaces};
use crate::framebuffer::Xrgb8888Frame;
use crate::gesture::{DisplayContact, GestureEngine, GestureSettings};
use crate::intro::Intro;
use crate::media::MediaWatch;
use crate::overlay_state::{OverlayState, OverlayWatcher};
use crate::screensaver::{Screensaver, Zone};
use crate::stock_slice::{
    FunctionKey, Layer, SliderKind, SliderSpot, StepDirection, StockAction, StockBar,
    StockCapabilities, StockLevels,
};
use crate::theme::Theme;
use crate::touch_id_overlay::TouchIdOverlay;

const BUFFER_ID: u32 = 1;
const FIRST_FRAME_ID: u64 = 1;
const MAX_PENDING_REQUESTS: usize = 64;
const SEND_ATTEMPTS: usize = 64;
const SEND_RETRY_DELAY: Duration = Duration::from_millis(2);
const RECEIVE_RETRY_DELAY: Duration = Duration::from_millis(5);
const INITIAL_RECONNECT_DELAY: Duration = Duration::from_millis(50);
const MAX_RECONNECT_DELAY: Duration = Duration::from_secs(2);
const VOLUME_STEP_PERCENT: u8 = 6;
/// The keyboard backlight's level when its icon turns it on with no earlier level to restore.
const KEYBOARD_ON_DEFAULT: u8 = 50;
/// Brightness and keyboard backlight levels are re-read this often (the keyboard's keys
/// change them too). Levels read or reported this soon after a slider set one are ignored
/// (the set lands late, and the reports lag a drag).
const LIGHT_POLL_INTERVAL: Duration = Duration::from_millis(500);
const LIGHT_SETTLE: Duration = Duration::from_secs(1);
const HOLD_REPEAT_DELAY: Duration = Duration::from_millis(350);
const HOLD_REPEAT_INTERVAL: Duration = Duration::from_millis(90);
const OVERLAY_FADE_DURATION: Duration = Duration::from_millis(150);
const ENROLLMENT_PROGRESS_DURATION: Duration = Duration::from_millis(220);
/// Double-tap Fn opens the extras layer, which stays until Fn is pressed again: the first
/// Fn press must be a short untouched tap, and the second press must start within the gap
/// after it.
const FN_TAP_MAX: Duration = Duration::from_millis(300);
const FN_DOUBLE_TAP_GAP: Duration = Duration::from_millis(300);
/// Live extras faces (camera picture, mic spectrum) redraw at most this often (~25 fps:
/// enough to show they're working).
const LIVE_FACE_INTERVAL: Duration = Duration::from_millis(40);
/// Stock frames are submitted at most this often (~40 fps); touches in between land in the
/// next frame, so dragging a slider doesn't queue a frame per input report.
const STOCK_FRAME_INTERVAL: Duration = Duration::from_millis(25);
/// While the screensaver animates, poll the hardware socket more often so a released
/// buffer is reused quickly (the socket is non-blocking and std has no poll).
const SCREENSAVER_POLL_DELAY: Duration = Duration::from_millis(1);
const MAX_SCREENSAVER_BUFFERS: u32 = 3;
/// The startup animation draws full frames into up to two buffers, uncapped: about 95 fps
/// on the test screen's full-frame scenes, against about 70 with one.
const INTRO_BUFFERS: usize = 2;

/// Static, redaction-safe failure from one hardware-service connection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BuiltinRendererError {
    ConnectionUnavailable,
    Transport,
    Protocol,
    Negotiation,
    Frame,
    Gesture,
    ServiceRejected,
}

impl fmt::Display for BuiltinRendererError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::ConnectionUnavailable => "Touch Bar hardware service is unavailable",
            Self::Transport => "Touch Bar hardware connection was lost",
            Self::Protocol => "Touch Bar hardware protocol failed",
            Self::Negotiation => "Touch Bar display negotiation failed",
            Self::Frame => "Touch Bar frame setup failed",
            Self::Gesture => "Touch Bar input sequence failed",
            Self::ServiceRejected => "Touch Bar hardware request was rejected",
        })
    }
}

impl std::error::Error for BuiltinRendererError {}

/// The config's settings each new session takes on.
#[derive(Clone, Copy)]
#[allow(clippy::struct_excessive_bools, reason = "independent on/off settings")]
struct Settings {
    function_row_first: bool,
    theme: Theme,
    fahrenheit: bool,
    debug_gestures: bool,
}

/// Runs the built-in renderer and reconnects forever with capped backoff.
pub fn run_forever(provider_path: Option<&Path>) {
    let provider = provider_path.and_then(DesktopProvider::start);
    let config = Config::load();
    let settings = Settings {
        function_row_first: config.function_row_first,
        theme: config.theme,
        fahrenheit: config.fahrenheit,
        debug_gestures: config.debug_gestures,
    };
    let extras = Extras::start(config.extras);
    // Once per start, not on every reconnect (such as after sleep).
    let mut intro_pending = config.startup_animation;
    let media = MediaWatch::start().map(Arc::new);
    let mut delay = INITIAL_RECONNECT_DELAY;
    loop {
        if let Err(error) = t1_platform::diagnostics::observe(
            t1_platform::diagnostics::Component::Renderer,
            t1_platform::diagnostics::Stage::RendererConnect,
            || {
                run_once(
                    provider.as_ref(),
                    &extras,
                    settings,
                    &mut intro_pending,
                    media.as_ref(),
                )
            },
        ) {
            eprintln!("t1-touchbar: {error}; reconnecting");
        }
        thread::sleep(delay);
        delay = next_reconnect_delay(delay);
    }
}

fn xrgb_frame(
    dimensions: DisplayDimensions,
    stride: u32,
    pixels: &mut [u8],
) -> Result<Xrgb8888Frame<'_>, BuiltinRendererError> {
    Xrgb8888Frame::new(dimensions.width(), dimensions.height(), stride, pixels)
        .map_err(|_| BuiltinRendererError::Frame)
}

fn next_reconnect_delay(delay: Duration) -> Duration {
    delay.saturating_mul(2).min(MAX_RECONNECT_DELAY)
}

fn run_once(
    provider: Option<&DesktopProvider>,
    extras: &Extras,
    settings: Settings,
    intro_pending: &mut bool,
    media: Option<&Arc<MediaWatch>>,
) -> Result<(), BuiltinRendererError> {
    let NegotiatedConnection {
        mut connection,
        request_ids,
        ack,
        features,
    } = negotiate_default_features()?;
    let dimensions = DisplayDimensions::new(ack.width, ack.height)
        .map_err(|_| BuiltinRendererError::Negotiation)?;
    let mut session = RendererSession::new(
        dimensions,
        request_ids,
        stock_capabilities(features, ack.minor),
    )?;
    session.set_function_row_first(settings.function_row_first);
    session.set_theme(settings.theme);
    session.live_faces.set_fahrenheit(settings.fahrenheit);
    session.input.set_debug_gestures(settings.debug_gestures);
    session.set_extras(extras.clone());
    session.media = media.cloned();
    session.max_buffers = ack.max_buffers;
    session.register(&connection)?;
    if std::mem::take(intro_pending) {
        session.intro = Some(Intro::new(dimensions.width(), dimensions.height()));
    }
    let overlay_watcher = OverlayWatcher::start();
    let clock = Instant::now();

    loop {
        crate::record::poll();
        session.advance(clock.elapsed(), &connection, provider)?;
        if let Some(state) = provider.and_then(DesktopProvider::take_latest) {
            session.set_desktop_state(state)?;
            session.submit_if_ready(&connection)?;
        }
        if let Some(observation) = overlay_watcher.take_latest() {
            let overlay = observation.overlay();
            session.set_overlay(
                overlay.map(|overlay| overlay.state),
                overlay.and_then(|overlay| overlay.enrollment_progress),
            );
            session.submit_if_ready(&connection)?;
        }
        session.set_extras_status(extras.status());
        session.sync_extras_visibility();
        session.submit_if_ready(&connection)?;
        if let Some(envelope) = connection.try_receive_service(Some(dimensions))? {
            session.handle_with_provider(envelope, &connection, provider)?;
        } else if session.screensaver.is_some() || session.intro.is_some() {
            thread::sleep(SCREENSAVER_POLL_DELAY);
        } else {
            thread::sleep(RECEIVE_RETRY_DELAY);
        }
    }
}

struct HardwareConnection {
    descriptor: OwnedFd,
    receive_buffer: Vec<u8>,
}

impl HardwareConnection {
    fn new(descriptor: OwnedFd) -> Self {
        Self {
            descriptor,
            receive_buffer: vec![0; MAX_PACKET_LENGTH],
        }
    }

    fn client(&self) -> SeqPacketClient<'_> {
        SeqPacketClient::new(self.descriptor.as_fd())
    }

    fn send_request(
        &self,
        envelope: &Envelope<ClientMessage>,
        dimensions: Option<DisplayDimensions>,
        descriptor: Option<BorrowedFd<'_>>,
    ) -> Result<(), BuiltinRendererError> {
        let packet = wire::encode_client(envelope, dimensions)
            .map_err(|_| BuiltinRendererError::Protocol)?;
        for _ in 0..SEND_ATTEMPTS {
            match self.client().send_with_fd(&packet, descriptor) {
                Ok(()) => return Ok(()),
                Err(SeqPacketError::WouldBlock | SeqPacketError::Interrupted) => {
                    thread::sleep(SEND_RETRY_DELAY);
                }
                Err(_) => return Err(BuiltinRendererError::Transport),
            }
        }
        Err(BuiltinRendererError::Transport)
    }

    fn receive_service(
        &mut self,
        dimensions: Option<DisplayDimensions>,
    ) -> Result<Envelope<ServiceMessage>, BuiltinRendererError> {
        loop {
            let client = SeqPacketClient::new(self.descriptor.as_fd());
            match client.receive(&mut self.receive_buffer) {
                Ok(length) => {
                    return wire::decode_service(
                        &self.receive_buffer[..length],
                        AncillaryMetadata::default(),
                        dimensions,
                    )
                    .map_err(|_| BuiltinRendererError::Protocol);
                }
                Err(SeqPacketError::WouldBlock | SeqPacketError::Interrupted) => {
                    thread::sleep(RECEIVE_RETRY_DELAY);
                }
                Err(_) => return Err(BuiltinRendererError::Transport),
            }
        }
    }

    fn try_receive_service(
        &mut self,
        dimensions: Option<DisplayDimensions>,
    ) -> Result<Option<Envelope<ServiceMessage>>, BuiltinRendererError> {
        let client = SeqPacketClient::new(self.descriptor.as_fd());
        match client.receive(&mut self.receive_buffer) {
            Ok(length) => wire::decode_service(
                &self.receive_buffer[..length],
                AncillaryMetadata::default(),
                dimensions,
            )
            .map(Some)
            .map_err(|_| BuiltinRendererError::Protocol),
            Err(SeqPacketError::WouldBlock | SeqPacketError::Interrupted) => Ok(None),
            Err(_) => Err(BuiltinRendererError::Transport),
        }
    }
}

struct NegotiatedConnection {
    connection: HardwareConnection,
    request_ids: RequestIds,
    ack: HelloAck,
    features: FeatureBits,
}

enum HelloOutcome {
    Accepted(NegotiatedConnection),
    Unsupported,
}

fn negotiate_default_features() -> Result<NegotiatedConnection, BuiltinRendererError> {
    negotiate_default_features_with(|features| {
        Ok(match connect_and_hello(features)? {
            HelloOutcome::Accepted(connection) => Some(connection),
            HelloOutcome::Unsupported => None,
        })
    })?
    .ok_or(BuiltinRendererError::Negotiation)
}

fn negotiate_default_features_with<Connection, Error>(
    mut connect: impl FnMut(FeatureBits) -> Result<Option<Connection>, Error>,
) -> Result<Option<Connection>, Error> {
    let base = FeatureBits::INITIAL_SERVICE | FeatureBits::TOUCH_ID_CANCELLATION;
    let optional = FeatureBits::DISPLAY_BRIGHTNESS | FeatureBits::KEYBOARD_BACKLIGHT;
    if let Some(connection) = connect(base | optional)? {
        return Ok(Some(connection));
    }

    let mut supported = base;
    for feature in [
        FeatureBits::DISPLAY_BRIGHTNESS,
        FeatureBits::KEYBOARD_BACKLIGHT,
    ] {
        if connect(base | feature)?.is_some() {
            supported = supported | feature;
        }
    }
    connect(supported)
}

fn connect_and_hello(features: FeatureBits) -> Result<HelloOutcome, BuiltinRendererError> {
    let descriptor =
        seqpacket::connect_touchbar().map_err(|_| BuiltinRendererError::ConnectionUnavailable)?;
    let mut connection = HardwareConnection::new(descriptor);
    let mut request_ids = RequestIds::new();
    let request_id = request_ids.next(&BTreeMap::new())?;
    connection.send_request(
        &Envelope {
            request_id,
            message: ClientMessage::Hello(Hello {
                minor: wire::PROTOCOL_MINOR,
                required_features: features,
            }),
        },
        None,
        None,
    )?;
    let response = connection.receive_service(None)?;
    if response.request_id != request_id {
        return Err(BuiltinRendererError::Protocol);
    }
    match response.message {
        ServiceMessage::HelloAck(ack) => Ok(HelloOutcome::Accepted(NegotiatedConnection {
            connection,
            request_ids,
            ack,
            features,
        })),
        ServiceMessage::Error(ErrorCode::UnsupportedFeature) => Ok(HelloOutcome::Unsupported),
        ServiceMessage::Error(_) => Err(BuiltinRendererError::ServiceRejected),
        _ => Err(BuiltinRendererError::Protocol),
    }
}

fn stock_capabilities(features: FeatureBits, protocol_minor: u16) -> StockCapabilities {
    let mut capabilities = StockCapabilities::default();
    if protocol_minor >= 1 && features.contains(FeatureBits::DISPLAY_BRIGHTNESS) {
        capabilities = capabilities | StockCapabilities::DISPLAY_BRIGHTNESS;
    }
    if protocol_minor >= 1 && features.contains(FeatureBits::KEYBOARD_BACKLIGHT) {
        capabilities = capabilities | StockCapabilities::KEYBOARD_BACKLIGHT;
    }
    capabilities
}

fn desktop_stock_capabilities(state: DesktopState) -> StockCapabilities {
    let mut capabilities = StockCapabilities::default();
    if state.capabilities.contains(DesktopCapabilities::AUDIO) {
        capabilities = capabilities | StockCapabilities::AUDIO;
    }
    if state.capabilities.contains(DesktopCapabilities::MEDIA) {
        capabilities = capabilities | StockCapabilities::MEDIA;
    }
    capabilities
}

/// The keyboard icon's tap: from `current`, the level to set and the level to restore next.
/// On (above 0): off, remembering the level. Off: back to the remembered level, or a default.
fn keyboard_toggle(current: u8, restore: Option<u8>) -> (u8, Option<u8>) {
    if current > 0 {
        (0, Some(current))
    } else {
        (restore.unwrap_or(KEYBOARD_ON_DEFAULT), restore)
    }
}

fn dispatch_desktop(provider: Option<&DesktopProvider>, action: DesktopAction) {
    if let Some(provider) = provider {
        let _ = provider.dispatch(action);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PendingRequest {
    Register,
    RegisterSpare { buffer_id: u32 },
    Submit { frame_id: u64 },
    TapKeys,
    CancelTouchId,
    HardwareAction(Option<DesktopAction>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct InFlightFrame {
    frame_id: u64,
    acknowledged: bool,
}

/// An extra frame buffer, registered the first time the screensaver or the startup
/// animation asks for more than one and only ever used by them, so the stock bar keeps
/// upstream's single-buffer flow.
struct SpareBuffer {
    id: u32,
    frame: RendererFrame,
    registered: bool,
    in_flight: Option<InFlightFrame>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BufferSlot {
    Primary,
    Spare(usize),
}

#[allow(clippy::struct_excessive_bools)] // independent flags, not a state machine
struct RendererSession {
    dimensions: DisplayDimensions,
    stride: u32,
    byte_length: u64,
    stock: StockBar,
    hardware_capabilities: StockCapabilities,
    desktop_state: DesktopState,
    pending_desktop_state: Option<DesktopState>,
    display_off: bool,
    levels: StockLevels,
    touch_id: TouchIdOverlay,
    overlay_state: Option<OverlayState>,
    overlay_visual_state: Option<OverlayState>,
    overlay_opacity: u8,
    overlay_start_opacity: u8,
    overlay_target_opacity: u8,
    overlay_last_update: Duration,
    enrollment_progress: Option<u8>,
    enrollment_progress_start: u16,
    enrollment_progress_visual: u16,
    enrollment_progress_target: u16,
    enrollment_progress_last_update: Duration,
    now: Duration,
    input: InputInterpreter,
    frame: RendererFrame,
    registered: bool,
    dirty: bool,
    in_flight: Option<InFlightFrame>,
    next_frame_id: u64,
    request_ids: RequestIds,
    pending: BTreeMap<u32, PendingRequest>,
    extras: Option<Extras>,
    extras_visible: bool,
    live_faces: LiveFaces,
    live_faces_drawn: Duration,
    /// Watches whether media plays, for the play/pause button (none in tests).
    media: Option<Arc<MediaWatch>>,
    /// The config's theme, and where a quad-tap of Fn (debug gestures) has stepped to:
    /// 0 = the configured theme, `n` = `Theme::NAMES[n - 1]`.
    configured_theme: Theme,
    theme_step: usize,
    /// Started on the first change from their sliders.
    display_light: Option<brightness::Setter>,
    keyboard_light: Option<brightness::Setter>,
    /// The keyboard backlight's last level above 0, for its icon to turn it back on to.
    keyboard_restore: Option<u8>,
    lights_polled: Option<Duration>,
    lights_set_at: Option<Duration>,
    volume_set_at: Option<Duration>,
    /// When the last stock frame was submitted (see [`STOCK_FRAME_INTERVAL`]).
    stock_submitted: Option<Duration>,
    /// From `HelloAck`: how many buffers the service lets us register.
    max_buffers: u32,
    spares: Vec<SpareBuffer>,
    screensaver: Option<Screensaver>,
    /// The startup animation, while it plays.
    intro: Option<Intro>,
    /// The stock frame the animation last drew over, for the frames it draws into a spare
    /// buffer (the primary one, where the stock bar renders, may be in flight then).
    intro_base: Vec<u8>,
    /// The clock reading of the animation's last frame (a repeat would be the same frame).
    intro_drawn: Option<Duration>,
    /// What the panel shows after our last stock frame, to send only what changed.
    /// `None` after the screensaver drew, or before the first frame: send it whole.
    shown: Option<Vec<u8>>,
}

impl RendererSession {
    fn new(
        dimensions: DisplayDimensions,
        request_ids: RequestIds,
        capabilities: StockCapabilities,
    ) -> Result<Self, BuiltinRendererError> {
        let stride = dimensions
            .width()
            .checked_mul(4)
            .ok_or(BuiltinRendererError::Negotiation)?;
        let byte_length = u64::from(stride)
            .checked_mul(u64::from(dimensions.height()))
            .ok_or(BuiltinRendererError::Negotiation)?;
        let frame = RendererFrame::new(byte_length).map_err(|_| BuiltinRendererError::Frame)?;
        let touch_id = TouchIdOverlay::new(dimensions.width(), dimensions.height());
        let stock = StockBar::new(dimensions.width(), dimensions.height(), capabilities)
            .map_err(|_| BuiltinRendererError::Negotiation)?;
        let input = InputInterpreter::new(dimensions, stock)?;
        Ok(Self {
            dimensions,
            stride,
            byte_length,
            stock,
            hardware_capabilities: capabilities,
            desktop_state: DesktopState::default(),
            pending_desktop_state: None,
            display_off: false,
            levels: StockLevels::default(),
            touch_id,
            overlay_state: None,
            overlay_visual_state: None,
            overlay_opacity: 0,
            overlay_start_opacity: 0,
            overlay_target_opacity: 0,
            overlay_last_update: Duration::ZERO,
            enrollment_progress: None,
            enrollment_progress_start: 0,
            enrollment_progress_visual: 0,
            enrollment_progress_target: 0,
            enrollment_progress_last_update: Duration::ZERO,
            now: Duration::ZERO,
            input,
            frame,
            registered: false,
            dirty: true,
            in_flight: None,
            next_frame_id: FIRST_FRAME_ID,
            request_ids,
            pending: BTreeMap::new(),
            extras: None,
            extras_visible: false,
            live_faces: LiveFaces::default(),
            media: None,
            configured_theme: Theme::DEFAULT,
            theme_step: 0,
            live_faces_drawn: Duration::ZERO,
            display_light: None,
            keyboard_light: None,
            keyboard_restore: None,
            lights_polled: None,
            lights_set_at: None,
            volume_set_at: None,
            stock_submitted: None,
            max_buffers: 1,
            spares: Vec::new(),
            screensaver: None,
            intro: None,
            intro_base: Vec::new(),
            intro_drawn: None,
            shown: None,
        })
    }

    fn register(&mut self, connection: &HardwareConnection) -> Result<(), BuiltinRendererError> {
        let request_id = self.allocate_request(PendingRequest::Register)?;
        let message = Envelope {
            request_id,
            message: ClientMessage::RegisterBuffer(RegisterBuffer {
                buffer_id: BUFFER_ID,
                stride: self.stride,
                byte_length: self.byte_length,
            }),
        };
        if let Err(error) = connection.send_request(
            &message,
            Some(self.dimensions),
            Some(self.frame.descriptor()),
        ) {
            self.pending.remove(&request_id);
            return Err(error);
        }
        Ok(())
    }

    #[cfg(test)]
    fn handle(
        &mut self,
        envelope: Envelope<ServiceMessage>,
        connection: &HardwareConnection,
    ) -> Result<(), BuiltinRendererError> {
        self.handle_with_provider(envelope, connection, None)
    }

    fn handle_with_provider(
        &mut self,
        envelope: Envelope<ServiceMessage>,
        connection: &HardwareConnection,
        provider: Option<&DesktopProvider>,
    ) -> Result<(), BuiltinRendererError> {
        match envelope.message {
            ServiceMessage::Ack => self.handle_ack(envelope.request_id, connection, provider),
            ServiceMessage::Error(code) => self.handle_error(envelope.request_id, code, connection),
            ServiceMessage::FrameReleased(release) => {
                if envelope.request_id != 0 {
                    return Err(BuiltinRendererError::Protocol);
                }
                let slot = if release.buffer_id == BUFFER_ID {
                    &mut self.in_flight
                } else {
                    &mut self
                        .spares
                        .iter_mut()
                        .find(|spare| spare.id == release.buffer_id)
                        .ok_or(BuiltinRendererError::Protocol)?
                        .in_flight
                };
                let Some(in_flight) = *slot else {
                    return Err(BuiltinRendererError::Protocol);
                };
                if !in_flight.acknowledged || release.frame_id != in_flight.frame_id {
                    return Err(BuiltinRendererError::Protocol);
                }
                *slot = None;
                if crate::record::active() {
                    crate::record::event(&format!("release {}", release.frame_id));
                }
                if let Some(saver) = self.screensaver.as_mut() {
                    saver.released(release.frame_id);
                }
                self.submit_if_ready(connection)
            }
            ServiceMessage::InputFrame(frame) => {
                if envelope.request_id != 0 {
                    return Err(BuiltinRendererError::Protocol);
                }
                let outcome = self.observe_input(frame)?;
                self.dirty |= outcome.redraw;
                for action in outcome.actions {
                    self.dispatch(action, connection, provider)?;
                }
                if outcome.cancel_touch_id {
                    self.send_cancel_touch_id(connection)?;
                }
                self.apply_pending_desktop_state()?;
                self.submit_if_ready(connection)
            }
            ServiceMessage::HelloAck(_) => Err(BuiltinRendererError::Protocol),
        }
    }

    fn handle_ack(
        &mut self,
        request_id: u32,
        connection: &HardwareConnection,
        provider: Option<&DesktopProvider>,
    ) -> Result<(), BuiltinRendererError> {
        let pending = self
            .pending
            .remove(&request_id)
            .ok_or(BuiltinRendererError::Protocol)?;
        match pending {
            PendingRequest::Register if !self.registered => {
                self.registered = true;
                self.submit_if_ready(connection)
            }
            PendingRequest::RegisterSpare { buffer_id } => {
                if let Some(spare) = self.spares.iter_mut().find(|spare| spare.id == buffer_id) {
                    spare.registered = true;
                }
                self.submit_if_ready(connection)
            }
            PendingRequest::Submit { frame_id } => {
                let Some(in_flight) = self
                    .in_flight
                    .iter_mut()
                    .chain(
                        self.spares
                            .iter_mut()
                            .filter_map(|spare| spare.in_flight.as_mut()),
                    )
                    .find(|in_flight| in_flight.frame_id == frame_id)
                else {
                    return Err(BuiltinRendererError::Protocol);
                };
                if in_flight.acknowledged {
                    return Err(BuiltinRendererError::Protocol);
                }
                in_flight.acknowledged = true;
                Ok(())
            }
            PendingRequest::HardwareAction(feedback) => {
                if let Some(feedback) = feedback {
                    dispatch_desktop(provider, feedback);
                }
                Ok(())
            }
            PendingRequest::CancelTouchId => {
                emit(Record::new(
                    Component::Renderer,
                    Stage::TouchIdCancel,
                    Outcome::Ok,
                    None,
                ));
                Ok(())
            }
            PendingRequest::TapKeys => Ok(()),
            PendingRequest::Register => Err(BuiltinRendererError::Protocol),
        }
    }

    fn handle_error(
        &mut self,
        request_id: u32,
        _code: ErrorCode,
        _connection: &HardwareConnection,
    ) -> Result<(), BuiltinRendererError> {
        let pending = self
            .pending
            .remove(&request_id)
            .ok_or(BuiltinRendererError::Protocol)?;
        if matches!(pending, PendingRequest::CancelTouchId) {
            emit(Record::new(
                Component::Renderer,
                Stage::TouchIdCancel,
                Outcome::Error,
                None,
            ));
        }
        match pending {
            PendingRequest::TapKeys
            | PendingRequest::CancelTouchId
            | PendingRequest::HardwareAction(_) => {
                eprintln!("t1-touchbar: typed action was rejected");
                Ok(())
            }
            PendingRequest::RegisterSpare { .. } => {
                eprintln!("fnstrip screensaver: the service rejected a spare frame buffer");
                Ok(())
            }
            PendingRequest::Register | PendingRequest::Submit { .. } => {
                Err(BuiltinRendererError::ServiceRejected)
            }
        }
    }

    /// Feeds one hardware input frame through the interpreter.
    ///
    /// While the desktop display is off the panel is dark, so contacts are
    /// dropped before interpretation: nothing is pressed, repeated, tapped, or
    /// used to cancel Touch ID. Fn state is still tracked so the row is right
    /// when the display returns. Suppressed touches must end before a fresh
    /// gesture can begin on the restored display.
    fn observe_input(
        &mut self,
        mut frame: WireInputFrame,
    ) -> Result<InputOutcome, BuiltinRendererError> {
        if crate::record::active() {
            let contacts: Vec<String> = frame
                .contacts
                .iter()
                .filter(|contact| contact.tip)
                .map(|contact| format!("{},{}", contact.x, contact.y))
                .collect();
            crate::record::event(&format!(
                "input fn={} touches=[{}]",
                frame.fn_pressed,
                contacts.join(" ")
            ));
        }
        // A touch or an Fn press skips the startup animation; the touch doesn't press
        // whatever is under it.
        if self.intro.is_some()
            && (frame.fn_pressed != self.input.fn_pressed
                || frame.contacts.iter().any(|contact| contact.tip))
        {
            self.stop_intro(if frame.fn_pressed == self.input.fn_pressed {
                "skipped (touch)"
            } else {
                "skipped (Fn)"
            });
            self.input.wait_for_touch_release |= frame.contacts.iter().any(|contact| contact.tip);
        }
        // The screensaver takes every touch (none reach the bar) and a new Fn press.
        let mut stop_screensaver = false;
        if let Some(saver) = self.screensaver.as_mut() {
            stop_screensaver = frame.fn_pressed && !self.input.fn_pressed;
            if !self.input.wait_for_touch_release
                && let Some(contact) = frame.contacts.iter().find(|contact| contact.tip)
            {
                self.input.wait_for_touch_release = true;
                match saver.zone(contact.x) {
                    Zone::NextScene => saver.next_scene(),
                    Zone::NextBuffering => saver.next_buffering(),
                    Zone::Exit => stop_screensaver = true,
                }
            }
        }
        if stop_screensaver {
            self.stop_screensaver();
        }
        if self.display_off || self.input.wait_for_touch_release {
            self.input.wait_for_touch_release = frame.contacts.iter().any(|contact| contact.tip);
            frame.contacts.clear();
        }
        let outcome = self.input.ingest_at(&frame, self.overlay_state, self.now)?;
        self.take_debug_requests();
        Ok(outcome)
    }

    /// Acts on a triple-tap-and-hold (test screen) or quad-tap (next theme) of Fn.
    fn take_debug_requests(&mut self) {
        if self.input.take_screensaver_request(self.now) {
            self.start_screensaver();
        }
        if self.input.take_theme_request() {
            self.next_theme();
        }
    }

    /// Debug gestures: the next built-in theme, then back to the configured one. Until restart.
    fn next_theme(&mut self) {
        self.theme_step = (self.theme_step + 1) % (Theme::NAMES.len() + 1);
        let (name, theme) = match Theme::NAMES.get(self.theme_step.wrapping_sub(1)) {
            Some(name) if self.theme_step > 0 => (*name, Theme::named(name).unwrap_or_default()),
            _ => ("from the config", self.configured_theme),
        };
        eprintln!("fnstrip: theme {name} (debug gesture; restart for the configured one)");
        self.live_faces.set_theme(theme);
        // Only colours change, not where anything is, so touches in progress carry on.
        self.stock = self.stock.with_theme(theme);
        self.input.stock = self.stock;
        self.dirty = true;
    }

    fn start_screensaver(&mut self) {
        if self.screensaver.is_some() || self.display_off || self.overlay_state.is_some() {
            return;
        }
        self.input.cancel_touches();
        self.screensaver = Some(Screensaver::new(
            self.dimensions.width(),
            self.dimensions.height(),
            self.max_buffers.clamp(1, MAX_SCREENSAVER_BUFFERS),
        ));
        self.dirty = true;
    }

    /// Ends the startup animation and logs how it went (`why`: played, skipped, …).
    fn stop_intro(&mut self, why: &str) {
        if let Some(intro) = self.intro.take() {
            let seconds = self.now.saturating_sub(intro.started().unwrap_or(self.now));
            eprintln!(
                "fnstrip: startup animation {why}: {} frames in {:.2} s",
                intro.frames(),
                seconds.as_secs_f64()
            );
            self.intro_base = Vec::new();
            self.intro_drawn = None;
            self.shown = None;
            self.dirty = true;
        }
    }

    fn stop_screensaver(&mut self) {
        if let Some(mut saver) = self.screensaver.take() {
            saver.finish();
            self.dirty = true;
        }
    }

    /// Registers the spare buffers (once per connection) for screensaver and startup
    /// animation pipelining.
    fn register_spares(
        &mut self,
        connection: &HardwareConnection,
    ) -> Result<(), BuiltinRendererError> {
        if !self.spares.is_empty() {
            return Ok(());
        }
        for buffer_id in 2..=self.max_buffers.min(MAX_SCREENSAVER_BUFFERS) {
            let frame =
                RendererFrame::new(self.byte_length).map_err(|_| BuiltinRendererError::Frame)?;
            let request_id = self.allocate_request(PendingRequest::RegisterSpare { buffer_id })?;
            let message = Envelope {
                request_id,
                message: ClientMessage::RegisterBuffer(RegisterBuffer {
                    buffer_id,
                    stride: self.stride,
                    byte_length: self.byte_length,
                }),
            };
            if let Err(error) =
                connection.send_request(&message, Some(self.dimensions), Some(frame.descriptor()))
            {
                self.pending.remove(&request_id);
                return Err(error);
            }
            self.spares.push(SpareBuffer {
                id: buffer_id,
                frame,
                registered: false,
                in_flight: None,
            });
        }
        Ok(())
    }

    /// A buffer we may draw into now: the primary one, or while the screensaver runs
    /// with more than one buffer, or the startup animation plays, a registered spare.
    fn free_slot(&self) -> Option<BufferSlot> {
        if self.in_flight.is_none() {
            return Some(BufferSlot::Primary);
        }
        let buffers = match (&self.screensaver, &self.intro) {
            (Some(saver), _) => saver.buffers(),
            (None, Some(_)) if !self.intro_base.is_empty() => INTRO_BUFFERS,
            _ => return None,
        };
        let spares = buffers.saturating_sub(1);
        self.spares
            .iter()
            .take(spares)
            .position(|spare| spare.registered && spare.in_flight.is_none())
            .map(BufferSlot::Spare)
    }

    fn send_keys(
        &mut self,
        keys: &[KeyCode],
        connection: &HardwareConnection,
    ) -> Result<(), BuiltinRendererError> {
        for keys in keys.chunks(wire::MAX_KEYS) {
            let request_id = self.allocate_request(PendingRequest::TapKeys)?;
            let message = Envelope {
                request_id,
                message: ClientMessage::TapKeys(TapKeys {
                    keys: keys.to_vec(),
                }),
            };
            if let Err(error) = connection.send_request(&message, Some(self.dimensions), None) {
                self.pending.remove(&request_id);
                return Err(error);
            }
        }
        Ok(())
    }

    fn send_cancel_touch_id(
        &mut self,
        connection: &HardwareConnection,
    ) -> Result<(), BuiltinRendererError> {
        let request_id = self.allocate_request(PendingRequest::CancelTouchId)?;
        let message = cancellation_request(request_id);
        emit(Record::new(
            Component::Renderer,
            Stage::TouchIdCancel,
            Outcome::Begin,
            None,
        ));
        if let Err(error) = connection.send_request(&message, Some(self.dimensions), None) {
            emit(Record::new(
                Component::Renderer,
                Stage::TouchIdCancel,
                Outcome::Error,
                None,
            ));
            self.pending.remove(&request_id);
            return Err(error);
        }
        Ok(())
    }

    fn dispatch(
        &mut self,
        action: StockAction,
        connection: &HardwareConnection,
        provider: Option<&DesktopProvider>,
    ) -> Result<(), BuiltinRendererError> {
        if crate::record::active() {
            crate::record::event(&format!("action {action:?}"));
        }
        if let Some(key) = action_key(action) {
            return self.send_keys(&[key], connection);
        }
        match action {
            StockAction::StepDisplayBrightness(direction) => {
                let feedback = self
                    .desktop_state
                    .capabilities
                    .contains(DesktopCapabilities::LEVEL_FEEDBACK)
                    .then_some(DesktopAction::ShowDisplayBrightness);
                self.send_hardware_action(
                    ClientMessage::StepDisplayBrightness(wire_step(direction)),
                    feedback,
                    connection,
                )
            }
            StockAction::StepKeyboardBacklight(direction) => {
                let feedback = self
                    .desktop_state
                    .capabilities
                    .contains(DesktopCapabilities::LEVEL_FEEDBACK)
                    .then_some(DesktopAction::ShowKeyboardBacklight);
                self.send_hardware_action(
                    ClientMessage::StepKeyboardBacklight(wire_step(direction)),
                    feedback,
                    connection,
                )
            }
            StockAction::ToggleMute => {
                self.levels.muted = !self.levels.muted;
                self.dirty = true;
                dispatch_desktop(provider, DesktopAction::ToggleMute);
                Ok(())
            }
            StockAction::StepVolume(direction) => {
                let current = self.levels.volume.unwrap_or(0);
                let value = match direction {
                    StepDirection::Down => current.saturating_sub(VOLUME_STEP_PERCENT),
                    StepDirection::Up => current.saturating_add(VOLUME_STEP_PERCENT).min(100),
                };
                self.levels.volume = Some(value);
                self.dirty = true;
                dispatch_desktop(provider, DesktopAction::SetVolume(value));
                Ok(())
            }
            StockAction::MediaPrevious => {
                dispatch_desktop(provider, DesktopAction::MediaPrevious);
                Ok(())
            }
            StockAction::MediaPlayPause => {
                dispatch_desktop(provider, DesktopAction::MediaPlayPause);
                Ok(())
            }
            StockAction::MediaNext => {
                dispatch_desktop(provider, DesktopAction::MediaNext);
                Ok(())
            }
            StockAction::Extra(index) => {
                if let Some(extras) = &self.extras {
                    extras.run(usize::from(index));
                }
                Ok(())
            }
            StockAction::SetLevel(SliderKind::Volume, value) => {
                self.levels.volume = Some(value);
                self.volume_set_at = Some(self.now);
                self.dirty = true;
                dispatch_desktop(provider, DesktopAction::SetVolume(value));
                Ok(())
            }
            StockAction::ToggleKeyboardBacklight => {
                let Some(current) = self.levels.keyboard_backlight else {
                    return Ok(()); // no backlight level known (none, or not read yet)
                };
                let (value, restore) = keyboard_toggle(current, self.keyboard_restore);
                self.keyboard_restore = restore;
                self.set_light(SliderKind::Keyboard, value);
                Ok(())
            }
            StockAction::SetLevel(
                kind @ (SliderKind::Brightness | SliderKind::Keyboard),
                value,
            ) => {
                self.set_light(kind, value);
                Ok(())
            }
            StockAction::Escape | StockAction::Function(_) => unreachable!(),
        }
    }

    /// Sets the display brightness or keyboard backlight from its slider (or icon).
    fn set_light(&mut self, kind: SliderKind, value: u8) {
        if kind == SliderKind::Keyboard && value > 0 {
            self.keyboard_restore = Some(value);
        }
        let (light, level, setter) = match kind {
            SliderKind::Brightness => (
                Light::Display,
                &mut self.levels.display_brightness,
                &mut self.display_light,
            ),
            _ => (
                Light::Keyboard,
                &mut self.levels.keyboard_backlight,
                &mut self.keyboard_light,
            ),
        };
        *level = Some(value);
        setter
            .get_or_insert_with(|| brightness::Setter::start(light))
            .set(value);
        self.lights_set_at = Some(self.now);
        self.dirty = true;
    }

    fn send_hardware_action(
        &mut self,
        action: ClientMessage,
        feedback: Option<DesktopAction>,
        connection: &HardwareConnection,
    ) -> Result<(), BuiltinRendererError> {
        let request_id = self.allocate_request(PendingRequest::HardwareAction(feedback))?;
        let message = Envelope {
            request_id,
            message: action,
        };
        if let Err(error) = connection.send_request(&message, Some(self.dimensions), None) {
            self.pending.remove(&request_id);
            return Err(error);
        }
        Ok(())
    }

    fn submit_if_ready(
        &mut self,
        connection: &HardwareConnection,
    ) -> Result<(), BuiltinRendererError> {
        if !self.registered || !self.dirty {
            return Ok(());
        }
        if self
            .screensaver
            .as_ref()
            .is_some_and(|saver| saver.buffers() > 1)
            || (self.intro.is_some() && self.max_buffers > 1)
        {
            self.register_spares(connection)?;
        }
        if self.intro.is_some() && self.intro_drawn == Some(self.now) {
            return Ok(());
        }
        if self.screensaver.is_none()
            && self.intro.is_none()
            && self
                .stock_submitted
                .is_some_and(|at| self.now.saturating_sub(at) < STOCK_FRAME_INTERVAL)
        {
            return Ok(());
        }
        let Some(slot) = self.free_slot() else {
            return Ok(());
        };
        let mut damage = Vec::new();
        let buffer_id = match slot {
            BufferSlot::Primary => {
                if let Some(saver) = self.screensaver.as_mut() {
                    damage = saver.render(self.frame.as_mut_slice());
                    self.shown = None;
                } else if self.intro.is_some() {
                    self.render_intro(slot)?;
                } else {
                    self.render()?;
                    if !self.stock_changed() {
                        self.dirty = false;
                        return Ok(());
                    }
                }
                BUFFER_ID
            }
            BufferSlot::Spare(index) => {
                if let Some(saver) = self.screensaver.as_mut() {
                    damage = saver.render(self.spares[index].frame.as_mut_slice());
                } else {
                    self.render_intro(slot)?;
                }
                self.shown = None;
                self.spares[index].id
            }
        };
        let frame_id = self.take_frame_id();
        if crate::record::active() && slot == BufferSlot::Primary {
            let rectangles: Vec<String> = damage
                .iter()
                .map(|r| format!("{}+{}", r.x, r.width))
                .collect();
            let damage = if rectangles.is_empty() {
                "full".to_owned()
            } else {
                rectangles.join(",")
            };
            crate::record::frame(frame_id, &damage, self.frame.as_mut_slice());
        }
        let request_id = self.allocate_request(PendingRequest::Submit { frame_id })?;
        let message = Envelope {
            request_id,
            message: ClientMessage::SubmitFrame(SubmitFrame {
                buffer_id,
                frame_id,
                damage,
            }),
        };
        if let Err(error) = connection.send_request(&message, Some(self.dimensions), None) {
            self.pending.remove(&request_id);
            return Err(error);
        }
        let in_flight = Some(InFlightFrame {
            frame_id,
            acknowledged: false,
        });
        match slot {
            BufferSlot::Primary => self.in_flight = in_flight,
            BufferSlot::Spare(index) => self.spares[index].in_flight = in_flight,
        }
        if let Some(saver) = self.screensaver.as_mut() {
            saver.submitted(frame_id);
        } else {
            self.stock_submitted = Some(self.now);
        }
        self.dirty = false;
        Ok(())
    }

    /// A startup animation frame into `slot`: over a fresh stock frame in the primary
    /// buffer (kept for later spare frames), or over the last one in a spare.
    fn render_intro(&mut self, slot: BufferSlot) -> Result<(), BuiltinRendererError> {
        let pixels = match slot {
            BufferSlot::Primary => {
                self.render()?;
                self.intro_base.clear();
                self.intro_base.extend_from_slice(self.frame.as_mut_slice());
                self.frame.as_mut_slice()
            }
            BufferSlot::Spare(index) => {
                let pixels = self.spares[index].frame.as_mut_slice();
                pixels.copy_from_slice(&self.intro_base);
                pixels
            }
        };
        if let Some(intro) = self.intro.as_mut() {
            intro.draw(pixels, self.now);
        }
        self.intro_drawn = Some(self.now);
        self.shown = None;
        Ok(())
    }

    /// Whether the freshly rendered stock frame differs from what the panel shows, and
    /// records it as shown. Stock frames always go whole (empty damage): the panel stalls
    /// ~1 s (appletbdrm -110) on a few percent of partial updates of every shape we tried,
    /// and never on a full frame (see LEARNINGS.md).
    fn stock_changed(&mut self) -> bool {
        let next = self.frame.as_mut_slice();
        match &mut self.shown {
            Some(shown) if shown.as_slice() == &*next => false,
            Some(shown) => {
                shown.copy_from_slice(next);
                true
            }
            None => {
                self.shown = Some(next.to_vec());
                true
            }
        }
    }

    /// The stock row for `layer`, with the extras' live faces.
    fn render_rows(
        &mut self,
        layer: Layer,
        levels: StockLevels,
        pressed: Option<StockAction>,
    ) -> Result<(), BuiltinRendererError> {
        self.stock
            .render_pressed(
                &mut xrgb_frame(self.dimensions, self.stride, self.frame.as_mut_slice())?,
                layer,
                levels,
                pressed,
            )
            .map_err(|_| BuiltinRendererError::Frame)?;
        if levels.extras_live != 0 {
            for index in (0..16).filter(|index| levels.extras_live & (1 << index) != 0) {
                if let Some(bounds) = self.stock.extra_bounds(index) {
                    self.live_faces.draw(
                        self.frame.as_mut_slice(),
                        self.stride as usize,
                        index,
                        bounds,
                        self.dimensions.width(),
                    );
                }
            }
            self.stock
                .render_live_extras(
                    &mut xrgb_frame(self.dimensions, self.stride, self.frame.as_mut_slice())?,
                    levels,
                    pressed,
                )
                .map_err(|_| BuiltinRendererError::Frame)?;
        }
        if layer == Layer::Controls
            && levels.media_live
            && let Some(bounds) = self.stock.play_pause_bounds()
        {
            self.live_faces.draw_speakers(
                self.frame.as_mut_slice(),
                self.stride as usize,
                bounds,
                self.dimensions.width(),
            );
            self.stock
                .render_live_media(
                    &mut xrgb_frame(self.dimensions, self.stride, self.frame.as_mut_slice())?,
                    pressed,
                )
                .map_err(|_| BuiltinRendererError::Frame)?;
        }
        Ok(())
    }

    fn render(&mut self) -> Result<(), BuiltinRendererError> {
        if self.display_off {
            self.frame.as_mut_slice().fill(0);
            return Ok(());
        }
        let layer = if self.overlay_visual_state.is_none() {
            self.input.layer()
        } else {
            Layer::Controls
        };
        let levels = if layer == Layer::Extras {
            self.levels
        } else {
            StockLevels {
                extras_live: 0,
                ..self.levels
            }
        };
        let pressed = self.input.pressed();
        self.render_rows(layer, levels, pressed)?;
        let mut frame = xrgb_frame(self.dimensions, self.stride, self.frame.as_mut_slice())?;
        if self.overlay_opacity > 0 {
            let left = self.stock.escape_right();
            if left < self.dimensions.width() {
                frame
                    .blend_rectangle(
                        crate::framebuffer::Rectangle {
                            x: left,
                            y: 0,
                            width: self.dimensions.width() - left,
                            height: self.dimensions.height(),
                        },
                        crate::framebuffer::Rgb {
                            red: 0,
                            green: 0,
                            blue: 0,
                        },
                        self.overlay_opacity,
                    )
                    .map_err(|_| BuiltinRendererError::Frame)?;
            }
        }
        self.touch_id
            .render_with_progress(
                &mut frame,
                self.overlay_visual_state,
                (self.overlay_visual_state == Some(OverlayState::Enrollment))
                    .then_some(self.enrollment_progress_visual),
                self.overlay_opacity,
            )
            .map_err(|_| BuiltinRendererError::Frame)
    }

    /// The volume to show for one the desktop reported: the slider's own while it moves and
    /// shortly after (the desktop's reports lag a drag and would pull the fill back).
    fn reported_volume(&self, reported: Option<u8>) -> Option<u8> {
        let settled = self
            .volume_set_at
            .is_none_or(|set| self.now.saturating_sub(set) >= LIGHT_SETTLE);
        if settled && !self.input.sliding() {
            reported
        } else {
            self.levels.volume
        }
    }

    /// Re-reads the display and keyboard backlight levels for their sliders, unless a
    /// slider is moving or just set one.
    fn poll_lights(&mut self, now: Duration) {
        let due = self
            .lights_polled
            .is_none_or(|polled| now.saturating_sub(polled) >= LIGHT_POLL_INTERVAL);
        let settled = self
            .lights_set_at
            .is_none_or(|set| now.saturating_sub(set) >= LIGHT_SETTLE);
        if !due || !settled || self.display_off || self.input.sliding() {
            return;
        }
        self.lights_polled = Some(now);
        let display = brightness::read(Light::Display);
        let keyboard = brightness::read(Light::Keyboard);
        if (display, keyboard)
            != (
                self.levels.display_brightness,
                self.levels.keyboard_backlight,
            )
        {
            if crate::record::active() {
                crate::record::event(&format!("lights display={display:?} keyboard={keyboard:?}"));
            }
            self.levels.display_brightness = display;
            self.levels.keyboard_backlight = keyboard;
            self.dirty = true;
        }
    }

    /// Sets which row shows with Fn up (the `default_row` setting).
    fn set_function_row_first(&mut self, function_row_first: bool) {
        self.stock = self.stock.with_function_row_first(function_row_first);
        self.input.set_stock(self.stock);
        self.dirty = true;
    }

    fn set_theme(&mut self, theme: Theme) {
        self.configured_theme = theme;
        self.theme_step = 0;
        self.live_faces.set_theme(theme);
        self.stock = self.stock.with_theme(theme);
        self.input.set_stock(self.stock);
        self.dirty = true;
    }

    /// Adopts the configured extras buttons (layout from their icons).
    fn set_extras(&mut self, extras: Extras) {
        self.stock = self.stock.with_extras(extras.icons());
        self.input.set_stock(self.stock);
        self.extras = Some(extras);
        self.dirty = true;
    }

    fn set_extras_status(&mut self, status: u16) {
        if self.levels.extras_on != status {
            self.levels.extras_on = status;
            self.dirty = true;
        }
    }

    /// Tells the extras worker whether to keep polling button states, and starts or
    /// stops the live faces' feeds.
    fn sync_extras_visibility(&mut self) {
        let visible = !self.display_off
            && self.overlay_state.is_none()
            && self.input.layer() == Layer::Extras;
        if visible != self.extras_visible {
            self.extras_visible = visible;
            if let Some(extras) = &self.extras {
                extras.set_visible(visible);
            }
        }
        let plate = self.stock.extra_bounds(0).map_or((0, 0), |bounds| {
            (bounds.width as usize, bounds.height as usize)
        });
        let live = self.live_faces.sync(
            visible,
            self.extras.as_ref().map_or(&[], Extras::buttons),
            self.levels.extras_on,
            plate,
        );
        if live != self.levels.extras_live {
            self.levels.extras_live = live;
            self.dirty = true;
        }
        if let Some(bars) = self.live_faces.battery_bars()
            && bars != self.levels.battery_bars
        {
            self.levels.battery_bars = bars;
            self.dirty = true;
        }
        self.sync_media();
    }

    /// Lights the play/pause button while media plays, with the speakers' spectrum.
    fn sync_media(&mut self) {
        let controls = !self.display_off
            && self.overlay_state.is_none()
            && self.input.layer() == Layer::Controls;
        let playing = self.media.as_ref().is_some_and(|media| {
            media.set_watching(controls);
            media.playing()
        });
        let width = self
            .stock
            .play_pause_bounds()
            .map_or(0, |bounds| bounds.width as usize);
        let live = self.live_faces.sync_speakers(controls && playing, width);
        if playing != self.levels.playing || live != self.levels.media_live {
            self.levels.playing = playing;
            self.levels.media_live = live;
            self.dirty = true;
        }
    }

    fn set_overlay(&mut self, state: Option<OverlayState>, progress: Option<u8>) {
        if self.overlay_state == state && self.enrollment_progress == progress {
            return;
        }
        let state_changed = self.overlay_state != state;
        self.overlay_state = state;
        self.enrollment_progress = progress;
        self.enrollment_progress_start = self.enrollment_progress_visual;
        self.enrollment_progress_target = u16::from(progress.unwrap_or(0)) * 10;
        self.enrollment_progress_last_update = self.now;
        if state_changed {
            emit(Record::new(
                Component::Renderer,
                Stage::Overlay,
                if state.is_some() {
                    Outcome::Begin
                } else {
                    Outcome::Ok
                },
                None,
            ));
            self.overlay_start_opacity = self.overlay_opacity;
            self.overlay_last_update = self.now;
            match state {
                Some(state) => {
                    self.input.cancel_repeat();
                    self.overlay_visual_state = Some(state);
                    self.overlay_target_opacity = u8::MAX;
                }
                None => self.overlay_target_opacity = 0,
            }
        }
        self.dirty = true;
    }

    fn advance(
        &mut self,
        now: Duration,
        connection: &HardwareConnection,
        provider: Option<&DesktopProvider>,
    ) -> Result<(), BuiltinRendererError> {
        self.now = now;
        // A third Fn press held long enough starts the test screen, with no new input.
        self.take_debug_requests();
        self.advance_overlay_fade(now);
        self.advance_enrollment_progress(now);
        if self.display_off || self.overlay_state.is_some() {
            self.stop_screensaver();
            self.stop_intro(if self.display_off {
                "stopped (display off)"
            } else {
                "stopped (Touch ID)"
            });
        }
        if let Some(intro) = &self.intro {
            if intro.finished(now) {
                self.stop_intro("played");
            } else {
                self.dirty = true;
            }
        }
        self.poll_lights(now);
        let animate = self.live_faces.animating()
            && now.saturating_sub(self.live_faces_drawn) >= LIVE_FACE_INTERVAL;
        let live = self.levels.extras_live != 0 || self.levels.media_live;
        if live && (animate || self.live_faces.take_fresh()) {
            self.live_faces_drawn = now;
            self.dirty = true;
        }
        if let Some(saver) = self.screensaver.as_mut() {
            let sweep_finished = saver.tick();
            self.dirty = true;
            if sweep_finished {
                self.stop_screensaver();
            }
        }
        if !self.display_off
            && !self.input.wait_for_touch_release
            && let Some(action) = self.input.take_repeat(now)
        {
            self.dispatch(action, connection, provider)?;
        }
        self.submit_if_ready(connection)
    }

    fn advance_overlay_fade(&mut self, now: Duration) {
        if self.overlay_opacity == self.overlay_target_opacity {
            return;
        }
        let elapsed = now.saturating_sub(self.overlay_last_update);
        let duration_nanos = OVERLAY_FADE_DURATION.as_nanos().max(1);
        let progress = elapsed.as_nanos().min(duration_nanos);
        self.overlay_opacity = if self.overlay_target_opacity >= self.overlay_start_opacity {
            let span = self.overlay_target_opacity - self.overlay_start_opacity;
            self.overlay_start_opacity.saturating_add(
                u8::try_from(u128::from(span) * progress / duration_nanos).unwrap_or(span),
            )
        } else {
            let span = self.overlay_start_opacity - self.overlay_target_opacity;
            self.overlay_start_opacity.saturating_sub(
                u8::try_from(u128::from(span) * progress / duration_nanos).unwrap_or(span),
            )
        };
        if self.overlay_opacity == 0 && self.overlay_target_opacity == 0 {
            self.overlay_visual_state = None;
        }
        self.dirty = true;
    }

    fn advance_enrollment_progress(&mut self, now: Duration) {
        if self.enrollment_progress_visual == self.enrollment_progress_target {
            return;
        }
        let elapsed = now.saturating_sub(self.enrollment_progress_last_update);
        let duration_nanos = ENROLLMENT_PROGRESS_DURATION.as_nanos().max(1);
        let progress = elapsed.as_nanos().min(duration_nanos);
        let start = self.enrollment_progress_start;
        let target = self.enrollment_progress_target;
        self.enrollment_progress_visual = if target >= start {
            start.saturating_add(
                u16::try_from(u128::from(target - start) * progress / duration_nanos)
                    .unwrap_or(target - start),
            )
        } else {
            start.saturating_sub(
                u16::try_from(u128::from(start - target) * progress / duration_nanos)
                    .unwrap_or(start - target),
            )
        };
        self.dirty = true;
    }

    fn set_desktop_state(&mut self, state: DesktopState) -> Result<(), BuiltinRendererError> {
        if crate::record::active() {
            crate::record::event(&format!(
                "desktop volume={:?} muted={}",
                state.volume, state.muted
            ));
        }
        // Display power follows the desktop immediately, even under an active
        // touch: a dark panel must not keep showing controls.
        if self.display_off != state.display_off {
            self.display_off = state.display_off;
            if self.display_off {
                self.input.cancel_touches();
            }
            self.dirty = true;
        }
        if self.desktop_state == state && self.pending_desktop_state.is_none() {
            let volume = self.reported_volume(state.volume);
            if self.levels.volume != volume || self.levels.muted != state.muted {
                self.levels.volume = volume;
                self.levels.muted = state.muted;
                self.dirty = true;
            }
            return Ok(());
        }
        if !self.input.is_idle() {
            self.pending_desktop_state = Some(state);
            return Ok(());
        }
        self.apply_desktop_state(state)
    }

    fn apply_pending_desktop_state(&mut self) -> Result<(), BuiltinRendererError> {
        if !self.input.is_idle() {
            return Ok(());
        }
        let Some(state) = self.pending_desktop_state.take() else {
            return Ok(());
        };
        self.apply_desktop_state(state)
    }

    fn apply_desktop_state(&mut self, state: DesktopState) -> Result<(), BuiltinRendererError> {
        let capabilities = self.hardware_capabilities | desktop_stock_capabilities(state);
        let stock = StockBar::new(
            self.dimensions.width(),
            self.dimensions.height(),
            capabilities,
        )
        .map_err(|_| BuiltinRendererError::Negotiation)?
        .with_extras(self.extras.as_ref().map_or(&[], Extras::icons))
        .with_function_row_first(self.stock.function_row_first())
        .with_theme(self.stock.theme());
        self.stock = stock;
        self.input.set_stock(stock);
        self.levels.volume = self.reported_volume(state.volume);
        self.levels.muted = state.muted;
        self.desktop_state = state;
        self.pending_desktop_state = None;
        self.dirty = true;
        Ok(())
    }

    fn allocate_request(&mut self, kind: PendingRequest) -> Result<u32, BuiltinRendererError> {
        if self.pending.len() >= MAX_PENDING_REQUESTS {
            return Err(BuiltinRendererError::Protocol);
        }
        let request_id = self.request_ids.next(&self.pending)?;
        self.pending.insert(request_id, kind);
        Ok(request_id)
    }

    fn take_frame_id(&mut self) -> u64 {
        let current = self.next_frame_id;
        self.next_frame_id = self.next_frame_id.wrapping_add(1);
        if self.next_frame_id == 0 {
            self.next_frame_id = 1;
        }
        current
    }
}

const fn wire_step(direction: StepDirection) -> WireStepDirection {
    match direction {
        StepDirection::Down => WireStepDirection::Down,
        StepDirection::Up => WireStepDirection::Up,
    }
}

const fn cancellation_request(request_id: u32) -> Envelope<ClientMessage> {
    Envelope {
        request_id,
        message: ClientMessage::CancelTouchId,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RequestIds {
    next: u32,
}

impl RequestIds {
    const fn new() -> Self {
        Self { next: 1 }
    }

    fn next(
        &mut self,
        pending: &BTreeMap<u32, PendingRequest>,
    ) -> Result<u32, BuiltinRendererError> {
        for _ in 0..=MAX_PENDING_REQUESTS {
            let candidate = self.next;
            self.next = self.next.wrapping_add(1);
            if self.next == 0 {
                self.next = 1;
            }
            if candidate != 0 && !pending.contains_key(&candidate) {
                return Ok(candidate);
            }
        }
        Err(BuiltinRendererError::Protocol)
    }
}

#[derive(Debug, Eq, PartialEq)]
struct InputOutcome {
    redraw: bool,
    actions: Vec<StockAction>,
    cancel_touch_id: bool,
}

struct InputInterpreter {
    stock: StockBar,
    touch_id: TouchIdOverlay,
    gestures: GestureEngine,
    fn_pressed: bool,
    fn_taps: FnTaps,
    wait_for_touch_release: bool,
    pressed: Option<StockAction>,
    repeat: Option<RepeatState>,
    suppress_tap: Option<StockAction>,
    slider: Option<SliderTouch>,
}

/// A touch on a level slider (controls row): it sets the level under the finger until
/// released; on the volume slider's icon it is a mute button instead.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SliderTouch {
    kind: SliderKind,
    /// Started on the volume icon, and still on it.
    on_icon: bool,
    /// The last level sent.
    level: Option<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RepeatState {
    action: StockAction,
    next_at: Duration,
    fired: bool,
}

impl InputInterpreter {
    fn new(dimensions: DisplayDimensions, stock: StockBar) -> Result<Self, BuiltinRendererError> {
        let touch_id = TouchIdOverlay::new(dimensions.width(), dimensions.height());
        let drag_distance = (f64::from(dimensions.height()) / 4.0).max(1.0);
        let settings = GestureSettings::new(drag_distance, Duration::MAX, wire::MAX_CONTACTS)
            .map_err(|_| BuiltinRendererError::Gesture)?;
        Ok(Self {
            stock,
            touch_id,
            gestures: GestureEngine::new(settings),
            fn_pressed: false,
            fn_taps: FnTaps::default(),
            wait_for_touch_release: false,
            pressed: None,
            repeat: None,
            suppress_tap: None,
            slider: None,
        })
    }

    /// True while a touch is moving a slider.
    const fn sliding(&self) -> bool {
        self.slider.is_some()
    }

    /// Handles a frame for the level sliders: a fresh touch on one, or the touch already on
    /// one. `None`: not a slider touch (the normal row handles it).
    fn slider_input(
        &mut self,
        contacts: &[DisplayContact],
        overlay_state: Option<OverlayState>,
    ) -> Option<InputOutcome> {
        let mut actions = Vec::new();
        let level = match (self.slider.as_mut(), contacts.first()) {
            (None, None) => return None,
            (None, Some(contact)) => {
                if self.pressed.is_some()
                    || !self.gestures.is_idle()
                    || overlay_state.is_some()
                    || self.layer() != Layer::Controls
                {
                    return None;
                }
                let (kind, spot) = self.stock.slider_at(*contact)?;
                self.slider = Some(SliderTouch {
                    kind,
                    on_icon: spot == SliderSpot::Icon,
                    level: None,
                });
                match spot {
                    SliderSpot::Icon => None,
                    SliderSpot::Level(level) => Some(level),
                }
            }
            (Some(touch), Some(contact)) if touch.on_icon => {
                touch.on_icon =
                    self.stock.slider_at(*contact) == Some((touch.kind, SliderSpot::Icon));
                None
            }
            (Some(touch), Some(contact)) => {
                Some(self.stock.slider_level_at(touch.kind, contact.x()))
            }
            (Some(touch), None) => {
                if touch.on_icon {
                    actions.push(match touch.kind {
                        SliderKind::Volume => StockAction::ToggleMute,
                        _ => StockAction::ToggleKeyboardBacklight,
                    });
                }
                self.slider = None;
                None
            }
        };
        if let (Some(level), Some(touch)) = (level, self.slider.as_mut())
            && touch.level != Some(level)
        {
            touch.level = Some(level);
            actions.push(StockAction::SetLevel(touch.kind, level));
        }
        Some(InputOutcome {
            redraw: !actions.is_empty(),
            actions,
            cancel_touch_id: false,
        })
    }

    #[cfg(test)]
    const fn fn_pressed(&self) -> bool {
        self.fn_pressed
    }

    const fn layer(&self) -> Layer {
        if self.fn_taps.extras_latched {
            Layer::Extras
        } else {
            Layer::from_fn(
                self.fn_pressed,
                self.fn_taps.double_tapped(),
                self.stock.function_row_first(),
            )
        }
    }

    /// True once after a triple-tap-and-hold of Fn (debug gestures), as of session time `now`.
    const fn take_screensaver_request(&mut self, now: Duration) -> bool {
        self.fn_taps.poll(now);
        let requested = self.fn_taps.screensaver;
        self.fn_taps.screensaver = false;
        requested
    }

    /// True once after a quad-tap of Fn (debug gestures).
    const fn take_theme_request(&mut self) -> bool {
        let requested = self.fn_taps.next_theme;
        self.fn_taps.next_theme = false;
        requested
    }

    /// Lets triple-tap-and-hold (test screen) and quad-tap (next theme) through.
    const fn set_debug_gestures(&mut self, on: bool) {
        self.fn_taps.debug = on;
    }

    const fn pressed(&self) -> Option<StockAction> {
        self.pressed
    }

    fn is_idle(&self) -> bool {
        !self.wait_for_touch_release && self.gestures.is_idle()
    }

    fn set_stock(&mut self, stock: StockBar) {
        debug_assert!(self.is_idle());
        self.stock = stock;
    }

    #[cfg(test)]
    fn ingest(
        &mut self,
        frame: &WireInputFrame,
        overlay_state: Option<OverlayState>,
    ) -> Result<InputOutcome, BuiltinRendererError> {
        self.ingest_at(
            frame,
            overlay_state,
            Duration::from_nanos(frame.monotonic_ns),
        )
    }

    // One pass over a frame's contacts; the slider and gesture branches share its state.
    #[allow(clippy::too_many_lines)]
    fn ingest_at(
        &mut self,
        frame: &WireInputFrame,
        overlay_state: Option<OverlayState>,
        now: Duration,
    ) -> Result<InputOutcome, BuiltinRendererError> {
        let mut redraw = self.fn_pressed != frame.fn_pressed;
        self.fn_taps.observe(
            Duration::from_nanos(frame.monotonic_ns),
            now,
            self.fn_pressed,
            frame.fn_pressed,
            frame.contacts.iter().any(|contact| contact.tip),
        );
        self.fn_pressed = frame.fn_pressed;
        let contacts = frame
            .contacts
            .iter()
            .filter(|contact| contact.tip)
            .map(display_contact)
            .collect::<Result<Vec<_>, _>>()?;
        if overlay_state.is_some() || self.layer() != Layer::Controls {
            self.slider = None;
        }
        if let Some(mut outcome) = self.slider_input(&contacts, overlay_state) {
            // Keep the gesture engine in step, but the slider takes the touch.
            self.gestures
                .ingest_frame(Duration::from_nanos(frame.monotonic_ns), &contacts)
                .map_err(|_| BuiltinRendererError::Gesture)?;
            outcome.redraw |= redraw;
            return Ok(outcome);
        }
        let pressed = contacts
            .first()
            .filter(|contact| {
                !self
                    .touch_id
                    .covers_contact(**contact, overlay_state, self.fn_pressed)
            })
            .and_then(|contact| self.stock.action_at(*contact, self.layer()));
        redraw |= self.pressed != pressed;
        if self.pressed != pressed {
            if pressed.is_none()
                && let Some(repeat) = self.repeat
                && repeat.fired
            {
                self.suppress_tap = Some(repeat.action);
            }
            self.repeat = pressed
                .filter(|action| repeatable(*action))
                .map(|action| RepeatState {
                    action,
                    next_at: now.saturating_add(HOLD_REPEAT_DELAY),
                    fired: false,
                });
        }
        self.pressed = pressed;
        let gestures = self
            .gestures
            .ingest_frame(Duration::from_nanos(frame.monotonic_ns), &contacts)
            .map_err(|_| BuiltinRendererError::Gesture)?;
        let mut actions = Vec::new();
        let mut cancel_touch_id = false;
        for gesture in gestures {
            if matches!(
                gesture,
                crate::gesture::Gesture::DragStart(_) | crate::gesture::Gesture::Drag { .. }
            ) {
                self.repeat = None;
                self.suppress_tap = None;
            }
            let cancels =
                self.touch_id
                    .cancellation_for_gesture(gesture, overlay_state, self.fn_pressed);
            if overlay_state.is_some() && matches!(gesture, crate::gesture::Gesture::Press(_)) {
                emit(Record::new(
                    Component::Renderer,
                    Stage::OverlayPress,
                    if cancels { Outcome::Ok } else { Outcome::Error },
                    None,
                ));
            }
            if cancels {
                cancel_touch_id = true;
                continue;
            }
            if let crate::gesture::Gesture::Tap(contact) = gesture
                && self
                    .touch_id
                    .covers_contact(contact, overlay_state, self.fn_pressed)
            {
                continue;
            }
            let Some(action) = self.stock.action_for_gesture(gesture, self.layer()) else {
                continue;
            };
            if self.suppress_tap == Some(action) {
                self.suppress_tap = None;
                continue;
            }
            if !actions.contains(&action) {
                actions.push(action);
            }
        }
        Ok(InputOutcome {
            redraw,
            actions,
            cancel_touch_id,
        })
    }

    fn take_repeat(&mut self, now: Duration) -> Option<StockAction> {
        let repeat = self.repeat.as_mut()?;
        if now < repeat.next_at || self.pressed != Some(repeat.action) {
            return None;
        }
        repeat.fired = true;
        repeat.next_at = now.saturating_add(HOLD_REPEAT_INTERVAL);
        Some(repeat.action)
    }

    fn cancel_repeat(&mut self) {
        self.repeat = None;
        self.suppress_tap = None;
    }

    fn cancel_touches(&mut self) {
        self.slider = None;
        self.wait_for_touch_release |= !self.gestures.is_idle();
        self.gestures.cancel_all();
        self.pressed = None;
        self.cancel_repeat();
    }
}

/// Recognises Fn taps, using the hardware's input timestamps: a double-tap shows the extras
/// row, and, with debug gestures on (`debug_gestures = "on"`), a triple-tap-and-hold shows
/// the test screen and a quad-tap switches to the next theme.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "a tap recogniser's independent flags"
)]
struct FnTaps {
    /// Triple-tap-and-hold and quad-tap count only with this on.
    debug: bool,
    pressed_at: Duration,
    touched: bool,
    tap_released_at: Option<Duration>,
    /// Quick untouched taps in a row, up to the last release.
    taps: u8,
    /// Taps chained before the current press: 1 = double-tap (extras row), 2 = a third
    /// press (the test screen if held), 3 = quad-tap (next theme, which ends the chain).
    taps_before_press: u8,
    /// A third press, and when (session time) it went down: held long enough, it starts
    /// the test screen; released quickly, it's a tap toward a quad-tap.
    hold_since: Option<Duration>,
    /// The current press already did its thing, so its release isn't a tap.
    chain_over: bool,
    screensaver: bool,
    next_theme: bool,
    /// The extras row stays after a double-tap until the next Fn press.
    extras_latched: bool,
}

impl FnTaps {
    /// Taps the chain can hold: up to a quad-tap with debug gestures, else a double-tap.
    const fn longest_chain(&self) -> u8 {
        if self.debug { 3 } else { 1 }
    }

    /// `at`: the input's hardware timestamp; `now`: session time, for the hold.
    fn observe(
        &mut self,
        at: Duration,
        now: Duration,
        was_pressed: bool,
        pressed: bool,
        touching: bool,
    ) {
        match (was_pressed, pressed) {
            (false, true) => {
                let chained = self
                    .tap_released_at
                    .is_some_and(|released| at.saturating_sub(released) <= FN_DOUBLE_TAP_GAP);
                self.taps_before_press = if chained { self.taps } else { 0 };
                self.chain_over = false;
                self.hold_since = None;
                if self.debug && self.taps_before_press == 2 {
                    self.hold_since = Some(now);
                } else if self.debug && self.taps_before_press == 3 {
                    self.next_theme = true;
                    self.chain_over = true;
                }
                self.extras_latched = self.taps_before_press == 1;
                self.tap_released_at = None;
                self.pressed_at = at;
                self.touched = touching;
            }
            (true, true) => self.touched |= touching,
            (true, false) => {
                let was_tap = !self.chain_over
                    && self.taps_before_press <= self.longest_chain()
                    && !self.touched
                    && at.saturating_sub(self.pressed_at) <= FN_TAP_MAX;
                self.taps = if was_tap {
                    self.taps_before_press + 1
                } else {
                    0
                };
                self.tap_released_at = was_tap.then_some(at);
                self.taps_before_press = 0;
                self.hold_since = None;
            }
            (false, false) => {}
        }
    }

    /// A third press held for [`FN_TAP_MAX`] (session time `now`) starts the test screen.
    const fn poll(&mut self, now: Duration) {
        if let Some(since) = self.hold_since
            && now.saturating_sub(since).as_nanos() >= FN_TAP_MAX.as_nanos()
        {
            self.hold_since = None;
            self.chain_over = true;
            self.screensaver = !self.touched;
        }
    }

    const fn double_tapped(&self) -> bool {
        self.taps_before_press == 1
    }
}

const fn repeatable(action: StockAction) -> bool {
    matches!(
        action,
        StockAction::StepDisplayBrightness(_)
            | StockAction::StepKeyboardBacklight(_)
            | StockAction::StepVolume(_)
    )
}

fn display_contact(contact: &InputContact) -> Result<DisplayContact, BuiltinRendererError> {
    DisplayContact::new(
        u32::from(contact.id),
        f64::from(contact.x),
        f64::from(contact.y),
    )
    .map_err(|_| BuiltinRendererError::Gesture)
}

const fn action_key(action: StockAction) -> Option<KeyCode> {
    Some(match action {
        StockAction::Escape => KeyCode::Escape,
        StockAction::Function(FunctionKey::F1) => KeyCode::F1,
        StockAction::Function(FunctionKey::F2) => KeyCode::F2,
        StockAction::Function(FunctionKey::F3) => KeyCode::F3,
        StockAction::Function(FunctionKey::F4) => KeyCode::F4,
        StockAction::Function(FunctionKey::F5) => KeyCode::F5,
        StockAction::Function(FunctionKey::F6) => KeyCode::F6,
        StockAction::Function(FunctionKey::F7) => KeyCode::F7,
        StockAction::Function(FunctionKey::F8) => KeyCode::F8,
        StockAction::Function(FunctionKey::F9) => KeyCode::F9,
        StockAction::Function(FunctionKey::F10) => KeyCode::F10,
        StockAction::Function(FunctionKey::F11) => KeyCode::F11,
        StockAction::Function(FunctionKey::F12) => KeyCode::F12,
        StockAction::StepDisplayBrightness(_)
        | StockAction::StepKeyboardBacklight(_)
        | StockAction::ToggleMute
        | StockAction::ToggleKeyboardBacklight
        | StockAction::StepVolume(_)
        | StockAction::MediaPrevious
        | StockAction::MediaPlayPause
        | StockAction::MediaNext
        | StockAction::Extra(_)
        | StockAction::SetLevel(..) => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;

    mod display_power;

    fn dimensions() -> DisplayDimensions {
        DisplayDimensions::new(130, 20).expect("valid synthetic dimensions")
    }

    fn session() -> RendererSession {
        RendererSession::new(
            dimensions(),
            RequestIds { next: 2 },
            StockCapabilities::default(),
        )
        .expect("renderer session")
    }

    fn new_interpreter() -> InputInterpreter {
        let dimensions = dimensions();
        let stock = StockBar::new(
            dimensions.width(),
            dimensions.height(),
            StockCapabilities::default(),
        )
        .unwrap();
        InputInterpreter::new(dimensions, stock).expect("input interpreter")
    }

    fn inert_connection() -> HardwareConnection {
        let (connection, _peer) = UnixStream::pair().expect("synthetic descriptor pair");
        HardwareConnection::new(OwnedFd::from(connection))
    }

    /// `function_row`: whether the frame should show Esc + F1–F12. With the default
    /// `default_row` that is Fn held; upstream's tests are written by row.
    fn input(monotonic_ns: u64, function_row: bool, contacts: Vec<InputContact>) -> WireInputFrame {
        raw_input(monotonic_ns, function_row, contacts)
    }

    fn raw_input(
        monotonic_ns: u64,
        fn_pressed: bool,
        contacts: Vec<InputContact>,
    ) -> WireInputFrame {
        WireInputFrame {
            monotonic_ns,
            fn_pressed,
            contacts,
        }
    }

    fn contact(id: u8, x: u32, y: u32) -> InputContact {
        InputContact {
            id,
            tip: true,
            in_range: true,
            x,
            y,
        }
    }

    #[test]
    fn a_touch_skips_the_intro_without_pressing_anything() {
        let mut session = session();
        session.intro = Some(Intro::new(130, 20));
        let outcome = session
            .observe_input(input(1, false, vec![contact(1, 10, 10)]))
            .unwrap();
        assert!(session.intro.is_none());
        assert!(outcome.actions.is_empty());
        assert!(session.input.wait_for_touch_release);
        session.observe_input(input(2, false, Vec::new())).unwrap();
        assert!(!session.input.wait_for_touch_release);
    }

    #[test]
    fn the_intro_draws_into_a_spare_while_the_primary_buffer_is_in_flight() {
        let mut session = session();
        session.spares.push(SpareBuffer {
            id: 2,
            frame: RendererFrame::new(session.byte_length).unwrap(),
            registered: true,
            in_flight: None,
        });
        session.in_flight = Some(InFlightFrame {
            frame_id: 1,
            acknowledged: true,
        });
        // The stock bar never uses the spare.
        assert_eq!(session.free_slot(), None);
        session.intro = Some(Intro::new(130, 20));
        // Not before a stock frame exists to draw over.
        assert_eq!(session.free_slot(), None);
        session.intro_base = vec![0; 130 * 20 * 4];
        assert_eq!(session.free_slot(), Some(BufferSlot::Spare(0)));
        session.stop_intro("stopped (test)");
        assert_eq!(session.free_slot(), None);
    }

    #[test]
    fn fn_skips_the_intro() {
        let mut session = session();
        session.intro = Some(Intro::new(130, 20));
        session
            .observe_input(raw_input(1, true, Vec::new()))
            .unwrap();
        assert!(session.intro.is_none());
    }

    #[test]
    fn the_intro_ends_by_itself() {
        let mut session = session();
        let mut intro = Intro::new(130, 20);
        intro.draw(&mut vec![0; 130 * 20 * 4], Duration::ZERO);
        session.intro = Some(intro);
        session
            .advance(Duration::from_millis(100), &inert_connection(), None)
            .unwrap();
        assert!(session.intro.is_some());
        session
            .advance(crate::intro::DURATION, &inert_connection(), None)
            .unwrap();
        assert!(session.intro.is_none());
    }

    #[test]
    fn hello_requires_render_input_keys_and_touch_id_cancellation() {
        let encoded = wire::encode_client(
            &Envelope {
                request_id: 1,
                message: ClientMessage::Hello(Hello {
                    minor: wire::PROTOCOL_MINOR,
                    required_features: FeatureBits::INITIAL_SERVICE
                        | FeatureBits::TOUCH_ID_CANCELLATION,
                }),
            },
            None,
        )
        .expect("encode hello");
        let decoded = wire::decode_client(&encoded, AncillaryMetadata::default(), None)
            .expect("decode hello");
        let ClientMessage::Hello(hello) = decoded.message else {
            panic!("hello message expected");
        };
        assert_eq!(hello.required_features.bits(), 0x27);
    }

    #[test]
    fn feature_negotiation_keeps_the_first_complete_connection() {
        let base = FeatureBits::INITIAL_SERVICE | FeatureBits::TOUCH_ID_CANCELLATION;
        let all = base | FeatureBits::DISPLAY_BRIGHTNESS | FeatureBits::KEYBOARD_BACKLIGHT;
        let mut calls = Vec::new();

        let accepted = negotiate_default_features_with(|required| {
            calls.push(required);
            Ok::<_, ()>(Some(required))
        })
        .expect("feature probe succeeds");

        assert_eq!(accepted, Some(all));
        assert_eq!(calls, [all]);
    }

    #[test]
    fn feature_negotiation_discovers_optional_controls_independently() {
        let base = FeatureBits::INITIAL_SERVICE | FeatureBits::TOUCH_ID_CANCELLATION;
        let display = FeatureBits::DISPLAY_BRIGHTNESS;
        let keyboard = FeatureBits::KEYBOARD_BACKLIGHT;
        let supported = base | display;
        let mut calls = Vec::new();

        let accepted = negotiate_default_features_with(|required| {
            calls.push(required);
            Ok::<_, ()>(supported.contains(required).then_some(required))
        })
        .expect("feature probes succeed");

        assert_eq!(accepted, Some(supported));
        assert_eq!(
            calls,
            [
                base | display | keyboard,
                base | display,
                base | keyboard,
                supported
            ]
        );
    }

    #[test]
    fn feature_negotiation_rejects_a_service_missing_the_base_contract() {
        let base = FeatureBits::INITIAL_SERVICE | FeatureBits::TOUCH_ID_CANCELLATION;
        let display = FeatureBits::DISPLAY_BRIGHTNESS;
        let keyboard = FeatureBits::KEYBOARD_BACKLIGHT;
        let mut calls = Vec::new();

        let accepted = negotiate_default_features_with(|required| {
            calls.push(required);
            Ok::<_, ()>(None::<FeatureBits>)
        })
        .expect("unsupported probes are valid responses");

        assert_eq!(accepted, None);
        assert_eq!(
            calls,
            [
                base | display | keyboard,
                base | display,
                base | keyboard,
                base
            ]
        );
    }

    #[test]
    fn negotiated_root_features_enable_only_matching_surface_controls() {
        let base = FeatureBits::INITIAL_SERVICE | FeatureBits::TOUCH_ID_CANCELLATION;
        assert_eq!(stock_capabilities(base, 1), StockCapabilities::default());
        assert_eq!(
            stock_capabilities(base | FeatureBits::DISPLAY_BRIGHTNESS, 1),
            StockCapabilities::DISPLAY_BRIGHTNESS
        );
        assert_eq!(
            stock_capabilities(base | FeatureBits::KEYBOARD_BACKLIGHT, 1),
            StockCapabilities::KEYBOARD_BACKLIGHT
        );
        assert_eq!(
            stock_capabilities(
                base | FeatureBits::DISPLAY_BRIGHTNESS | FeatureBits::KEYBOARD_BACKLIGHT,
                0
            ),
            StockCapabilities::default()
        );
    }

    #[test]
    fn provider_state_changes_only_provider_controls_and_levels() {
        let mut session = RendererSession::new(
            dimensions(),
            RequestIds { next: 2 },
            StockCapabilities::DISPLAY_BRIGHTNESS,
        )
        .expect("renderer session");
        let state = DesktopState {
            capabilities: DesktopCapabilities::AUDIO | DesktopCapabilities::MEDIA,
            volume: Some(61),
            muted: true,
            display_off: false,
        };
        session
            .set_desktop_state(state)
            .expect("apply desktop state");
        assert_eq!(session.desktop_state, state);
        assert_eq!(session.levels.volume, Some(61));
        assert!(session.levels.muted);
        assert_eq!(
            session.stock,
            StockBar::new(
                dimensions().width(),
                dimensions().height(),
                StockCapabilities::DISPLAY_BRIGHTNESS
                    | StockCapabilities::AUDIO
                    | StockCapabilities::MEDIA,
            )
            .unwrap()
        );

        session
            .set_desktop_state(DesktopState::default())
            .expect("withdraw desktop state");
        assert_eq!(session.levels.volume, None);
        assert!(!session.levels.muted);
        assert_eq!(
            session.stock,
            StockBar::new(
                dimensions().width(),
                dimensions().height(),
                StockCapabilities::DISPLAY_BRIGHTNESS,
            )
            .unwrap()
        );
    }

    #[test]
    fn unchanged_provider_state_corrects_optimistic_audio_levels() {
        let mut session = session();
        let state = DesktopState {
            capabilities: DesktopCapabilities::AUDIO,
            volume: Some(42),
            muted: false,
            display_off: false,
        };
        session
            .set_desktop_state(state)
            .expect("apply authoritative audio state");
        session.levels.volume = Some(87);
        session.levels.muted = true;
        session.dirty = false;

        session
            .set_desktop_state(state)
            .expect("reconcile unchanged authoritative state");

        assert_eq!(session.desktop_state, state);
        assert_eq!(session.levels.volume, Some(42));
        assert!(!session.levels.muted);
        assert!(session.dirty);
    }

    #[test]
    fn provider_layout_change_waits_for_active_touch_to_end() {
        let mut session = session();
        session
            .input
            .ingest(&input(1, false, vec![contact(1, 60, 10)]), None)
            .expect("start active touch");
        let state = DesktopState {
            capabilities: DesktopCapabilities::AUDIO,
            volume: Some(50),
            muted: false,
            display_off: false,
        };
        session
            .set_desktop_state(state)
            .expect("defer desktop state");
        assert_eq!(session.desktop_state, DesktopState::default());
        assert_eq!(session.pending_desktop_state, Some(state));

        session
            .input
            .ingest(&input(2, false, Vec::new()), None)
            .expect("finish active touch");
        session
            .apply_pending_desktop_state()
            .expect("apply deferred desktop state");
        assert_eq!(session.desktop_state, state);
        assert_eq!(session.pending_desktop_state, None);
    }

    #[test]
    fn brightness_steps_map_to_typed_wire_directions() {
        assert_eq!(wire_step(StepDirection::Down), WireStepDirection::Down);
        assert_eq!(wire_step(StepDirection::Up), WireStepDirection::Up);
    }

    #[test]
    fn reconnect_backoff_doubles_and_stays_capped() {
        let mut delay = INITIAL_RECONNECT_DELAY;
        let expected = [100, 200, 400, 800, 1_600, 2_000, 2_000];
        for expected_millis in expected {
            delay = next_reconnect_delay(delay);
            assert_eq!(delay, Duration::from_millis(expected_millis));
        }
    }

    #[test]
    fn production_session_renders_initial_escape_and_fn_redraw() {
        let mut session = session();
        assert_eq!(session.stride, 520);
        assert_eq!(session.byte_length, 10_400);
        assert_eq!(session.frame.as_mut_slice().len(), 10_400);
        session.render().expect("render initial Escape frame");
        let initial = session.frame.as_mut_slice().to_vec();
        assert!(initial.iter().any(|byte| *byte != 0));

        session
            .input
            .ingest(&raw_input(1, true, Vec::new()), None)
            .expect("accept Fn transition");
        session.render().expect("render controls row");
        assert_ne!(session.frame.as_mut_slice(), initial);
    }

    #[test]
    fn cosmetic_overlay_transitions_dirty_and_change_the_frame_only_on_state_change() {
        let mut session = session();
        session.render().expect("render stock frame");
        let stock = session.frame.as_mut_slice().to_vec();
        session.dirty = false;

        session.set_overlay(Some(OverlayState::Enrollment), Some(0));
        assert!(session.dirty);
        session.advance_overlay_fade(OVERLAY_FADE_DURATION);
        session.render().expect("render enrollment overlay");
        let enrollment = session.frame.as_mut_slice().to_vec();
        assert_ne!(enrollment, stock);

        session.dirty = false;
        session.set_overlay(Some(OverlayState::Enrollment), Some(0));
        assert!(!session.dirty);
        session.set_overlay(Some(OverlayState::Success), None);
        assert!(session.dirty);
        session.render().expect("render success overlay");
        assert_ne!(session.frame.as_mut_slice(), enrollment);

        session.dirty = false;
        session.now = OVERLAY_FADE_DURATION;
        session.set_overlay(None, None);
        assert!(session.dirty);
        session.advance_overlay_fade(OVERLAY_FADE_DURATION.saturating_mul(2));
        session.render().expect("remove cosmetic overlay");
        assert_eq!(session.frame.as_mut_slice(), stock);
    }

    #[test]
    fn enrollment_progress_animates_to_each_bounded_target() {
        let mut session = session();
        session.set_overlay(Some(OverlayState::Enrollment), Some(0));
        session.advance_overlay_fade(OVERLAY_FADE_DURATION);
        session.render().expect("render empty progress");
        let empty = session.frame.as_mut_slice().to_vec();

        session.dirty = false;
        session.now = OVERLAY_FADE_DURATION;
        session.set_overlay(Some(OverlayState::Enrollment), Some(60));
        assert!(session.dirty);
        session
            .advance_enrollment_progress(OVERLAY_FADE_DURATION + ENROLLMENT_PROGRESS_DURATION / 2);
        assert!((1..600).contains(&session.enrollment_progress_visual));
        session.render().expect("render partial progress");
        assert_ne!(session.frame.as_mut_slice(), empty);

        session.advance_enrollment_progress(OVERLAY_FADE_DURATION + ENROLLMENT_PROGRESS_DURATION);
        assert_eq!(session.enrollment_progress_visual, 600);
    }

    #[test]
    fn overlay_crossfade_cancels_hidden_control_repeats() {
        let mut session = RendererSession::new(
            dimensions(),
            RequestIds { next: 2 },
            StockCapabilities::DISPLAY_BRIGHTNESS,
        )
        .expect("renderer session");
        session.input.repeat = Some(RepeatState {
            action: StockAction::StepDisplayBrightness(StepDirection::Down),
            next_at: HOLD_REPEAT_DELAY,
            fired: false,
        });
        session.input.pressed = Some(StockAction::StepDisplayBrightness(StepDirection::Down));

        session.set_overlay(Some(OverlayState::Authenticate), None);
        assert_eq!(session.input.repeat, None);
        session.advance_overlay_fade(OVERLAY_FADE_DURATION / 2);
        assert!(session.overlay_opacity > 0 && session.overlay_opacity < u8::MAX);
        session.advance_overlay_fade(OVERLAY_FADE_DURATION);
        assert_eq!(session.overlay_opacity, u8::MAX);

        session.render().expect("render opaque overlay");
        let hidden_control_offset = 20 * 4;
        assert_eq!(
            &session.frame.as_mut_slice()[hidden_control_offset..hidden_control_offset + 4],
            &[0, 0, 0, 0]
        );

        session.now = OVERLAY_FADE_DURATION;
        session.set_overlay(None, None);
        session.advance_overlay_fade(OVERLAY_FADE_DURATION + OVERLAY_FADE_DURATION / 2);
        assert!(session.overlay_opacity > 0 && session.overlay_opacity < u8::MAX);
        session.advance_overlay_fade(OVERLAY_FADE_DURATION.saturating_mul(2));
        assert_eq!(session.overlay_opacity, 0);
        assert_eq!(session.overlay_visual_state, None);
    }

    #[test]
    fn input_socket_routes_one_cancel_and_consumes_ack_without_release_repeat() {
        let (renderer, hardware) =
            t1_platform::seqpacket::pair_for_test().expect("local hardware protocol socket pair");
        let mut connection = HardwareConnection::new(renderer);
        let hardware = SeqPacketClient::new(hardware.as_fd());
        let mut session = session();
        session.set_overlay(Some(OverlayState::Authenticate), None);
        let mut packet = vec![0; MAX_PACKET_LENGTH];

        for (timestamp, contacts) in [
            (1, vec![contact(1, 60, 10)]),
            (2, vec![contact(1, 60, 10)]),
            (3, Vec::new()),
        ] {
            let frame = wire::encode_service(
                &Envelope {
                    request_id: 0,
                    message: ServiceMessage::InputFrame(input(timestamp, false, contacts)),
                },
                Some(dimensions()),
            )
            .expect("encode input frame");
            hardware.send(&frame).expect("deliver input frame");
            let frame = connection
                .receive_service(Some(dimensions()))
                .expect("receive input frame");
            session.handle(frame, &connection).expect("dispatch input");

            if timestamp != 1 {
                assert_eq!(
                    hardware.receive(&mut packet),
                    Err(SeqPacketError::WouldBlock)
                );
                assert!(session.pending.is_empty());
                continue;
            }
            let length = hardware.receive(&mut packet).expect("receive cancellation");
            let request = wire::decode_client(
                &packet[..length],
                AncillaryMetadata::default(),
                Some(dimensions()),
            )
            .expect("decode cancellation");
            assert!(matches!(request.message, ClientMessage::CancelTouchId));
            assert_eq!(session.pending.len(), 1);
            let ack = wire::encode_service(
                &Envelope {
                    request_id: request.request_id,
                    message: ServiceMessage::Ack,
                },
                Some(dimensions()),
            )
            .expect("encode acknowledgement");
            hardware.send(&ack).expect("deliver acknowledgement");
            let ack = connection
                .receive_service(Some(dimensions()))
                .expect("receive acknowledgement");
            session
                .handle(ack, &connection)
                .expect("consume acknowledgement");
            assert!(session.pending.is_empty());
            assert_eq!(session.overlay_state, Some(OverlayState::Authenticate));
        }
    }

    #[test]
    fn cancellation_request_is_typed_and_ack_is_cosmetic_neutral() {
        let mut session = session();
        session.set_overlay(Some(OverlayState::Authenticate), None);
        let request = cancellation_request(71);
        let packet =
            wire::encode_client(&request, Some(dimensions())).expect("encode typed cancellation");
        let request =
            wire::decode_client(&packet, AncillaryMetadata::default(), Some(dimensions()))
                .expect("decode typed cancellation");
        assert!(matches!(request.message, ClientMessage::CancelTouchId));

        let connection = inert_connection();
        session
            .pending
            .insert(request.request_id, PendingRequest::CancelTouchId);
        session
            .handle(
                Envelope {
                    request_id: request.request_id,
                    message: ServiceMessage::Ack,
                },
                &connection,
            )
            .expect("cancellation delivery acknowledgement");
        assert_eq!(session.overlay_state, Some(OverlayState::Authenticate));
        assert!(!session.pending.contains_key(&request.request_id));
    }

    #[test]
    fn submit_ack_release_and_typed_error_complete_owned_state() {
        let mut session = session();
        let connection = inert_connection();
        session.pending.insert(
            7,
            PendingRequest::Submit {
                frame_id: FIRST_FRAME_ID,
            },
        );
        session.in_flight = Some(InFlightFrame {
            frame_id: FIRST_FRAME_ID,
            acknowledged: false,
        });
        session.dirty = false;

        session
            .handle(
                Envelope {
                    request_id: 7,
                    message: ServiceMessage::Ack,
                },
                &connection,
            )
            .expect("accept matching submit Ack");
        assert_eq!(
            session.in_flight,
            Some(InFlightFrame {
                frame_id: FIRST_FRAME_ID,
                acknowledged: true,
            })
        );
        session
            .handle(
                Envelope {
                    request_id: 0,
                    message: ServiceMessage::FrameReleased(wire::FrameReleased {
                        buffer_id: BUFFER_ID,
                        frame_id: FIRST_FRAME_ID,
                    }),
                },
                &connection,
            )
            .expect("accept matching frame release");
        assert_eq!(session.in_flight, None);

        session.pending.insert(8, PendingRequest::TapKeys);
        session
            .handle(
                Envelope {
                    request_id: 8,
                    message: ServiceMessage::Error(ErrorCode::ActionDenied),
                },
                &connection,
            )
            .expect("consume typed key rejection without reconnecting");
        assert!(!session.pending.contains_key(&8));

        session.overlay_state = Some(OverlayState::Retry);
        session.pending.insert(10, PendingRequest::CancelTouchId);
        session
            .handle(
                Envelope {
                    request_id: 10,
                    message: ServiceMessage::Error(ErrorCode::ActionDenied),
                },
                &connection,
            )
            .expect("consume typed cancellation rejection without reconnecting");
        assert!(!session.pending.contains_key(&10));
        assert_eq!(session.overlay_state, Some(OverlayState::Retry));

        session.pending.insert(9, PendingRequest::Register);
        assert_eq!(
            session.handle(
                Envelope {
                    request_id: 9,
                    message: ServiceMessage::Error(ErrorCode::InvalidBuffer),
                },
                &connection,
            ),
            Err(BuiltinRendererError::ServiceRejected)
        );
    }

    #[test]
    fn mismatched_release_is_a_hard_protocol_failure() {
        let mut session = session();
        let connection = inert_connection();
        session.in_flight = Some(InFlightFrame {
            frame_id: 4,
            acknowledged: true,
        });

        assert_eq!(
            session.handle(
                Envelope {
                    request_id: 0,
                    message: ServiceMessage::FrameReleased(wire::FrameReleased {
                        buffer_id: BUFFER_ID,
                        frame_id: 5,
                    }),
                },
                &connection,
            ),
            Err(BuiltinRendererError::Protocol)
        );
    }

    #[test]
    fn fn_transition_redraws_and_completed_tap_maps_to_typed_function_key() {
        let mut interpreter = new_interpreter();
        assert_eq!(
            interpreter
                .ingest(&raw_input(1, true, Vec::new()), None)
                .expect("Fn transition"),
            InputOutcome {
                redraw: true,
                actions: Vec::new(),
                cancel_touch_id: false,
            }
        );
        assert!(interpreter.fn_pressed());
        assert_eq!(
            interpreter
                .ingest(&input(2, true, vec![contact(4, 15, 8)]), None)
                .expect("function press")
                .actions,
            []
        );
        assert_eq!(
            interpreter
                .ingest(&input(3, true, Vec::new()), None)
                .expect("function release")
                .actions,
            [StockAction::Function(FunctionKey::F1)]
        );
    }

    #[test]
    fn double_tap_fn_shows_extras_until_the_next_press() {
        const MS: u64 = 1_000_000;
        let fn_sequence = |presses: &[(u64, bool, bool)]| {
            let mut interpreter = new_interpreter();
            for &(at, fn_pressed, touching) in presses {
                let contacts = if touching {
                    vec![contact(1, 1_000, 30)]
                } else {
                    Vec::new()
                };
                interpreter
                    .ingest(&raw_input(at * MS, fn_pressed, contacts), None)
                    .expect("fn frame");
            }
            interpreter.layer()
        };
        // Tap, then press again quickly.
        let quick = [(10, true, false), (100, false, false), (250, true, false)];
        assert_eq!(fn_sequence(&quick), Layer::Extras);
        // The row stays after the second release; the next Fn press shows the controls
        // while held, and the F-key row after.
        let mut released = quick.to_vec();
        released.push((900, false, false));
        assert_eq!(fn_sequence(&released), Layer::Extras);
        released.push((2_000, true, false));
        assert_eq!(fn_sequence(&released), Layer::from_fn(true, false, false));
        released.push((2_500, false, false));
        assert_eq!(fn_sequence(&released), Layer::from_fn(false, false, false));
        // Too slow between taps, first press too long, or the first press used the bar.
        assert_eq!(
            fn_sequence(&[(10, true, false), (100, false, false), (600, true, false)]),
            Layer::from_fn(true, false, false)
        );
        assert_eq!(
            fn_sequence(&[(10, true, false), (700, false, false), (800, true, false)]),
            Layer::from_fn(true, false, false)
        );
        assert_eq!(
            fn_sequence(&[
                (10, true, false),
                (50, true, true),
                (100, false, false),
                (200, true, false)
            ]),
            Layer::from_fn(true, false, false)
        );
        // A third quick press after an extras hold doesn't chain into extras again
        // (it starts the screensaver; see the next test).
        assert_eq!(
            fn_sequence(&[
                (10, true, false),
                (100, false, false),
                (200, true, false),
                (300, false, false),
                (400, true, false),
            ]),
            Layer::from_fn(true, false, false)
        );
    }

    /// Feeds Fn presses (ms, Fn down, touching) and counts test-screen and theme requests,
    /// polling the hold every 10 ms as the session does.
    fn debug_requests(debug: bool, presses: &[(u64, bool, bool)]) -> (u8, u8) {
        const MS: u64 = 1_000_000;
        let mut interpreter = new_interpreter();
        interpreter.set_debug_gestures(debug);
        let (mut screens, mut themes) = (0, 0);
        let end = presses.last().map_or(0, |press| press.0) + 1_000;
        let mut next = presses.iter().peekable();
        for ms in (0..=end).step_by(10) {
            while let Some(&&(at, fn_pressed, touching)) = next.peek()
                && at <= ms
            {
                let contacts = if touching {
                    vec![contact(1, 1_000, 30)]
                } else {
                    Vec::new()
                };
                interpreter
                    .ingest(&raw_input(at * MS, fn_pressed, contacts), None)
                    .expect("fn frame");
                next.next();
            }
            let now = Duration::from_millis(ms);
            screens += u8::from(interpreter.take_screensaver_request(now));
            themes += u8::from(interpreter.take_theme_request());
        }
        (screens, themes)
    }

    #[test]
    fn triple_tap_and_hold_fn_shows_the_test_screen_with_debug_gestures_only() {
        // Three presses, the third held.
        let held = [
            (10, true, false),
            (100, false, false),
            (200, true, false),
            (300, false, false),
            (400, true, false),
            (1_200, false, false),
        ];
        assert_eq!(debug_requests(true, &held), (1, 0));
        assert_eq!(debug_requests(false, &held), (0, 0));
        // A quick third tap is no test screen (it may become a quad-tap).
        let quick = [
            (10, true, false),
            (100, false, false),
            (200, true, false),
            (300, false, false),
            (400, true, false),
            (480, false, false),
        ];
        assert_eq!(debug_requests(true, &quick), (0, 0));
        // The second press used the bar, or the third came too late.
        let touched = [
            (10, true, false),
            (100, false, false),
            (200, true, true),
            (300, false, false),
            (400, true, false),
            (1_200, false, false),
        ];
        assert_eq!(debug_requests(true, &touched), (0, 0));
        let late = [
            (10, true, false),
            (100, false, false),
            (200, true, false),
            (300, false, false),
            (700, true, false),
            (1_500, false, false),
        ];
        assert_eq!(debug_requests(true, &late), (0, 0));
    }

    #[test]
    fn quad_tap_fn_asks_for_the_next_theme_with_debug_gestures_only() {
        let quad = [
            (10, true, false),
            (100, false, false),
            (200, true, false),
            (300, false, false),
            (400, true, false),
            (480, false, false),
            (560, true, false),
            (640, false, false),
        ];
        assert_eq!(debug_requests(true, &quad), (0, 1));
        assert_eq!(debug_requests(false, &quad), (0, 0));
        // Two quad-taps in a row: two themes, no test screen.
        let mut twice = quad.to_vec();
        twice.extend(
            quad.iter()
                .map(|&(at, down, touching)| (at + 1_000, down, touching)),
        );
        assert_eq!(debug_requests(true, &twice), (0, 2));
    }

    #[test]
    fn escape_remains_active_without_fn_and_function_regions_do_not() {
        let mut interpreter = new_interpreter();
        interpreter
            .ingest(&input(1, false, vec![contact(1, 4, 8)]), None)
            .expect("Escape press");
        assert_eq!(
            interpreter
                .ingest(&input(2, false, Vec::new()), None)
                .expect("Escape release")
                .actions,
            [StockAction::Escape]
        );
        interpreter
            .ingest(&input(3, false, vec![contact(2, 15, 8)]), None)
            .expect("inactive function press");
        assert!(
            interpreter
                .ingest(&input(4, false, Vec::new()), None)
                .expect("inactive function release")
                .actions
                .is_empty()
        );
    }

    #[test]
    fn drag_does_not_emit_a_key_and_duplicate_slot_taps_are_deduplicated() {
        let mut interpreter = new_interpreter();
        interpreter
            .ingest(&input(1, true, vec![contact(1, 15, 8)]), None)
            .expect("Fn press");
        interpreter
            .ingest(&input(2, true, vec![contact(1, 80, 8)]), None)
            .expect("drag");
        assert!(
            interpreter
                .ingest(&input(3, true, Vec::new()), None)
                .expect("drag release")
                .actions
                .is_empty()
        );

        interpreter
            .ingest(
                &input(4, true, vec![contact(2, 15, 8), contact(3, 16, 8)]),
                None,
            )
            .expect("two presses");
        assert_eq!(
            interpreter
                .ingest(&input(5, true, Vec::new()), None)
                .expect("two releases")
                .actions,
            [StockAction::Function(FunctionKey::F1)]
        );
    }

    #[test]
    fn cancellation_on_press_deduplicates_contacts_and_never_repeats_on_release() {
        let active = Some(OverlayState::Authenticate);
        let mut interpreter = new_interpreter();
        let outcome = interpreter
            .ingest(
                &input(1, false, vec![contact(1, 24, 10), contact(2, 25, 10)]),
                active,
            )
            .expect("two overlay presses");
        assert!(outcome.cancel_touch_id);
        assert!(outcome.actions.is_empty());
        let outcome = interpreter
            .ingest(&input(2, false, Vec::new()), active)
            .expect("two overlay releases");
        assert!(!outcome.cancel_touch_id);
        assert!(outcome.actions.is_empty());

        let mut absent = new_interpreter();
        absent
            .ingest(&input(1, false, vec![contact(1, 24, 10)]), None)
            .expect("press without overlay");
        assert!(
            !absent
                .ingest(&input(2, false, Vec::new()), None)
                .expect("release without overlay")
                .cancel_touch_id
        );

        let mut label = new_interpreter();
        assert!(
            label
                .ingest(&input(1, false, vec![contact(1, 60, 10)]), active)
                .expect("label press")
                .cancel_touch_id
        );
        assert!(
            !label
                .ingest(&input(2, false, Vec::new()), active)
                .expect("label release")
                .cancel_touch_id
        );

        let mut dragged = new_interpreter();
        assert!(
            dragged
                .ingest(&input(1, false, vec![contact(1, 24, 10)]), active)
                .expect("overlay press")
                .cancel_touch_id
        );
        dragged
            .ingest(&input(2, false, vec![contact(1, 80, 10)]), active)
            .expect("overlay drag");
        assert!(
            !dragged
                .ingest(&input(3, false, Vec::new()), active)
                .expect("drag release")
                .cancel_touch_id
        );

        let mut function_row = new_interpreter();
        assert!(
            function_row
                .ingest(&input(1, true, vec![contact(1, 80, 8)]), active)
                .expect("function press")
                .cancel_touch_id
        );
        let outcome = function_row
            .ingest(&input(2, true, Vec::new()), active)
            .expect("function release");
        assert!(!outcome.cancel_touch_id);
        assert!(outcome.actions.is_empty());
    }

    #[test]
    fn held_contact_cannot_cancel_a_new_overlay_and_escape_stays_available() {
        let mut interpreter = new_interpreter();
        let held = vec![contact(1, 80, 10)];
        assert!(
            interpreter
                .ingest(
                    &input(1, false, held.clone()),
                    Some(OverlayState::Authenticate)
                )
                .unwrap()
                .cancel_touch_id
        );
        assert!(
            !interpreter
                .ingest(&input(2, false, held.clone()), None)
                .unwrap()
                .cancel_touch_id
        );
        assert!(
            !interpreter
                .ingest(&input(3, false, held), Some(OverlayState::Approve))
                .unwrap()
                .cancel_touch_id
        );
        assert!(
            !interpreter
                .ingest(&input(4, false, Vec::new()), Some(OverlayState::Approve))
                .unwrap()
                .cancel_touch_id
        );

        let outcome = interpreter
            .ingest(
                &input(5, false, vec![contact(2, 4, 10)]),
                Some(OverlayState::Authenticate),
            )
            .unwrap();
        assert!(!outcome.cancel_touch_id);
        let outcome = interpreter
            .ingest(
                &input(6, false, Vec::new()),
                Some(OverlayState::Authenticate),
            )
            .unwrap();
        assert!(!outcome.cancel_touch_id);
        assert_eq!(outcome.actions, [StockAction::Escape]);
    }

    #[test]
    fn the_keyboard_icon_turns_the_backlight_off_and_back_to_its_level() {
        assert_eq!(keyboard_toggle(40, None), (0, Some(40)));
        assert_eq!(keyboard_toggle(0, Some(40)), (40, Some(40)));
        // Never set above 0 here (say it was off at start): a sensible default.
        assert_eq!(keyboard_toggle(0, None), (KEYBOARD_ON_DEFAULT, None));
    }

    #[test]
    fn sliders_set_the_level_under_the_finger_and_the_volume_icon_mutes() {
        let dimensions = DisplayDimensions::new(2_170, 60).unwrap();
        let all = StockCapabilities::DISPLAY_BRIGHTNESS
            | StockCapabilities::KEYBOARD_BACKLIGHT
            | StockCapabilities::AUDIO
            | StockCapabilities::MEDIA;
        let stock = StockBar::new(2_170, 60, all).unwrap();
        let mut interpreter = InputInterpreter::new(dimensions, stock).unwrap();
        let mut ms = 0;
        let mut at = |interpreter: &mut InputInterpreter, x: Option<u32>| {
            ms += 10;
            let contacts = x.map(|x| vec![contact(1, x, 30)]).unwrap_or_default();
            interpreter
                .ingest_at(&input(ms, false, contacts), None, Duration::from_millis(ms))
                .unwrap()
                .actions
        };
        let level = |kind, level| vec![StockAction::SetLevel(kind, level)];
        // Volume plate 1608..2170: the touch sets the level, dragging follows, past the
        // end clamps; release does nothing more.
        assert_eq!(
            at(&mut interpreter, Some(1_608 + 281)),
            level(SliderKind::Volume, 50)
        );
        assert_eq!(
            at(&mut interpreter, Some(1_608 + 562 / 4)),
            level(SliderKind::Volume, 25)
        );
        assert_eq!(
            at(&mut interpreter, Some(100)),
            level(SliderKind::Volume, 0)
        );
        assert_eq!(at(&mut interpreter, None), []);
        // The speaker icon is a mute button (only if released on it).
        assert_eq!(at(&mut interpreter, Some(1_620)), []);
        assert_eq!(at(&mut interpreter, None), [StockAction::ToggleMute]);
        assert_eq!(at(&mut interpreter, Some(1_620)), []);
        assert_eq!(at(&mut interpreter, Some(1_900)), []);
        assert_eq!(at(&mut interpreter, None), []);
        // Display brightness 194..566 and keyboard 602..974.
        assert_eq!(
            at(&mut interpreter, Some(194 + 93)),
            level(SliderKind::Brightness, 25)
        );
        assert_eq!(at(&mut interpreter, None), []);
        assert_eq!(
            at(&mut interpreter, Some(602 + 186)),
            level(SliderKind::Keyboard, 50)
        );
        assert_eq!(at(&mut interpreter, None), []);
        // The keyboard icon turns the backlight off and on (only if released on it); the
        // brightness icon is just the low end of its slider.
        assert_eq!(at(&mut interpreter, Some(614)), []);
        assert_eq!(
            at(&mut interpreter, None),
            [StockAction::ToggleKeyboardBacklight]
        );
        assert_eq!(at(&mut interpreter, Some(614)), []);
        assert_eq!(at(&mut interpreter, Some(800)), []);
        assert_eq!(at(&mut interpreter, None), []);
        assert_eq!(
            at(&mut interpreter, Some(200)),
            level(SliderKind::Brightness, 2)
        );
        assert_eq!(at(&mut interpreter, None), []);
        // Media buttons are still buttons.
        assert_eq!(at(&mut interpreter, Some(1_100)), []);
        assert_eq!(at(&mut interpreter, None), [StockAction::MediaPrevious]);
    }

    #[test]
    fn volume_reports_do_not_pull_back_a_slider_just_set() {
        let mut session = session();
        let connection = inert_connection();
        let reported = |volume| DesktopState {
            capabilities: DesktopCapabilities::AUDIO,
            volume: Some(volume),
            muted: false,
            display_off: false,
        };
        session.set_desktop_state(reported(70)).unwrap();
        assert_eq!(session.levels.volume, Some(70));
        session.now = Duration::from_secs(10);
        session
            .dispatch(
                StockAction::SetLevel(SliderKind::Volume, 30),
                &connection,
                None,
            )
            .unwrap();
        // A lagging report during the drag (or just after) is ignored...
        session.set_desktop_state(reported(55)).unwrap();
        assert_eq!(session.levels.volume, Some(30));
        // ...and a report once it has settled is shown.
        session.now = Duration::from_secs(10) + LIGHT_SETTLE;
        session.set_desktop_state(reported(31)).unwrap();
        assert_eq!(session.levels.volume, Some(31));
    }

    #[test]
    fn request_ids_skip_zero_and_pending_values_after_wrap() {
        let mut ids = RequestIds { next: u32::MAX };
        let mut pending = BTreeMap::new();
        pending.insert(u32::MAX, PendingRequest::TapKeys);

        assert_eq!(ids.next(&pending), Ok(1));
        assert_eq!(ids.next(&pending), Ok(2));
    }

    #[test]
    fn every_stock_action_has_the_exact_wire_key() {
        let expected = [
            (StockAction::Escape, KeyCode::Escape),
            (StockAction::Function(FunctionKey::F1), KeyCode::F1),
            (StockAction::Function(FunctionKey::F2), KeyCode::F2),
            (StockAction::Function(FunctionKey::F3), KeyCode::F3),
            (StockAction::Function(FunctionKey::F4), KeyCode::F4),
            (StockAction::Function(FunctionKey::F5), KeyCode::F5),
            (StockAction::Function(FunctionKey::F6), KeyCode::F6),
            (StockAction::Function(FunctionKey::F7), KeyCode::F7),
            (StockAction::Function(FunctionKey::F8), KeyCode::F8),
            (StockAction::Function(FunctionKey::F9), KeyCode::F9),
            (StockAction::Function(FunctionKey::F10), KeyCode::F10),
            (StockAction::Function(FunctionKey::F11), KeyCode::F11),
            (StockAction::Function(FunctionKey::F12), KeyCode::F12),
        ];
        for (action, key) in expected {
            assert_eq!(action_key(action), Some(key));
        }
    }
}
