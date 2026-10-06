//! Each scene owns one QuickJS-NG runtime, confined to the rendering thread.
mod compat;
mod creation;
mod matrix;
mod storage;
pub use storage::ScriptStorage;
#[cfg(test)]
mod tests;

use crate::{
    assets::Assets,
    scene::bindings::{Properties, resolve},
};
use anyhow::{Context as _, Result, ensure};
use rand_chacha::ChaCha8Rng;
use rand_core::{Rng, SeedableRng};
use rquickjs::{
    Array, CaughtError, Context, Ctx, Exception, Function, IntoJs, Module, Object, Runtime,
    Value as JsValue,
    function::Func,
    loader::{ImportAttributes, Loader, Resolver},
    module::Declared,
    object::Property,
};
use serde_json::{Value, json};
use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, VecDeque},
    path::{Component, Path, PathBuf},
    rc::Rc,
    time::{Duration, Instant},
};

const MEMORY: usize = 64 * 1024 * 1024;
const FRAME_BUDGET: Duration = Duration::from_millis(20);
const LOAD_BUDGET: Duration = Duration::from_millis(250);

pub(crate) trait FrameData: serde::Serialize {
    fn to_frame<'js>(&self, ctx: &Ctx<'js>) -> Result<JsValue<'js>> {
        to_js(ctx, self)
    }
}
impl FrameData for Value {}

/// Open script fields retain JSON's own-property and number semantics.
pub(crate) struct Json<'a>(pub &'a Value);
impl<'js> IntoJs<'js> for Json<'_> {
    fn into_js(self, ctx: &Ctx<'js>) -> rquickjs::Result<JsValue<'js>> {
        match self.0 {
            Value::Null => Ok(JsValue::new_null(ctx.clone())),
            Value::Bool(value) => value.into_js(ctx),
            Value::Number(value) => value.as_f64().unwrap().into_js(ctx),
            Value::String(value) => value.into_js(ctx),
            Value::Array(values) => {
                let array = Array::new(ctx.clone())?;
                for (index, value) in values.iter().enumerate() {
                    array.set(index, Json(value))?;
                }
                Ok(array.into_value())
            }
            Value::Object(values) => {
                let object = Object::new(ctx.clone())?;
                for (key, value) in values {
                    object.prop(
                        key.as_str(),
                        Property::from(Json(value))
                            .writable()
                            .enumerable()
                            .configurable(),
                    )?;
                }
                Ok(object.into_value())
            }
        }
    }
}

struct Binding {
    node: usize,
    path: Vec<String>,
    source: String,
    properties: Value,
}
pub(crate) struct Scripts {
    context: Context,
    runtime: Runtime,
    deadline: Rc<Cell<Instant>>,
    logs: Rc<RefCell<VecDeque<String>>>,
    error_generation: Rc<Cell<u64>>,
    bindings: Vec<Binding>,
    dynamic_bindings: Rc<RefCell<Vec<Binding>>>,
    pub model_data: crate::model_data::Shared,
    poses: Rc<RefCell<HashMap<usize, Vec<glam::Mat4>>>>,
    properties: Properties,
    pub animated: bool,
    pub pointer: bool,
    pub audio: bool,
    pub media_timeline: bool,
    exhausted: bool,
}
impl Scripts {
    #[cfg(test)]
    pub fn load(
        assets: &Assets,
        objects: &[Value],
        properties: &Properties,
        size: [f32; 2],
        seed: u64,
    ) -> Result<Option<Self>> {
        Self::load_with_samples(assets, objects, properties, size, seed, None, None)
    }
    pub fn load_with_samples(
        assets: &Assets,
        objects: &[Value],
        properties: &Properties,
        size: [f32; 2],
        seed: u64,
        samples: Option<crate::animation::Samples>,
        storage: Option<ScriptStorage>,
    ) -> Result<Option<Self>> {
        let mut bindings = Vec::new();
        for (node, object) in objects.iter().enumerate() {
            collect(object, node, &mut Vec::new(), &mut bindings)?;
        }
        if bindings.is_empty() {
            return Ok(None);
        }
        ensure!(bindings.len() <= 512, "scene exceeds 512 property scripts");
        ensure!(
            bindings.iter().map(|b| b.source.len()).sum::<usize>() <= 8 * 1024 * 1024,
            "scene property scripts exceed 8 MiB"
        );
        let runtime = Runtime::new()?;
        runtime.set_memory_limit(MEMORY);
        runtime.set_max_stack_size(512 * 1024);
        runtime.set_gc_threshold(8 * 1024 * 1024);
        let deadline = Rc::new(Cell::new(Instant::now() + LOAD_BUDGET));
        let interrupt = deadline.clone();
        runtime.set_interrupt_handler(Some(Box::new(move || Instant::now() >= interrupt.get())));
        runtime.set_loader(
            ModuleResolver,
            AssetLoader {
                assets: assets.clone(),
                bytes: 0,
                modules: 0,
            },
        );
        let context = Context::full(&runtime)?;
        let logs = Rc::new(RefCell::new(VecDeque::new()));
        let log = logs.clone();
        let error_generation = Rc::new(Cell::new(0u64));
        let log_generation = error_generation.clone();
        let safe_assets = assets.clone();
        let texture_assets = assets.clone();
        let random = Rc::new(RefCell::new(ChaCha8Rng::seed_from_u64(seed)));
        let storage = Rc::new(RefCell::new(storage::Storage::new(
            assets.project(),
            storage,
        )));
        let samples = samples.unwrap_or_default();
        let dynamic_bindings = Rc::new(RefCell::new(Vec::new()));
        let model_data = Rc::new(RefCell::new(crate::model_data::Registry::default()));
        let poses = Rc::new(RefCell::new(HashMap::<usize, Vec<glam::Mat4>>::new()));
        let bone_poses = poses.clone();
        context.with(|ctx| -> Result<()> {
            ctx.globals().set(
                "__weBonePose",
                Func::from(move |node: usize, bone: usize| -> Option<Vec<f64>> {
                    bone_poses
                        .borrow()
                        .get(&node)
                        .and_then(|pose| pose.get(bone))
                        .map(|matrix| matrix.to_cols_array().into_iter().map(f64::from).collect())
                }),
            )?;
            crate::model_data::host::install(&ctx, assets, model_data.clone())?;
            creation::install(
                &ctx,
                assets,
                samples.clone(),
                (
                    bindings.len(),
                    bindings.iter().map(|b| b.source.len()).sum(),
                ),
                size,
                dynamic_bindings.clone(),
                model_data.clone(),
            )?;
            ctx.globals().set(
                "__weTextureInfo",
                Func::from(
                    move |ctx: Ctx<'_>, name: String| -> rquickjs::Result<String> {
                        let result = texture_assets
                            .texture_info(&name)
                            .map(|info| crate::scene::texture_metadata(&info));
                        result
                            .and_then(|v| Ok(serde_json::to_string(&v)?))
                            .map_err(|e| Exception::throw_type(&ctx, &format!("{e:#}")))
                    },
                ),
            )?;
            ctx.globals().set(
                "__weStorage",
                Func::from(
                    move |ctx: Ctx<'_>,
                          action: String,
                          key: String,
                          data: String,
                          location: String|
                          -> rquickjs::Result<Option<String>> {
                        storage
                            .borrow_mut()
                            .apply(&action, &key, &data, &location)
                            .map_err(|e| Exception::throw_type(&ctx, &format!("{e:#}")))
                    },
                ),
            )?;
            ctx.globals().set(
                "__weLog",
                Func::from(move |message: String| {
                    let message: String = message.chars().take(4096).collect();
                    let mut logs = log.borrow_mut();
                    let removed_error = logs.len() == 32
                        && logs
                            .front()
                            .is_some_and(|s: &String| !s.starts_with("console."));
                    if removed_error || !message.starts_with("console.") {
                        log_generation.set(log_generation.get().wrapping_add(1));
                    }
                    if logs.len() == 32 {
                        logs.pop_front();
                    }
                    logs.push_back(message);
                }),
            )?;
            ctx.globals().set(
                "__weValidateAsset",
                Func::from(move |ctx: Ctx<'_>, name: String| -> rquickjs::Result<()> {
                    safe_assets
                        .validate(&name)
                        .map_err(|e| Exception::throw_type(&ctx, &format!("{e:#}")))
                }),
            )?;
            ctx.globals().set(
                "__weRandom",
                Func::from(move || {
                    (random.borrow_mut().next_u64() >> 11) as f64 / ((1u64 << 53) as f64)
                }),
            )?;
            ctx.globals().set(
                "__weSampleAnimation",
                Func::from(
                    move |ctx: Ctx<'_>, index: usize, frame: f64| -> rquickjs::Result<String> {
                        let sample = samples
                            .sample(index, frame)
                            .map_err(|e| Exception::throw_type(&ctx, &format!("{e:#}")))?;
                        serde_json::to_string(&sample)
                            .map_err(|e| Exception::throw_type(&ctx, &e.to_string()))
                    },
                ),
            )?;
            checked(&ctx, ctx.eval::<(), _>(include_str!("script/vector.js")))?;
            ctx.globals()
                .set("__weMatrix", Func::from(matrix::calculate))?;
            checked(&ctx, ctx.eval::<(), _>(include_str!("script/matrix.js")))?;
            checked(&ctx, ctx.eval::<(), _>(include_str!("script/host.js")))?;
            checked(&ctx, ctx.eval::<(), _>(include_str!("script/model.js")))?;
            checked(
                &ctx,
                ctx.eval::<(), _>(include_str!("script/model_data.js")),
            )?;
            checked(
                &ctx,
                ctx.eval::<(), _>(include_str!("script/model_animation.js")),
            )?;
            checked(&ctx, ctx.eval::<(), _>(include_str!("script/material.js")))?;
            checked(&ctx, ctx.eval::<(), _>(include_str!("script/sound.js")))?;
            checked(&ctx, ctx.eval::<(), _>(include_str!("script/particle.js")))?;
            checked(&ctx, ctx.eval::<(), _>(include_str!("script/animation.js")))?;
            checked(&ctx, ctx.eval::<(), _>(include_str!("script/texture.js")))?;
            checked(&ctx, ctx.eval::<(), _>(include_str!("script/video.js")))?;
            checked(&ctx, ctx.eval::<(), _>(include_str!("script/storage.js")))?;
            checked(&ctx, ctx.eval::<(), _>(include_str!("script/creation.js")))?;
            checked(&ctx, ctx.eval::<(), _>("Math.random=__weRandom;"))?;
            let nodes = objects
                .iter()
                .map(|v| resolve(v, properties))
                .collect::<Result<Vec<_>>>()?;
            let initialize: Function = ctx.globals().get("__weInitNodes")?;
            checked(
                &ctx,
                initialize.call::<_, ()>((
                    to_js(&ctx, &nodes)?,
                    to_js(&ctx, &size)?,
                    to_js(&ctx, properties)?,
                )),
            )?;
            let initial: Function = ctx.globals().get("__weSetInitial")?;
            checked(&ctx, initial.call::<_, ()>((to_js(&ctx, objects)?,)))?;
            Ok(())
        })?;
        let mut scripts = Self {
            context,
            runtime,
            deadline,
            logs,
            error_generation,
            bindings,
            dynamic_bindings,
            model_data,
            poses,
            properties: properties.clone(),
            animated: false,
            pointer: false,
            audio: false,
            media_timeline: false,
            exhausted: false,
        };
        scripts.load_bindings()?;
        Ok(Some(scripts))
    }
    fn load_bindings(&mut self) -> Result<()> {
        self.deadline.set(Instant::now() + LOAD_BUDGET);
        self.context.with(|ctx| -> Result<()> {
            for (index,binding) in self.bindings.iter().enumerate() {
                let source = compat::lower(&binding.source).with_context(|| format!("SceneScript {} {:?}",binding.node,binding.path))?;
                let overrides = resolve(&binding.properties,&self.properties)?;
                let reserve: Function = ctx.globals().get("__weReserve")?;
                let slot: usize = checked(&ctx, reserve.call((binding.node, to_js(&ctx, &binding.path)?, to_js(&ctx, &overrides)?)))?;
                let module = checked(&ctx,Module::declare(ctx.clone(),format!("scripts/__property_{index}.js"),source))?;
                let (module,promise)=checked(&ctx,module.eval())?;
                // QuickJS jobs are also bounded; unresolved top-level awaits fail the candidate.
                checked(&ctx,promise.finish::<()>())?;
                let namespace=checked(&ctx,module.namespace())?;
                let attach:Function=ctx.globals().get("__weAttach")?;
                checked(&ctx,attach.call::<_,()>((namespace,binding.node,to_js(&ctx,&binding.path)?,to_js(&ctx,&overrides)?,slot)))?;
            }
            checked(&ctx,ctx.eval::<(),_>("__weEvent('applyGeneralSettings',{language:'en-us'});for(let i=0;i<__weScripts.length;i++)__weCall(i,'init');__weEvent('applyUserProperties',engine.userProperties);"))?;
            Ok(())
        })?;
        self.refresh_features()
    }
    fn refresh_features(&mut self) -> Result<()> {
        if self.exhausted {
            self.animated = false;
            self.pointer = false;
            self.audio = false;
            self.media_timeline = false;
            return Ok(());
        }
        let features = self.context.with(|ctx| -> Result<u32> {
            let f: Function = ctx.globals().get("__weFeatures")?;
            checked(&ctx, f.call(()))
        })?;
        self.animated = features & 1 != 0;
        self.audio = features & 2 != 0;
        self.media_timeline = features & 4 != 0;
        self.pointer = features & 8 != 0;
        Ok(())
    }
    pub fn flush(&self) -> Result<Value> {
        self.deadline.set(Instant::now() + FRAME_BUDGET);
        self.context.with(|ctx| {
            let function: Function = ctx.globals().get("__weFlush")?;
            let data: String = checked(&ctx, function.call(()))?;
            decode_patch(&data)
        })
    }
    pub fn sync_values(&self, values: &Value) -> Result<()> {
        self.deadline.set(Instant::now() + FRAME_BUDGET);
        self.context.with(|ctx| {
            let f: Function = ctx.globals().get("__weSyncValues")?;
            checked(&ctx, f.call::<_, ()>((to_js(&ctx, values)?,)))
        })
    }
    pub fn frame(&mut self, frame: &impl FrameData) -> Result<Value> {
        if self.exhausted {
            return Ok(json!([]));
        }
        self.deadline.set(Instant::now() + FRAME_BUDGET);
        let result = self.context.with(|ctx| {
            let function: Function = ctx.globals().get("__weFrame")?;
            let frame = frame.to_frame(&ctx)?;
            let data: String = checked(&ctx, function.call((frame,)))?;
            decode_patch(&data)
        });
        if let Err(error) = &result {
            self.exhausted = true;
            self.error_generation
                .set(self.error_generation.get().wrapping_add(1));
            self.logs
                .borrow_mut()
                .push_back(format!("SceneScript execution disabled: {error:#}"));
        }
        self.refresh_features()?;
        result
    }
    pub fn set_poses<'a>(
        &self,
        values: impl Iterator<Item = (usize, &'a [glam::Mat4])>,
        objects: &[Value],
    ) {
        let mut poses = self.poses.borrow_mut();
        poses.retain(|node, _| objects.get(*node).is_some_and(|o| o["__destroyed"] != true));
        for (node, local) in values {
            let pose = poses.entry(node).or_default();
            pose.clear();
            pose.extend_from_slice(local);
        }
    }
    pub fn set_clock(&self, time: f64) -> Result<()> {
        if self.exhausted {
            return Ok(());
        }
        ensure!(
            time.is_finite() && time >= 0.,
            "invalid SceneScript event time"
        );
        self.deadline.set(Instant::now() + FRAME_BUDGET);
        self.context.with(|ctx| {
            let function: Function = ctx.globals().get("__weClock")?;
            checked(&ctx, function.call::<_, ()>((time,)))
        })
    }
    pub fn set_properties(&mut self, values: &Properties) -> Result<Value> {
        if self.exhausted {
            return Ok(json!([]));
        }
        let mut changed = values
            .iter()
            .filter(|(key, value)| self.properties.get(*key) != Some(*value))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect::<Properties>();
        for key in self
            .properties
            .keys()
            .filter(|key| !values.contains_key(*key))
        {
            changed.insert(key.clone(), Value::Null);
        }
        let overrides = self
            .bindings
            .iter()
            .chain(self.dynamic_bindings.borrow().iter())
            .map(|b| Ok(json!({"node":b.node,"path":b.path,"properties":resolve(&b.properties, values)?})))
            .collect::<Result<Vec<_>>>()?;
        self.deadline.set(Instant::now() + FRAME_BUDGET);
        let patch = self.context.with(|ctx| {
            let function: Function = ctx.globals().get("__weProperties")?;
            let data: String = checked(
                &ctx,
                function.call((
                    to_js(&ctx, values)?,
                    to_js(&ctx, &changed)?,
                    to_js(&ctx, &overrides)?,
                )),
            )?;
            decode_patch(&data)
        });
        if let Err(error) = &patch {
            self.exhausted = true;
            self.error_generation
                .set(self.error_generation.get().wrapping_add(1));
            self.logs.borrow_mut().push_back(format!(
                "SceneScript property execution disabled: {error:#}"
            ));
        } else {
            self.properties = values.clone();
        }
        self.refresh_features()?;
        patch
    }
    pub fn sync_nodes(&self, nodes: &[Value]) -> Result<()> {
        if self.exhausted {
            return Ok(());
        }
        self.deadline.set(Instant::now() + FRAME_BUDGET);
        self.context.with(|ctx| {
            let function: Function = ctx.globals().get("__weSyncNodes")?;
            checked(&ctx, function.call::<_, ()>((to_js(&ctx, nodes)?,)))
        })
    }
    pub fn cancel_pointer(&self) -> Result<()> {
        if self.exhausted {
            return Ok(());
        }
        self.deadline.set(Instant::now() + FRAME_BUDGET);
        self.context.with(|ctx| {
            let function: Function = ctx.globals().get("__weCancelPointer")?;
            checked(&ctx, function.call::<_, ()>(()))
        })
    }
    pub fn event(&mut self, name: &str, value: &Value) -> Result<Value> {
        if self.exhausted {
            return Ok(json!([]));
        }
        self.deadline.set(Instant::now() + FRAME_BUDGET);
        let result = self.context.with(|ctx| {
            let event: Function = ctx.globals().get("__weEvent")?;
            checked(&ctx, event.call::<_, ()>((name, to_js(&ctx, value)?)))?;
            let flush: Function = ctx.globals().get("__weFlush")?;
            let data: String = checked(&ctx, flush.call(()))?;
            decode_patch(&data)
        });
        if result.is_err() {
            self.exhausted = true;
        }
        self.refresh_features()?;
        result
    }
    pub fn diagnostics(&self) -> Vec<String> {
        self.logs.borrow().iter().cloned().collect()
    }
    pub fn error_generation(&self) -> u64 {
        self.error_generation.get()
    }
}

impl Drop for Scripts {
    fn drop(&mut self) {
        self.deadline.set(Instant::now() + FRAME_BUDGET);
        self.context.with(|ctx| {
            let _: rquickjs::Result<()> = ctx.eval("__weShutdown();");
        });
        self.runtime.run_gc();
    }
}
fn checked<T>(ctx: &Ctx<'_>, result: rquickjs::Result<T>) -> Result<T> {
    result.map_err(|error| anyhow::anyhow!("{}", CaughtError::from_error(ctx, error)))
}
fn to_js<'js>(ctx: &Ctx<'js>, value: &(impl serde::Serialize + ?Sized)) -> Result<JsValue<'js>> {
    Ok(ctx.json_parse(serde_json::to_vec(value)?)?)
}
fn decode_patch(data: &str) -> Result<Value> {
    ensure!(
        data.len() <= 8 * 1024 * 1024,
        "SceneScript mutations exceed 8 MiB"
    );
    let patch: Value = serde_json::from_str(data)?;
    ensure!(
        patch.as_array().is_some_and(|v| v.len() <= 16384),
        "excessive SceneScript mutations"
    );
    Ok(patch)
}
fn collect(
    value: &Value,
    node: usize,
    path: &mut Vec<String>,
    bindings: &mut Vec<Binding>,
) -> Result<()> {
    match value {
        Value::Object(object) => {
            if let Some(source) = object.get("script").and_then(Value::as_str) {
                ensure!(
                    !path.is_empty() && object.contains_key("value"),
                    "script must be bound to a property"
                );
                ensure!(source.len() <= 1024 * 1024, "SceneScript exceeds 1 MiB");
                bindings.push(Binding {
                    node,
                    path: path.clone(),
                    source: source.into(),
                    properties: object
                        .get("scriptproperties")
                        .cloned()
                        .unwrap_or_else(|| json!({})),
                });
                return Ok(());
            }
            for (key, child) in object {
                path.push(key.clone());
                collect(child, node, path, bindings)?;
                path.pop();
            }
        }
        Value::Array(values) => {
            for (index, child) in values.iter().enumerate() {
                path.push(index.to_string());
                collect(child, node, path, bindings)?;
                path.pop();
            }
        }
        _ => {}
    }
    Ok(())
}
struct ModuleResolver;
impl Resolver for ModuleResolver {
    fn resolve<'js>(
        &mut self,
        _ctx: &Ctx<'js>,
        base: &str,
        name: &str,
        _attributes: Option<ImportAttributes<'js>>,
    ) -> rquickjs::Result<String> {
        if ["WEMath", "WEVector", "WEColor"].contains(&name) {
            return Ok(name.into());
        }
        let error = || {
            rquickjs::Error::new_resolving_message(base, name, "module escapes scene script assets")
        };
        if name.contains(':')
            || name.contains('\\')
            || name.contains('\0')
            || Path::new(name).is_absolute()
        {
            return Err(error());
        }
        let mut path = if name.starts_with('.') {
            Path::new(base)
                .parent()
                .unwrap_or(Path::new(""))
                .to_path_buf()
        } else {
            PathBuf::new()
        };
        for component in Path::new(name).components() {
            match component {
                Component::Normal(c) => path.push(c),
                Component::CurDir => {}
                Component::ParentDir => {
                    if !path.pop() {
                        return Err(error());
                    }
                }
                _ => return Err(error()),
            }
        }
        let name = path.to_str().ok_or_else(error)?;
        if !name.ends_with(".js") {
            return Err(error());
        }
        Ok(name.into())
    }
}
struct AssetLoader {
    assets: Assets,
    bytes: usize,
    modules: usize,
}
impl Loader for AssetLoader {
    fn load<'js>(
        &mut self,
        ctx: &Ctx<'js>,
        name: &str,
        _attributes: Option<ImportAttributes<'js>>,
    ) -> rquickjs::Result<Module<'js, Declared>> {
        let source = match name {
            "WEMath" => "export const {mix,smoothStep,deg2rad,rad2deg}=__weBuiltins.WEMath;".into(),
            "WEVector" => "export const {angleVector2,vectorAngle2}=__weBuiltins.WEVector;".into(),
            "WEColor" => {
                "export const {rgb2hsv,hsv2rgb,normalizeColor,expandColor}=__weBuiltins.WEColor;"
                    .into()
            }
            _ => {
                let error =
                    |e: anyhow::Error| rquickjs::Error::new_loading_message(name, format!("{e:#}"));
                let data = self.assets.read(name).map_err(error)?;
                self.bytes += data.len();
                self.modules += 1;
                if self.bytes > 8 * 1024 * 1024 || self.modules > 128 {
                    return Err(rquickjs::Error::new_loading_message(
                        name,
                        "module budget exceeded",
                    ));
                }
                let text = std::str::from_utf8(&data).map_err(|e| error(e.into()))?;
                compat::lower(text).map_err(error)?
            }
        };
        Module::declare(ctx.clone(), name, source)
    }
}
