use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use egui::{ViewportId, ViewportInfo};
use objc2::rc::Retained;
use objc2_core_foundation::CGSize;
use objc2_metal::{MTLCreateSystemDefaultDevice, MTLPixelFormat};
use objc2_quartz_core::CAMetalLayer;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::{
    application::ApplicationHandler,
    dpi::LogicalSize,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy},
    platform::macos::WindowAttributesExtMacOS,
    window::{Window, WindowId},
};

mod renderer;

enum Event {
    Repaint { at: Instant, pass: u64 },
    AccessKit(accesskit_winit::Event),
}
impl From<accesskit_winit::Event> for Event {
    fn from(event: accesskit_winit::Event) -> Self {
        Self::AccessKit(event)
    }
}

struct Runner {
    context: egui::Context,
    proxy: EventLoopProxy<Event>,
    window: Option<Arc<Window>>,
    input: Option<egui_winit::State>,
    app: Option<crate::app::MusicApp>,
    layer: Option<Retained<CAMetalLayer>>,
    painter: Option<renderer::Painter>,
    viewport: ViewportInfo,
    deadline: Option<Instant>,
    hidden: bool,
    #[cfg(feature = "ui-qa")]
    painted: bool,
    error: Option<String>,
}
impl Runner {
    fn logic(&mut self) {
        if let Some(app) = &mut self.app {
            // Playback and the queue continue while the window is occluded.
            app.receive();
            #[cfg(feature = "ui-qa")]
            crate::qa::report_startup(self.painted);
        }
    }
    fn redraw(&mut self, event_loop: &ActiveEventLoop) -> Result<(), String> {
        // Bound temporary AppKit/accessibility objects as well as Metal objects
        // on timer-driven frames, which can outnumber native input events.
        objc2::rc::autoreleasepool(|_| self.redraw_frame(event_loop))
    }
    fn redraw_frame(&mut self, event_loop: &ActiveEventLoop) -> Result<(), String> {
        self.deadline = None;
        self.logic();
        if self.hidden {
            return Ok(());
        }
        let window = self.window.as_ref().expect("initialized window");
        let size = window.inner_size();
        if size.width == 0 || size.height == 0 {
            return Ok(());
        }
        let input = self.input.as_mut().expect("initialized input");
        egui_winit::update_viewport_info(&mut self.viewport, &self.context, window, false);
        input
            .egui_input_mut()
            .viewports
            .insert(ViewportId::ROOT, self.viewport.clone());
        let raw = input.take_egui_input(window);
        let mut output = self.context.run_ui(raw, |ui| {
            self.app.as_mut().expect("initialized app").draw_ui(ui);
        });
        // Keep the final pass's timer even when egui coalesces a callback from
        // an earlier pass. Idle notices and delayed work still need to wake.
        self.deadline = output
            .viewport_output
            .get(&ViewportId::ROOT)
            .and_then(|viewport| Instant::now().checked_add(viewport.repaint_delay));
        input.handle_platform_output_with_event_loop(window, event_loop, output.platform_output);
        let mut actions = Vec::new();
        for viewport in output.viewport_output.into_values() {
            for command in &viewport.commands {
                if matches!(command, egui::ViewportCommand::Close) {
                    event_loop.exit();
                }
            }
            egui_winit::process_viewport_commands(
                &self.context,
                &mut self.viewport,
                viewport.commands,
                window,
                &mut actions,
            );
        }
        let screenshots: Vec<_> = actions
            .iter()
            .filter_map(|action| {
                if let egui_winit::ActionRequested::Screenshot(data) = action {
                    Some(data.clone())
                } else {
                    None
                }
            })
            .collect();
        for action in actions {
            match action {
                egui_winit::ActionRequested::Cut => {
                    input.egui_input_mut().events.push(egui::Event::Cut)
                }
                egui_winit::ActionRequested::Copy => {
                    input.egui_input_mut().events.push(egui::Event::Copy)
                }
                egui_winit::ActionRequested::Paste => {
                    if let Some(text) = input.clipboard_text() {
                        input.egui_input_mut().events.push(egui::Event::Paste(text));
                    }
                }
                egui_winit::ActionRequested::Screenshot(_) => {}
            }
        }
        let layer = self.layer.as_ref().expect("initialized layer");
        layer.setContentsScale(window.scale_factor());
        layer.setDrawableSize(CGSize::new(f64::from(size.width), f64::from(size.height)));
        let primitives = self
            .context
            .tessellate(output.shapes, output.pixels_per_point);
        if let Some(image) = self.painter.as_mut().expect("initialized painter").paint(
            layer,
            output.pixels_per_point,
            &primitives,
            &mut output.textures_delta,
            !screenshots.is_empty(),
        )? {
            let image = Arc::new(image);
            for user_data in screenshots {
                input.egui_input_mut().events.push(egui::Event::Screenshot {
                    viewport_id: ViewportId::ROOT,
                    user_data,
                    image: image.clone(),
                });
            }
            self.context.request_repaint();
        }
        if window.is_visible() == Some(false) {
            window.set_visible(true);
        }
        #[cfg(feature = "ui-qa")]
        {
            self.painted = true;
        }
        if self
            .context
            .input(|i| i.modifiers.command && i.key_pressed(egui::Key::Q))
        {
            event_loop.exit();
        }
        Ok(())
    }
    fn initialize(&mut self, event_loop: &ActiveEventLoop) -> Result<(), String> {
        let attributes = Window::default_attributes()
            .with_title("Riff")
            .with_visible(false)
            .with_inner_size(LogicalSize::new(1280., 800.))
            .with_min_inner_size(LogicalSize::new(960., 600.))
            .with_fullsize_content_view(true)
            .with_title_hidden(true)
            .with_titlebar_transparent(true);
        #[cfg(feature = "ui-qa")]
        let attributes = if std::env::var_os("RIFF_CAPTURE").is_some() {
            let [width, height] = crate::qa::window_size();
            let size = LogicalSize::new(width, height);
            attributes
                .with_inner_size(size)
                .with_min_inner_size(size)
                .with_max_inner_size(size)
                .with_window_level(winit::window::WindowLevel::AlwaysOnTop)
        } else {
            attributes
        };
        let window = Arc::new(
            event_loop
                .create_window(attributes)
                .map_err(|e| e.to_string())?,
        );
        let mut input = egui_winit::State::new(
            self.context.clone(),
            ViewportId::ROOT,
            &*window,
            Some(window.scale_factor() as f32),
            window.theme(),
            // Riff's largest image is 1280px. A bounded atlas also avoids
            // retaining oversized font textures after browsing many titles.
            Some(2048),
        );
        // AccessKit must be installed before the first show operation on macOS.
        input.init_accesskit(event_loop, &window, self.proxy.clone());
        egui_winit::update_viewport_info(&mut self.viewport, &self.context, &window, true);
        let RawWindowHandle::AppKit(handle) =
            window.window_handle().map_err(|e| e.to_string())?.as_raw()
        else {
            return Err("Metal requires an AppKit window".into());
        };
        // SAFETY: Winit owns the live NSView, and this code runs on its main event-loop thread.
        // into_raw transfers the layer's +1 retain to Retained. Runner drops it before the window.
        let layer = unsafe {
            let raw = raw_window_metal::Layer::from_ns_view(handle.ns_view).into_raw();
            Retained::<CAMetalLayer>::from_raw(raw.as_ptr().cast()).expect("retained Metal layer")
        };
        let device = MTLCreateSystemDefaultDevice().ok_or("No Metal device available")?;
        layer.setDevice(Some(&device));
        layer.setPixelFormat(MTLPixelFormat::BGRA8Unorm);
        layer.setFramebufferOnly(true);
        layer.setMaximumDrawableCount(2);
        layer.setOpaque(true);
        layer.setDisplaySyncEnabled(true);
        self.painter = Some(renderer::Painter::new(&layer)?);
        self.layer = Some(layer);
        self.app = Some(crate::app::MusicApp::from_context(&self.context));
        self.input = Some(input);
        self.window = Some(window.clone());
        window.request_redraw();
        Ok(())
    }
    fn fail(&mut self, event_loop: &ActiveEventLoop, error: String) {
        self.error = Some(error);
        event_loop.exit();
    }
}
impl ApplicationHandler<Event> for Runner {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_none()
            && let Err(error) = self.initialize(event_loop)
        {
            self.fail(event_loop, error);
        }
    }
    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: Event) {
        match event {
            Event::Repaint { at, pass } => {
                self.logic();
                // Repaints requested by an older pass have already been satisfied.
                if pass + 1 >= self.context.cumulative_pass_nr()
                    && self.deadline.is_none_or(|old| at < old)
                {
                    self.deadline = Some(at);
                }
            }
            Event::AccessKit(event) => {
                if self
                    .window
                    .as_ref()
                    .is_none_or(|w| w.id() != event.window_id)
                {
                    return;
                }
                match event.window_event {
                    accesskit_winit::WindowEvent::InitialTreeRequested => {
                        self.context.enable_accesskit()
                    }
                    accesskit_winit::WindowEvent::ActionRequested(request) => {
                        if let Some(input) = &mut self.input {
                            input.on_accesskit_action_request(request);
                        }
                    }
                    accesskit_winit::WindowEvent::AccessibilityDeactivated => {
                        self.context.disable_accesskit()
                    }
                }
                self.deadline = Some(Instant::now());
            }
        }
    }
    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        let Some(window) = self.window.clone() else {
            return;
        };
        if window.id() != id {
            return;
        }
        let response = self
            .input
            .as_mut()
            .expect("initialized input")
            .on_window_event(&window, &event);
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::RedrawRequested => {
                if let Err(error) = self.redraw(event_loop) {
                    self.fail(event_loop, error);
                }
            }
            WindowEvent::Occluded(hidden) => {
                self.hidden = hidden;
                if hidden {
                    self.deadline = Some(Instant::now());
                }
                if !hidden {
                    window.request_redraw();
                }
            }
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                window.request_redraw()
            }
            _ => {
                if response.repaint {
                    window.request_redraw();
                }
            }
        }
    }
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        match self.deadline {
            Some(deadline) if deadline <= Instant::now() => {
                self.deadline = None;
                if self.hidden {
                    // egui coalesces repaint callbacks until another UI pass.
                    // Covered windows therefore drain worker events on a small
                    // timer, without drawing or accumulating a playback backlog.
                    self.logic();
                    let next = Instant::now() + Duration::from_millis(250);
                    self.deadline = Some(next);
                    event_loop.set_control_flow(ControlFlow::WaitUntil(next));
                } else {
                    if let Some(window) = &self.window {
                        window.request_redraw();
                    }
                    event_loop.set_control_flow(ControlFlow::Wait);
                }
            }
            Some(deadline) => event_loop.set_control_flow(ControlFlow::WaitUntil(deadline)),
            None => event_loop.set_control_flow(ControlFlow::Wait),
        }
    }
}
impl Drop for Runner {
    fn drop(&mut self) {
        // Stop the audio worker, then finish GPU work before releasing the native view.
        self.app.take();
        self.painter.take();
        self.input.take();
        self.layer.take();
        self.window.take();
    }
}
pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let event_loop = EventLoop::<Event>::with_user_event().build()?;
    let proxy = event_loop.create_proxy();
    let context = egui::Context::default();
    let redraw = proxy.clone();
    context.set_request_repaint_callback(move |info| {
        if let Some(at) = Instant::now().checked_add(info.delay) {
            let _ = redraw.send_event(Event::Repaint {
                at,
                pass: info.current_cumulative_pass_nr,
            });
        }
    });
    let mut runner = Runner {
        context,
        proxy,
        window: None,
        input: None,
        app: None,
        layer: None,
        painter: None,
        viewport: ViewportInfo::default(),
        deadline: None,
        hidden: false,
        #[cfg(feature = "ui-qa")]
        painted: false,
        error: None,
    };
    event_loop.run_app(&mut runner)?;
    if let Some(error) = runner.error.take() {
        return Err(error.into());
    }
    Ok(())
}
