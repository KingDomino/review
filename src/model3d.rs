//! 3D models: glTF/GLB, OBJ, STL and USD (usda/usdc/usdz), flattened into world-space
//! triangle meshes for the viewer.

use std::path::Path;

use image::RgbaImage;

pub const MODEL_EXTS: &[&str] = &["gltf", "glb", "obj", "stl", "usd", "usda", "usdc", "usdz"];

pub fn is_model_path(p: &Path) -> bool {
    p.extension().map(|e| MODEL_EXTS.contains(&e.to_string_lossy().to_lowercase().as_str())).unwrap_or(false)
}

#[derive(Clone, Default)]
pub struct Mesh {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub uvs: Vec<[f32; 2]>,
    /// Per-vertex colour (linear RGBA), already multiplied by the material colour.
    pub colors: Vec<[f32; 4]>,
    pub indices: Vec<u32>,
    pub texture: Option<usize>,
}

pub struct Scene {
    pub meshes: Vec<Mesh>,
    pub textures: Vec<RgbaImage>,
    pub min: [f32; 3],
    pub max: [f32; 3],
    pub format: String,
}

impl Scene {
    pub fn triangles(&self) -> usize {
        self.meshes.iter().map(|m| m.indices.len() / 3).sum()
    }
    pub fn vertices(&self) -> usize {
        self.meshes.iter().map(|m| m.positions.len()).sum()
    }
    pub fn center(&self) -> [f32; 3] {
        [(self.min[0] + self.max[0]) / 2.0, (self.min[1] + self.max[1]) / 2.0, (self.min[2] + self.max[2]) / 2.0]
    }
    pub fn radius(&self) -> f32 {
        let d = [self.max[0] - self.min[0], self.max[1] - self.min[1], self.max[2] - self.min[2]];
        ((d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt() / 2.0).max(1e-6)
    }
}

// ---------------------------------------------------------------------------
// Small column-major 4x4 matrix helpers.

pub type Mat4 = [f32; 16];

pub const IDENTITY: Mat4 = [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0];

pub fn mul(a: &Mat4, b: &Mat4) -> Mat4 {
    let mut r = [0.0; 16];
    for c in 0..4 {
        for row in 0..4 {
            r[c * 4 + row] = (0..4).map(|k| a[k * 4 + row] * b[c * 4 + k]).sum();
        }
    }
    r
}

pub fn transform_point(m: &Mat4, p: [f32; 3]) -> [f32; 3] {
    [
        m[0] * p[0] + m[4] * p[1] + m[8] * p[2] + m[12],
        m[1] * p[0] + m[5] * p[1] + m[9] * p[2] + m[13],
        m[2] * p[0] + m[6] * p[1] + m[10] * p[2] + m[14],
    ]
}

fn transform_dir(m: &Mat4, p: [f32; 3]) -> [f32; 3] {
    normalize([m[0] * p[0] + m[4] * p[1] + m[8] * p[2], m[1] * p[0] + m[5] * p[1] + m[9] * p[2], m[2] * p[0] + m[6] * p[1] + m[10] * p[2]])
}

pub fn normalize(v: [f32; 3]) -> [f32; 3] {
    let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if l < 1e-12 {
        [0.0, 1.0, 0.0]
    } else {
        [v[0] / l, v[1] / l, v[2] / l]
    }
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn translation(t: [f32; 3]) -> Mat4 {
    let mut m = IDENTITY;
    m[12] = t[0];
    m[13] = t[1];
    m[14] = t[2];
    m
}

fn scale(s: [f32; 3]) -> Mat4 {
    let mut m = IDENTITY;
    m[0] = s[0];
    m[5] = s[1];
    m[10] = s[2];
    m
}

fn rotation_axis(axis: usize, deg: f32) -> Mat4 {
    let (s, c) = deg.to_radians().sin_cos();
    let mut m = IDENTITY;
    let (i, j) = match axis {
        0 => (1, 2),
        1 => (2, 0),
        _ => (0, 1),
    };
    m[i * 4 + i] = c;
    m[j * 4 + j] = c;
    m[i * 4 + j] = s;
    m[j * 4 + i] = -s;
    m
}

fn quat(w: f32, x: f32, y: f32, z: f32) -> Mat4 {
    let (xx, yy, zz, xy, xz, yz, wx, wy, wz) = (x * x, y * y, z * z, x * y, x * z, y * z, w * x, w * y, w * z);
    [
        1.0 - 2.0 * (yy + zz), 2.0 * (xy + wz), 2.0 * (xz - wy), 0.0,
        2.0 * (xy - wz), 1.0 - 2.0 * (xx + zz), 2.0 * (yz + wx), 0.0,
        2.0 * (xz + wy), 2.0 * (yz - wx), 1.0 - 2.0 * (xx + yy), 0.0,
        0.0, 0.0, 0.0, 1.0,
    ]
}

// ---------------------------------------------------------------------------

pub fn load(path: &Path) -> Result<Scene, String> {
    let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    let mut scene = match ext.as_str() {
        "gltf" | "glb" => load_gltf(path)?,
        "obj" => load_obj(path)?,
        "stl" => load_stl(path)?,
        "usd" | "usda" | "usdc" | "usdz" => load_usd(path)?,
        _ => return Err("Unsupported 3D format".into()),
    };
    scene.meshes.retain(|m| !m.indices.is_empty());
    if scene.meshes.is_empty() {
        return Err("The file contains no triangle meshes.".into());
    }
    for m in &mut scene.meshes {
        finish_mesh(m);
    }
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    for p in scene.meshes.iter().flat_map(|m| m.positions.iter()) {
        for k in 0..3 {
            min[k] = min[k].min(p[k]);
            max[k] = max[k].max(p[k]);
        }
    }
    scene.min = min;
    scene.max = max;
    Ok(scene)
}

/// Fill in missing normals (flat shading), UVs and colours so every mesh is complete.
fn finish_mesh(m: &mut Mesh) {
    if m.normals.len() != m.positions.len() {
        // Unweld so each triangle gets its own face normal.
        let mut pos = Vec::with_capacity(m.indices.len());
        let mut nrm = Vec::with_capacity(m.indices.len());
        let mut uvs = Vec::new();
        let mut col = Vec::new();
        for tri in m.indices.chunks_exact(3) {
            let [a, b, c] = [tri[0] as usize, tri[1] as usize, tri[2] as usize];
            if a >= m.positions.len() || b >= m.positions.len() || c >= m.positions.len() {
                continue;
            }
            let n = normalize(cross(sub(m.positions[b], m.positions[a]), sub(m.positions[c], m.positions[a])));
            for &i in &[a, b, c] {
                pos.push(m.positions[i]);
                nrm.push(n);
                if let Some(uv) = m.uvs.get(i) {
                    uvs.push(*uv);
                }
                if let Some(cl) = m.colors.get(i) {
                    col.push(*cl);
                }
            }
        }
        m.indices = (0..pos.len() as u32).collect();
        m.positions = pos;
        m.normals = nrm;
        m.uvs = if uvs.len() == m.positions.len() { uvs } else { Vec::new() };
        m.colors = if col.len() == m.positions.len() { col } else { Vec::new() };
    }
    if m.uvs.len() != m.positions.len() {
        m.uvs = vec![[0.0, 0.0]; m.positions.len()];
    }
    if m.colors.len() != m.positions.len() {
        m.colors = vec![[0.8, 0.8, 0.8, 1.0]; m.positions.len()];
    }
}

fn load_gltf(path: &Path) -> Result<Scene, String> {
    let (doc, buffers, images) = gltf::import(path).map_err(|e| e.to_string())?;
    let mut textures: Vec<RgbaImage> = Vec::new();
    let mut tex_of_image: Vec<Option<usize>> = Vec::new();
    for img in &images {
        use gltf::image::Format as F;
        let (w, h) = (img.width, img.height);
        let rgba: Option<Vec<u8>> = match img.format {
            F::R8G8B8A8 => Some(img.pixels.clone()),
            F::R8G8B8 => Some(img.pixels.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect()),
            F::R8G8 => Some(img.pixels.chunks_exact(2).flat_map(|p| [p[0], p[0], p[0], p[1]]).collect()),
            F::R8 => Some(img.pixels.iter().flat_map(|p| [*p, *p, *p, 255]).collect()),
            _ => None,
        };
        tex_of_image.push(rgba.and_then(|d| RgbaImage::from_raw(w, h, d)).map(|t| {
            textures.push(t);
            textures.len() - 1
        }));
    }
    let mut meshes = Vec::new();
    let scene = doc.default_scene().or_else(|| doc.scenes().next()).ok_or("glTF has no scene")?;
    fn visit(node: gltf::Node, parent: Mat4, buffers: &[gltf::buffer::Data], tex: &[Option<usize>], out: &mut Vec<Mesh>) {
        let local: Mat4 = bytemuck::cast(node.transform().matrix());
        let world = mul(&parent, &local);
        if let Some(mesh) = node.mesh() {
            for prim in mesh.primitives() {
                if prim.mode() != gltf::mesh::Mode::Triangles {
                    continue;
                }
                let r = prim.reader(|b| buffers.get(b.index()).map(|d| &d.0[..]));
                let Some(pos) = r.read_positions() else { continue };
                let positions: Vec<[f32; 3]> = pos.map(|p| transform_point(&world, p)).collect();
                let normals: Vec<[f32; 3]> = r.read_normals().map(|n| n.map(|n| transform_dir(&world, n)).collect()).unwrap_or_default();
                let uvs: Vec<[f32; 2]> = r.read_tex_coords(0).map(|t| t.into_f32().collect()).unwrap_or_default();
                let mat = prim.material();
                let pbr = mat.pbr_metallic_roughness();
                let base = pbr.base_color_factor();
                let colors: Vec<[f32; 4]> = match r.read_colors(0) {
                    Some(c) => c.into_rgba_f32().map(|c| [c[0] * base[0], c[1] * base[1], c[2] * base[2], c[3] * base[3]]).collect(),
                    None => vec![base; positions.len()],
                };
                let indices: Vec<u32> = match r.read_indices() {
                    Some(i) => i.into_u32().collect(),
                    None => (0..positions.len() as u32).collect(),
                };
                let texture = pbr.base_color_texture().and_then(|t| tex.get(t.texture().source().index()).copied().flatten());
                out.push(Mesh { positions, normals, uvs, colors, indices, texture });
            }
        }
        for child in node.children() {
            visit(child, world, buffers, tex, out);
        }
    }
    for node in scene.nodes() {
        visit(node, IDENTITY, &buffers, &tex_of_image, &mut meshes);
    }
    Ok(Scene { meshes, textures, min: [0.0; 3], max: [0.0; 3], format: "glTF model".into() })
}

fn load_obj(path: &Path) -> Result<Scene, String> {
    let opts = tobj::LoadOptions { triangulate: true, single_index: true, ..Default::default() };
    let (models, materials) = tobj::load_obj(path, &opts).map_err(|e| e.to_string())?;
    let materials = materials.unwrap_or_default();
    let dir = path.parent().unwrap_or(Path::new("."));
    let mut textures = Vec::new();
    let mut tex_for_mat: Vec<Option<usize>> = Vec::new();
    for m in &materials {
        let t = m.diffuse_texture.as_ref().and_then(|f| image::open(dir.join(f.replace('\\', "/"))).ok()).map(|i| {
            textures.push(i.to_rgba8());
            textures.len() - 1
        });
        tex_for_mat.push(t);
    }
    let meshes = models
        .into_iter()
        .map(|m| {
            let mesh = m.mesh;
            let positions: Vec<[f32; 3]> = mesh.positions.chunks_exact(3).map(|p| [p[0], p[1], p[2]]).collect();
            let normals = mesh.normals.chunks_exact(3).map(|n| [n[0], n[1], n[2]]).collect();
            // OBJ texture V runs bottom-up.
            let uvs = mesh.texcoords.chunks_exact(2).map(|t| [t[0], 1.0 - t[1]]).collect();
            let mat = mesh.material_id.and_then(|i| materials.get(i));
            let d = mat.and_then(|m| m.diffuse).unwrap_or([0.8, 0.8, 0.8]);
            let colors = if mesh.vertex_color.len() == mesh.positions.len() {
                mesh.vertex_color.chunks_exact(3).map(|c| [c[0], c[1], c[2], 1.0]).collect()
            } else {
                vec![[d[0], d[1], d[2], 1.0]; positions.len()]
            };
            let texture = mesh.material_id.and_then(|i| tex_for_mat.get(i).copied().flatten());
            Mesh { positions, normals, uvs, colors, indices: mesh.indices, texture }
        })
        .collect();
    Ok(Scene { meshes, textures, min: [0.0; 3], max: [0.0; 3], format: "OBJ model".into() })
}

fn load_stl(path: &Path) -> Result<Scene, String> {
    let mut f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let stl = stl_io::read_stl(&mut f).map_err(|e| e.to_string())?;
    let positions: Vec<[f32; 3]> = stl.vertices.iter().map(|v| [v[0], v[1], v[2]]).collect();
    let indices: Vec<u32> = stl.faces.iter().flat_map(|f| f.vertices.iter().map(|i| *i as u32)).collect();
    // STL is Z-up: rotate so it stands upright in a Y-up viewer.
    let rot = rotation_axis(0, -90.0);
    let positions = positions.into_iter().map(|p| transform_point(&rot, p)).collect();
    let mesh = Mesh { positions, indices, colors: Vec::new(), ..Default::default() };
    Ok(Scene { meshes: vec![mesh], textures: Vec::new(), min: [0.0; 3], max: [0.0; 3], format: "STL model".into() })
}

// ---------------------------------------------------------------------------
// USD

fn vec3s(v: &openusd::sdf::Value) -> Option<Vec<[f32; 3]>> {
    use openusd::sdf::Value as V;
    Some(match v {
        V::Vec3fVec(a) => a.iter().map(|p| [p.x, p.y, p.z]).collect(),
        V::Vec3dVec(a) => a.iter().map(|p| [p.x as f32, p.y as f32, p.z as f32]).collect(),
        V::Vec3hVec(a) => a.iter().map(|p| [f32::from(p.x), f32::from(p.y), f32::from(p.z)]).collect(),
        _ => return None,
    })
}

fn vec3(v: &openusd::sdf::Value) -> Option<[f32; 3]> {
    use openusd::sdf::Value as V;
    Some(match v {
        V::Vec3f(p) => [p.x, p.y, p.z],
        V::Vec3d(p) => [p.x as f32, p.y as f32, p.z as f32],
        V::Vec3h(p) => [f32::from(p.x), f32::from(p.y), f32::from(p.z)],
        V::Float(f) => [*f; 3],
        V::Double(f) => [*f as f32; 3],
        _ => vec3s(v)?.first().copied()?,
    })
}

fn ints(v: &openusd::sdf::Value) -> Option<Vec<i64>> {
    use openusd::sdf::Value as V;
    Some(match v {
        V::IntVec(a) => a.iter().map(|x| *x as i64).collect(),
        V::Int64Vec(a) => a.clone(),
        V::UintVec(a) => a.iter().map(|x| *x as i64).collect(),
        _ => return None,
    })
}

fn attr(prim: &openusd::usd::Prim, name: &str) -> Option<openusd::sdf::Value> {
    prim.attribute(name).get::<openusd::sdf::Value>().ok().flatten()
}

/// The prim's local transform from its xformOps.
fn usd_local(prim: &openusd::usd::Prim) -> (Mat4, bool) {
    use openusd::sdf::Value as V;
    let Some(V::TokenVec(order)) = attr(prim, "xformOpOrder") else { return (IDENTITY, false) };
    let mut m = IDENTITY;
    let mut reset = false;
    for op in order.iter() {
        let name = op.as_str();
        if name == "!resetXformStack!" {
            reset = true;
            m = IDENTITY;
            continue;
        }
        let (inverse, name) = match name.strip_prefix("!invert!") {
            Some(n) => (true, n),
            None => (false, name),
        };
        let Some(v) = attr(prim, name) else { continue };
        let kind = name.trim_start_matches("xformOp:").split(':').next().unwrap_or("");
        let t: Mat4 = match (kind, &v) {
            ("translate", v) => translation(vec3(v).unwrap_or([0.0; 3])),
            ("scale", v) => scale(vec3(v).unwrap_or([1.0; 3])),
            ("rotateX", V::Float(d)) => rotation_axis(0, *d),
            ("rotateY", V::Float(d)) => rotation_axis(1, *d),
            ("rotateZ", V::Float(d)) => rotation_axis(2, *d),
            ("rotateX", V::Double(d)) => rotation_axis(0, *d as f32),
            ("rotateY", V::Double(d)) => rotation_axis(1, *d as f32),
            ("rotateZ", V::Double(d)) => rotation_axis(2, *d as f32),
            (k, v) if k.starts_with("rotate") && k.len() == 9 => {
                let a = vec3(v).unwrap_or([0.0; 3]);
                // rotateXYZ etc: apply in the named order.
                let mut r = IDENTITY;
                for ch in k[6..].chars() {
                    let (i, ang) = match ch {
                        'X' => (0, a[0]),
                        'Y' => (1, a[1]),
                        _ => (2, a[2]),
                    };
                    r = mul(&rotation_axis(i, ang), &r);
                }
                r
            }
            ("orient", V::Quatf(q)) => quat(q.w, q.x, q.y, q.z),
            ("orient", V::Quatd(q)) => quat(q.w as f32, q.x as f32, q.y as f32, q.z as f32),
            ("transform", V::Matrix4d(mat)) => {
                let mut out = [0.0f32; 16];
                for (i, x) in mat.0.iter().enumerate() {
                    out[i] = *x as f32;
                }
                out
            }
            _ => IDENTITY,
        };
        let t = if inverse { invert_affine(&t) } else { t };
        m = mul(&m, &t);
    }
    (m, reset)
}

fn invert_affine(m: &Mat4) -> Mat4 {
    // Inverse of [R|t] for rotation/scale/translation matrices.
    let a = [[m[0], m[4], m[8]], [m[1], m[5], m[9]], [m[2], m[6], m[10]]];
    let det = a[0][0] * (a[1][1] * a[2][2] - a[1][2] * a[2][1]) - a[0][1] * (a[1][0] * a[2][2] - a[1][2] * a[2][0]) + a[0][2] * (a[1][0] * a[2][1] - a[1][1] * a[2][0]);
    if det.abs() < 1e-12 {
        return IDENTITY;
    }
    let inv = |r: usize, c: usize| -> f32 {
        let (r1, r2) = ((r + 1) % 3, (r + 2) % 3);
        let (c1, c2) = ((c + 1) % 3, (c + 2) % 3);
        (a[c1][r1] * a[c2][r2] - a[c1][r2] * a[c2][r1]) / det
    };
    let mut o = IDENTITY;
    for r in 0..3 {
        for c in 0..3 {
            o[c * 4 + r] = inv(r, c);
        }
    }
    let t = [m[12], m[13], m[14]];
    for r in 0..3 {
        o[12 + r] = -(o[r] * t[0] + o[4 + r] * t[1] + o[8 + r] * t[2]);
    }
    o
}

/// Base colour of a mesh: displayColor, else its bound UsdPreviewSurface diffuseColor.
fn usd_color(stage: &openusd::usd::Stage, prim: &openusd::usd::Prim) -> Option<[f32; 3]> {
    if let Some(c) = attr(prim, "primvars:displayColor").as_ref().and_then(vec3) {
        return Some(c);
    }
    let targets = prim.relationship("material:binding").targets().ok()?;
    let mat = stage.prim(targets.first()?.clone()).ok()?;
    let mut stack = mat.children().ok()?;
    while let Some(child) = stack.pop() {
        if let Some(c) = attr(&child, "inputs:diffuseColor").as_ref().and_then(vec3) {
            return Some(c);
        }
        stack.extend(child.children().unwrap_or_default());
    }
    None
}

fn load_usd(path: &Path) -> Result<Scene, String> {
    use openusd::usd::{PrimPredicate, Stage};
    let stage = Stage::open(path.to_string_lossy().as_ref()).map_err(|e| format!("Can't read USD: {e}"))?;
    let z_up = matches!(stage.stage_metadata("upAxis"), Ok(Some(openusd::sdf::Value::Token(t))) if t.as_str() == "Z");
    let mut paths = Vec::new();
    stage.traverse(PrimPredicate::DEFAULT, |p| paths.push(p.clone())).map_err(|e| e.to_string())?;
    let mut meshes = Vec::new();
    let up = if z_up { rotation_axis(0, -90.0) } else { IDENTITY };
    for p in paths {
        let Ok(prim) = stage.prim(p) else { continue };
        if prim.type_name().ok().flatten().map(|t| t.as_str() != "Mesh").unwrap_or(true) {
            continue;
        }
        let (Some(points), Some(counts), Some(idx)) = (
            attr(&prim, "points").as_ref().and_then(vec3s),
            attr(&prim, "faceVertexCounts").as_ref().and_then(ints),
            attr(&prim, "faceVertexIndices").as_ref().and_then(ints),
        ) else {
            continue;
        };
        // World transform: compose ancestors' xformOps.
        let mut world = IDENTITY;
        let mut chain = vec![prim.clone()];
        let mut cur = prim.parent();
        while let Some(pp) = cur {
            cur = pp.parent();
            chain.push(pp);
        }
        for node in chain.iter().rev() {
            let (local, reset) = usd_local(node);
            world = if reset { local } else { mul(&world, &local) };
        }
        let world = mul(&up, &world);
        let left = matches!(attr(&prim, "orientation"), Some(openusd::sdf::Value::Token(t)) if t.as_str() == "leftHanded");
        let mut indices = Vec::with_capacity(idx.len() * 2);
        let mut at = 0usize;
        for &n in &counts {
            let n = n.max(0) as usize;
            if at + n > idx.len() {
                break;
            }
            for k in 1..n.saturating_sub(1) {
                let (a, b, c) = (idx[at] as u32, idx[at + k] as u32, idx[at + k + 1] as u32);
                if left {
                    indices.extend_from_slice(&[a, c, b]);
                } else {
                    indices.extend_from_slice(&[a, b, c]);
                }
            }
            at += n;
        }
        let positions: Vec<[f32; 3]> = points.into_iter().map(|q| transform_point(&world, q)).collect();
        let color = usd_color(&stage, &prim).unwrap_or([0.75, 0.75, 0.78]);
        let colors = vec![[color[0], color[1], color[2], 1.0]; positions.len()];
        meshes.push(Mesh { positions, colors, indices, ..Default::default() });
    }
    let fmt = path.extension().map(|e| e.to_string_lossy().to_uppercase()).unwrap_or_default();
    Ok(Scene { meshes, textures: Vec::new(), min: [0.0; 3], max: [0.0; 3], format: format!("{fmt} model") })
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub fn write_samples(dir: &Path) -> Vec<std::path::PathBuf> {
        let obj = dir.join("cube.obj");
        let mut s = String::new();
        for z in [-1.0, 1.0] {
            for y in [-1.0, 1.0] {
                for x in [-1.0, 1.0] {
                    s += &format!("v {x} {y} {z}\n");
                }
            }
        }
        for f in ["1 3 4 2", "5 6 8 7", "1 2 6 5", "3 7 8 4", "1 5 7 3", "2 4 8 6"] {
            s += &format!("f {f}\n");
        }
        std::fs::write(&obj, s).unwrap();
        let usda = dir.join("pyramid.usda");
        std::fs::write(
            &usda,
            r#"#usda 1.0
(
    upAxis = "Y"
)

def Xform "World"
{
    double3 xformOp:translate = (0, 1, 0)
    uniform token[] xformOpOrder = ["xformOp:translate"]

    def Mesh "Pyramid"
    {
        point3f[] points = [(-1, 0, -1), (1, 0, -1), (1, 0, 1), (-1, 0, 1), (0, 2, 0)]
        int[] faceVertexCounts = [4, 3, 3, 3, 3]
        int[] faceVertexIndices = [0, 1, 2, 3, 0, 4, 1, 1, 4, 2, 2, 4, 3, 3, 4, 0]
        color3f[] primvars:displayColor = [(0.9, 0.3, 0.1)]
    }
}
"#,
        )
        .unwrap();
        // A glTF quad (two triangles) with an embedded buffer and a material colour.
        let mut buf: Vec<u8> = Vec::new();
        for p in [[-1.0f32, -1.0, 0.0], [1.0, -1.0, 0.0], [1.0, 1.0, 0.0], [-1.0, 1.0, 0.0]] {
            for c in p {
                buf.extend_from_slice(&c.to_le_bytes());
            }
        }
        for i in [0u16, 1, 2, 0, 2, 3] {
            buf.extend_from_slice(&i.to_le_bytes());
        }
        let b64 = base64_encode(&buf);
        let gltf = dir.join("quad.gltf");
        std::fs::write(
            &gltf,
            format!(
                r#"{{"asset":{{"version":"2.0"}},"scene":0,"scenes":[{{"nodes":[0]}}],"nodes":[{{"mesh":0,"translation":[0,0,-1]}}],
"meshes":[{{"primitives":[{{"attributes":{{"POSITION":0}},"indices":1,"material":0}}]}}],
"materials":[{{"pbrMetallicRoughness":{{"baseColorFactor":[0.1,0.4,0.9,1.0]}}}}],
"buffers":[{{"byteLength":{},"uri":"data:application/octet-stream;base64,{b64}"}}],
"bufferViews":[{{"buffer":0,"byteOffset":0,"byteLength":48}},{{"buffer":0,"byteOffset":48,"byteLength":12}}],
"accessors":[{{"bufferView":0,"componentType":5126,"count":4,"type":"VEC3","min":[-1,-1,0],"max":[1,1,0]}},{{"bufferView":1,"componentType":5123,"count":6,"type":"SCALAR"}}]}}"#,
                buf.len()
            ),
        )
        .unwrap();
        vec![obj, usda, gltf]
    }

    fn base64_encode(data: &[u8]) -> String {
        const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for c in data.chunks(3) {
            let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
            for i in 0..4 {
                if i <= c.len() {
                    out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
                } else {
                    out.push('=');
                }
            }
        }
        out
    }

    #[test]
    fn loads_obj_and_usd() {
        let dir = std::env::temp_dir().join(format!("review-3d-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let files = write_samples(&dir);
        let cube = load(&files[0]).unwrap();
        assert_eq!(cube.triangles(), 12);
        assert!((cube.radius() - 3f32.sqrt()).abs() < 1e-3);
        let pyr = load(&files[1]).unwrap();
        assert_eq!(pyr.triangles(), 6, "quad base + 4 sides");
        assert!((pyr.min[1] - 1.0).abs() < 1e-4 && (pyr.max[1] - 3.0).abs() < 1e-4, "xform applied: {:?} {:?}", pyr.min, pyr.max);
        assert!(pyr.meshes[0].colors[0][0] > 0.8, "displayColor read");
        let quad = load(&files[2]).unwrap();
        assert_eq!(quad.triangles(), 2);
        assert!((quad.min[2] + 1.0).abs() < 1e-4, "node translation applied");
        assert!(quad.meshes[0].colors[0][2] > 0.8, "baseColorFactor read");
    }
}
