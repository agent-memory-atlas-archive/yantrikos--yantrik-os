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

use std::rc::Rc;
use std::time::Duration;

use slint::platform::software_renderer::{MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType};
use slint::platform::{Platform, WindowAdapter};
use slint::{ComponentHandle, ModelRc, VecModel};
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
use wayland_client::{
    globals::registry_queue_init,
    protocol::{wl_buffer, wl_output, wl_region, wl_shm, wl_surface},
    Connection, Dispatch, QueueHandle, WEnum,
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

const FLAG_CURRENT: u32 = 1;
const FLAG_MINIMIZED: u32 = 2;
const FLAG_FOCUSED: u32 = 4;

/// `YANTRIK_SWITCHER_DEBUG=1`: say each step of a session on stderr (labwc's log).
fn debug(what: std::fmt::Arguments) {
    if std::env::var_os("YANTRIK_SWITCHER_DEBUG").is_some() {
        eprintln!("yantrik-switcher: {what}");
    }
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
}

impl Session {
    fn new(id: u32) -> Session {
        Session { id, windows: Vec::new(), incoming: Vec::new(), collecting: false, selected: None, acked: false, scheduled: false, shown: false, typed: String::new(), started: std::time::Instant::now() }
    }

    fn selected_index(&self) -> usize {
        self.selected.and_then(|id| self.windows.iter().position(|w| w.id == id)).unwrap_or(0)
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
}

fn main() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
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
    let Ok(renderer) = globals.bind::<ZlabwcWindowSwitcherRendererV1, _, _>(&qh, 1..=1, ()) else {
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
        alpha: 1.0,
        front: true,
        name: "Warming up ABCDEFGHIJKLMNOPQRSTUVWXYZ abcdefghijklmnopqrstuvwxyz 0123456789".into(),
        letter: "W".into(),
        tile: slint::Color::from_rgb_u8(0x5b, 0x9b, 0xe8),
        ago: "4 min ago".into(),
        other_desktop: true,
        minimized: true,
    };
    view.set_cards(ModelRc::new(VecModel::from(vec![card])));
    view.set_caption_title("Warming up".into());
    view.set_caption_where("this desktop · used 4 min ago".into());
    view.set_typed("abc".into());
    window.set_size(slint::PhysicalSize::new(w, h));
    let mut pixels = vec![PremultipliedRgbaColor::default(); (w * h) as usize];
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
                self.draw();
            }
            zr::Event::Selected { session, id } => {
                let Some(s) = self.session.as_mut().filter(|s| s.id == session) else { return };
                s.selected = Some(id);
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
        layer.commit();
        debug(format_args!("overlay requested"));
        self.layer = Some((layer, (0, 0)));
        if let Some(s) = self.session.as_mut() {
            s.shown = true;
        }
    }

    fn hide(&mut self) {
        // Dropping the layer surface destroys it; the stack is gone at once.
        self.layer = None;
    }

    fn draw(&mut self) {
        let Some(s) = self.session.as_ref() else { return };
        let Some((layer, (w, h))) = self.layer.as_ref() else { return };
        let (w, h) = (*w, *h);
        if w == 0 || h == 0 || s.windows.is_empty() {
            return;
        }
        let (sx, sy) = (w as f32 / DESIGN.0, h as f32 / DESIGN.1);
        let scale = sx.min(sy);
        // Slint's `length` is logical pixels as f32; the output is drawn at scale 1.
        let px = |v: f32| v;
        let n = s.windows.len();
        let sel = s.selected_index();

        // Cards, deepest first so the selected one is painted last.
        let mut cards: Vec<(usize, Card)> = s
            .windows
            .iter()
            .enumerate()
            .filter_map(|(i, win)| {
                let d = model::depth(i, sel, n);
                (d < model::VISIBLE).then(|| {
                    let p = model::place(d);
                    let rgb = TILES[model::tile_slot(win)];
                    (
                        d,
                        Card {
                            x: px(p.x * sx),
                            y: px(p.y * sy),
                            w: px(p.w * scale),
                            h: px(p.h * scale),
                            alpha: p.alpha,
                            front: d == 0,
                            name: model::name(win).into(),
                            letter: model::letter(win).into(),
                            tile: slint::Color::from_rgb_u8((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8),
                            ago: model::ago(win.idle_ms).unwrap_or_default().into(),
                            other_desktop: !win.current_workspace,
                            minimized: win.minimized,
                        },
                    )
                })
            })
            .collect();
        cards.sort_by_key(|c| std::cmp::Reverse(c.0));
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
        let mut pixels = vec![PremultipliedRgbaColor::default(); (w * h) as usize];
        self.window.request_redraw();
        self.window.draw_if_needed(|renderer| {
            renderer.render(&mut pixels, w as usize);
        });
        let Ok(mut pool) = RawPool::new((w * h * 4) as usize, &self.shm) else { return };
        // Premultiplied ARGB, little-endian: B, G, R, A.
        for (p, out) in pixels.iter().zip(pool.mmap().chunks_exact_mut(4)) {
            out.copy_from_slice(&[p.blue, p.green, p.red, p.alpha]);
        }
        let buffer = pool.create_buffer(0, w as i32, h as i32, (w * 4) as i32, wl_shm::Format::Argb8888, (), &self.qh);
        let surface = layer.wl_surface();
        surface.attach(Some(&buffer), 0, 0);
        surface.damage_buffer(0, 0, w as i32, h as i32);
        surface.commit();
        buffer.destroy();
        debug(format_args!("drew {w}x{h}, {n} windows, selected {sel}, {} ms into the session", s.started.elapsed().as_millis()));
        // The session is labwc's to draw until this frame is on the screen: only now is it acked.
        if let Some(s) = self.session.as_mut() {
            if !s.acked {
                s.acked = true;
                self.renderer.ack(s.id);
            }
        }
    }
}

impl Dispatch<ZlabwcWindowSwitcherRendererV1, ()> for App {
    fn event(app: &mut Self, _: &ZlabwcWindowSwitcherRendererV1, event: zr::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        app.on_event(event);
    }
}

impl Dispatch<wl_buffer::WlBuffer, ()> for App {
    fn event(_: &mut Self, _: &wl_buffer::WlBuffer, _: wl_buffer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
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
