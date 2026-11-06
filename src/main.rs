//! display-dimmer — a click-through black overlay used to dim a panel whose
//! hardware backlight is already at its floor.
//!
//! The overlay lives on the `zwlr_layer_shell_v1` overlay layer, anchored to all
//! screen edges, with an empty input region, so pointer and keyboard input pass
//! straight through it.
//!
//! usage: dimmer [percent]   (percent = how much black is laid over the screen)
//!                        or set DIMMER_PERCENT; 0 disables the effect

use std::env;
use std::os::fd::{AsFd, OwnedFd};

use memmap2::MmapMut;
use wayland_client::globals::registry_queue_init;
use wayland_client::protocol::{
    wl_buffer, wl_compositor, wl_region, wl_registry, wl_shm, wl_shm_pool, wl_surface,
};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols_wlr::layer_shell::v1::client::{zwlr_layer_shell_v1, zwlr_layer_surface_v1};

struct ShmBuffer {
    buffer: wl_buffer::WlBuffer,
    _file: OwnedFd,
    width: i32,
    height: i32,
}

struct State {
    // held so the globals stay bound for the lifetime of the overlay
    #[allow(dead_code)]
    compositor: wl_compositor::WlCompositor,
    shm: wl_shm::WlShm,
    #[allow(dead_code)]
    layer_shell: zwlr_layer_shell_v1::ZwlrLayerShellV1,
    surface: wl_surface::WlSurface,
    alpha: u8,
    buffer: Option<ShmBuffer>,
}

impl State {
    fn attach_buffer(&mut self, qh: &QueueHandle<Self>, width: i32, height: i32) {
        let stride = width * 4;
        let size = stride * height;

        let Some(file) = memfd("dimmer", size as usize) else {
            eprintln!("dimmer: cannot create shared memory file");
            return;
        };
        let Ok(map) = (unsafe { MmapMut::map_mut(&file) }) else {
            eprintln!("dimmer: cannot map shared memory");
            return;
        };

        let pool = self.shm.create_pool(file.as_fd(), size, qh, ());
        let buffer = pool.create_buffer(0, width, height, stride, wl_shm::Format::Argb8888, qh, ());
        pool.destroy();

        let mut map = map;
        let pixel: [u8; 4] = [0, 0, 0, self.alpha]; // premultiplied ARGB8888
        for px in map.as_chunks_mut::<4>().0 {
            px.copy_from_slice(&pixel);
        }
        drop(map); // the memfd pages stay mapped by the compositor; only the fd must live on

        self.surface.attach(Some(&buffer), 0, 0);
        if self.surface.version() >= 4 {
            self.surface.damage_buffer(0, 0, width, height);
        } else {
            self.surface.damage(0, 0, width, height);
        }
        self.surface.commit();

        self.buffer = Some(ShmBuffer {
            buffer,
            _file: file,
            width,
            height,
        });
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
    // SAFETY: ftruncate on a memfd with a valid fd.
    if unsafe { libc::ftruncate(std::os::fd::AsRawFd::as_raw_fd(&fd), size as libc::off_t) } < 0 {
        return None;
    }
    Some(fd)
}

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

impl Dispatch<wl_compositor::WlCompositor, ()> for State {
    fn event(
        _: &mut Self,
        _: &wl_compositor::WlCompositor,
        _: <wl_compositor::WlCompositor as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_region::WlRegion, ()> for State {
    fn event(
        _: &mut Self,
        _: &wl_region::WlRegion,
        _: <wl_region::WlRegion as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_shm::WlShm, ()> for State {
    fn event(
        _: &mut Self,
        _: &wl_shm::WlShm,
        _: <wl_shm::WlShm as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_shm_pool::WlShmPool, ()> for State {
    fn event(
        _: &mut Self,
        _: &wl_shm_pool::WlShmPool,
        _: <wl_shm_pool::WlShmPool as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_buffer::WlBuffer, ()> for State {
    fn event(
        _: &mut Self,
        _: &wl_buffer::WlBuffer,
        _: <wl_buffer::WlBuffer as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_surface::WlSurface, ()> for State {
    fn event(
        _: &mut Self,
        _: &wl_surface::WlSurface,
        _: <wl_surface::WlSurface as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<zwlr_layer_shell_v1::ZwlrLayerShellV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &zwlr_layer_shell_v1::ZwlrLayerShellV1,
        _: <zwlr_layer_shell_v1::ZwlrLayerShellV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1, ()> for State {
    fn event(
        state: &mut Self,
        layer: &zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
        event: <zwlr_layer_surface_v1::ZwlrLayerSurfaceV1 as wayland_client::Proxy>::Event,
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
                let unchanged = state
                    .buffer
                    .as_ref()
                    .is_some_and(|b| b.width == width as i32 && b.height == height as i32);
                if unchanged {
                    // same size as the current buffer: just re-attach it
                    if let Some(buffer) = state.buffer.as_ref() {
                        state.surface.attach(Some(&buffer.buffer), 0, 0);
                        state.surface.commit();
                    }
                } else {
                    state.attach_buffer(qh, width as i32, height as i32);
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

fn percent() -> u8 {
    let raw = env::args()
        .nth(1)
        .or_else(|| env::var("DIMMER_PERCENT").ok());
    raw.and_then(|v| v.trim().parse::<i32>().ok())
        .unwrap_or(70)
        .clamp(0, 100) as u8
}

fn main() {
    let alpha = (u16::from(percent()) * 255 / 100) as u8;

    let conn = match Connection::connect_to_env() {
        Ok(conn) => conn,
        Err(err) => {
            eprintln!("dimmer: cannot connect to a Wayland display: {err}");
            std::process::exit(1);
        }
    };
    let (globals, mut queue) = match registry_queue_init::<State>(&conn) {
        Ok(pair) => pair,
        Err(err) => {
            eprintln!("dimmer: cannot read the registry: {err}");
            std::process::exit(1);
        }
    };
    let qh = queue.handle();

    let bind = |interface: &str, err: wayland_client::globals::BindError| -> ! {
        eprintln!("dimmer: cannot bind {interface}: {err}");
        std::process::exit(1)
    };
    let compositor = globals
        .bind::<wl_compositor::WlCompositor, State, ()>(&qh, 4..=6, ())
        .unwrap_or_else(|err| bind("wl_compositor", err));
    let shm = globals
        .bind::<wl_shm::WlShm, State, ()>(&qh, 1..=1, ())
        .unwrap_or_else(|err| bind("wl_shm", err));
    let layer_shell = globals
        .bind::<zwlr_layer_shell_v1::ZwlrLayerShellV1, State, ()>(&qh, 1..=4, ())
        .unwrap_or_else(|err| bind("zwlr_layer_shell_v1", err));

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
        shm,
        layer_shell,
        surface,
        alpha,
        buffer: None,
    };

    while queue.blocking_dispatch(&mut state).is_ok() {}

    eprintln!("dimmer: wayland connection lost");
    std::process::exit(1);
}
