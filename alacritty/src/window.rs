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
use std::convert::From;
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
use std::ffi::c_void;
use std::fmt;

use glutin::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use glutin::event_loop::EventLoop;
#[cfg(target_os = "macos")]
use glutin::platform::macos::{WindowBuilderExtMacOS, WindowExtMacOS};
#[cfg(not(any(target_os = "macos", windows)))]
use glutin::platform::unix::{EventLoopExtUnix, WindowBuilderExtUnix, WindowExtUnix};
#[cfg(not(target_os = "macos"))]
use glutin::window::Icon;
use glutin::window::{CursorIcon, Window as GlutinWindow, WindowBuilder};
use glutin::{
    self, ContextBuilder, PossiblyCurrent, RawContext, WindowedContext as GlutinWindowedContext,
};
#[cfg(not(target_os = "macos"))]
use image::ImageFormat;

use alacritty_terminal::config::{Config, Decorations, StartupMode, WindowConfig};
use alacritty_terminal::event::Event;
use alacritty_terminal::gl;
use alacritty_terminal::renderer::GlyphCache;
use alacritty_terminal::term::{SizeInfo, Term};

// It's required to be in this directory due to the `windows.rc` file
#[cfg(not(target_os = "macos"))]
static WINDOW_ICON: &'static [u8] = include_bytes!("../../extra/windows/alacritty.ico");

/// Default Alacritty name, used for window title and class.
pub const DEFAULT_NAME: &str = "Alacritty";

/// Window errors
#[derive(Debug)]
pub enum Error {
    /// Error creating the window
    ContextCreation(glutin::CreationError),

    /// Error dealing with fonts
    Font(font::Error),

    /// Error manipulating the rendering context
    Context(glutin::ContextError),
}

/// Result of fallible operations concerning a Window.
type Result<T> = ::std::result::Result<T, Error>;

impl ::std::error::Error for Error {
    fn cause(&self) -> Option<&dyn (::std::error::Error)> {
        match *self {
            Error::ContextCreation(ref err) => Some(err),
            Error::Context(ref err) => Some(err),
            Error::Font(ref err) => Some(err),
        }
    }

    fn description(&self) -> &str {
        match *self {
            Error::ContextCreation(ref _err) => "Error creating gl context",
            Error::Context(ref _err) => "Error operating on render context",
            Error::Font(ref err) => err.description(),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Error::ContextCreation(ref err) => write!(f, "Error creating GL context; {}", err),
            Error::Context(ref err) => write!(f, "Error operating on render context; {}", err),
            Error::Font(ref err) => err.fmt(f),
        }
    }
}

impl From<glutin::CreationError> for Error {
    fn from(val: glutin::CreationError) -> Error {
        Error::ContextCreation(val)
    }
}

impl From<glutin::ContextError> for Error {
    fn from(val: glutin::ContextError) -> Error {
        Error::Context(val)
    }
}

impl From<font::Error> for Error {
    fn from(val: font::Error) -> Error {
        Error::Font(val)
    }
}

fn create_gl_window(
    mut window: WindowBuilder,
    event_loop: &EventLoop<Event>,
    srgb: bool,
    dimensions: Option<LogicalSize>,
) -> Result<GlutinWindowedContext<PossiblyCurrent>> {
    if let Some(dimensions) = dimensions {
        window = window.with_inner_size(dimensions);
    }

    let windowed_context = ContextBuilder::new()
        .with_srgb(srgb)
        .with_vsync(true)
        .with_hardware_acceleration(None)
        .build_windowed(window, event_loop)?;

    // Make the context current so OpenGL operations can run
    let windowed_context = unsafe { windowed_context.make_current().map_err(|(_, e)| e)? };

    Ok(windowed_context)
}

pub struct WindowedContext {
    pub context: RawContext<PossiblyCurrent>,
    pub window: Window,
}

impl WindowedContext {
    /// Create a new window
    ///
    /// This creates a window and fully initializes a window.
    pub fn new(config: &Config, event_loop: &EventLoop<Event>) -> Result<WindowedContext> {
        // Guess DPR based on first monitor
        let estimated_dpr =
            event_loop.available_monitors().next().map(|m| m.hidpi_factor()).unwrap_or(1.);

        // Guess the target window dimensions
        let metrics = GlyphCache::static_metrics(config, estimated_dpr as f32)?;
        let (cell_width, cell_height) = GlyphCache::compute_cell_size(config, &metrics);
        let dimensions =
            GlyphCache::calculate_dimensions(config, estimated_dpr, cell_width, cell_height);

        debug!("Estimated DPR: {}", estimated_dpr);
        debug!("Estimated Cell Size: {} x {}", cell_width, cell_height);
        debug!("Estimated Dimensions: {:?}", dimensions);

        // Create the window where Alacritty will be displayed
        let logical = dimensions.map(|d| PhysicalSize::new(d.0, d.1).to_logical(estimated_dpr));

        let title = config.window.title.as_ref().map_or(DEFAULT_NAME, |t| t);
        let class = config.window.class.as_ref().map_or(DEFAULT_NAME, |c| c);

        let window_builder = Window::get_platform_window(title, class, &config.window);
        let windowed_context =
            create_gl_window(window_builder.clone(), &event_loop, false, logical)
                .or_else(|_| create_gl_window(window_builder, &event_loop, true, logical))?;
        let (context, window) = unsafe { windowed_context.split() };
        window.set_visible(true);

        // Maximize window after mapping in X11
        #[cfg(not(any(target_os = "macos", windows)))]
        {
            if event_loop.is_x11() && config.window.startup_mode() == StartupMode::Maximized {
                window.set_maximized(true);
            }
        }

        // Set window position
        //
        // TODO: replace `set_position` with `with_position` once available
        // Upstream issue: https://github.com/tomaka/winit/issues/806
        if let Some(position) = config.window.position {
            let physical = PhysicalPosition::from((position.x, position.y));
            let logical = physical.to_logical(window.hidpi_factor());
            window.set_outer_position(logical);
        }

        if let StartupMode::Fullscreen = config.window.startup_mode() {
            let current_monitor = window.current_monitor();
            window.set_fullscreen(Some(current_monitor));
        }

        #[cfg(target_os = "macos")]
        {
            if let StartupMode::SimpleFullscreen = config.window.startup_mode() {
                window.set_simple_fullscreen(true);
            }
        }

        // Text cursor
        window.set_cursor_icon(CursorIcon::Text);

        // Set OpenGL symbol loader. This call MUST be after window.make_current on windows.
        gl::load_with(|symbol| context.get_proc_address(symbol) as *const _);

        let window = Window {
            window,
            mouse_visible: true,
            is_fullscreen: false,
            is_simple_fullscreen: false,
        };

        window.run_os_extensions();

        Ok(WindowedContext { window, context })
    }
}

/// A window which can be used for displaying the terminal
///
/// Wraps the underlying windowing library to provide a stable API in Alacritty
pub struct Window {
    window: GlutinWindow,
    mouse_visible: bool,
    is_simple_fullscreen: bool,
    is_fullscreen: bool,
}

impl Window {
    pub fn set_inner_size(&mut self, size: LogicalSize) {
        self.window.set_inner_size(size);
    }

    pub fn inner_size(&self) -> LogicalSize {
        self.window.inner_size()
    }

    #[inline]
    pub fn hidpi_factor(&self) -> f64 {
        self.window.hidpi_factor()
    }

    /// Set the window title
    #[inline]
    pub fn set_title(&self, title: &str) {
        self.window.set_title(title);
    }

    #[inline]
    pub fn set_mouse_cursor(&self, cursor: CursorIcon) {
        self.window.set_cursor_icon(cursor);
    }

    /// Set mouse cursor visible
    pub fn set_mouse_visible(&mut self, visible: bool) {
        if visible != self.mouse_visible {
            self.mouse_visible = visible;
            self.window.set_cursor_visible(visible);
        }
    }

    #[cfg(not(any(target_os = "macos", windows)))]
    pub fn get_platform_window(
        title: &str,
        class: &str,
        window_config: &WindowConfig,
    ) -> WindowBuilder {
        let decorations = match window_config.decorations {
            Decorations::None => false,
            _ => true,
        };

        let image = image::load_from_memory_with_format(WINDOW_ICON, ImageFormat::ICO)
            .expect("loading icon")
            .to_rgba();
        let (width, height) = image.dimensions();
        let icon = Icon::from_rgba(image.into_raw(), width, height);

        WindowBuilder::new()
            .with_title(title)
            .with_visibility(false)
            .with_transparency(true)
            .with_decorations(decorations)
            .with_maximized(window_config.startup_mode() == StartupMode::Maximized)
            .with_window_icon(icon.ok())
            // X11
            .with_class(class.into(), DEFAULT_NAME.into())
            // Wayland
            .with_app_id(class.into())
    }

    #[cfg(windows)]
    pub fn get_platform_window(
        title: &str,
        _class: &str,
        window_config: &WindowConfig,
    ) -> WindowBuilder {
        let decorations = match window_config.decorations {
            Decorations::None => false,
            _ => true,
        };

        let image = image::load_from_memory_with_format(WINDOW_ICON, ImageFormat::ICO)
            .expect("loading icon")
            .to_rgba();
        let (width, height) = image.dimensions();
        let icon = Icon::from_rgba(image.into_raw(), width, height);

        WindowBuilder::new()
            .with_title(title)
            .with_visibility(cfg!(windows))
            .with_decorations(decorations)
            .with_transparency(true)
            .with_maximized(window_config.startup_mode() == StartupMode::Maximized)
            .with_window_icon(icon.ok())
    }

    #[cfg(target_os = "macos")]
    pub fn get_platform_window(
        title: &str,
        _class: &str,
        window_config: &WindowConfig,
    ) -> WindowBuilder {
        let window = WindowBuilder::new()
            .with_title(title)
            .with_visibility(false)
            .with_transparency(true)
            .with_maximized(window_config.startup_mode() == StartupMode::Maximized);

        match window_config.decorations {
            Decorations::Full => window,
            Decorations::Transparent => window
                .with_title_hidden(true)
                .with_titlebar_transparent(true)
                .with_fullsize_content_view(true),
            Decorations::Buttonless => window
                .with_title_hidden(true)
                .with_titlebar_buttons_hidden(true)
                .with_titlebar_transparent(true)
                .with_fullsize_content_view(true),
            Decorations::None => window.with_titlebar_hidden(true),
        }
    }

    #[cfg(any(
        target_os = "linux",
        target_os = "freebsd",
        target_os = "dragonfly",
        target_os = "openbsd"
    ))]
    pub fn set_urgent(&self, is_urgent: bool) {
        self.window.set_urgent(is_urgent);
    }

    #[cfg(target_os = "macos")]
    pub fn set_urgent(&self, is_urgent: bool) {
        self.window.request_user_attention(is_urgent);
    }

    #[cfg(windows)]
    pub fn set_urgent(&self, _is_urgent: bool) {}

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    pub fn get_window_id(&self) -> Option<usize> {
        match self.window.xlib_window() {
            Some(xlib_window) => Some(xlib_window as usize),
            None => None,
        }
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    pub fn get_window_id(&self) -> Option<usize> {
        None
    }

    /// Hide the window
    pub fn hide(&self) {
        self.window.set_visible(false);
    }

    /// Toggle the window's fullscreen state
    pub fn toggle_fullscreen(&mut self) {
        if self.is_simple_fullscreen {
            return;
        }

        if self.is_fullscreen {
            self.window.set_fullscreen(None);
        } else {
            let current_monitor = self.window.current_monitor();
            self.window.set_fullscreen(Some(current_monitor));
        }
        self.is_fullscreen = !self.is_fullscreen;
    }

    #[cfg(target_os = "macos")]
    pub fn toggle_simple_fullscreen(&mut self) {
        if self.is_fullscreen {
            return;
        }

        self.window.set_simple_fullscreen(!self.is_simple_fullscreen);
        self.is_simple_fullscreen = !self.is_simple_fullscreen;
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    pub fn wayland_display(&self) -> Option<*mut c_void> {
        self.window.wayland_display()
    }

    /// Adjust the IME editor position according to the new location of the cursor
    pub fn update_ime_position<T>(&mut self, terminal: &Term<T>, size_info: &SizeInfo) {
        let point = terminal.cursor().point;
        let SizeInfo { cell_width: cw, cell_height: ch, padding_x: px, padding_y: py, .. } =
            size_info;

        let dpr = self.hidpi_factor();
        let nspot_x = f64::from(px + point.col.0 as f32 * cw);
        let nspot_y = f64::from(py + (point.line.0 + 1) as f32 * ch);

        self.window.set_ime_position(PhysicalPosition::from((nspot_x, nspot_y)).to_logical(dpr));
    }
}

pub trait OsExtensions {
    fn run_os_extensions(&self) {}
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "freebsd",
    target_os = "dragonfly",
    target_os = "openbsd"
)))]
impl OsExtensions for Window {}

#[cfg(any(
    target_os = "linux",
    target_os = "freebsd",
    target_os = "dragonfly",
    target_os = "openbsd"
))]
impl OsExtensions for Window {
    fn run_os_extensions(&self) {
        use libc::getpid;
        use std::ffi::CStr;
        use std::ptr;
        use x11_dl::xlib::{self, PropModeReplace, XA_CARDINAL};

        let xlib_display = self.window.xlib_display();
        let xlib_window = self.window.xlib_window();

        if let (Some(xlib_window), Some(xlib_display)) = (xlib_window, xlib_display) {
            let xlib = xlib::Xlib::open().expect("get xlib");

            // Set _NET_WM_PID to process pid
            unsafe {
                let _net_wm_pid = CStr::from_ptr(b"_NET_WM_PID\0".as_ptr() as *const _);
                let atom = (xlib.XInternAtom)(xlib_display as *mut _, _net_wm_pid.as_ptr(), 0);
                let pid = getpid();

                (xlib.XChangeProperty)(
                    xlib_display as _,
                    xlib_window as _,
                    atom,
                    XA_CARDINAL,
                    32,
                    PropModeReplace,
                    &pid as *const i32 as *const u8,
                    1,
                );
            }
            // Although this call doesn't actually pass any data, it does cause
            // WM_CLIENT_MACHINE to be set. WM_CLIENT_MACHINE MUST be set if _NET_WM_PID is set
            // (which we do above).
            unsafe {
                (xlib.XSetWMProperties)(
                    xlib_display as _,
                    xlib_window as _,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                    0,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                );
            }
        }
    }
}
