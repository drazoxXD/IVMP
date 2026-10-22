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
    vertices: Option<IDirect3DVertexBuffer9>,
    indices: Option<IDirect3DIndexBuffer9>,
    vertex_capacity: usize,
    index_capacity: usize,
}

impl Renderer {
    pub fn new() -> Self {
        Renderer { font: None, vertices: None, indices: None, vertex_capacity: 0, index_capacity: 0 }
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

    /// Drops the buffers in the default pool; call before the game resets the device.
    pub fn invalidate(&mut self) {
        self.vertices = None;
        self.indices = None;
        self.vertex_capacity = 0;
        self.index_capacity = 0;
    }

    fn reserve(&mut self, device: &IDirect3DDevice9, vertices: usize, indices: usize) -> bool {
        if self.vertices.is_none() || self.vertex_capacity < vertices {
            let capacity = vertices + 5000;
            let mut buffer: Option<IDirect3DVertexBuffer9> = None;
            let created = unsafe {
                device.CreateVertexBuffer(
                    (capacity * size_of::<Vertex>()) as u32,
                    (D3DUSAGE_DYNAMIC | D3DUSAGE_WRITEONLY) as u32,
                    FVF,
                    D3DPOOL_DEFAULT,
                    &mut buffer,
                    std::ptr::null_mut::<HANDLE>(),
                )
            };
            if created.is_err() || buffer.is_none() {
                return false;
            }
            self.vertices = buffer;
            self.vertex_capacity = capacity;
        }
        if self.indices.is_none() || self.index_capacity < indices {
            let capacity = indices + 10000;
            let mut buffer: Option<IDirect3DIndexBuffer9> = None;
            let created = unsafe {
                device.CreateIndexBuffer(
                    (capacity * size_of::<DrawIdx>()) as u32,
                    (D3DUSAGE_DYNAMIC | D3DUSAGE_WRITEONLY) as u32,
                    if size_of::<DrawIdx>() == 2 { D3DFMT_INDEX16 } else { D3DFMT_INDEX32 },
                    D3DPOOL_DEFAULT,
                    &mut buffer,
                    std::ptr::null_mut::<HANDLE>(),
                )
            };
            if created.is_err() || buffer.is_none() {
                return false;
            }
            self.indices = buffer;
            self.index_capacity = capacity;
        }
        true
    }

    pub fn render(&mut self, device: &IDirect3DDevice9, data: &DrawData) {
        let [width, height] = data.display_size;
        if width <= 0.0 || height <= 0.0 || data.total_vtx_count == 0 {
            return;
        }
        if self.font.is_none() || !self.reserve(device, data.total_vtx_count as usize, data.total_idx_count as usize) {
            return;
        }
        let (Some(vertex_buffer), Some(index_buffer)) = (self.vertices.clone(), self.indices.clone()) else { return };

        // Copy every list into the two buffers.
        unsafe {
            let mut vertex_ptr: *mut c_void = std::ptr::null_mut();
            let mut index_ptr: *mut c_void = std::ptr::null_mut();
            if vertex_buffer.Lock(0, (data.total_vtx_count as usize * size_of::<Vertex>()) as u32, &mut vertex_ptr, D3DLOCK_DISCARD as u32).is_err()
            {
                return;
            }
            if index_buffer.Lock(0, (data.total_idx_count as usize * size_of::<DrawIdx>()) as u32, &mut index_ptr, D3DLOCK_DISCARD as u32).is_err() {
                let _ = vertex_buffer.Unlock();
                return;
            }
            let mut vertices = vertex_ptr as *mut Vertex;
            let mut indices = index_ptr as *mut DrawIdx;
            for list in data.draw_lists() {
                for v in list.vtx_buffer() {
                    *vertices = convert(v);
                    vertices = vertices.add(1);
                }
                let idx = list.idx_buffer();
                std::ptr::copy_nonoverlapping(idx.as_ptr(), indices, idx.len());
                indices = indices.add(idx.len());
            }
            let _ = vertex_buffer.Unlock();
            let _ = index_buffer.Unlock();
        }

        // Save everything the game had set, then set what imgui needs.
        let block = unsafe { device.CreateStateBlock(D3DSBT_ALL) }.ok();
        unsafe { self.set_state(device, &vertex_buffer, &index_buffer, width, height) };

        let mut vertex_offset = 0usize;
        let mut index_offset = 0usize;
        for list in data.draw_lists() {
            for command in list.commands() {
                match command {
                    DrawCmd::Elements { count, cmd_params } => unsafe {
                        let [left, top, right, bottom] = cmd_params.clip_rect;
                        let rect = RECT { left: left as i32, top: top as i32, right: right as i32, bottom: bottom as i32 };
                        if rect.right <= rect.left || rect.bottom <= rect.top {
                            continue;
                        }
                        let _ = device.SetScissorRect(&rect);
                        let texture: Option<IDirect3DBaseTexture9> = self.font.as_ref().and_then(|t| t.cast().ok());
                        let _ = device.SetTexture(0, texture.as_ref());
                        let _ = device.DrawIndexedPrimitive(
                            D3DPT_TRIANGLELIST,
                            (vertex_offset + cmd_params.vtx_offset) as i32,
                            0,
                            list.vtx_buffer().len() as u32,
                            (index_offset + cmd_params.idx_offset) as u32,
                            (count / 3) as u32,
                        );
                    },
                    DrawCmd::ResetRenderState => unsafe { self.set_state(device, &vertex_buffer, &index_buffer, width, height) },
                    DrawCmd::RawCallback { .. } => {}
                }
            }
            vertex_offset += list.vtx_buffer().len();
            index_offset += list.idx_buffer().len();
        }

        if let Some(block) = block {
            let _ = unsafe { block.Apply() };
        }
    }

    unsafe fn set_state(
        &self,
        device: &IDirect3DDevice9,
        vertices: &IDirect3DVertexBuffer9,
        indices: &IDirect3DIndexBuffer9,
        width: f32,
        height: f32,
    ) {
        unsafe {
            let viewport = D3DVIEWPORT9 { X: 0, Y: 0, Width: width as u32, Height: height as u32, MinZ: 0.0, MaxZ: 1.0 };
            let _ = device.SetViewport(&viewport);
            let _ = device.SetPixelShader(None::<&IDirect3DPixelShader9>);
            let _ = device.SetVertexShader(None::<&IDirect3DVertexShader9>);
            let _ = device.SetFVF(FVF);
            let _ = device.SetStreamSource(0, vertices, 0, size_of::<Vertex>() as u32);
            let _ = device.SetIndices(indices);
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
