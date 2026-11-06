//! display-dimmer — a click-through black overlay used to dim a panel whose
//! hardware backlight is already at its floor.
//!
//! The overlay lives on the `zwlr_layer_shell_v1` overlay layer, anchored to all
//! screen edges, with an empty input region, so pointer and keyboard input pass
//! straight through it.
//!
//! The daemon is controlled over a Unix socket in `$XDG_RUNTIME_DIR`, so the
//! intensity can be changed without restarting it:
//!
//! ```text
//! dimmer              run the daemon (level from the saved state, $DIMMER_PERCENT or 70)
//! dimmer set <0-100>  set the amount of black laid over the screen
//! dimmer up [step]    reduce the overlay by <step> percent (default 5)
//! dimmer down [step]  raise the overlay by <step> percent (default 5)
//! dimmer get          print the current level
//! ```

use std::env;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::process::ExitCode;

use memmap2::MmapMut;
use wayland_client::globals::registry_queue_init;
use wayland_client::protocol::{
    wl_buffer, wl_compositor, wl_region, wl_registry, wl_shm, wl_shm_pool, wl_surface,
};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols_wlr::layer_shell::v1::client::{zwlr_layer_shell_v1, zwlr_layer_surface_v1};

const DEFAULT_LEVEL: u8 = 70;
const DEFAULT_STEP: u8 = 5;

// ---------------------------------------------------------------- paths

fn runtime_path(name: &str) -> PathBuf {
    let dir = env::var_os("XDG_RUNTIME_DIR").map_or_else(|| PathBuf::from("/tmp"), PathBuf::from);
    dir.join(name)
}

fn socket_path() -> PathBuf {
    runtime_path("dimmer.sock")
}

fn state_path() -> PathBuf {
    match env::var_os("XDG_CONFIG_HOME") {
        Some(dir) => PathBuf::from(dir).join("dimmer/level"),
        None => match env::var_os("HOME") {
            Some(home) => PathBuf::from(home).join(".config/dimmer/level"),
            None => runtime_path("dimmer.level"),
        },
    }
}

fn saved_level() -> Option<u8> {
    fs::read_to_string(state_path())
        .ok()?
        .trim()
        .parse::<u8>()
        .ok()
        .map(|level| level.min(100))
}

fn save_level(level: u8) {
    let path = state_path();
    if let Some(parent) = path.parent()
        && fs::create_dir_all(parent).is_err()
    {
        return;
    }
    let _ = fs::write(path, format!("{level}\n"));
}

fn initial_level() -> u8 {
    saved_level()
        .or_else(|| env::var("DIMMER_PERCENT").ok()?.trim().parse().ok())
        .unwrap_or(DEFAULT_LEVEL)
        .min(100)
}

fn alpha_for(level: u8) -> u8 {
    (u16::from(level.min(100)) * 255 / 100) as u8
}

// ---------------------------------------------------------------- commands

#[derive(Clone, Copy)]
enum Command {
    Set(u8),
    Up(u8),
    Down(u8),
    Get,
}

fn apply(command: Command, level: u8) -> u8 {
    match command {
        Command::Set(value) => value,
        Command::Up(step) => level.saturating_sub(step),
        Command::Down(step) => level.saturating_add(step).min(100),
        Command::Get => level,
    }
}

fn parse_command(line: &str) -> Option<Command> {
    let mut words = line.split_whitespace();
    let verb = words.next()?;
    let argument = words.next().and_then(|word| word.parse::<u8>().ok());
    match (verb, argument) {
        ("set", Some(value)) => Some(Command::Set(value.min(100))),
        ("up", step) => Some(Command::Up(step.unwrap_or(DEFAULT_STEP))),
        ("down", step) => Some(Command::Down(step.unwrap_or(DEFAULT_STEP))),
        ("get", _) => Some(Command::Get),
        _ => None,
    }
}

// ---------------------------------------------------------------- daemon

struct ShmBuffer {
    buffer: wl_buffer::WlBuffer,
    _file: OwnedFd,
    map: MmapMut,
    width: i32,
    height: i32,
}

struct State {
    // held so the globals stay bound for the lifetime of the overlay
    #[allow(dead_code)]
    compositor: wl_compositor::WlCompositor,
    #[allow(dead_code)]
    layer_shell: zwlr_layer_shell_v1::ZwlrLayerShellV1,
    shm: wl_shm::WlShm,
    surface: wl_surface::WlSurface,
    level: u8,
    buffer: Option<ShmBuffer>,
}

impl State {
    fn create_buffer(&mut self, qh: &QueueHandle<Self>, width: i32, height: i32) {
        let stride = width * 4;
        let size = stride * height;

        let Some(file) = memfd("dimmer", size as usize) else {
            eprintln!("dimmer: cannot create shared memory file");
            return;
        };
        let Ok(mut map) = (unsafe { MmapMut::map_mut(&file) }) else {
            eprintln!("dimmer: cannot map shared memory");
            return;
        };

        let pool = self.shm.create_pool(file.as_fd(), size, qh, ());
        let buffer = pool.create_buffer(0, width, height, stride, wl_shm::Format::Argb8888, qh, ());
        pool.destroy();

        fill(&mut map, self.level);
        self.buffer = Some(ShmBuffer {
            buffer,
            _file: file,
            map,
            width,
            height,
        });
        self.paint();
    }

    /// repaint with the current level: refill the shm buffer and commit it
    fn paint(&mut self) {
        let Some(buffer) = self.buffer.as_mut() else {
            return;
        };
        fill(&mut buffer.map, self.level);
        self.surface.attach(Some(&buffer.buffer), 0, 0);
        if self.surface.version() >= 4 {
            self.surface
                .damage_buffer(0, 0, buffer.width, buffer.height);
        } else {
            self.surface.damage(0, 0, buffer.width, buffer.height);
        }
        self.surface.commit();
    }

    fn set_level(&mut self, level: u8) {
        if level == self.level {
            return;
        }
        self.level = level;
        self.paint();
        save_level(level);
    }
}

fn fill(map: &mut MmapMut, level: u8) {
    let pixel: [u8; 4] = [0, 0, 0, alpha_for(level)]; // premultiplied ARGB8888
    for px in map.as_chunks_mut::<4>().0 {
        px.copy_from_slice(&pixel);
    }
}

fn memfd(name: &str, size: usize) -> Option<OwnedFd> {
    use std::os::fd::FromRawFd;

    let c_name = std::ffi::CString::new(name).ok()?;
    // SAFETY: memfd_create takes a valid NUL-terminated string and a flags int.
    let fd = unsafe { libc::memfd_create(c_name.as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        return None;
    }
    // SAFETY: fd is a fresh, owned descriptor from memfd_create.
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    // SAFETY: ftruncate on a valid memfd.
    if unsafe { libc::ftruncate(fd.as_raw_fd(), size as libc::off_t) } < 0 {
        return None;
    }
    Some(fd)
}

macro_rules! inert_dispatch {
    ($($interface:ty),* $(,)?) => {
        $(
            impl Dispatch<$interface, ()> for State {
                fn event(
                    _: &mut Self,
                    _: &$interface,
                    _: <$interface as Proxy>::Event,
                    _: &(),
                    _: &Connection,
                    _: &QueueHandle<Self>,
                ) {
                }
            }
        )*
    };
}

inert_dispatch!(
    wl_compositor::WlCompositor,
    wl_region::WlRegion,
    wl_shm::WlShm,
    wl_shm_pool::WlShmPool,
    wl_buffer::WlBuffer,
    wl_surface::WlSurface,
    zwlr_layer_shell_v1::ZwlrLayerShellV1,
);

impl Dispatch<wl_registry::WlRegistry, wayland_client::globals::GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: <wl_registry::WlRegistry as Proxy>::Event,
        _: &wayland_client::globals::GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1, ()> for State {
    fn event(
        state: &mut Self,
        layer: &zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
        event: <zwlr_layer_surface_v1::ZwlrLayerSurfaceV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_layer_surface_v1::Event::Configure {
                serial,
                width,
                height,
            } => {
                layer.ack_configure(serial);
                if width == 0 || height == 0 {
                    return;
                }
                let same_size = state
                    .buffer
                    .as_ref()
                    .is_some_and(|b| b.width == width as i32 && b.height == height as i32);
                if same_size {
                    state.paint();
                } else {
                    state.create_buffer(qh, width as i32, height as i32);
                }
            }
            zwlr_layer_surface_v1::Event::Closed => {
                eprintln!("dimmer: layer surface closed by compositor");
                std::process::exit(0);
            }
            _ => {}
        }
    }
}

fn serve(level: u8) -> ExitCode {
    let conn = match Connection::connect_to_env() {
        Ok(conn) => conn,
        Err(err) => {
            eprintln!("dimmer: cannot connect to a Wayland display: {err}");
            return ExitCode::FAILURE;
        }
    };
    let (globals, mut queue) = match registry_queue_init::<State>(&conn) {
        Ok(pair) => pair,
        Err(err) => {
            eprintln!("dimmer: cannot read the registry: {err}");
            return ExitCode::FAILURE;
        }
    };
    let qh = queue.handle();

    let fail = |interface: &str, err: wayland_client::globals::BindError| {
        eprintln!("dimmer: cannot bind {interface}: {err}");
        ExitCode::FAILURE
    };
    let compositor = match globals.bind::<wl_compositor::WlCompositor, State, ()>(&qh, 4..=6, ()) {
        Ok(proxy) => proxy,
        Err(err) => return fail("wl_compositor", err),
    };
    let shm = match globals.bind::<wl_shm::WlShm, State, ()>(&qh, 1..=1, ()) {
        Ok(proxy) => proxy,
        Err(err) => return fail("wl_shm", err),
    };
    let layer_shell =
        match globals.bind::<zwlr_layer_shell_v1::ZwlrLayerShellV1, State, ()>(&qh, 1..=4, ()) {
            Ok(proxy) => proxy,
            Err(err) => return fail("zwlr_layer_shell_v1", err),
        };

    let surface = compositor.create_surface(&qh, ());

    // empty input region: the overlay never receives pointer or keyboard input
    let region = compositor.create_region(&qh, ());
    surface.set_input_region(Some(&region));
    region.destroy();

    let layer = layer_shell.get_layer_surface(
        &surface,
        None,
        zwlr_layer_shell_v1::Layer::Overlay,
        "dimmer".to_string(),
        &qh,
        (),
    );
    layer.set_anchor(
        zwlr_layer_surface_v1::Anchor::Top
            | zwlr_layer_surface_v1::Anchor::Bottom
            | zwlr_layer_surface_v1::Anchor::Left
            | zwlr_layer_surface_v1::Anchor::Right,
    );
    layer.set_exclusive_zone(-1);
    layer.set_keyboard_interactivity(zwlr_layer_surface_v1::KeyboardInteractivity::None);
    layer.set_size(0, 0);
    surface.commit();

    let mut state = State {
        compositor,
        layer_shell,
        shm,
        surface,
        level: level.min(100),
        buffer: None,
    };

    let path = socket_path();
    let _ = fs::remove_file(&path);
    let listener = match UnixListener::bind(&path) {
        Ok(listener) => listener,
        Err(err) => {
            eprintln!("dimmer: cannot listen on {}: {err}", path.display());
            return ExitCode::FAILURE;
        }
    };
    // only the session user may talk to the daemon
    let _ = fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o600));

    let wayland_fd = conn.backend().poll_fd().as_raw_fd();
    let listener_fd = listener.as_raw_fd();

    loop {
        // push everything the handlers produced (paint/commit) to the compositor
        if let Err(err) = conn.flush() {
            eprintln!("dimmer: flush failed: {err}");
            return ExitCode::FAILURE;
        }
        if let Err(err) = queue.dispatch_pending(&mut state) {
            eprintln!("dimmer: dispatch failed: {err}");
            return ExitCode::FAILURE;
        }

        let mut fds = [
            libc::pollfd {
                fd: wayland_fd,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: listener_fd,
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        // SAFETY: fds is a valid array of pollfd with two initialised entries.
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) };
        if ready < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            eprintln!("dimmer: poll failed: {err}");
            return ExitCode::FAILURE;
        }

        if fds[1].revents != 0 {
            handle_connection(&listener, &mut state);
        }

        if fds[0].revents != 0
            && let Some(guard) = conn.prepare_read()
        {
            let _ = guard.read();
        }
    }
}

fn handle_connection(listener: &UnixListener, state: &mut State) {
    let Ok((stream, _)) = listener.accept() else {
        return;
    };
    let Ok(request) = BufReader::new(&stream).lines().next().transpose() else {
        return;
    };
    let mut writer = &stream;
    let Some(request) = request else {
        return;
    };
    let Some(command) = parse_command(&request) else {
        let _ = writeln!(writer, "error: unknown command {request:?}");
        return;
    };
    state.set_level(apply(command, state.level));
    let _ = writeln!(writer, "{}", state.level);
}

// ---------------------------------------------------------------- client

fn request(command: &str) -> Result<String, String> {
    let path = socket_path();
    let mut stream = UnixStream::connect(&path)
        .map_err(|err| format!("cannot reach the daemon on {}: {err}", path.display()))?;
    writeln!(stream, "{command}").map_err(|err| err.to_string())?;
    let mut reply = String::new();
    BufReader::new(&stream)
        .read_line(&mut reply)
        .map_err(|err| err.to_string())?;
    Ok(reply.trim().to_string())
}

fn usage() -> ExitCode {
    eprintln!(
        "usage:\n  \
         dimmer               run the daemon\n  \
         dimmer set <0-100>   set the overlay level (percent of black)\n  \
         dimmer up [step]     reduce the overlay (default {DEFAULT_STEP})\n  \
         dimmer down [step]   raise the overlay (default {DEFAULT_STEP})\n  \
         dimmer get           print the current level"
    );
    ExitCode::FAILURE
}

fn main() -> ExitCode {
    let mut args = env::args().skip(1);
    let Some(verb) = args.next() else {
        return serve(initial_level());
    };
    let argument = args.next();
    let command = match (verb.as_str(), argument.as_deref()) {
        ("serve", None) => return serve(initial_level()),
        ("set", Some(value)) => match value.parse::<u8>() {
            Ok(value) => Command::Set(value.min(100)),
            Err(_) => return usage(),
        },
        ("up", step) => Command::Up(step.and_then(|s| s.parse().ok()).unwrap_or(DEFAULT_STEP)),
        ("down", step) => Command::Down(step.and_then(|s| s.parse().ok()).unwrap_or(DEFAULT_STEP)),
        ("get", None) => Command::Get,
        _ => return usage(),
    };

    let wire = match command {
        Command::Set(value) => format!("set {value}"),
        Command::Up(step) => format!("up {step}"),
        Command::Down(step) => format!("down {step}"),
        Command::Get => "get".to_string(),
    };

    match request(&wire) {
        Ok(reply) if reply.starts_with("error:") => {
            eprintln!("dimmer: {reply}");
            ExitCode::FAILURE
        }
        Ok(reply) => {
            if matches!(command, Command::Get) {
                println!("{reply}");
            }
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("dimmer: {err}");
            ExitCode::FAILURE
        }
    }
}
