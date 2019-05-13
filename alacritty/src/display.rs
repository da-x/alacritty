// Copyright 2016 Joe Wilm, The Alacritty Project Contributors
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! The display subsystem including window management, font rasterization, and
//! GPU drawing.
use std::f64;
use std::sync::mpsc;

use font::{self, Rasterize};
use glutin::dpi::PhysicalSize;
use glutin::event_loop::EventLoopProxy;
use glutin::{ContextCurrentState, NotCurrent, PossiblyCurrent, RawContext};

use alacritty_terminal::config::Config;
use alacritty_terminal::event::Event;
use alacritty_terminal::event::OnResize;
use alacritty_terminal::index::Line;
use alacritty_terminal::message_bar::Message;
use alacritty_terminal::meter::Meter;
use alacritty_terminal::renderer::rects::{Rect, Rects};
use alacritty_terminal::renderer::{self, GlyphCache, QuadRenderer};
use alacritty_terminal::term::color::Rgb;
use alacritty_terminal::term::{RenderableCell, SizeInfo};

use crate::event::Resize;
use crate::window::{self, Window};

#[derive(Debug)]
pub enum Error {
    /// Error with window management
    Window(window::Error),

    /// Error dealing with fonts
    Font(font::Error),

    /// Error in renderer
    Render(renderer::Error),
}

impl ::std::error::Error for Error {
    fn cause(&self) -> Option<&dyn (::std::error::Error)> {
        match *self {
            Error::Window(ref err) => Some(err),
            Error::Font(ref err) => Some(err),
            Error::Render(ref err) => Some(err),
        }
    }

    fn description(&self) -> &str {
        match *self {
            Error::Window(ref err) => err.description(),
            Error::Font(ref err) => err.description(),
            Error::Render(ref err) => err.description(),
        }
    }
}

impl ::std::fmt::Display for Error {
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        match *self {
            Error::Window(ref err) => err.fmt(f),
            Error::Font(ref err) => err.fmt(f),
            Error::Render(ref err) => err.fmt(f),
        }
    }
}

impl From<window::Error> for Error {
    fn from(val: window::Error) -> Error {
        Error::Window(val)
    }
}

impl From<font::Error> for Error {
    fn from(val: font::Error) -> Error {
        Error::Font(val)
    }
}

impl From<renderer::Error> for Error {
    fn from(val: renderer::Error) -> Error {
        Error::Render(val)
    }
}

pub struct RenderUpdate {
    pub grid_cells: Vec<RenderableCell>,
    pub message_buffer: Option<Message>,
    pub visual_bell_intensity: f64,
    pub background_color: Rgb,
    pub config: Config,
}

/// The display wraps a window, font rasterizer, and GPU renderer
pub struct Display<T: ContextCurrentState> {
    context: RawContext<T>,
    renderer: QuadRenderer,
    glyph_cache: GlyphCache,
    rx: mpsc::Receiver<Resize>,
    tx: mpsc::Sender<Resize>,
    meter: Meter,
    font_size: font::Size,
    size_info: SizeInfo,
    event_proxy: EventLoopProxy<Event>,
}

impl<T: ContextCurrentState> Display<T> {
    /// Get size info about the display
    pub fn size(&self) -> &SizeInfo {
        &self.size_info
    }

    pub fn new(
        config: &Config,
        window: &mut Window,
        context: RawContext<T>,
        event_proxy: EventLoopProxy<Event>,
    ) -> Result<Display<T>, Error> {
        let dpr = window.hidpi_factor();
        info!("Device pixel ratio: {}", dpr);

        // get window properties for initializing the other subsystems
        let mut viewport_size = window.inner_size().to_physical(dpr);

        // Create renderer
        let mut renderer = QuadRenderer::new()?;

        let (glyph_cache, cell_width, cell_height) =
            Self::new_glyph_cache(dpr, &mut renderer, config)?;

        let mut padding_x = f64::from(config.window.padding.x) * dpr;
        let mut padding_y = f64::from(config.window.padding.y) * dpr;

        if let Some((width, height)) =
            GlyphCache::calculate_dimensions(config, dpr, cell_width, cell_height)
        {
            let PhysicalSize { width: w, height: h } = window.inner_size().to_physical(dpr);
            if (w - width).abs() < f64::EPSILON && (h - height).abs() < f64::EPSILON {
                info!("Estimated DPR correctly, skipping resize");
            } else {
                viewport_size = PhysicalSize::new(width, height);
                window.set_inner_size(viewport_size.to_logical(dpr));
            }
        } else if config.window.dynamic_padding {
            // Make sure additional padding is spread evenly
            let cw = f64::from(cell_width);
            let ch = f64::from(cell_height);
            padding_x = padding_x + (viewport_size.width - 2. * padding_x) % cw / 2.;
            padding_y = padding_y + (viewport_size.height - 2. * padding_y) % ch / 2.;
        }

        padding_x = padding_x.floor();
        padding_y = padding_y.floor();

        // Update OpenGL projection
        renderer.resize(viewport_size, padding_x as f32, padding_y as f32);

        info!("Cell Size: {} x {}", cell_width, cell_height);
        info!("Padding: {} x {}", padding_x, padding_y);

        let size_info = SizeInfo {
            dpr,
            width: viewport_size.width as f32,
            height: viewport_size.height as f32,
            cell_width: cell_width as f32,
            cell_height: cell_height as f32,
            padding_x: padding_x as f32,
            padding_y: padding_y as f32,
        };

        // Channel for resize events
        //
        // macOS has a callback for getting resize events, the channel is used
        // to queue resize events until the next draw call. Unfortunately, it
        // seems that the event loop is blocked until the window is done
        // resizing. If any drawing were to happen during a resize, it would
        // need to be in the callback.
        let (tx, rx) = mpsc::channel();

        // Clear screen
        let background_color = config.colors.primary.background;
        renderer.with_api(config, &size_info, |api| {
            api.clear(background_color);
        });

        Ok(Display {
            context,
            renderer,
            glyph_cache,
            tx,
            rx,
            meter: Meter::new(),
            font_size: config.font.size,
            size_info,
            event_proxy,
        })
    }

    fn new_glyph_cache(
        dpr: f64,
        renderer: &mut QuadRenderer,
        config: &Config,
    ) -> Result<(GlyphCache, f32, f32), Error> {
        let font = config.font.clone();
        let rasterizer = font::Rasterizer::new(dpr as f32, config.font.use_thin_strokes())?;

        // Initialize glyph cache
        let glyph_cache = {
            info!("Initializing glyph cache...");
            let init_start = ::std::time::Instant::now();

            let cache =
                renderer.with_loader(|mut api| GlyphCache::new(rasterizer, &font, &mut api))?;

            let stop = init_start.elapsed();
            let stop_f = stop.as_secs() as f64 + f64::from(stop.subsec_nanos()) / 1_000_000_000f64;
            info!("... finished initializing glyph cache in {}s", stop_f);

            cache
        };

        // Need font metrics to resize the window properly. This suggests to me the
        // font metrics should be computed before creating the window in the first
        // place so that a resize is not needed.
        let (cw, ch) = GlyphCache::compute_cell_size(config, &glyph_cache.font_metrics());

        Ok((glyph_cache, cw, ch))
    }

    pub fn update_glyph_cache(&mut self, config: &Config) {
        let cache = &mut self.glyph_cache;
        let dpr = self.size_info.dpr;
        let size = self.font_size;

        self.renderer.with_loader(|mut api| {
            let _ = cache.update_font_size(&config.font, size, dpr, &mut api);
        });

        let (cw, ch) = GlyphCache::compute_cell_size(config, &cache.font_metrics());
        self.size_info.cell_width = cw;
        self.size_info.cell_height = ch;
    }

    #[inline]
    pub fn resize_channel(&self) -> mpsc::Sender<Resize> {
        self.tx.clone()
    }
}

impl Display<PossiblyCurrent> {
    /// Process pending resize events
    pub fn handle_resize(&mut self, config: &Config, pty_resize_handle: &mut dyn OnResize) {
        let previous_cols = self.size_info.cols();
        let previous_lines = self.size_info.lines();

        // Resize events new_size and are handled outside the poll_events
        // iterator. This has the effect of coalescing multiple resize
        // events into one.
        let mut new_size = None;
        let mut new_dpr = self.size_info.dpr;
        let mut new_font_size = self.font_size;
        let mut message_bar_lines = None;

        // Take most recent resize event, if any
        while let Ok(resize) = self.rx.try_recv() {
            match resize {
                Resize::MessageBar(lines) => message_bar_lines = Some(lines),
                Resize::FontSize(size) => new_font_size = size,
                Resize::Size(size) => new_size = Some(size),
                Resize::DPR(dpr) => new_dpr = dpr,
            }
        }

        // Font size/DPI factor modification detected
        let font_changed =
            new_font_size != self.font_size || (new_dpr - self.size_info.dpr).abs() > f64::EPSILON;

        // Skip resize if nothing changed
        if let Some(new_size) = new_size {
            if !font_changed
                && (new_size.width - f64::from(self.size_info.width)).abs() < f64::EPSILON
                && (new_size.height - f64::from(self.size_info.height)).abs() < f64::EPSILON
            {
                return;
            }
        }

        if font_changed || message_bar_lines.is_some() {
            if new_size == None {
                // Force a resize to refresh things
                new_size = Some(PhysicalSize::new(
                    f64::from(self.size_info.width) / self.size_info.dpr * new_dpr,
                    f64::from(self.size_info.height) / self.size_info.dpr * new_dpr,
                ));
            }

            self.font_size = new_font_size;
            self.size_info.dpr = new_dpr;
        }

        if font_changed {
            self.update_glyph_cache(config);
        }

        if let Some(psize) = new_size.take() {
            let width = psize.width as f32;
            let height = psize.height as f32;
            let cell_width = self.size_info.cell_width;
            let cell_height = self.size_info.cell_height;

            self.size_info.width = width;
            self.size_info.height = height;

            let mut padding_x = f32::from(config.window.padding.x) * new_dpr as f32;
            let mut padding_y = f32::from(config.window.padding.y) * new_dpr as f32;

            if config.window.dynamic_padding {
                padding_x = padding_x + ((width - 2. * padding_x) % cell_width) / 2.;
                padding_y = padding_y + ((height - 2. * padding_y) % cell_height) / 2.;
            }

            self.size_info.padding_x = padding_x.floor();
            self.size_info.padding_y = padding_y.floor();

            // Subtract message bar lines for pty size
            let mut pty_size = self.size_info;
            if let Some(lines) = message_bar_lines {
                pty_size.height -= pty_size.cell_height * lines as f32;
            }

            if message_bar_lines.is_some()
                || previous_cols != pty_size.cols()
                || previous_lines != pty_size.lines()
            {
                pty_resize_handle.on_resize(&pty_size);
            }

            self.context.resize(psize);
            self.renderer.resize(psize, self.size_info.padding_x, self.size_info.padding_y);
            let _ = self.event_proxy.send_event(Event::Resize(self.size_info));
        }
    }

    /// Draw the screen
    ///
    /// A reference to Term whose state is being drawn must be provided.
    ///
    /// This call may block if vsync is enabled
    pub fn draw(&mut self, render_update: &RenderUpdate) {
        let RenderUpdate {
            visual_bell_intensity,
            background_color,
            message_buffer,
            grid_cells,
            config,
        } = render_update;
        let metrics = self.glyph_cache.font_metrics();
        let size_info = self.size_info;

        self.renderer.with_api(&config, &size_info, |api| {
            api.clear(*background_color);
        });

        {
            let glyph_cache = &mut self.glyph_cache;
            let mut rects = Rects::new(&metrics, &size_info);

            // Draw grid
            {
                let _sampler = self.meter.sampler();

                self.renderer.with_api(&config, &size_info, |mut api| {
                    // Iterate over all non-empty cells in the grid
                    for cell in grid_cells {
                        // Update underline/strikeout
                        rects.update_lines(&size_info, *cell);

                        // Draw the cell
                        api.render_cell(*cell, glyph_cache);
                    }
                });
            }

            if let Some(message) = message_buffer {
                let text = message.text(&size_info);

                // Create a new rectangle for the background
                let start_line = size_info.lines().0 - text.len();
                let y = size_info.padding_y + size_info.cell_height * start_line as f32;
                let rect = Rect::new(0., y, size_info.width, size_info.height - y);
                rects.push(rect, message.color());

                // Draw rectangles including the new background
                self.renderer.draw_rects(&config, &size_info, *visual_bell_intensity, rects);

                // Relay messages to the user
                let mut offset = 1;
                for message_text in text.iter().rev() {
                    self.renderer.with_api(&config, &size_info, |mut api| {
                        api.render_string(
                            &message_text,
                            Line(size_info.lines().saturating_sub(offset)),
                            glyph_cache,
                            None,
                        );
                    });
                    offset += 1;
                }
            } else {
                // Draw rectangles
                self.renderer.draw_rects(&config, &size_info, *visual_bell_intensity, rects);
            }

            // Draw render timer
            if config.render_timer() {
                let timing = format!("{:.3} usec", self.meter.average());
                let color = Rgb { r: 0xd5, g: 0x4e, b: 0x53 };
                self.renderer.with_api(&config, &size_info, |mut api| {
                    api.render_string(&timing[..], size_info.lines() - 2, glyph_cache, Some(color));
                });
            }
        }

        self.context.swap_buffers().expect("swap buffers");
    }
}

impl From<Display<PossiblyCurrent>> for Display<NotCurrent> {
    fn from(display: Display<PossiblyCurrent>) -> Self {
        unsafe {
            Display {
                context: display.context.make_not_current().expect("disabling context"),
                renderer: display.renderer,
                glyph_cache: display.glyph_cache,
                rx: display.rx,
                tx: display.tx,
                meter: display.meter,
                font_size: display.font_size,
                size_info: display.size_info,
                event_proxy: display.event_proxy,
            }
        }
    }
}

impl From<Display<NotCurrent>> for Display<PossiblyCurrent> {
    fn from(display: Display<NotCurrent>) -> Self {
        unsafe {
            Display {
                context: display.context.make_current().expect("enabling context"),
                renderer: display.renderer,
                glyph_cache: display.glyph_cache,
                rx: display.rx,
                tx: display.tx,
                meter: display.meter,
                font_size: display.font_size,
                size_info: display.size_info,
                event_proxy: display.event_proxy,
            }
        }
    }
}
