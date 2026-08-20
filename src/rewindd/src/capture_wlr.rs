//! Persistent wlr-screencopy session. Compiled only with `--features wayland`.
//! Isolated so a protocol mismatch cannot take down the grim fallback.

#![allow(dead_code)]

use crate::capture::RawFrame;
use memmap2::MmapMut;
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
    format: u32,
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
            format: 0,
            ready: false,
            failed: false,
        }
    }
}

pub fn grab(output: &str) -> Result<RawFrame, String> {
    let conn = Connection::connect_to_env().map_err(|e| e.to_string())?;
    let display = conn.display();
    let mut event_queue: EventQueue<State> = conn.new_event_queue();
    let qh = event_queue.handle();
    let _registry = display.get_registry(&qh, ());
    let mut state = State {
        want_output: output.to_string(),
        ..State::default()
    };
    event_queue
        .roundtrip(&mut state)
        .map_err(|e| e.to_string())?;
    let manager = state
        .manager
        .clone()
        .ok_or_else(|| "zwlr_screencopy_manager_v1 missing".to_string())?;
    let shm = state
        .shm
        .clone()
        .ok_or_else(|| "wl_shm missing".to_string())?;
    let output_obj = pick_output(&state).ok_or_else(|| "no matching wl_output".to_string())?;
    let frame = manager.capture_output(0, &output_obj, &qh, ());
    while !state.ready && !state.failed && state.width == 0 {
        event_queue
            .blocking_dispatch(&mut state)
            .map_err(|e| e.to_string())?;
    }
    if state.failed || state.width <= 0 {
        return Err("screencopy frame failed".into());
    }
    let (fd, _file) = shm_file((state.stride * state.height) as i64)?;
    let pool = shm.create_pool(fd.as_fd(), state.stride * state.height, &qh, ());
    let buffer = pool.create_buffer(
        0,
        state.width,
        state.height,
        state.stride,
        wl_shm::Format::Argb8888,
        &qh,
        (),
    );
    frame.copy(&buffer);
    while !state.ready && !state.failed {
        event_queue
            .blocking_dispatch(&mut state)
            .map_err(|e| e.to_string())?;
    }
    if state.failed {
        return Err("screencopy copy failed".into());
    }
    let rgba = read_shm(&_file, state.width as u32, state.height as u32, state.stride as u32)?;
    drop(buffer);
    drop(pool);
    drop(frame);
    Ok(RawFrame {
        rgba,
        width: state.width as u32,
        height: state.height as u32,
    })
}

fn pick_output(state: &State) -> Option<WlOutput> {
    if state.want_output.is_empty() {
        return state.outputs.first().map(|o| o.1.clone());
    }
    state
        .outputs
        .iter()
        .find(|o| o.2 == state.want_output)
        .map(|o| o.1.clone())
        .or_else(|| state.outputs.first().map(|o| o.1.clone()))
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

fn read_shm(file: &File, width: u32, height: u32, stride: u32) -> Result<Vec<u8>, String> {
    let mmap = unsafe { MmapMut::map_mut(file) }.map_err(|e| e.to_string())?;
    let mut rgba = vec![0u8; (width * height * 4) as usize];
    for y in 0..height {
        let row = (y * stride) as usize;
        for x in 0..width {
            let i = row + (x as usize) * 4;
            if i + 3 >= mmap.len() {
                continue;
            }
            // wl_shm Argb8888 is actually x8r8g8b8 in little endian: B,G,R,A
            let b = mmap[i];
            let g = mmap[i + 1];
            let r = mmap[i + 2];
            let a = mmap[i + 3];
            let o = ((y * width + x) * 4) as usize;
            rgba[o] = r;
            rgba[o + 1] = g;
            rgba[o + 2] = b;
            rgba[o + 3] = a;
        }
    }
    Ok(rgba)
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
                    let output =
                        registry.bind::<WlOutput, _, _>(name, version.min(4), qh, name);
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
                format: _,
                width,
                height,
                stride,
            } => {
                state.width = width as i32;
                state.height = height as i32;
                state.stride = stride as i32;
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

// Silence unused import on some sctk versions.
#[allow(dead_code)]
fn _raw_fd(file: &File) -> i32 {
    file.as_raw_fd()
}
