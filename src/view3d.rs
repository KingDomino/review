//! 3D viewer: renders a [`Scene`] with wgpu into an offscreen texture shown by egui,
//! with orbit / pan / zoom controls like Preview's.

use std::time::Instant;

use eframe::egui_wgpu::{self, wgpu};
use egui::{pos2, vec2, Color32, CursorIcon, FontId, Rect, Sense, Ui};
use wgpu::util::DeviceExt;

use crate::model3d::{mul, normalize, Mat4, Scene};

const SAMPLES: u32 = 4;
/// egui samples native textures as gamma-encoded Rgba8Unorm, so the shader encodes its
/// linear lighting result to sRGB itself (works on Vulkan and the GL fallback alike).
const COLOR: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
/// Model textures are sRGB images; sampling them decodes to linear for lighting.
const TEXTURE: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const DEPTH: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    pos: [f32; 3],
    nrm: [f32; 3],
    col: [f32; 4],
    uv: [f32; 2],
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    view_proj: [f32; 16],
    eye: [f32; 4],
    light: [f32; 4],
}

const SHADER: &str = r#"
struct U { view_proj: mat4x4<f32>, eye: vec4<f32>, light: vec4<f32> };
@group(0) @binding(0) var<uniform> u: U;
@group(1) @binding(0) var tex: texture_2d<f32>;
@group(1) @binding(1) var samp: sampler;

struct VIn { @location(0) pos: vec3<f32>, @location(1) nrm: vec3<f32>, @location(2) col: vec4<f32>, @location(3) uv: vec2<f32> };
struct VOut { @builtin(position) clip: vec4<f32>, @location(0) nrm: vec3<f32>, @location(1) col: vec4<f32>, @location(2) uv: vec2<f32>, @location(3) wpos: vec3<f32> };

@vertex fn vs(v: VIn) -> VOut {
    var o: VOut;
    o.clip = u.view_proj * vec4<f32>(v.pos, 1.0);
    o.nrm = v.nrm;
    o.col = v.col;
    o.uv = v.uv;
    o.wpos = v.pos;
    return o;
}

@fragment fn fs(i: VOut) -> @location(0) vec4<f32> {
    let view = normalize(u.eye.xyz - i.wpos);
    var n = normalize(i.nrm);
    // Two-sided lighting: many models have inconsistent winding.
    if (dot(n, view) < 0.0) { n = -n; }
    let key = normalize(u.light.xyz);
    let fill = normalize(vec3<f32>(-key.x, 0.4, -key.z));
    let albedo = i.col * textureSample(tex, samp, i.uv);
    let hemi = mix(0.22, 0.42, n.y * 0.5 + 0.5);
    let diffuse = max(dot(n, key), 0.0) * 0.72 + max(dot(n, fill), 0.0) * 0.18;
    let h = normalize(key + view);
    let spec = pow(max(dot(n, h), 0.0), 48.0) * 0.18;
    let lin = clamp(albedo.rgb * (hemi + diffuse) + vec3<f32>(spec), vec3<f32>(0.0), vec3<f32>(1.0));
    // Encode to sRGB gamma for the Rgba8Unorm target.
    let lo = lin * 12.92;
    let hi = 1.055 * pow(lin, vec3<f32>(1.0 / 2.4)) - 0.055;
    return vec4<f32>(select(hi, lo, lin <= vec3<f32>(0.0031308)), 1.0);
}
"#;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    pub yaw: f32,
    pub pitch: f32,
    pub dist: f32,
    pub target: [f32; 3],
    pub radius: f32,
}

impl Camera {
    pub fn fit(scene: &Scene) -> Self {
        let r = scene.radius();
        Camera { yaw: 0.6, pitch: 0.35, dist: r / (20f32.to_radians()).sin() * 1.15, target: scene.center(), radius: r }
    }

    fn eye(&self) -> [f32; 3] {
        let (cp, sp) = (self.pitch.cos(), self.pitch.sin());
        [
            self.target[0] + self.dist * cp * self.yaw.sin(),
            self.target[1] + self.dist * sp,
            self.target[2] + self.dist * cp * self.yaw.cos(),
        ]
    }

    fn view_proj(&self, aspect: f32) -> Mat4 {
        let eye = self.eye();
        let z = normalize([eye[0] - self.target[0], eye[1] - self.target[1], eye[2] - self.target[2]]);
        let x = normalize(cross([0.0, 1.0, 0.0], z));
        let y = cross(z, x);
        let d = |a: [f32; 3]| a[0] * eye[0] + a[1] * eye[1] + a[2] * eye[2];
        let view: Mat4 = [x[0], y[0], z[0], 0.0, x[1], y[1], z[1], 0.0, x[2], y[2], z[2], 0.0, -d(x), -d(y), -d(z), 1.0];
        let near = (self.dist - self.radius * 2.0).max(self.dist * 0.01).max(1e-4);
        let far = self.dist + self.radius * 4.0;
        let f = 1.0 / (20f32.to_radians()).tan();
        let proj: Mat4 = [f / aspect, 0.0, 0.0, 0.0, 0.0, f, 0.0, 0.0, 0.0, 0.0, far / (near - far), -1.0, 0.0, 0.0, near * far / (near - far), 0.0];
        mul(&proj, &view)
    }

    /// Move the target in the view plane by a screen delta (points).
    fn pan(&mut self, d: egui::Vec2, view_h: f32) {
        let eye = self.eye();
        let z = normalize([eye[0] - self.target[0], eye[1] - self.target[1], eye[2] - self.target[2]]);
        let x = normalize(cross([0.0, 1.0, 0.0], z));
        let y = cross(z, x);
        let k = 2.0 * self.dist * (20f32.to_radians()).tan() / view_h.max(1.0);
        for i in 0..3 {
            self.target[i] += (-x[i] * d.x + y[i] * d.y) * k;
        }
    }
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

struct GpuMesh {
    vb: wgpu::Buffer,
    ib: wgpu::Buffer,
    count: u32,
    bind: wgpu::BindGroup,
}

struct Target {
    size: [u32; 2],
    msaa: wgpu::TextureView,
    #[allow(dead_code)]
    resolve: wgpu::Texture,
    resolve_view: wgpu::TextureView,
    depth: wgpu::TextureView,
    tex_id: egui::TextureId,
}

pub struct Viewer {
    pipeline: wgpu::RenderPipeline,
    uniforms: wgpu::Buffer,
    uniform_bind: wgpu::BindGroup,
    meshes: Vec<GpuMesh>,
    target: Option<Target>,
    pub cam: Camera,
    pub home: Camera,
    rendered: Option<(Camera, [u32; 2], [u8; 4])>,
    pub opened: Instant,
}

impl Viewer {
    pub fn new(rs: &egui_wgpu::RenderState, scene: &Scene) -> Self {
        let device = &rs.device;
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("review-3d"), source: wgpu::ShaderSource::Wgsl(SHADER.into()) });
        let ubl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                count: None,
            }],
        });
        let tbl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry { binding: 1, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering), count: None },
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: None, bind_group_layouts: &[Some(&ubl), Some(&tbl)], immediate_size: 0 });
        let attrs = wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x4, 3 => Float32x2];
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("review-3d"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout { array_stride: std::mem::size_of::<Vertex>() as u64, step_mode: wgpu::VertexStepMode::Vertex, attributes: &attrs })],
            },
            primitive: wgpu::PrimitiveState { cull_mode: None, ..Default::default() },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: wgpu::MultisampleState { count: SAMPLES, ..Default::default() },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState { format: COLOR, blend: None, write_mask: wgpu::ColorWrites::ALL })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let uniform_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &ubl,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: uniforms.as_entire_binding() }],
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let upload = |img: &image::RgbaImage| -> wgpu::TextureView {
            let size = wgpu::Extent3d { width: img.width().max(1), height: img.height().max(1), depth_or_array_layers: 1 };
            let tex = device.create_texture(&wgpu::TextureDescriptor {
                label: None,
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: TEXTURE,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            rs.queue.write_texture(
                wgpu::TexelCopyTextureInfo { texture: &tex, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
                img.as_raw(),
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4 * size.width), rows_per_image: Some(size.height) },
                size,
            );
            tex.create_view(&Default::default())
        };
        let white = upload(&image::RgbaImage::from_pixel(1, 1, image::Rgba([255, 255, 255, 255])));
        let textures: Vec<wgpu::TextureView> = scene.textures.iter().map(upload).collect();
        let meshes = scene
            .meshes
            .iter()
            .map(|m| {
                let verts: Vec<Vertex> = (0..m.positions.len()).map(|i| Vertex { pos: m.positions[i], nrm: m.normals[i], col: m.colors[i], uv: m.uvs[i] }).collect();
                let vb = device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: None, contents: bytemuck::cast_slice(&verts), usage: wgpu::BufferUsages::VERTEX });
                let ib = device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: None, contents: bytemuck::cast_slice(&m.indices), usage: wgpu::BufferUsages::INDEX });
                let view = m.texture.and_then(|t| textures.get(t)).unwrap_or(&white);
                let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: None,
                    layout: &tbl,
                    entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(view) }, wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&sampler) }],
                });
                GpuMesh { vb, ib, count: m.indices.len() as u32, bind }
            })
            .collect();
        let cam = Camera::fit(scene);
        Viewer { pipeline, uniforms, uniform_bind, meshes, target: None, cam, home: cam, rendered: None, opened: Instant::now() }
    }

    fn attachments(rs: &egui_wgpu::RenderState, size: [u32; 2]) -> (wgpu::TextureView, wgpu::Texture, wgpu::TextureView, wgpu::TextureView) {
        let extent = wgpu::Extent3d { width: size[0], height: size[1], depth_or_array_layers: 1 };
        let make = |format, samples, usage| {
            rs.device.create_texture(&wgpu::TextureDescriptor { label: None, size: extent, mip_level_count: 1, sample_count: samples, dimension: wgpu::TextureDimension::D2, format, usage, view_formats: &[] })
        };
        let msaa = make(COLOR, SAMPLES, wgpu::TextureUsages::RENDER_ATTACHMENT).create_view(&Default::default());
        let resolve = make(COLOR, 1, wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC);
        let resolve_view = resolve.create_view(&Default::default());
        let depth = make(DEPTH, SAMPLES, wgpu::TextureUsages::RENDER_ATTACHMENT).create_view(&Default::default());
        (msaa, resolve, resolve_view, depth)
    }

    fn ensure_target(&mut self, rs: &egui_wgpu::RenderState, size: [u32; 2]) {
        if self.target.as_ref().map(|t| t.size == size).unwrap_or(false) {
            return;
        }
        let (msaa, resolve, resolve_view, depth) = Self::attachments(rs, size);
        let mut renderer = rs.renderer.write();
        let tex_id = match self.target.as_ref() {
            Some(old) => {
                renderer.update_egui_texture_from_wgpu_texture(&rs.device, &resolve_view, wgpu::FilterMode::Linear, old.tex_id);
                old.tex_id
            }
            None => renderer.register_native_texture(&rs.device, &resolve_view, wgpu::FilterMode::Linear),
        };
        drop(renderer);
        self.target = Some(Target { size, msaa, resolve, resolve_view, depth, tex_id });
        self.rendered = None;
    }

    /// Render if anything changed and return the texture to draw.
    pub fn render(&mut self, rs: &egui_wgpu::RenderState, size: [u32; 2], bg: Color32) -> egui::TextureId {
        let size = [size[0].clamp(16, 4096), size[1].clamp(16, 4096)];
        self.ensure_target(rs, size);
        let key = (self.cam, size, bg.to_array());
        if self.rendered != Some(key) {
            let t = self.target.as_ref().unwrap();
            self.draw(rs, &t.msaa, &t.resolve_view, &t.depth, size, bg);
            self.rendered = Some(key);
        }
        self.target.as_ref().unwrap().tex_id
    }

    fn draw(&self, rs: &egui_wgpu::RenderState, msaa: &wgpu::TextureView, resolve: &wgpu::TextureView, depth: &wgpu::TextureView, size: [u32; 2], bg: Color32) {
        let aspect = size[0] as f32 / size[1] as f32;
        let eye = self.cam.eye();
        let u = Uniforms { view_proj: self.cam.view_proj(aspect), eye: [eye[0], eye[1], eye[2], 1.0], light: [0.45, 0.85, 0.55, 0.0] };
        // A light that follows the camera keeps the front of the model readable.
        let yaw = self.cam.yaw;
        let l = normalize([0.45 * yaw.cos() + 0.55 * yaw.sin(), 0.85, -0.45 * yaw.sin() + 0.55 * yaw.cos()]);
        let u = Uniforms { light: [l[0], l[1], l[2], 0.0], ..u };
        rs.queue.write_buffer(&self.uniforms, 0, bytemuck::bytes_of(&u));
        // The target holds gamma-encoded values, so clear with the colour's sRGB bytes.
        let c = egui::Rgba::from_rgba_unmultiplied(bg.r() as f32 / 255.0, bg.g() as f32 / 255.0, bg.b() as f32 / 255.0, 1.0);
        let mut enc = rs.device.create_command_encoder(&Default::default());
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("review-3d"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: msaa,
                    depth_slice: None,
                    resolve_target: Some(resolve),
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color { r: c.r() as f64, g: c.g() as f64, b: c.b() as f64, a: 1.0 }), store: wgpu::StoreOp::Discard },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: depth,
                    depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Discard }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.uniform_bind, &[]);
            for m in &self.meshes {
                pass.set_bind_group(1, &m.bind, &[]);
                pass.set_vertex_buffer(0, m.vb.slice(..));
                pass.set_index_buffer(m.ib.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..m.count, 0, 0..1);
            }
        }
        rs.queue.submit([enc.finish()]);
    }

    /// Render the current view at `size` and read it back (for Export).
    pub fn snapshot(&mut self, rs: &egui_wgpu::RenderState, size: [u32; 2], bg: Color32) -> Result<image::RgbaImage, String> {
        let size = [size[0].clamp(16, 4096), size[1].clamp(16, 4096)];
        let (msaa, resolve, resolve_view, depth) = Self::attachments(rs, size);
        self.draw(rs, &msaa, &resolve_view, &depth, size, bg);
        let row = (4 * size[0]).div_ceil(256) * 256;
        let buf = rs.device.create_buffer(&wgpu::BufferDescriptor { label: None, size: (row * size[1]) as u64, usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ, mapped_at_creation: false });
        let mut enc = rs.device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo { texture: &resolve, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            wgpu::TexelCopyBufferInfo { buffer: &buf, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: Some(size[1]) } },
            wgpu::Extent3d { width: size[0], height: size[1], depth_or_array_layers: 1 },
        );
        rs.queue.submit([enc.finish()]);
        let slice = buf.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        let _ = rs.device.poll(wgpu::PollType::wait_indefinitely());
        rx.recv().map_err(|e| e.to_string())?.map_err(|e| e.to_string())?;
        let data = slice.get_mapped_range().map_err(|e| format!("{e:?}"))?;
        let mut out = Vec::with_capacity((size[0] * size[1] * 4) as usize);
        for y in 0..size[1] as usize {
            out.extend_from_slice(&data[y * row as usize..y * row as usize + size[0] as usize * 4]);
        }
        drop(data);
        buf.unmap();
        image::RgbaImage::from_raw(size[0], size[1], out).ok_or_else(|| "readback failed".into())
    }

    pub fn thumbnail_id(&self) -> Option<egui::TextureId> {
        self.target.as_ref().map(|t| t.tex_id)
    }
}

/// The canvas for a 3D document.
pub fn show(app: &mut crate::app::App, ui: &mut Ui, bg: Color32) {
    let ctx = ui.ctx().clone();
    let rect = ui.max_rect();
    let Some(rs) = app.wgpu.clone() else {
        ui.centered_and_justified(|ui| ui.label("3D models need GPU rendering, which isn't available here."));
        return;
    };
    let Some(doc) = app.docs.get(app.active) else { return };
    let id = doc.id;
    let Some(scene) = doc.scene.clone() else { return };
    let viewer = app.viewers.entry(id).or_insert_with(|| Viewer::new(&rs, &scene));

    let resp = ui.allocate_rect(rect, Sense::click_and_drag());
    let (shift, middle, secondary) = ctx.input(|i| (i.modifiers.shift, i.pointer.middle_down(), i.pointer.secondary_down()));
    if resp.dragged() {
        let d = resp.drag_delta();
        if shift || middle || secondary {
            viewer.cam.pan(d, rect.height());
            ctx.set_cursor_icon(CursorIcon::Grabbing);
        } else {
            viewer.cam.yaw -= d.x * 0.008;
            viewer.cam.pitch = (viewer.cam.pitch + d.y * 0.008).clamp(-1.55, 1.55);
            ctx.set_cursor_icon(CursorIcon::AllScroll);
        }
    } else if resp.hovered() {
        ctx.set_cursor_icon(CursorIcon::Grab);
    }
    if resp.hovered() {
        let (scroll, zoom) = ctx.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta()));
        if scroll.abs() > 0.0 {
            viewer.cam.dist *= (-scroll * 0.0025).exp();
        }
        if (zoom - 1.0).abs() > 1e-4 {
            viewer.cam.dist /= zoom;
        }
        let r = viewer.cam.radius;
        viewer.cam.dist = viewer.cam.dist.clamp(r * 0.05, r * 200.0);
    }
    if resp.double_clicked() {
        viewer.cam = viewer.home;
    }
    if !ctx.egui_wants_keyboard_input() {
        ctx.input(|i| {
            let step = 0.06;
            if i.key_down(egui::Key::ArrowLeft) {
                viewer.cam.yaw += step;
            }
            if i.key_down(egui::Key::ArrowRight) {
                viewer.cam.yaw -= step;
            }
            if i.key_down(egui::Key::ArrowUp) {
                viewer.cam.pitch = (viewer.cam.pitch + step).min(1.55);
            }
            if i.key_down(egui::Key::ArrowDown) {
                viewer.cam.pitch = (viewer.cam.pitch - step).max(-1.55);
            }
        });
    }

    let ppp = ctx.pixels_per_point();
    let size = [(rect.width() * ppp) as u32, (rect.height() * ppp) as u32];
    let tex = viewer.render(&rs, size, bg);
    ui.painter().image(tex, rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);

    // A gentle hint for the first few seconds.
    let age = viewer.opened.elapsed().as_secs_f32();
    if age < 6.0 {
        let a = ((6.0 - age) / 1.0).min(1.0);
        let text = "Drag to rotate · Shift-drag to pan · Scroll to zoom · Double-click to reset";
        let painter = ui.painter();
        let galley = painter.layout_no_wrap(text.into(), FontId::proportional(12.5), Color32::WHITE.gamma_multiply(a));
        let r = Rect::from_center_size(pos2(rect.center().x, rect.max.y - 34.0), galley.size() + vec2(24.0, 12.0));
        painter.rect_filled(r, r.height() / 2.0, Color32::from_black_alpha((150.0 * a) as u8));
        painter.galley(r.min + vec2(12.0, 6.0), galley, Color32::WHITE);
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
}
