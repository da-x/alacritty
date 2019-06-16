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
//
//! Alacritty - The GPU Enhanced Terminal
#![deny(clippy::all, clippy::if_not_else, clippy::enum_glob_use, clippy::wrong_pub_self_convention)]
#![cfg_attr(feature = "nightly", feature(core_intrinsics))]
#![cfg_attr(all(test, feature = "bench"), feature(test))]
// With the default subsystem, 'console', windows creates an additional console
// window for the program.
// This is silently ignored on non-windows systems.
// See https://msdn.microsoft.com/en-us/library/4cc7ya5b.aspx for more details.
#![windows_subsystem = "windows"]

#[macro_use]
extern crate log;

#[cfg(target_os = "macos")]
use std::env;
use std::error::Error;
use std::fs;
use std::io::{self, Write};
#[cfg(not(windows))]
use std::os::unix::io::AsRawFd;
use std::sync::mpsc;
use std::sync::Arc;

#[cfg(target_os = "macos")]
use dirs;
use glutin::event_loop::EventLoop as GlutinEventLoop;
use glutin::{NotCurrent, PossiblyCurrent};
use log::{error, info};
#[cfg(windows)]
use winapi::um::wincon::{AttachConsole, FreeConsole, ATTACH_PARENT_PROCESS};

use alacritty_terminal::clipboard::Clipboard;
use alacritty_terminal::config::Config;
use alacritty_terminal::die;
use alacritty_terminal::event::Event;
use alacritty_terminal::event_loop::{self, EventLoop, Msg};
#[cfg(target_os = "macos")]
use alacritty_terminal::locale;
use alacritty_terminal::message_bar::MessageBuffer;
use alacritty_terminal::panic;
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::Term;
use alacritty_terminal::tty;
use alacritty_terminal::util;
use alacritty_terminal::util::fmt::Red;

mod cli;
mod config;
mod display;
mod event;
mod input;
mod logging;
mod window;

use crate::cli::Options;
use crate::config::monitor::Monitor;
use crate::display::Display;
use crate::event::Processor;
use crate::window::WindowedContext;

fn main() {
    panic::attach_handler();

    // When linked with the windows subsystem windows won't automatically attach
    // to the console of the parent process, so we do it explicitly. This fails
    // silently if the parent has no console.
    #[cfg(windows)]
    unsafe {
        AttachConsole(ATTACH_PARENT_PROCESS);
    }

    // Load command line options
    let options = Options::new();

    // Setup storage for message UI
    let message_buffer = MessageBuffer::new();

    // Initialize the logger as soon as possible as to capture output from other subsystems
    let log_file =
        logging::initialize(&options, message_buffer.tx()).expect("Unable to initialize logger");

    // Load configuration file
    // If the file is a command line argument, we won't write a generated default file
    let config_path = options
        .config_path()
        .or_else(config::installed_config)
        .or_else(|| config::write_defaults().ok())
        .map(|path| path.to_path_buf());
    let config = if let Some(path) = config_path {
        config::load_from(path)
    } else {
        error!("Unable to write the default config");
        Config::default()
    };
    let config = options.into_config(config);

    // Update the log level from config
    log::set_max_level(config.debug.log_level);

    // Switch to home directory
    #[cfg(target_os = "macos")]
    env::set_current_dir(dirs::home_dir().unwrap()).unwrap();
    // Set locale
    #[cfg(target_os = "macos")]
    locale::set_locale_environment();

    // Store if log file should be deleted before moving config
    let persistent_logging = config.persistent_logging();

    // Run alacritty
    if let Err(err) = run(config, message_buffer) {
        die!("Alacritty encountered an unrecoverable error:\n\n\t{}\n", Red(err));
    }

    // Clean up logfile
    if let Some(log_file) = log_file {
        if !persistent_logging && fs::remove_file(&log_file).is_ok() {
            let _ = writeln!(io::stdout(), "Deleted log file at {:?}", log_file);
        }
    }
}

/// Run Alacritty
///
/// Creates a window, the terminal state, pty, I/O event loop, input processor,
/// config change monitor, and runs the main display loop.
fn run(mut config: Config, message_buffer: MessageBuffer) -> Result<(), Box<dyn Error>> {
    info!("Welcome to Alacritty");
    if let Some(config_path) = &config.config_path {
        info!("Configuration loaded from {:?}", config_path.display());
    };

    // Set environment variables
    tty::setup_env(&config);

    // Create the window
    let window_event_loop = GlutinEventLoop::<Event>::new_user_event();
    let event_proxy = window_event_loop.create_proxy();
    let WindowedContext { mut window, context } =
        WindowedContext::new(&config, &window_event_loop)?;

    // Create a display.
    //
    // The display manages a window and can draw the terminal
    let display = Display::new(&config, &mut window, context, event_proxy.clone())?;

    info!("PTY Dimensions: {:?} x {:?}", display.size().lines(), display.size().cols());

    // Create new native clipboard
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let clipboard = Clipboard::new(window.wayland_display());
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    let clipboard = Clipboard::new();

    // Create the terminal
    //
    // This object contains all of the state about what's being displayed. It's
    // wrapped in a clonable mutex since both the I/O loop and display need to
    // access it.
    let terminal = Term::new(
        &config,
        display.size().to_owned(),
        message_buffer,
        clipboard,
        event_proxy.clone(),
    );
    let terminal = Arc::new(FairMutex::new(terminal));

    // Create the pty
    //
    // The pty forks a process to run the shell on the slave side of the
    // pseudoterminal. A file descriptor for the master side is retained for
    // reading/writing to the shell.
    let pty = tty::new(&config, &display.size(), window.get_window_id());

    // Get a reference to something that we can resize
    //
    // This exists because rust doesn't know the interface is thread-safe
    // and we need to be able to resize the PTY from the main thread while the IO
    // thread owns the EventedRW object.
    #[cfg(windows)]
    let mut resize_handle = pty.resize_handle();
    #[cfg(not(windows))]
    let mut resize_handle = pty.fd.as_raw_fd();

    // Create the pseudoterminal I/O loop
    //
    // pty I/O is ran on another thread as to not occupy cycles used by the
    // renderer and input processing. Note that access to the terminal state is
    // synchronized since the I/O loop updates the state, and the display
    // consumes it periodically.
    let event_loop =
        EventLoop::new(Arc::clone(&terminal), event_proxy.clone(), pty, config.debug.ref_test);

    // The event loop channel allows write requests from the event processor
    // to be sent to the loop and ultimately written to the pty.
    let loop_tx = event_loop.channel();

    // Event processor
    //
    // Need the Rc<RefCell<_>> here since a ref is shared in the resize callback
    let mut processor = Processor::new(
        &config,
        event_loop::Notifier(event_loop.channel()),
        display.resize_channel(),
        display.size().to_owned(),
    );

    // Create a config monitor when config was loaded from path
    //
    // The monitor watches the config file for changes and reloads it. Pending
    // config changes are processed in the main loop.
    if config.live_config_reload() {
        config.config_path.as_ref().map(|path| Monitor::new(path, event_proxy.clone()));
    }

    // Kick off the I/O thread
    let io_thread = event_loop.spawn();

    info!("Initialisation complete");

    // Render queue
    let (render_tx, render_rx) = mpsc::channel();

    // Deactivate display context to move it between threads
    let display: Display<NotCurrent> = display.into();

    let mut conf = config.clone();
    let render_thread = util::thread::spawn_named("rendering", move || {
        // Reactivate display context
        let mut display: Display<PossiblyCurrent> = display.into();

        loop {
            // Check for resize
            display.handle_resize(&conf, &mut resize_handle);

            // Request redraw
            let _ = event_proxy.send_event(Event::RedrawRequest);

            let render_update = match render_rx.recv() {
                Ok(update) => update,
                Err(_) => break,
            };

            // Draw the terminal
            display.draw(&render_update);

            conf = render_update.config;
        }
    });

    // XXX: The window needs to be borrowed so it is dropped after the context
    // Start event loop and block until shutdown
    processor.process_events(&mut config, terminal, &mut window, window_event_loop, render_tx);

    loop_tx.send(Msg::Shutdown).expect("Error sending shutdown to pty event loop");

    render_thread.join().expect("join render thread");
    io_thread.join().expect("join io thread");

    // FIXME patch notify library to have a shutdown method
    // config_reloader.join().ok();

    // Without explicitly detaching the console cmd won't redraw it's prompt
    #[cfg(windows)]
    unsafe {
        FreeConsole();
    }

    info!("Goodbye");

    Ok(())
}
