//! One Direct3D 11 device for capture, conversion and encoding, so frames stay on the GPU
//! from the screen to the encoder.

use std::sync::Mutex;

use anyhow::{Context, Result};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDevice,
    ID3D11Device, ID3D11DeviceContext, ID3D11Multithread,
};
use windows::core::Interface;

#[derive(Clone)]
pub struct Device {
    pub device: ID3D11Device,
    pub context: ID3D11DeviceContext,
}

// SAFETY: the device is multithread protected (below), so it and its immediate context may
// be used from several threads.
unsafe impl Send for Device {}
unsafe impl Sync for Device {}

static SHARED: Mutex<Option<Device>> = Mutex::new(None);

/// The process's shared device, created on first use.
pub fn shared() -> Result<Device> {
    let mut shared = SHARED.lock().unwrap();
    if let Some(d) = &*shared {
        return Ok(d.clone());
    }
    let (mut device, mut context) = (None, None);
    // SAFETY: standard device creation with valid out pointers.
    unsafe {
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )?;
    }
    let device: ID3D11Device = device.context("no Direct3D 11 device")?;
    // Capture callbacks, the encoder thread and Media Foundation all use it.
    // SAFETY: a plain setting on our device.
    let _ = unsafe { device.cast::<ID3D11Multithread>()?.SetMultithreadProtected(true) };
    let d = Device { device, context: context.context("no device context")? };
    *shared = Some(d.clone());
    Ok(d)
}
