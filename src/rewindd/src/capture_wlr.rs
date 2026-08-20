//! Persistent wlr-screencopy session. Compiled only with `--features wayland`.
//! The Wayland connection, registry, manager, outputs, event queue, and
//! reusable shm pool/buffer stay alive across capture ticks.

#![allow(dead_code)]

use crate::capture::RawFrame;
use crate::pixel::{self, ShmFormat};
use memmap2::MmapMut;
use wayland_client::WEnum;
use std::fs::File;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::io::AsRawFd;
use wayland_client::{
    protocol::{
        wl_buffer::WlBuffer,
        wl_output::{self, WlOutput},
        wl_registry,
        wl_shm::{self, WlShm},
        wl_shm_pool::WlShmPool,
    },
    Connection, Dispatch, EventQueue, QueueHandle,
};
use wayland_protocols_wlr::screencopy::v1::client::{
    zwlr_screencopy_frame_v1::{self, ZwlrScreencopyFrameV1},
    zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1,
};

struct State {
    manager: Option<ZwlrScreencopyManagerV1>,
    shm: Option<WlShm>,
    outputs: Vec<(u32, WlOutput, String)>,
    want_output: String,
    width: i32,
    height: i32,
    stride: i32,
    format: wl_shm::Format,
    buffer_event: bool,
    ready: bool,
    failed: bool,
}

impl Default for State {
    fn default() -> Self {
        Self {
            manager: None,
            shm: None,
            outputs: Vec::new(),
            want_output: String::new(),
            width: 0,
            height: 0,
            stride: 0,
            format: wl_shm::Format::Argb8888,
            buffer_event: false,
            ready: false,
            failed: false,
        }
    }
}

/// Long-lived screencopy session. Construct once; call [`Session::grab`] each tick.
pub struct Session {
    _conn: Connection,
    event_queue: EventQueue<State>,
    state: State,
    _registry: wl_registry::WlRegistry,
    shm_file: Option<File>,
    pool: Option<WlShmPool>,
    buffer: Option<WlBuffer>,
    buf_w: i32,
    buf_h: i32,
    buf_stride: i32,
    buf_format: wl_shm::Format,
}

impl Session {
    pub fn connect() -> Result<Self, String> {
        let conn = Connection::connect_to_env().map_err(|e| e.to_string())?;
        let display = conn.display();
        let mut event_queue: EventQueue<State> = conn.new_event_queue();
        let qh = event_queue.handle();
        let registry = display.get_registry(&qh, ());
        let mut state = State::default();
        event_queue
            .roundtrip(&mut state)
            .map_err(|e| e.to_string())?;
        event_queue
            .roundtrip(&mut state)
            .map_err(|e| e.to_string())?;
        if state.manager.is_none() {
            return Err("zwlr_screencopy_manager_v1 missing".into());
        }
        if state.shm.is_none() {
            return Err("wl_shm missing".into());
        }
        Ok(Self {
            _conn: conn,
            event_queue,
            state,
            _registry: registry,
            shm_file: None,
            pool: None,
            buffer: None,
            buf_w: 0,
            buf_h: 0,
            buf_stride: 0,
            buf_format: wl_shm::Format::Argb8888,
        })
    }

    pub fn grab(&mut self, output: &str) -> Result<RawFrame, String> {
        self.state.want_output = output.to_string();
        self.state.buffer_event = false;
        self.state.ready = false;
        self.state.failed = false;

        let manager = self
            .state
            .manager
            .clone()
            .ok_or_else(|| "zwlr_screencopy_manager_v1 missing".to_string())?;
        let output_obj =
            pick_output(&self.state).ok_or_else(|| "no matching wl_output".to_string())?;
        let qh = self.event_queue.handle();
        let frame = manager.capture_output(0, &output_obj, &qh, ());

        while !self.state.buffer_event && !self.state.failed {
            self.event_queue
                .blocking_dispatch(&mut self.state)
                .map_err(|e| e.to_string())?;
        }
        if self.state.failed || self.state.width <= 0 {
            return Err("screencopy frame failed".into());
        }

        self.ensure_buffer()?;
        let buffer = self
            .buffer
            .as_ref()
            .ok_or_else(|| "shm buffer missing".to_string())?;
        frame.copy(buffer);
        while !self.state.ready && !self.state.failed {
            self.event_queue
                .blocking_dispatch(&mut self.state)
                .map_err(|e| e.to_string())?;
        }
        if self.state.failed {
            return Err("screencopy copy failed".into());
        }
        let file = self
            .shm_file
            .as_ref()
            .ok_or_else(|| "shm file missing".to_string())?;
        let rgba = read_shm(
            file,
            self.state.width as u32,
            self.state.height as u32,
            self.state.stride as u32,
            map_shm_format(self.state.format),
        )?;
        Ok(RawFrame {
            rgba,
            width: self.state.width as u32,
            height: self.state.height as u32,
        })
    }

    fn ensure_buffer(&mut self) -> Result<(), String> {
        let w = self.state.width;
        let h = self.state.height;
        let stride = self.state.stride;
        let format = self.state.format;
        if self.buffer.is_some()
            && self.buf_w == w
            && self.buf_h == h
            && self.buf_stride == stride
            && self.buf_format == format
        {
            return Ok(());
        }
        self.buffer = None;
        self.pool = None;
        self.shm_file = None;

        let (fd, file) = shm_file((stride * h) as i64)?;
        let qh = self.event_queue.handle();
        let shm = self
            .state
            .shm
            .clone()
            .ok_or_else(|| "wl_shm missing".to_string())?;
        let pool = shm.create_pool(fd.as_fd(), stride * h, &qh, ());
        let buffer = pool.create_buffer(0, w, h, stride, format, &qh, ());
        self.pool = Some(pool);
        self.buffer = Some(buffer);
        self.shm_file = Some(file);
        self.buf_w = w;
        self.buf_h = h;
        self.buf_stride = stride;
        self.buf_format = format;
        Ok(())
    }
}

fn pick_output(state: &State) -> Option<WlOutput> {
    if state.want_output.is_empty() {
        // Fail closed: an empty output name is an unresolved focused monitor, not
        // a request for "any" display. Never substitute one (it could be a
        // sensitive secondary monitor). Return None so `grab` errors; callers do
        // not reach here with an empty name anyway (guarded in `grab`).
        return None;
    }
    // A focused output WAS named. Match it exactly and NEVER substitute a
    // different one — recording a sensitive secondary display would be a privacy
    // leak. On no match, return None so `grab` errors and the caller falls back
    // to `grim -o <focused-output>`, which targets the correct display.
    state
        .outputs
        .iter()
        .find(|o| o.2 == state.want_output)
        .map(|o| o.1.clone())
}

fn shm_file(size: i64) -> Result<(OwnedFd, File), String> {
    let path = format!(
        "/rewind-screencopy-{}-{}",
        std::process::id(),
        crate::now_ms()
    );
    let fd = shm_open(&path, size)?;
    let file = File::from(fd.try_clone().map_err(|e| e.to_string())?);
    file.set_len(size as u64).map_err(|e| e.to_string())?;
    Ok((fd, file))
}

#[cfg(unix)]
fn shm_open(name: &str, _size: i64) -> Result<OwnedFd, String> {
    use std::ffi::CString;
    let c = CString::new(name).map_err(|e| e.to_string())?;
    unsafe {
        let fd = libc::shm_open(
            c.as_ptr(),
            libc::O_CREAT | libc::O_RDWR | libc::O_EXCL,
            0o600,
        );
        if fd < 0 {
            return Err("shm_open failed".into());
        }
        libc::shm_unlink(c.as_ptr());
        Ok(OwnedFd::from_raw_fd_checked(fd).map_err(|_| "owned fd")?)
    }
}

trait FromRaw {
    fn from_raw_fd_checked(fd: i32) -> Result<OwnedFd, ()>;
}

impl FromRaw for OwnedFd {
    fn from_raw_fd_checked(fd: i32) -> Result<OwnedFd, ()> {
        use std::os::fd::FromRawFd;
        if fd < 0 {
            return Err(());
        }
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }
}

fn map_shm_format(f: wl_shm::Format) -> ShmFormat {
    match f {
        wl_shm::Format::Argb8888 => ShmFormat::Argb8888,
        wl_shm::Format::Xrgb8888 => ShmFormat::Xrgb8888,
        other => {
            let _ = other;
            ShmFormat::Unknown(0)
        }
    }
}

fn read_shm(
    file: &File,
    width: u32,
    height: u32,
    stride: u32,
    fmt: ShmFormat,
) -> Result<Vec<u8>, String> {
    let mmap = unsafe { MmapMut::map_mut(file) }.map_err(|e| e.to_string())?;
    Ok(pixel::decode_buffer(fmt, &mmap, width, height, stride))
}

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "wl_shm" => {
                    state.shm = Some(registry.bind::<WlShm, _, _>(name, version.min(1), qh, ()));
                }
                "wl_output" => {
                    let output = registry.bind::<WlOutput, _, _>(name, version.min(4), qh, name);
                    state.outputs.push((name, output, String::new()));
                }
                "zwlr_screencopy_manager_v1" => {
                    state.manager = Some(registry.bind::<ZwlrScreencopyManagerV1, _, _>(
                        name,
                        version.min(3),
                        qh,
                        (),
                    ));
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<WlShm, ()> for State {
    fn event(
        _: &mut Self,
        _: &WlShm,
        _: wl_shm::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlOutput, u32> for State {
    fn event(
        state: &mut Self,
        output: &WlOutput,
        event: wl_output::Event,
        name: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_output::Event::Name { name: n } = event {
            if let Some(slot) = state.outputs.iter_mut().find(|o| o.0 == *name) {
                slot.2 = n;
            }
            let _ = output;
        }
    }
}

impl Dispatch<ZwlrScreencopyManagerV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &ZwlrScreencopyManagerV1,
        _: wayland_protocols_wlr::screencopy::v1::client::zwlr_screencopy_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwlrScreencopyFrameV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ZwlrScreencopyFrameV1,
        event: zwlr_screencopy_frame_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_screencopy_frame_v1::Event::Buffer {
                format,
                width,
                height,
                stride,
            } => {
                state.format = match format {
                    WEnum::Value(v) => v,
                    WEnum::Unknown(_) => wl_shm::Format::Xrgb8888,
                };
                state.width = width as i32;
                state.height = height as i32;
                state.stride = stride as i32;
                state.buffer_event = true;
            }
            zwlr_screencopy_frame_v1::Event::Ready { .. } => state.ready = true,
            zwlr_screencopy_frame_v1::Event::Failed => state.failed = true,
            _ => {}
        }
    }
}

impl Dispatch<WlShmPool, ()> for State {
    fn event(
        _: &mut Self,
        _: &WlShmPool,
        _: wayland_client::protocol::wl_shm_pool::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlBuffer, ()> for State {
    fn event(
        _: &mut Self,
        _: &WlBuffer,
        _: wayland_client::protocol::wl_buffer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

#[allow(dead_code)]
fn _raw_fd(file: &File) -> i32 {
    file.as_raw_fd()
}
