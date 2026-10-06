//! SceneScript geometry owns bounded Rust buffers; live layers share each GPU buffer.

use crate::gpu::Mesh;
use crate::model_bounds::{self, Bounds};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    cell::{Cell, RefCell},
    rc::{Rc, Weak},
};
pub(crate) const CPU_BUDGET: usize = 64 * 1024 * 1024;

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Format(pub Vec<(&'static str, i32)>);
impl Format {
    pub fn new(values: &[String]) -> Result<Self> {
        let names = [
            ("position", "a_Position", 3),
            ("normal", "a_Normal", 3),
            ("tangentSigned", "a_Tangent4", 4),
            ("uv", "a_TexCoord", 2),
            ("color", "a_Color", 4),
        ];
        ensure!(
            values.first().is_some_and(|v| v == "position"),
            "vertexFormat must start with IModelData.POSITION"
        );
        let mut previous = None;
        let mut attributes = Vec::new();
        for name in values {
            let index = names
                .iter()
                .position(|(key, _, _)| key == name)
                .context("unknown custom vertex format")?;
            ensure!(
                previous.is_none_or(|p| index > p),
                "vertexFormat components are out of order or duplicated"
            );
            previous = Some(index);
            attributes.push((names[index].1, names[index].2));
        }
        Ok(Self(attributes))
    }
    pub fn stride(&self) -> usize {
        self.0.iter().map(|(_, count)| *count as usize).sum()
    }
}
pub(crate) struct GpuShape {
    pub mesh: RefCell<Mesh>,
    pub vertex_revision: Cell<u64>,
    pub index_revision: Cell<u64>,
}
#[derive(Clone)]
pub(crate) struct Shape {
    pub vertices: Rc<Vec<f32>>,
    pub indices: Option<Rc<Vec<u32>>>,
    pub index_type: Option<u8>,
    pub format: Format,
    pub dynamic: [bool; 2],
    pub material: String,
    pub passes: Value,
    pub vertex_revision: u64,
    pub index_revision: u64,
    pub gpu: Weak<GpuShape>,
    pub bounds: Option<Bounds>,
}
impl Shape {
    pub fn bytes(&self) -> usize {
        self.vertices.len() * 4 + self.indices.as_ref().map_or(0, |i| i.len() * 4)
    }
    pub fn validate(&self) -> Result<()> {
        let stride = self.format.stride();
        ensure!(
            self.vertices.len().is_multiple_of(stride)
                && self.vertices.len() / stride <= 2_000_000
                && self.vertices.iter().all(|f| f.is_finite()),
            "invalid custom vertex buffer"
        );
        if let Some(indices) = &self.indices {
            ensure!(
                indices.len() <= 6_000_000
                    && indices.len().is_multiple_of(3)
                    && indices
                        .iter()
                        .all(|i| (*i as usize) < self.vertices.len() / stride),
                "custom index buffer does not form valid triangles"
            );
        } else {
            ensure!(
                (self.vertices.len() / stride).is_multiple_of(3),
                "custom vertex buffer does not form triangles"
            );
        }
        ensure!(self.bytes() <= CPU_BUDGET, "custom shape exceeds 64 MiB");
        Ok(())
    }
}
pub(crate) struct Data {
    pub shapes: Vec<Option<Shape>>,
    pub bounds: Option<[[f32; 3]; 2]>,
    pub layout_revision: u64,
    pub revision: u64,
}
impl Data {
    pub fn effective_bounds(&self) -> Option<Bounds> {
        self.bounds.or_else(|| {
            let mut bounds = None;
            for shape in self.shapes.iter().flatten() {
                model_bounds::merge(&mut bounds, shape.bounds);
            }
            bounds
        })
    }
    pub fn bytes(&self) -> usize {
        self.shapes.iter().flatten().map(Shape::bytes).sum()
    }
    pub fn metadata(&self) -> Value {
        json!({"materials":self.shapes.iter().map(|s|s.as_ref().map_or_else(||json!([]),|s|s.passes.clone())).collect::<Vec<_>>(),"bounds":self.bounds})
    }
}
struct Entry {
    live: Option<Rc<RefCell<Data>>>,
    retained: Weak<RefCell<Data>>,
}
#[derive(Default)]
pub(crate) struct Registry {
    entries: Vec<Entry>,
}
pub(crate) type Shared = Rc<RefCell<Registry>>;
impl Registry {
    pub fn get(&self, id: usize) -> Result<Rc<RefCell<Data>>> {
        self.entries
            .get(id)
            .and_then(|entry| entry.live.clone())
            .context("ModelData handle has been destroyed or is invalid")
    }
    pub fn retained(&self, id: usize) -> Result<Rc<RefCell<Data>>> {
        self.entries
            .get(id)
            .and_then(|entry| entry.retained.upgrade())
            .context("ModelData has no surviving owner")
    }
    pub fn create(&mut self, data: Data) -> Result<usize> {
        ensure!(
            self.entries.len() < 1024,
            "ModelData handle budget exceeds 1024"
        );
        self.validate_budget(None, data.bytes())?;
        let id = self.entries.len();
        let data = Rc::new(RefCell::new(data));
        self.entries.push(Entry {
            retained: Rc::downgrade(&data),
            live: Some(data),
        });
        Ok(id)
    }
    pub fn destroy(&mut self, id: usize) -> Result<()> {
        self.get(id)?;
        self.entries[id].live = None;
        Ok(())
    }
    pub fn validate_budget(&self, replacing: Option<usize>, bytes: usize) -> Result<()> {
        let retained = self
            .entries
            .iter()
            .enumerate()
            .filter(|(id, _)| Some(*id) != replacing)
            .filter_map(|(_, entry)| entry.retained.upgrade())
            .map(|data| data.borrow().bytes())
            .sum::<usize>();
        ensure!(
            retained.saturating_add(bytes) <= CPU_BUDGET,
            "scene ModelData buffers exceed 64 MiB"
        );
        Ok(())
    }
}

pub(crate) mod host {
    //! Typed-array access is copied while JS is stopped; no Rust borrow spans a JS getter.
    use super::*;
    use crate::assets::Assets;
    use rquickjs::{Ctx, Exception, Object, TypedArray, Value as JsValue, function::Func};

    pub(crate) fn install(ctx: &Ctx<'_>, assets: &Assets, shared: Shared) -> Result<()> {
        let assets = assets.clone();
        let registry = shared;
        ctx.globals().set(
            "__weModelData",
            Func::from(
                move |ctx: Ctx<'_>,
                      action: String,
                      id: usize,
                      input: JsValue<'_>|
                      -> rquickjs::Result<usize> {
                    let result = match action.as_str() {
                        "create" => create(&registry, &assets, input),
                        "apply" => update(&registry, &assets, id, input, false).map(|_| id),
                        "replace" => update(&registry, &assets, id, input, true).map(|_| id),
                        "destroy" => registry.borrow_mut().destroy(id).map(|_| id),
                        "validate" => registry.borrow().get(id).map(|_| id),
                        _ => Err(anyhow::anyhow!("unknown ModelData operation")),
                    };
                    result.map_err(|error| {
                        let message = format!("{error:#}");
                        if message.contains("handle has been destroyed") {
                            Exception::throw_reference(&ctx, &message)
                        } else {
                            Exception::throw_type(&ctx, &message)
                        }
                    })
                },
            ),
        )?;
        Ok(())
    }
    fn inputs(value: JsValue<'_>, creating: bool) -> Result<Vec<JsValue<'_>>> {
        let value = if let Some(object) = value.as_object() {
            if object.contains_key("shapes")? {
                object.get("shapes")?
            } else {
                value
            }
        } else {
            value
        };
        if let Some(array) = value.as_array() {
            ensure!(array.len() <= 64, "ModelData exceeds 64 shapes");
            array.iter::<JsValue<'_>>().map(|v| Ok(v?)).collect()
        } else {
            ensure!(!creating, "createModelData requires a shapes array");
            Ok(vec![value])
        }
    }
    fn create(registry: &Shared, assets: &Assets, input: JsValue<'_>) -> Result<usize> {
        let bounds = bounds(&input)?;
        let mut shapes = Vec::new();
        let mut bytes = 0;
        for value in inputs(input, true)? {
            let shape = read_shape(assets, value, None, true)?;
            bytes += shape.as_ref().map_or(0, Shape::bytes);
            ensure!(bytes <= CPU_BUDGET, "ModelData exceeds 64 MiB");
            shapes.push(shape);
        }
        let data = Data {
            shapes,
            bounds,
            layout_revision: 1,
            revision: 1,
        };
        ensure!(data.bytes() <= CPU_BUDGET, "ModelData exceeds 64 MiB");
        registry.borrow_mut().create(data)
    }
    fn update(
        registry: &Shared,
        assets: &Assets,
        id: usize,
        input: JsValue<'_>,
        replace: bool,
    ) -> Result<()> {
        let data = registry.borrow().get(id)?;
        let (mut shapes, revision, bounds, layout_revision) = {
            let data = data.borrow();
            (
                data.shapes.clone(),
                data.revision,
                data.bounds,
                data.layout_revision,
            )
        };
        let patch = inputs(input, false)?;
        ensure!(
            replace || patch.len() <= shapes.len(),
            "applyData cannot add shapes"
        );
        if shapes.len() < patch.len() {
            shapes.resize_with(patch.len(), || None);
        }
        let mut bytes = shapes.iter().flatten().map(Shape::bytes).sum::<usize>();
        for (index, value) in patch.into_iter().enumerate() {
            if !value.is_undefined() {
                let shape = read_shape(assets, value, shapes[index].as_ref(), replace)?;
                bytes = bytes - shapes[index].as_ref().map_or(0, Shape::bytes)
                    + shape.as_ref().map_or(0, Shape::bytes);
                ensure!(bytes <= CPU_BUDGET, "ModelData exceeds 64 MiB");
                shapes[index] = shape;
            }
        }
        {
            let registry = registry.borrow();
            let owner = registry.get(id)?;
            ensure!(
                Rc::ptr_eq(&owner, &data) && data.borrow().revision == revision,
                "ModelData changed during input validation"
            );
            registry.validate_budget(Some(id), bytes)?;
        }
        *data.borrow_mut() = Data {
            shapes,
            bounds,
            layout_revision: if replace {
                layout_revision.wrapping_add(1)
            } else {
                layout_revision
            },
            revision: revision.wrapping_add(1),
        };
        Ok(())
    }
    fn field<'js>(object: &Object<'js>, key: &str) -> Result<Option<JsValue<'js>>> {
        let value: JsValue<'js> = object.get(key)?;
        Ok((!value.is_undefined()).then_some(value))
    }
    fn material(assets: &Assets, value: JsValue<'_>) -> Result<(String, Value)> {
        let file = if let Some(name) = value.as_string() {
            name.to_string()?
        } else {
            value
                .as_object()
                .context("material must be an IAssetHandle")?
                .get::<_, String>("file")?
        };
        assets.validate(&file)?;
        let mut passes = assets.json(&file)?["passes"].clone();
        let definitions = passes
            .as_array_mut()
            .context("custom material has no passes")?;
        ensure!(
            !definitions.is_empty() && definitions.len() <= 64,
            "custom material pass budget"
        );
        for pass in definitions {
            crate::shader::material_properties(assets, pass)?;
        }
        Ok((file, passes))
    }
    fn floats(value: JsValue<'_>) -> Result<Rc<Vec<f32>>> {
        let array =
            TypedArray::<f32>::from_value(value).context("vertexBuffer must be a Float32Array")?;
        // SAFETY: copy immediately, without calling JS while its buffer is borrowed.
        let bytes = unsafe { array.as_bytes() }.context("vertexBuffer is detached")?;
        ensure!(bytes.len() <= CPU_BUDGET, "vertexBuffer exceeds 64 MiB");
        Ok(Rc::new(
            bytes
                .as_chunks::<4>()
                .0
                .iter()
                .map(|bytes| f32::from_ne_bytes(*bytes))
                .collect(),
        ))
    }
    fn indices(value: JsValue<'_>) -> Result<(Rc<Vec<u32>>, u8)> {
        if let Ok(array) = TypedArray::<u16>::from_value(value.clone()) {
            // SAFETY: copy before any subsequent property access or JS execution.
            let bytes = unsafe { array.as_bytes() }.context("indexBuffer is detached")?;
            ensure!(
                bytes.len() <= CPU_BUDGET / 2,
                "indexBuffer exceeds converted 64 MiB"
            );
            Ok((
                Rc::new(
                    bytes
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|b| u16::from_ne_bytes(*b) as u32)
                        .collect(),
                ),
                16,
            ))
        } else {
            let array = TypedArray::<u32>::from_value(value)
                .context("indexBuffer must be a Uint16Array or Uint32Array")?;
            // SAFETY: copy before any subsequent property access or JS execution.
            let bytes = unsafe { array.as_bytes() }.context("indexBuffer is detached")?;
            ensure!(bytes.len() <= CPU_BUDGET, "indexBuffer exceeds 64 MiB");
            Ok((
                Rc::new(
                    bytes
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .map(|b| u32::from_ne_bytes(*b))
                        .collect(),
                ),
                32,
            ))
        }
    }
    fn read_shape(
        assets: &Assets,
        input: JsValue<'_>,
        old: Option<&Shape>,
        replace: bool,
    ) -> Result<Option<Shape>> {
        if input.is_null() {
            ensure!(replace, "applyData cannot remove shapes");
            return Ok(None);
        }
        let object = input
            .as_object()
            .context("custom shape must be an object")?;
        let vertex = field(object, "vertexBuffer")?;
        let index = field(object, "indexBuffer")?;
        let vertex_updated = vertex.is_some();
        let index_updated = index.is_some();
        let format = field(object, "vertexFormat")?
            .map(|value| {
                let array = value.as_array().context("vertexFormat must be an array")?;
                ensure!(array.len() <= 5, "vertexFormat has excessive components");
                let names = array
                    .iter::<String>()
                    .map(|value| Ok(value?))
                    .collect::<Result<Vec<_>>>()?;
                Format::new(&names)
            })
            .transpose()?;
        let dynamic = [
            field(object, "isVertexBufferDynamic")?
                .map(|v| v.as_bool().context("dynamic vertex flag must be boolean"))
                .transpose()?,
            field(object, "isIndexBufferDynamic")?
                .map(|v| v.as_bool().context("dynamic index flag must be boolean"))
                .transpose()?,
        ];
        let material = field(object, "material")?
            .map(|v| material(assets, v))
            .transpose()?;
        if !replace {
            let old = old.context("applyData cannot restore deleted shapes")?;
            ensure!(
                format.as_ref().is_none_or(|f| f == &old.format)
                    && dynamic
                        .iter()
                        .enumerate()
                        .all(|(i, v)| v.is_none_or(|v| v == old.dynamic[i]))
                    && material.as_ref().is_none_or(|(m, _)| m == &old.material),
                "applyData changes format, material or buffer flags"
            );
            ensure!(
                vertex.is_none() || old.dynamic[0],
                "vertexBuffer was not allocated as dynamic"
            );
            ensure!(
                index.is_none() || old.dynamic[1],
                "indexBuffer was not allocated as dynamic"
            );
        }
        let vertices = vertex
            .map(floats)
            .transpose()?
            .or_else(|| old.map(|s| s.vertices.clone()))
            .context("new shape requires vertexBuffer")?;
        let indices = match index {
            Some(value) if value.is_null() => {
                ensure!(replace, "applyData cannot remove indexBuffer");
                None
            }
            Some(value) => Some(indices(value)?),
            None => old.and_then(|s| s.indices.clone().zip(s.index_type)),
        };
        let format = format
            .or_else(|| old.map(|s| s.format.clone()))
            .context("new shape requires vertexFormat")?;
        let (material, passes) = material
            .or_else(|| old.map(|s| (s.material.clone(), s.passes.clone())))
            .context("new shape requires material")?;
        if !replace {
            let old = old.unwrap();
            ensure!(
                vertices.len() == old.vertices.len()
                    && indices.as_ref().map(|(i, t)| (i.len(), *t))
                        == old
                            .indices
                            .as_ref()
                            .zip(old.index_type)
                            .map(|(i, t)| (i.len(), t)),
                "applyData changes buffer lengths or index type"
            );
        }
        let mut shape = Shape {
            vertices,
            indices: indices.as_ref().map(|(i, _)| i.clone()),
            index_type: indices.map(|(_, t)| t),
            format,
            material,
            passes,
            dynamic: [
                dynamic[0].unwrap_or_else(|| old.is_some_and(|s| s.dynamic[0])),
                dynamic[1].unwrap_or_else(|| old.is_some_and(|s| s.dynamic[1])),
            ],
            vertex_revision: old.map_or(1, |s| {
                if vertex_updated {
                    s.vertex_revision.wrapping_add(1)
                } else {
                    s.vertex_revision
                }
            }),
            index_revision: old.map_or(1, |s| {
                if index_updated {
                    s.index_revision.wrapping_add(1)
                } else {
                    s.index_revision
                }
            }),
            gpu: if replace {
                Weak::new()
            } else {
                old.unwrap().gpu.clone()
            },
            bounds: None,
        };
        shape.validate()?;
        shape.bounds = if let Some(old) = old
            && old.format == shape.format
            && old.vertices == shape.vertices
            && old.indices == shape.indices
        {
            old.bounds
        } else {
            let mut bounds = None;
            let stride = shape.format.stride();
            if let Some(indices) = &shape.indices {
                for &index in indices.iter() {
                    let position = &shape.vertices[index as usize * stride..][..3];
                    model_bounds::include(&mut bounds, position.try_into().unwrap());
                }
            } else {
                for vertex in shape.vertices.chunks_exact(stride) {
                    model_bounds::include(&mut bounds, vertex[..3].try_into().unwrap());
                }
            }
            bounds
        };
        Ok(Some(shape))
    }
    fn bounds(input: &JsValue<'_>) -> Result<Option<[[f32; 3]; 2]>> {
        let object = input
            .as_object()
            .context("ModelData configuration must be an object")?;
        let (min, max) = (
            field(object, "boundingBoxMins")?,
            field(object, "boundingBoxMaxs")?,
        );
        if min.is_none() && max.is_none() {
            return Ok(None);
        }
        let read = |value: Option<JsValue<'_>>| -> Result<[f32; 3]> {
            let value = value.context("both bounding box corners are required")?;
            let object = value
                .as_object()
                .context("bounding box corner must be Vec3")?;
            Ok([object.get("x")?, object.get("y")?, object.get("z")?])
        };
        let (min, max) = (read(min)?, read(max)?);
        ensure!(
            min.iter()
                .zip(max)
                .all(|(a, b)| a.is_finite() && b.is_finite() && *a <= b),
            "invalid custom bounding box"
        );
        Ok(Some([min, max]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assets::Assets;

    #[test]
    fn indexed_multishape_bounds_layout_updates_reuse_explicit_and_atomic_failure() {
        let root = tempfile::tempdir().unwrap();
        let mut pkg = 8u32.to_le_bytes().to_vec();
        pkg.extend(b"PKGV0001");
        pkg.extend(0u32.to_le_bytes());
        std::fs::write(root.path().join("scene.pkg"), pkg).unwrap();
        std::fs::write(
            root.path().join("material.json"),
            br#"{"passes":[{"shader":"mesh"}]}"#,
        )
        .unwrap();
        std::fs::create_dir(root.path().join("shaders")).unwrap();
        std::fs::write(
            root.path().join("shaders/mesh.vert"),
            b"attribute vec3 a_Position;void main(){gl_Position=vec4(a_Position,1);}",
        )
        .unwrap();
        std::fs::write(
            root.path().join("shaders/mesh.frag"),
            b"void main(){gl_FragColor=vec4(1);}",
        )
        .unwrap();
        let assets = Assets::open(root.path(), &root.path().join("scene.pkg"), None).unwrap();
        let shared = Rc::new(RefCell::new(Registry::default()));
        let runtime = rquickjs::Runtime::new().unwrap();
        let context = rquickjs::Context::full(&runtime).unwrap();
        context.with(|ctx| {
        host::install(&ctx, &assets, shared.clone()).unwrap();
        ctx.eval::<(), _>(r#"
            const shape={vertexBuffer:new Float32Array([
                -2,-3,-4, 100,101,102, 0,0,
                 5, 6, 7, 100,101,102, 1,0,
                 1, 2, 3, 100,101,102, 0,1,
                 999,999,999, 100,101,102, 1,1
            ]),indexBuffer:new Uint16Array([0,1,2]),vertexFormat:['position','normal','uv'],material:'material.json',isVertexBufferDynamic:true,isIndexBufferDynamic:true};
            __weModelData('create',0,{shapes:[shape,{vertexBuffer:new Float32Array([-10,0,0,-9,1,0,-8,0,0]),vertexFormat:['position'],material:'material.json'}]});
            __weModelData('create',0,{shapes:[shape],boundingBoxMins:{x:-20,y:-20,z:-20},boundingBoxMaxs:{x:20,y:20,z:20}});
        "#).unwrap();
    });
        let data = shared.borrow().get(0).unwrap();
        assert_eq!(
            data.borrow().effective_bounds(),
            Some([[-10., -3., -4.], [5., 6., 7.]])
        );
        let before = data.borrow().shapes[0].as_ref().unwrap().vertices.clone();
        context.with(|ctx| {
            ctx.eval::<(), _>("__weModelData('apply',0,{});").unwrap();
        });
        assert!(Rc::ptr_eq(
            &before,
            &data.borrow().shapes[0].as_ref().unwrap().vertices
        ));
        context.with(|ctx| {
            ctx.eval::<(), _>("__weModelData('apply',0,{indexBuffer:new Uint16Array([1,2,3])});")
                .unwrap();
            assert!(
                ctx.eval::<(), _>(
                    "__weModelData('replace',0,[null,{vertexBuffer:new Float32Array([NaN,0,0])}]);"
                )
                .is_err()
            );
            ctx.catch();
        });
        assert_eq!(
            data.borrow().effective_bounds(),
            Some([[-10., 0., 0.], [999., 999., 999.]])
        );
        assert!(
            data.borrow().shapes[0].is_some(),
            "failed replacement must be atomic"
        );
        context.with(|ctx| {
        ctx.eval::<(), _>("__weModelData('replace',0,[null,{}]);__weModelData('apply',1,{indexBuffer:new Uint16Array([1,2,3])});").unwrap();
    });
        assert_eq!(
            data.borrow().effective_bounds(),
            Some([[-10., 0., 0.], [-8., 1., 0.]])
        );
        assert_eq!(
            shared.borrow().get(1).unwrap().borrow().effective_bounds(),
            Some([[-20.; 3], [20.; 3]])
        );
        shared.borrow_mut().destroy(0).unwrap();
        assert_eq!(
            shared
                .borrow()
                .retained(0)
                .unwrap()
                .borrow()
                .effective_bounds(),
            data.borrow().effective_bounds()
        );
    }
}
