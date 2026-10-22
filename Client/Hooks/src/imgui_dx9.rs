//! Direct3D 9 backend for dear imgui: uploads the font atlas and draws imgui's draw lists with the
//! game's device. Everything the game had set is restored afterwards with a state block.

use std::ffi::c_void;

use imgui::{DrawCmd, DrawData, DrawIdx, DrawVert, TextureId};
use windows::Win32::Foundation::{HANDLE, RECT};
use windows::Win32::Graphics::Direct3D9::*;
use windows::core::Interface;

use crate::log;

const FVF: u32 = D3DFVF_XYZ | D3DFVF_DIFFUSE | D3DFVF_TEX1;

/// `D3DFVF_XYZ | D3DFVF_DIFFUSE | D3DFVF_TEX1` vertex.
#[repr(C)]
#[derive(Clone, Copy)]
struct Vertex {
    pos: [f32; 3],
    color: u32,
    uv: [f32; 2],
}

pub struct Renderer {
    font: Option<IDirect3DTexture9>,
    /// The current list's vertices, converted.
    scratch: Vec<Vertex>,
}

impl Renderer {
    pub fn new() -> Self {
        Renderer { font: None, scratch: Vec::new() }
    }

    /// Uploads the font atlas (RGBA32, `width` x `height`). The texture lives in the managed pool,
    /// so it survives a device reset.
    pub fn create_font(&mut self, device: &IDirect3DDevice9, width: u32, height: u32, rgba: &[u8]) -> bool {
        let mut texture: Option<IDirect3DTexture9> = None;
        let created = unsafe {
            device.CreateTexture(width, height, 1, 0, D3DFMT_A8R8G8B8, D3DPOOL_MANAGED, &mut texture, std::ptr::null_mut::<HANDLE>())
        };
        let Some(texture) = texture.filter(|_| created.is_ok()) else {
            log!("imgui: could not create the font texture: {created:?}");
            return false;
        };
        let mut locked = D3DLOCKED_RECT::default();
        if unsafe { texture.LockRect(0, &mut locked, std::ptr::null::<RECT>(), 0) }.is_err() {
            log!("imgui: could not lock the font texture");
            return false;
        }
        for y in 0..height as usize {
            let row = unsafe { (locked.pBits as *mut u8).add(y * locked.Pitch as usize) };
            for x in 0..width as usize {
                let source = &rgba[(y * width as usize + x) * 4..][..4];
                // RGBA -> BGRA (D3DFMT_A8R8G8B8 in memory)
                unsafe {
                    let pixel = row.add(x * 4);
                    *pixel = source[2];
                    *pixel.add(1) = source[1];
                    *pixel.add(2) = source[0];
                    *pixel.add(3) = source[3];
                }
            }
        }
        let _ = unsafe { texture.UnlockRect(0) };
        self.font = Some(texture);
        true
    }

    /// Nothing of ours is in the default pool, so there is nothing to release on a Reset.
    pub fn invalidate(&mut self) {}

    /// Draws the draw lists from system memory (`DrawIndexedPrimitiveUP`), so nothing of ours lives
    /// in the default pool: the game's Reset can't fail because of the UI, even when the Steam
    /// overlay re-patches the device and our own Reset hook is bypassed.
    pub fn render(&mut self, device: &IDirect3DDevice9, data: &DrawData) {
        let [width, height] = data.display_size;
        if width <= 0.0 || height <= 0.0 || data.total_vtx_count == 0 {
            return;
        }
        let Some(font) = self.font.as_ref() else { return };
        let texture: Option<IDirect3DBaseTexture9> = font.cast().ok();

        // Save everything the game had set, then set what imgui needs.
        let block = unsafe { device.CreateStateBlock(D3DSBT_ALL) }.ok();
        unsafe { set_common_state(device, width, height) };

        for list in data.draw_lists() {
            self.scratch.clear();
            self.scratch.extend(list.vtx_buffer().iter().map(convert));
            let indices = list.idx_buffer();
            for command in list.commands() {
                match command {
                    DrawCmd::Elements { count, cmd_params } => unsafe {
                        let [left, top, right, bottom] = cmd_params.clip_rect;
                        let rect = RECT { left: left as i32, top: top as i32, right: right as i32, bottom: bottom as i32 };
                        if rect.right <= rect.left || rect.bottom <= rect.top {
                            continue;
                        }
                        let _ = device.SetScissorRect(&rect);
                        let _ = device.SetTexture(0, texture.as_ref());
                        let vertices = &self.scratch[cmd_params.vtx_offset.min(self.scratch.len())..];
                        let _ = device.DrawIndexedPrimitiveUP(
                            D3DPT_TRIANGLELIST,
                            0,
                            vertices.len() as u32,
                            (count / 3) as u32,
                            indices[cmd_params.idx_offset..].as_ptr().cast(),
                            if size_of::<DrawIdx>() == 2 { D3DFMT_INDEX16 } else { D3DFMT_INDEX32 },
                            vertices.as_ptr().cast(),
                            size_of::<Vertex>() as u32,
                        );
                    },
                    DrawCmd::ResetRenderState => unsafe { set_common_state(device, width, height) },
                    DrawCmd::RawCallback { .. } => {}
                }
            }
        }

        if let Some(block) = block {
            let _ = unsafe { block.Apply() };
        }
    }

}

/// Render state for drawing 2D UI in pixel coordinates.
unsafe fn set_common_state(device: &IDirect3DDevice9, width: f32, height: f32) {
    unsafe {
            let viewport = D3DVIEWPORT9 { X: 0, Y: 0, Width: width as u32, Height: height as u32, MinZ: 0.0, MaxZ: 1.0 };
            let _ = device.SetViewport(&viewport);
            let _ = device.SetPixelShader(None::<&IDirect3DPixelShader9>);
            let _ = device.SetVertexShader(None::<&IDirect3DVertexShader9>);
            let _ = device.SetFVF(FVF);
            for (state, value) in [
                (D3DRS_FILLMODE, D3DFILL_SOLID.0 as u32),
                (D3DRS_SHADEMODE, D3DSHADE_GOURAUD.0 as u32),
                (D3DRS_ZWRITEENABLE, 0),
                (D3DRS_ALPHATESTENABLE, 0),
                (D3DRS_CULLMODE, D3DCULL_NONE.0 as u32),
                (D3DRS_ZENABLE, 0),
                (D3DRS_ALPHABLENDENABLE, 1),
                (D3DRS_BLENDOP, D3DBLENDOP_ADD.0 as u32),
                (D3DRS_SRCBLEND, D3DBLEND_SRCALPHA.0 as u32),
                (D3DRS_DESTBLEND, D3DBLEND_INVSRCALPHA.0 as u32),
                (D3DRS_SEPARATEALPHABLENDENABLE, 1),
                (D3DRS_SRCBLENDALPHA, D3DBLEND_ONE.0 as u32),
                (D3DRS_DESTBLENDALPHA, D3DBLEND_INVSRCALPHA.0 as u32),
                (D3DRS_SCISSORTESTENABLE, 1),
                (D3DRS_FOGENABLE, 0),
                (D3DRS_RANGEFOGENABLE, 0),
                (D3DRS_SPECULARENABLE, 0),
                (D3DRS_STENCILENABLE, 0),
                (D3DRS_CLIPPING, 1),
                (D3DRS_LIGHTING, 0),
                (D3DRS_COLORWRITEENABLE, 0xF),
                (D3DRS_SRGBWRITEENABLE, 0),
            ] {
                let _ = device.SetRenderState(state, value);
            }
            for (stage, state, value) in [
                (0, D3DTSS_COLOROP, D3DTOP_MODULATE.0 as u32),
                (0, D3DTSS_COLORARG1, D3DTA_TEXTURE),
                (0, D3DTSS_COLORARG2, D3DTA_DIFFUSE),
                (0, D3DTSS_ALPHAOP, D3DTOP_MODULATE.0 as u32),
                (0, D3DTSS_ALPHAARG1, D3DTA_TEXTURE),
                (0, D3DTSS_ALPHAARG2, D3DTA_DIFFUSE),
                (1, D3DTSS_COLOROP, D3DTOP_DISABLE.0 as u32),
                (1, D3DTSS_ALPHAOP, D3DTOP_DISABLE.0 as u32),
            ] {
                let _ = device.SetTextureStageState(stage, state, value);
            }
            for (state, value) in [
                (D3DSAMP_MINFILTER, D3DTEXF_LINEAR.0 as u32),
                (D3DSAMP_MAGFILTER, D3DTEXF_LINEAR.0 as u32),
                (D3DSAMP_ADDRESSU, D3DTADDRESS_CLAMP.0 as u32),
                (D3DSAMP_ADDRESSV, D3DTADDRESS_CLAMP.0 as u32),
                (D3DSAMP_SRGBTEXTURE, 0),
            ] {
                let _ = device.SetSamplerState(0, state, value);
            }
            // Pixel-exact orthographic projection (the 0.5 offset lines texels up with pixels).
            let l = 0.5f32;
            let r = width + 0.5;
            let t = 0.5f32;
            let b = height + 0.5;
            let projection: [f32; 16] = [
                        2.0 / (r - l), 0.0, 0.0, 0.0,
                        0.0, 2.0 / (t - b), 0.0, 0.0,
                        0.0, 0.0, 0.5, 0.0,
                        (l + r) / (l - r), (t + b) / (b - t), 0.5, 1.0,
                    ];
            let identity: [f32; 16] = [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0];
            let _ = device.SetTransform(D3DTRANSFORMSTATETYPE(256), identity.as_ptr().cast());
            let _ = device.SetTransform(D3DTS_VIEW, identity.as_ptr().cast());
            let _ = device.SetTransform(D3DTS_PROJECTION, projection.as_ptr().cast());
    }
}

/// Draws `texture` (BGRA, premultiplied alpha, `width` x `height`) over the whole screen.
pub fn draw_texture(device: &IDirect3DDevice9, texture: &IDirect3DTexture9, width: f32, height: f32) {
    let block = unsafe { device.CreateStateBlock(D3DSBT_ALL) }.ok();
    unsafe {
        set_common_state(device, width, height);
        // Premultiplied alpha: the page's colors are already multiplied by its alpha.
        let _ = device.SetRenderState(D3DRS_SRCBLEND, D3DBLEND_ONE.0 as u32);
        let _ = device.SetRenderState(D3DRS_SEPARATEALPHABLENDENABLE, 0);
        let _ = device.SetRenderState(D3DRS_SCISSORTESTENABLE, 0);
        let white = 0xFFFF_FFFFu32;
        let corners = [
            Vertex { pos: [0.0, 0.0, 0.0], color: white, uv: [0.0, 0.0] },
            Vertex { pos: [width, 0.0, 0.0], color: white, uv: [1.0, 0.0] },
            Vertex { pos: [0.0, height, 0.0], color: white, uv: [0.0, 1.0] },
            Vertex { pos: [width, height, 0.0], color: white, uv: [1.0, 1.0] },
        ];
        let base: Option<IDirect3DBaseTexture9> = texture.cast().ok();
        let _ = device.SetTexture(0, base.as_ref());
        let _ = device.DrawPrimitiveUP(D3DPT_TRIANGLESTRIP, 2, corners.as_ptr().cast(), size_of::<Vertex>() as u32);
    }
    if let Some(block) = block {
        let _ = unsafe { block.Apply() };
    }
}

fn convert(v: &DrawVert) -> Vertex {
    // imgui colors are RGBA bytes, Direct3D wants ARGB.
    let [r, g, b, a] = v.col;
    Vertex {
        pos: [v.pos[0], v.pos[1], 0.0],
        color: (a as u32) << 24 | (r as u32) << 16 | (g as u32) << 8 | b as u32,
        uv: v.uv,
    }
}

/// The font atlas is the only texture imgui draws with.
pub const FONT_TEXTURE: TextureId = TextureId::new(1);
