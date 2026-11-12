//! GPU/CPU encoder targets: DXGI adapter discovery ranked by Windows' own GPU
//! preference, hardware encoders bound to a specific adapter (`MFTEnum2` +
//! `MFT_ENUM_ADAPTER_LUID`), and the explicit NV12 copies needed when the
//! encoder does not live on the capture GPU.

use anyhow::{Context, Result, ensure};
use mirage_native_capture_probe::selection::{Adapter, AdapterKind, vendor_name};
use windows::{
    Win32::{
        Foundation::{HMODULE, LUID},
        Graphics::{
            Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1},
            Direct3D11::*,
            Dxgi::{
                Common::{DXGI_FORMAT_NV12, DXGI_SAMPLE_DESC},
                *,
            },
        },
        Media::MediaFoundation::*,
    },
    core::{GUID, Interface},
};

pub const WIDTH: u32 = 640;
pub const HEIGHT: u32 = 360;
pub const NV12_BYTES: usize = (WIDTH * HEIGHT * 3 / 2) as usize;

pub struct GpuAdapter {
    pub info: Adapter,
    pub adapter: IDXGIAdapter1,
    pub luid: LUID,
    pub vendor: &'static str,
    pub name: String,
    pub dedicated_mib: u64,
}

pub fn luid_key(luid: LUID) -> u64 {
    u64::from(luid.LowPart) | ((luid.HighPart as u32 as u64) << 32)
}

pub fn device_luid(device: &ID3D11Device) -> Result<LUID> {
    // SAFETY: Read-only DXGI queries on a live device.
    unsafe {
        let dxgi: IDXGIDevice = device.cast()?;
        Ok(dxgi.GetAdapter()?.GetDesc()?.AdapterLuid)
    }
}

/// Hardware encoders for `subtype` bound to one adapter.
pub fn hardware_encoders_on(luid: LUID, subtype: GUID) -> Vec<IMFActivate> {
    let output = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: subtype,
    };
    let input = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_NV12,
    };
    // SAFETY: Attribute store and MF-allocated array handled as in `mft::enumerate`.
    unsafe {
        let mut attributes = None;
        if MFCreateAttributes(&mut attributes, 1).is_err() {
            return Vec::new();
        }
        let Some(attributes) = attributes else {
            return Vec::new();
        };
        // MFTEnum2 expects the LUID as an 8-byte blob (LowPart, HighPart).
        if attributes
            .SetBlob(&MFT_ENUM_ADAPTER_LUID, &luid_key(luid).to_le_bytes())
            .is_err()
        {
            return Vec::new();
        }
        let mut array: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut count = 0u32;
        if MFTEnum2(
            MFT_CATEGORY_VIDEO_ENCODER,
            MFT_ENUM_FLAG(MFT_ENUM_FLAG_HARDWARE.0 | MFT_ENUM_FLAG_SORTANDFILTER.0),
            Some(&input),
            Some(&output),
            &attributes,
            &mut array,
            &mut count,
        )
        .is_err()
        {
            return Vec::new();
        }
        crate::mft::take_activates(array, count)
    }
}

/// All adapters, with Windows' preference ranks and per-codec encoder counts.
pub fn adapters(capture: Option<LUID>, subtype: GUID) -> Result<Vec<GpuAdapter>> {
    // SAFETY: DXGI factory enumeration; every adapter is an owned COM reference.
    unsafe {
        let factory: IDXGIFactory6 = CreateDXGIFactory1()?;
        let rank = |preference| -> Vec<u64> {
            (0..)
                .map_while(|i| {
                    factory
                        .EnumAdapterByGpuPreference::<IDXGIAdapter1>(i, preference)
                        .ok()
                })
                .filter_map(|a| a.GetDesc1().ok().map(|d| luid_key(d.AdapterLuid)))
                .collect()
        };
        let high = rank(DXGI_GPU_PREFERENCE_HIGH_PERFORMANCE);
        let low = rank(DXGI_GPU_PREFERENCE_MINIMUM_POWER);
        let position = |list: &[u64], key| list.iter().position(|k| *k == key).unwrap_or(99) as u32;
        let mut found = Vec::new();
        for index in 0..16 {
            let Ok(adapter) = factory.EnumAdapters1(index) else {
                break;
            };
            let desc = adapter.GetDesc1()?;
            let key = luid_key(desc.AdapterLuid);
            let software = desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0;
            let kind = AdapterKind::classify(software, desc.DedicatedVideoMemory as u64);
            let encoders = if kind == AdapterKind::Software {
                0
            } else {
                hardware_encoders_on(desc.AdapterLuid, subtype).len() as u32
            };
            let end = desc.Description.iter().position(|c| *c == 0).unwrap_or(128);
            found.push(GpuAdapter {
                info: Adapter {
                    index,
                    kind,
                    high_performance_rank: position(&high, key),
                    minimum_power_rank: position(&low, key),
                    is_capture: capture.map_or(index == 0, |c| luid_key(c) == key),
                    hardware_encoders: encoders,
                },
                adapter,
                luid: desc.AdapterLuid,
                vendor: vendor_name(desc.VendorId),
                name: String::from_utf16_lossy(&desc.Description[..end]),
                dedicated_mib: desc.DedicatedVideoMemory as u64 / (1024 * 1024),
            });
        }
        Ok(found)
    }
}

/// A video-capable D3D11 device on a specific adapter.
pub fn create_device(adapter: &IDXGIAdapter1) -> Result<(ID3D11Device, ID3D11DeviceContext)> {
    let (mut device, mut context) = (None, None);
    // SAFETY: Standard device creation on an explicit adapter.
    unsafe {
        D3D11CreateDevice(
            adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
            Some(&[D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0]),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )?;
    }
    Ok((device.context("no device")?, context.context("no context")?))
}

/// Copy a 640x360 NV12 GPU texture into tightly packed system memory.
pub fn readback_nv12(
    device: &ID3D11Device,
    context: &ID3D11DeviceContext,
    texture: &ID3D11Texture2D,
) -> Result<Vec<u8>> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: WIDTH,
        Height: HEIGHT,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_NV12,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_STAGING,
        BindFlags: 0,
        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
        MiscFlags: 0,
    };
    let mut staging = None;
    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    let mut out = vec![0u8; NV12_BYTES];
    // SAFETY: The staging copy matches the source format and size; mapped rows
    // are read within RowPitch * (HEIGHT * 3 / 2) and unmapped before return.
    unsafe {
        device.CreateTexture2D(&desc, None, Some(&mut staging))?;
        let staging = staging.context("no staging texture")?;
        context.CopyResource(&staging, texture);
        context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
        let pitch = mapped.RowPitch as usize;
        let ok = !mapped.pData.is_null() && pitch >= WIDTH as usize;
        if ok {
            let base = mapped.pData.cast::<u8>();
            for row in 0..(HEIGHT * 3 / 2) as usize {
                std::ptr::copy_nonoverlapping(
                    base.add(row * pitch),
                    out.as_mut_ptr().add(row * WIDTH as usize),
                    WIDTH as usize,
                );
            }
        }
        context.Unmap(&staging, 0);
        ensure!(ok, "NV12 readback mapping invalid");
    }
    Ok(out)
}

/// Upload packed NV12 to a new texture on another adapter's device.
pub fn upload_nv12(device: &ID3D11Device, data: &[u8]) -> Result<ID3D11Texture2D> {
    ensure!(data.len() == NV12_BYTES, "unexpected NV12 size");
    let desc = D3D11_TEXTURE2D_DESC {
        Width: WIDTH,
        Height: HEIGHT,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_NV12,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    // NV12 initial data: the UV plane directly follows the Y plane.
    let initial = D3D11_SUBRESOURCE_DATA {
        pSysMem: data.as_ptr().cast(),
        SysMemPitch: WIDTH,
        SysMemSlicePitch: 0,
    };
    let mut texture = None;
    // SAFETY: `data` covers the full NV12 layout described by `desc`.
    unsafe {
        device.CreateTexture2D(&desc, Some(&initial), Some(&mut texture))?;
    }
    texture.context("no uploaded texture")
}
