//! yantrik-switcher — Alt+Tab, drawn (#867).
//!
//! labwc owns the window switcher session: the Alt+Tab chord, the order, the selection and the
//! commit when Alt comes up (packaging/labwc, `zlabwc_window_switcher_renderer_v1`). labwc
//! launches this program itself on a pre-connected socket and offers the protocol to it alone.
//! This only draws the session and, for a typed letter, asks labwc to move the selection.
//! It never takes the keyboard or the pointer, and it can neither focus nor raise a window.
//!
//! A session goes: `session_start`, the windows, `windows_done`, then `selected` as Tab moves,
//! then `ended`. The stack is drawn only if the session is still open `SHOW_AFTER` later, so a
//! quick Alt+Tab switches back with nothing drawn at all; the session is acked once its first
//! frame is on the screen. If that does not happen in labwc's time (it fails to draw, hangs or
//! dies), labwc draws its own OSD for the session.
//!
//! Pictures (protocol version 2): each card shows its window as labwc last drew it. They are
//! asked for when the list arrives, and the front card's again every `LIVE_EVERY` while the
//! stack is up, so it is a live picture; the others every `LIVE_EVERY * BACK_EVERY`. A card
//! with no picture yet (labwc too old, nothing drawn, or still on its way) shows the app's tile.

use std::collections::HashMap;
use std::io::Read;
use std::rc::Rc;
use std::time::Duration;

use slint::platform::software_renderer::{MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType, TargetPixel};
use slint::platform::{Platform, WindowAdapter};
use slint::{ComponentHandle, Image, ModelRc, Rgba8Pixel, SharedPixelBuffer, VecModel};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_layer, delegate_output, delegate_registry, delegate_shm,
    output::{OutputHandler, OutputState},
    reexports::{
        calloop::{
            timer::{TimeoutAction, Timer},
            EventLoop, LoopHandle,
        },
        calloop_wayland_source::WaylandSource,
    },
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    shell::{
        wlr_layer::{Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface, LayerSurfaceConfigure},
        WaylandSurface,
    },
    shm::{raw::RawPool, Shm, ShmHandler},
};
use wayland_protocols::wp::viewporter::client::{wp_viewport::WpViewport, wp_viewporter::WpViewporter};
use wayland_client::{
    globals::registry_queue_init,
    protocol::{wl_buffer, wl_output, wl_region, wl_shm, wl_surface},
    Connection, Dispatch, Proxy, QueueHandle, WEnum,
};

slint::include_modules!();

mod model;

/// Bindings for labwc's protocol, generated from the one copy of its XML in packaging/labwc.
#[allow(dead_code, unused_imports, non_upper_case_globals, non_camel_case_types, clippy::all)]
mod protocol {
    use wayland_client;
    use wayland_client::protocol::*;
    pub mod __interfaces {
        use wayland_client::protocol::__interfaces::*;
        wayland_scanner::generate_interfaces!("../../packaging/labwc/labwc-window-switcher-renderer-unstable-v1.xml");
    }
    use self::__interfaces::*;
    wayland_scanner::generate_client_code!("../../packaging/labwc/labwc-window-switcher-renderer-unstable-v1.xml");
}

use protocol::zlabwc_window_switcher_renderer_v1::{self as zr, ZlabwcWindowSwitcherRendererV1};

/// A quick Alt+Tab is over before this, and draws nothing.
const SHOW_AFTER: Duration = Duration::from_millis(100);
/// Most a person types into the jump box.
const MOST_TYPED: usize = 24;
/// The frame the design was drawn in; the output is scaled from it.
const DESIGN: (f32, f32) = (1280.0, 800.0);
/// Tile colours, the dock's family (blue, teal, green, amber, red, sky, violet, gold).
const TILES: [u32; 8] = [0x5b9be8, 0x2bb5a6, 0x4bbf6b, 0xe0a94a, 0xe5626a, 0x7cc4f0, 0x9b7be8, 0xd9c27a];

/// How quickly a card follows its place (the spring's natural frequency, radians a second).
/// Critically damped: it arrives without overshoot in about 4 / STIFFNESS seconds, and a new
/// target mid-way only bends its path, so held Tab streams the cards one after another.
const STIFFNESS: f32 = 22.0;
/// How long a card that re-enters at the back of the stack takes to fade in.
const APPEAR: f32 = 0.16;
/// A frame while cards glide (about 60 a second).
const FRAME: Duration = Duration::from_millis(16);
/// While cards glide, frames are drawn at this fraction of the output's size and the compositor
/// scales them up (wp_viewporter): motion hides the softness, and a quarter of the pixels is
/// what lets the software renderer keep FRAME. The frame they land on is drawn at full size.
const GLIDE_SCALE: u32 = 2;
/// How often the front card's picture is asked for again while the stack is up.
const LIVE_EVERY: Duration = Duration::from_millis(250);
/// The cards behind it, every this many of those.
const BACK_EVERY: u32 = 4;
/// The picture size asked for: the front card's body at the design size; labwc keeps the
/// window's aspect ratio inside it.
const PICTURE: (u32, u32) = (640, 360);
/// The largest picture labwc sends (its THUMB_MAX), so a bad size is refused before it is read.
const PICTURE_MOST: u32 = 1024;

const FLAG_CURRENT: u32 = 1;
const FLAG_MINIMIZED: u32 = 2;
const FLAG_FOCUSED: u32 = 4;

/// `YANTRIK_SWITCHER_DEBUG=1`: say each step of a session on stderr (labwc's log).
fn debug(what: std::fmt::Arguments) {
    if std::env::var_os("YANTRIK_SWITCHER_DEBUG").is_some() {
        eprintln!("yantrik-switcher: {what}");
    }
}

/// One pixel as the compositor takes it: wl_shm ARGB8888, little-endian (B, G, R, A),
/// premultiplied. Slint paints straight into the shared buffer in this order, so a frame needs no
/// second pass to reorder its bytes.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Bgra {
    b: u8,
    g: u8,
    r: u8,
    a: u8,
}

impl TargetPixel for Bgra {
    fn blend(&mut self, c: PremultipliedRgbaColor) {
        let k = (u8::MAX - c.alpha) as u16;
        self.r = (self.r as u16 * k / 255) as u8 + c.red;
        self.g = (self.g as u16 * k / 255) as u8 + c.green;
        self.b = (self.b as u16 * k / 255) as u8 + c.blue;
        self.a = (self.a as u16 + c.alpha as u16 - (self.a as u16 * c.alpha as u16) / 255) as u8;
    }
    fn from_rgb(r: u8, g: u8, b: u8) -> Self {
        Bgra { b, g, r, a: 255 }
    }
    fn background() -> Self {
        Bgra::default()
    }
}

/// One of the two shared buffers frames alternate between. Slint repaints only what changed
/// since the buffer was last drawn (SwappedBuffers), and a buffer is drawn into again only once
/// the compositor has let go of it.
struct Frame {
    pool: RawPool,
    buffer: wl_buffer::WlBuffer,
    busy: bool,
}

/// The two buffers of one size; frames at full size and while gliding each have their own.
#[derive(Default)]
struct Frames {
    size: (u32, u32),
    frames: Vec<Frame>,
    next: usize,
}

struct Headless(Rc<MinimalSoftwareWindow>);

impl Platform for Headless {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
}

/// One switcher session, as labwc tells it.
struct Session {
    id: u32,
    windows: Vec<model::Window>,
    incoming: Vec<model::Window>,
    collecting: bool,
    selected: Option<u32>,
    acked: bool,
    scheduled: bool,
    shown: bool,
    typed: String,
    started: std::time::Instant,
    /// Pictures by window id, as labwc last sent them this session.
    pictures: HashMap<u32, Image>,
    live: bool,
    /// Each card as drawn: where it is, how fast it is going and where it is going (depths).
    springs: HashMap<u32, model::Spring>,
    /// The frame loop is running: cards are moving.
    animating: bool,
}

impl Session {
    fn new(id: u32) -> Session {
        Session { id, windows: Vec::new(), incoming: Vec::new(), collecting: false, selected: None, acked: false, scheduled: false, shown: false, typed: String::new(), started: std::time::Instant::now(), pictures: HashMap::new(), live: false, springs: HashMap::new(), animating: false }
    }

    fn selected_index(&self) -> usize {
        self.selected.and_then(|id| self.windows.iter().position(|w| w.id == id)).unwrap_or(0)
    }

    /// Each window's depth as it is drawn now, and how visible it is (1, or less while it
    /// fades in at the back).
    fn drawn(&self) -> Vec<(f32, f32)> {
        let (n, sel) = (self.windows.len(), self.selected_index());
        self.windows
            .iter()
            .enumerate()
            .map(|(i, w)| match self.springs.get(&w.id) {
                Some(sp) => (sp.pos, sp.appear),
                None => (model::depth(i, sel, n) as f32, 1.0),
            })
            .collect()
    }

    /// The list (again): every card starts where it belongs, a card already on screen stays
    /// where it is drawn and moves on from there.
    fn place_springs(&mut self) {
        let (n, sel) = (self.windows.len(), self.selected_index());
        let mut springs = HashMap::new();
        for (i, w) in self.windows.iter().enumerate() {
            let target = model::depth(i, sel, n) as f32;
            let mut sp = self.springs.get(&w.id).copied().unwrap_or(model::Spring::at(target));
            sp.target = target;
            springs.insert(w.id, sp);
        }
        self.springs = springs;
    }

    /// Move the selection. Every card's target moves; the card that goes from the front to the
    /// back of the list leaves forward, past the viewer, and comes back in at the back; the one
    /// coming from the back to the front arrives from in front.
    fn select(&mut self, id: u32) {
        let old = self.selected_index();
        self.selected = Some(id);
        let (n, sel) = (self.windows.len(), self.selected_index());
        if n == 0 {
            return;
        }
        let animate = self.shown;
        for (i, w) in self.windows.iter().enumerate() {
            let target = model::depth(i, sel, n) as f32;
            let was = model::depth(i, old, n) as f32;
            let sp = self.springs.entry(w.id).or_insert(model::Spring::at(was));
            if !animate {
                *sp = model::Spring::at(target);
                continue;
            }
            sp.retarget(target, n);
        }
    }
}

struct App {
    qh: QueueHandle<App>,
    compositor: CompositorState,
    layer_shell: LayerShell,
    shm: Shm,
    outputs: OutputState,
    registry: RegistryState,
    renderer: ZlabwcWindowSwitcherRendererV1,
    session: Option<Session>,
    layer: Option<(LayerSurface, (u32, u32))>,
    loop_handle: LoopHandle<'static, App>,
    window: Rc<MinimalSoftwareWindow>,
    view: SwitcherView,
    /// [full size, gliding]
    frames: [Frames; 2],
    /// The size frames were last drawn at; a change repaints the next two frames whole, since
    /// each buffer of the new size still holds whatever it held before.
    drawn_size: (u32, u32),
    whole_frames: u8,
    /// A draw was asked for while both buffers were with the compositor: drawn on the release.
    pending: bool,
    viewporter: Option<WpViewporter>,
    viewport: Option<WpViewport>,
}

fn main() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::SwappedBuffers);
    slint::platform::set_platform(Box::new(Headless(window.clone()))).expect("one platform");
    let view = SwitcherView::new().expect("the switcher view");
    view.show().ok();
    warm_up(&window, &view);

    // WAYLAND_SOCKET when labwc launched us, which is the only way the protocol is offered.
    let Ok(conn) = Connection::connect_to_env() else {
        eprintln!("yantrik-switcher: no Wayland display");
        std::process::exit(1);
    };
    let Ok((globals, queue)) = registry_queue_init::<App>(&conn) else {
        std::process::exit(1);
    };
    let qh = queue.handle();
    let Ok(renderer) = globals.bind::<ZlabwcWindowSwitcherRendererV1, _, _>(&qh, 1..=2, ()) else {
        // Not launched by labwc as its renderer (or an unpatched labwc): nothing to draw.
        eprintln!("yantrik-switcher: labwc does not offer the window switcher renderer to this client");
        std::process::exit(0);
    };
    let mut event_loop: EventLoop<App> = EventLoop::try_new().expect("an event loop");
    let mut app = App {
        qh: qh.clone(),
        compositor: CompositorState::bind(&globals, &qh).expect("wl_compositor"),
        layer_shell: LayerShell::bind(&globals, &qh).expect("zwlr_layer_shell_v1"),
        shm: Shm::bind(&globals, &qh).expect("wl_shm"),
        outputs: OutputState::new(&globals, &qh),
        registry: RegistryState::new(&globals),
        renderer,
        session: None,
        layer: None,
        loop_handle: event_loop.handle(),
        window,
        view,
        frames: Default::default(),
        drawn_size: (0, 0),
        whole_frames: 0,
        pending: false,
        viewporter: globals.bind::<WpViewporter, _, _>(&qh, 1..=1, ()).ok(),
        viewport: None,
    };
    WaylandSource::new(conn, queue).insert(event_loop.handle()).expect("the Wayland source");
    loop {
        if event_loop.dispatch(Duration::from_secs(1), &mut app).is_err() {
            // labwc went away: so do we. It launches a new one when it needs one.
            std::process::exit(0);
        }
    }
}

/// Draw one full frame off-screen at start, so the fonts are loaded and the glyphs cached before
/// the first Alt+Tab: the first real frame then costs a frame, not a font load.
fn warm_up(window: &Rc<MinimalSoftwareWindow>, view: &SwitcherView) {
    let started = std::time::Instant::now();
    let (w, h) = (DESIGN.0 as u32, DESIGN.1 as u32);
    let card = Card {
        x: 150.0,
        y: 214.0,
        w: 640.0,
        h: 400.0,
        dim: 0.3,
        fade: 0.5,
        front: true,
        name: "Warming up ABCDEFGHIJKLMNOPQRSTUVWXYZ abcdefghijklmnopqrstuvwxyz 0123456789".into(),
        letter: "W".into(),
        tile: slint::Color::from_rgb_u8(0x5b, 0x9b, 0xe8),
        ago: "4 min ago".into(),
        other_desktop: true,
        minimized: true,
        picture: Image::default(),
        has_picture: false,
    };
    view.set_cards(ModelRc::new(VecModel::from(vec![card])));
    view.set_caption_title("Warming up".into());
    view.set_caption_where("this desktop · used 4 min ago".into());
    view.set_typed("abc".into());
    window.set_size(slint::PhysicalSize::new(w, h));
    let mut pixels = vec![Bgra::default(); (w * h) as usize];
    window.request_redraw();
    window.draw_if_needed(|renderer| {
        renderer.render(&mut pixels, w as usize);
    });
    view.set_typed("".into());
    debug(format_args!("warmed up in {} ms", started.elapsed().as_millis()));
}

impl App {
    fn on_event(&mut self, event: zr::Event) {
        match event {
            zr::Event::SessionStart { session } => {
                debug(format_args!("session {session} started"));
                self.hide();
                self.session = Some(Session::new(session));
            }
            zr::Event::Window { session, id, app_id, title, workspace, flags, idle_ms } => {
                let Some(s) = self.session.as_mut().filter(|s| s.id == session) else { return };
                if !s.collecting {
                    s.incoming.clear();
                    s.collecting = true;
                }
                let flags = match flags {
                    WEnum::Value(v) => v.bits(),
                    WEnum::Unknown(u) => u,
                };
                s.incoming.push(model::Window {
                    id,
                    app_id,
                    title,
                    workspace,
                    current_workspace: flags & FLAG_CURRENT != 0,
                    minimized: flags & FLAG_MINIMIZED != 0,
                    focused_at_start: flags & FLAG_FOCUSED != 0,
                    idle_ms,
                });
            }
            zr::Event::WindowsDone { session } => {
                let Some(s) = self.session.as_mut().filter(|s| s.id == session) else { return };
                s.windows = std::mem::take(&mut s.incoming);
                s.collecting = false;
                s.place_springs();
                debug(format_args!("session {session}: {} windows", s.windows.len()));
                if !s.scheduled {
                    s.scheduled = true;
                    let _ = self.loop_handle.insert_source(Timer::from_duration(SHOW_AFTER), move |_, _, app| {
                        if app.session.as_ref().is_some_and(|s| s.id == session && !s.shown) {
                            app.show();
                        }
                        TimeoutAction::Drop
                    });
                }
                let ids = self.session.as_ref().map(visible_ids).unwrap_or_default();
                self.capture(session, &ids);
                self.draw();
            }
            zr::Event::Selected { session, id } => {
                let Some(s) = self.session.as_mut().filter(|s| s.id == session) else { return };
                s.select(id);
                let missing = !s.pictures.contains_key(&id);
                if missing {
                    self.capture(session, &[id]);
                }
                self.animate();
                self.draw();
            }
            zr::Event::Key { session, keysym: _, utf8 } => {
                let Some(s) = self.session.as_mut().filter(|s| s.id == session) else { return };
                if s.typed.chars().count() + utf8.chars().count() > MOST_TYPED {
                    return;
                }
                s.typed.push_str(&model::clean(&utf8, 4));
                if let Some(i) = model::jump(&s.windows, s.selected_index(), &s.typed) {
                    let id = s.windows[i].id;
                    self.renderer.select(session, id);
                }
                self.draw();
            }
            zr::Event::Thumbnail { session, id, fd, width, height, stride } => {
                let Some(s) = self.session.as_mut().filter(|s| s.id == session) else { return };
                match picture(fd, width, height, stride) {
                    Some(image) => {
                        s.pictures.insert(id, image);
                        self.draw();
                    }
                    None => debug(format_args!("session {session}: a picture of {id} could not be read")),
                }
            }
            zr::Event::Ended { session, reason } => {
                debug(format_args!("session {session} ended: {reason:?}"));
                if self.session.as_ref().is_some_and(|s| s.id == session) {
                    self.session = None;
                    self.hide();
                }
            }
        }
    }

    /// Put up the overlay: a full-screen layer surface above everything, taking no keys and no
    /// pointer (an empty input region), so labwc keeps both. Drawn on its first configure.
    fn show(&mut self) {
        if self.layer.is_some() {
            return;
        }
        let surface = self.compositor.create_surface(&self.qh);
        let layer = self.layer_shell.create_layer_surface(&self.qh, surface, Layer::Overlay, Some("yantrik-switcher"), None);
        layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        let region = self.compositor.wl_compositor().create_region(&self.qh, ());
        layer.wl_surface().set_input_region(Some(&region));
        region.destroy();
        self.viewport = self.viewporter.as_ref().map(|v| v.get_viewport(layer.wl_surface(), &self.qh, ()));
        layer.commit();
        debug(format_args!("overlay requested"));
        self.layer = Some((layer, (0, 0)));
        if let Some(s) = self.session.as_mut() {
            s.shown = true;
        }
        self.start_live();
    }

    /// Draw frames while any card is moving: each frame steps every spring by the time that
    /// really passed, so a slow frame moves cards further rather than slowing them down. One loop
    /// at a time; a new Tab while it runs only moves targets.
    fn animate(&mut self) {
        let Some(s) = self.session.as_mut() else { return };
        if s.animating || s.springs.values().all(|sp| sp.settled()) {
            return;
        }
        s.animating = true;
        let session = s.id;
        let mut last = std::time::Instant::now();
        let mut beat = last + FRAME;
        let _ = self.loop_handle.insert_source(Timer::from_deadline(beat), move |_, _, app| {
            let Some(s) = app.session.as_mut().filter(|s| s.id == session) else { return TimeoutAction::Drop };
            let now = std::time::Instant::now();
            let dt = (now - last).as_secs_f32().min(0.05);
            last = now;
            let n = s.windows.len();
            for sp in s.springs.values_mut() {
                sp.step(dt, STIFFNESS, APPEAR, n);
            }
            let done = s.springs.values().all(|sp| sp.settled());
            if done {
                for sp in s.springs.values_mut() {
                    sp.snap();
                }
                s.animating = false;
            }
            app.draw();
            if done {
                return TimeoutAction::Drop;
            }
            beat += FRAME;
            if beat <= now {
                beat = now + FRAME / 4;
            }
            TimeoutAction::ToInstant(beat)
        });
    }

    /// Ask labwc for pictures of these windows (protocol 2; an older labwc has none to give).
    fn capture(&self, session: u32, ids: &[u32]) {
        if self.renderer.version() < 2 {
            return;
        }
        for &id in ids {
            self.renderer.capture(session, id, PICTURE.0, PICTURE.1);
        }
    }

    /// While the stack is up: the front card's picture again every LIVE_EVERY, the ones behind
    /// it every BACK_EVERY of those. Stops by itself when the session ends.
    fn start_live(&mut self) {
        let Some(s) = self.session.as_mut() else { return };
        if s.live || self.renderer.version() < 2 {
            return;
        }
        s.live = true;
        let session = s.id;
        let mut tick = 0u32;
        let _ = self.loop_handle.insert_source(Timer::from_duration(LIVE_EVERY), move |_, _, app| {
            let Some(s) = app.session.as_ref().filter(|s| s.id == session) else { return TimeoutAction::Drop };
            tick += 1;
            let ids = if tick % BACK_EVERY == 0 { visible_ids(s) } else { s.selected.into_iter().collect() };
            app.capture(session, &ids);
            TimeoutAction::ToDuration(LIVE_EVERY)
        });
    }

    fn hide(&mut self) {
        // Dropping the layer surface destroys it; the stack is gone at once.
        if let Some(v) = self.viewport.take() {
            v.destroy();
        }
        self.layer = None;
    }

    fn draw(&mut self) {
        let frame_started = std::time::Instant::now();
        let Some(s) = self.session.as_ref() else { return };
        let Some((layer, (w, h))) = self.layer.as_ref() else { return };
        let (out_w, out_h) = (*w, *h);
        // Gliding: a smaller frame, scaled up by the compositor. Everything below is laid out
        // from (w, h), so it is the same picture at a lower resolution.
        let gliding = s.animating && self.viewport.is_some();
        let set = gliding as usize;
        let (w, h) = if gliding { (out_w / GLIDE_SCALE, out_h / GLIDE_SCALE) } else { (out_w, out_h) };
        if w == 0 || h == 0 || s.windows.is_empty() {
            return;
        }
        let (sx, sy) = (w as f32 / DESIGN.0, h as f32 / DESIGN.1);
        let scale = sx.min(sy);
        // Slint's `length` is logical pixels as f32; the output is drawn at scale 1.
        let px = |v: f32| v;
        let n = s.windows.len();
        let sel = s.selected_index();

        // Cards, deepest first so the front one is painted last; a card leaving past the front
        // (depth below 0) is painted over everything.
        let drawn = s.drawn();
        let mut cards: Vec<(f32, Card)> = s
            .windows
            .iter()
            .enumerate()
            .filter_map(|(i, win)| {
                let (d, appear) = drawn[i];
                (d > -1.0 && d < model::VISIBLE as f32).then(|| {
                    let p = model::place_at(d);
                    let rgb = TILES[model::tile_slot(win)];
                    (
                        d,
                        Card {
                            x: px(p.x * sx),
                            y: px(p.y * sy),
                            w: px(p.w * scale),
                            h: px(p.h * scale),
                            dim: model::dim(d),
                            fade: model::fade(d) * appear,
                            front: d.abs() < 0.5,
                            name: model::name(win).into(),
                            letter: model::letter(win).into(),
                            tile: slint::Color::from_rgb_u8((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8),
                            ago: model::ago(win.idle_ms).unwrap_or_default().into(),
                            other_desktop: !win.current_workspace,
                            minimized: win.minimized,
                            picture: s.pictures.get(&win.id).cloned().unwrap_or_default(),
                            has_picture: s.pictures.contains_key(&win.id),
                        },
                    )
                })
            })
            .collect();
        cards.sort_by(|a, b| b.0.total_cmp(&a.0));
        let cards: Vec<Card> = cards.into_iter().map(|(_, c)| c).collect();

        let (line_top, line_height) = (120.0 * sy, 480.0 * sy);
        let ys = model::spread(s.windows.iter().map(|win| line_top + model::timeline_pos(win.idle_ms) * line_height).collect(), 18.0 * scale);
        let dots: Vec<Dot> = s
            .windows
            .iter()
            .enumerate()
            .map(|(i, win)| Dot { y: px(ys[i]), label: model::clean(&model::name(win), 28).into(), on: i == sel })
            .collect();
        let ticks: Vec<Tick> =
            model::ticks().into_iter().map(|(label, pos)| Tick { y: px(line_top + pos * line_height), label: label.into() }).collect();

        let front = &s.windows[sel];
        self.view.set_cards(ModelRc::new(VecModel::from(cards)));
        self.view.set_dots(ModelRc::new(VecModel::from(dots)));
        self.view.set_ticks(ModelRc::new(VecModel::from(ticks)));
        self.view.set_line_x(px(1040.0 * sx));
        self.view.set_line_top(px(line_top));
        self.view.set_line_height(px(line_height));
        self.view.set_caption_x(px(150.0 * sx));
        self.view.set_caption_y(px(640.0 * sy));
        self.view.set_caption_title(model::name(front).into());
        self.view.set_caption_where(model::whereabouts(front).into());
        self.view.set_typed(s.typed.clone().into());
        self.view.set_more(if n > model::VISIBLE { format!("+{} deeper", n - model::VISIBLE).into() } else { "".into() });
        self.view.set_scale(scale);

        self.window.set_size(slint::PhysicalSize::new(w, h));
        slint::platform::update_timers_and_animations();
        let surface = layer.wl_surface().clone();
        let set_frames = &mut self.frames[set];
        if set_frames.size != (w, h) {
            set_frames.frames.clear();
            for i in 0..2 {
                let Ok(mut pool) = RawPool::new((w * h * 4) as usize, &self.shm) else { return };
                let buffer = pool.create_buffer(0, w as i32, h as i32, (w * 4) as i32, wl_shm::Format::Argb8888, set * 2 + i, &self.qh);
                set_frames.frames.push(Frame { pool, buffer, busy: false });
            }
            set_frames.size = (w, h);
            set_frames.next = 0;
        }
        let i = set_frames.next;
        if set_frames.frames[i].busy {
            // Both buffers are still the compositor's: draw when one comes back.
            self.pending = true;
            return;
        }
        if self.drawn_size != (w, h) {
            self.drawn_size = (w, h);
            self.whole_frames = 2;
        }
        let whole = self.whole_frames > 0;
        self.whole_frames = self.whole_frames.saturating_sub(1);
        let frame = &mut self.frames[set].frames[i];
        let bytes = frame.pool.mmap();
        // Safety: Bgra is four u8s with alignment 1, and the pool holds exactly w * h of them.
        let pixels: &mut [Bgra] = unsafe { std::slice::from_raw_parts_mut(bytes.as_mut_ptr().cast::<Bgra>(), (w * h) as usize) };
        self.window.request_redraw();
        let mut dirty = None;
        self.window.draw_if_needed(|renderer| {
            renderer.set_repaint_buffer_type(if whole { RepaintBufferType::NewBuffer } else { RepaintBufferType::SwappedBuffers });
            dirty = Some(renderer.render(pixels, w as usize));
        });
        if let Some(v) = self.viewport.as_ref() {
            v.set_destination(out_w as i32, out_h as i32);
        }
        surface.attach(Some(&frame.buffer), 0, 0);
        match dirty.map(|r| (r.bounding_box_origin(), r.bounding_box_size())) {
            Some((o, s)) if s.width > 0 && s.height > 0 => surface.damage_buffer(o.x, o.y, s.width as i32, s.height as i32),
            _ => surface.damage_buffer(0, 0, w as i32, h as i32),
        }
        surface.commit();
        frame.busy = true;
        self.frames[set].next = 1 - i;
        debug(format_args!("drew {w}x{h}, {n} windows, selected {sel}, {} ms into the session, frame {} ms", s.started.elapsed().as_millis(), frame_started.elapsed().as_millis()));
        // The session is labwc's to draw until this frame is on the screen: only now is it acked.
        if let Some(s) = self.session.as_mut() {
            if !s.acked {
                s.acked = true;
                self.renderer.ack(s.id);
            }
        }
    }
}

/// The windows whose cards are drawn: the selected one and the ones stacked behind it.
fn visible_ids(s: &Session) -> Vec<u32> {
    let (n, sel) = (s.windows.len(), s.selected_index());
    s.windows.iter().enumerate().filter(|(i, _)| model::depth(*i, sel, n) < model::VISIBLE).map(|(_, w)| w.id).collect()
}

/// A picture labwc sent: a sealed memory file of `stride * height` bytes, ARGB8888 premultiplied
/// (B, G, R, A). Read and closed here; None for a size labwc would never send or a short file.
fn picture(fd: std::os::fd::OwnedFd, width: u32, height: u32, stride: u32) -> Option<Image> {
    if width == 0 || height == 0 || width > PICTURE_MOST || height > PICTURE_MOST || stride < width * 4 || stride > PICTURE_MOST * 4 {
        return None;
    }
    let size = (stride as usize) * (height as usize);
    let mut bytes = Vec::with_capacity(size);
    std::fs::File::from(fd).take(size as u64).read_to_end(&mut bytes).ok()?;
    if bytes.len() != size {
        return None;
    }
    let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(width, height);
    let out = buffer.make_mut_slice();
    for (y, row) in bytes.chunks_exact(stride as usize).enumerate() {
        for (x, px) in row[..(width as usize) * 4].chunks_exact(4).enumerate() {
            out[y * width as usize + x] = Rgba8Pixel { r: px[2], g: px[1], b: px[0], a: px[3] };
        }
    }
    Some(Image::from_rgba8_premultiplied(buffer))
}

impl Dispatch<ZlabwcWindowSwitcherRendererV1, ()> for App {
    fn event(app: &mut Self, _: &ZlabwcWindowSwitcherRendererV1, event: zr::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        app.on_event(event);
    }
}

impl Dispatch<wl_buffer::WlBuffer, usize> for App {
    fn event(app: &mut Self, buffer: &wl_buffer::WlBuffer, event: wl_buffer::Event, i: &usize, _: &Connection, _: &QueueHandle<Self>) {
        if let wl_buffer::Event::Release = event {
            if let Some(frame) = app.frames.get_mut(*i / 2).and_then(|s| s.frames.get_mut(*i % 2)).filter(|f| &f.buffer == buffer) {
                frame.busy = false;
            }
            if app.pending {
                app.pending = false;
                app.draw();
            }
        }
    }
}

impl Dispatch<WpViewporter, ()> for App {
    fn event(_: &mut Self, _: &WpViewporter, _: <WpViewporter as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<WpViewport, ()> for App {
    fn event(_: &mut Self, _: &WpViewport, _: <WpViewport as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<wl_region::WlRegion, ()> for App {
    fn event(_: &mut Self, _: &wl_region::WlRegion, _: wl_region::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl LayerShellHandler for App {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        self.layer = None;
    }

    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface, configure: LayerSurfaceConfigure, _: u32) {
        let Some((ours, size)) = self.layer.as_mut() else { return };
        if ours.wl_surface() != layer.wl_surface() {
            return;
        }
        let (w, h) = configure.new_size;
        *size = if w == 0 || h == 0 { (DESIGN.0 as u32, DESIGN.1 as u32) } else { (w, h) };
        debug(format_args!("overlay configured {w}x{h}"));
        self.draw();
    }
}

impl CompositorHandler for App {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: i32) {}
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {}
    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
}

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.outputs
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl ShmHandler for App {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState];
}

delegate_compositor!(App);
delegate_output!(App);
delegate_shm!(App);
delegate_layer!(App);
delegate_registry!(App);
