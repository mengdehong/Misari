//! Authored property curves use a frozen relative base and independent channel timelines.
use crate::scene::bindings::components;
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{cell::RefCell, rc::Rc};

#[derive(Clone)]
struct Key {
    frame: f64,
    value: f64,
    front: [f64; 2],
    back: [f64; 2],
}
#[derive(Clone)]
struct Curve {
    node: usize,
    path: Vec<String>,
    channels: Vec<Vec<Key>>,
    base: Vec<f32>,
    relative: bool,
    boolean: bool,
    wrap: bool,
    parent: Option<usize>,
    fps: f64,
    length: f64,
    mode: String,
    events: Vec<(f64, String)>,
    clock: Clock,
}
#[derive(Default, Clone)]
struct Clock {
    frame: f64,
    time: Option<f64>,
    revision: u64,
    running: bool,
}
pub(crate) struct Animations {
    curves: Vec<Curve>,
    pub events: Vec<(usize, Value)>,
    pub samples: Samples,
}
#[derive(Clone, Default)]
pub(crate) struct Samples(Rc<RefCell<Vec<Curve>>>);
impl Samples {
    pub fn append(
        &self,
        object: &mut Value,
        node: usize,
        properties: &crate::scene::bindings::Properties,
        time: f64,
    ) -> Result<()> {
        let mut objects = vec![object.clone()];
        let mut animations = Animations::new(&mut objects)?;
        animations.properties(&objects, properties)?;
        let mut samples = self.0.borrow_mut();
        let offset = samples.len();
        ensure!(
            offset + animations.curves.len() <= 512
                && samples
                    .iter()
                    .chain(&animations.curves)
                    .flat_map(|c| &c.channels)
                    .map(Vec::len)
                    .sum::<usize>()
                    <= 200_000,
            "dynamic property animation budget exceeded"
        );
        for control in objects[0]["__animations"].as_array_mut().unwrap() {
            control["index"] = json!(control["index"].as_u64().unwrap() + offset as u64);
            if let Some(parent) = control["parent"].as_u64() {
                control["parent"] = json!(parent + offset as u64);
            }
            control["__frame"] = json!(0);
            control["__time"] = json!(time);
            control["__revision"] = json!(1);
        }
        for mut curve in animations.curves {
            curve.node = node;
            if let Some(parent) = &mut curve.parent {
                *parent += offset;
            }
            samples.push(curve);
        }
        *object = objects.remove(0);
        Ok(())
    }
    pub fn sample(&self, index: usize, frame: f64) -> Result<Value> {
        ensure!(
            frame.is_finite() && frame.abs() <= 1e12,
            "invalid animation frame"
        );
        let curves = self.0.borrow();
        let curve = curves.get(index).context("unknown property animation")?;
        curve.value(fold(frame, curve.length, &curve.mode))
    }
}
impl Curve {
    fn value(&self, frame: f64) -> Result<Value> {
        self.value_with_base(frame, &self.base)
    }
    fn value_with_base(&self, frame: f64, base: &[f32]) -> Result<Value> {
        let values = self
            .channels
            .iter()
            .enumerate()
            .map(|(i, keys)| {
                sample(
                    keys,
                    frame,
                    if self.wrap && self.mode == "loop" {
                        Some(self.length)
                    } else {
                        None
                    },
                ) as f32
                    + if self.relative { base[i] } else { 0.0 }
            })
            .collect::<Vec<_>>();
        ensure!(
            values.iter().all(|v| v.is_finite()),
            "property animation overflows"
        );
        Ok(if self.boolean {
            json!(values[0] >= 0.5)
        } else if values.len() == 1 {
            json!(values[0])
        } else {
            json!(values)
        })
    }
}
/// Camera clips use the same authored channels and tangent semantics as properties.
pub(crate) struct Channel(Curve);
impl Channel {
    pub fn new(value: &Value, options: &Value, count: usize) -> Result<Self> {
        let mut definition = if value.is_array() {
            json!({"c0":value})
        } else {
            value.clone()
        };
        ensure!(definition.is_object(), "invalid camera animation channel");
        definition["options"] = options.clone();
        let mut curves = Vec::new();
        collect(
            &json!({"value":vec![0.;count],"animation":definition}),
            0,
            &mut vec!["camera".into()],
            &mut curves,
        )?;
        let curve = curves.pop().context("missing camera animation channel")?;
        ensure!(
            curve.channels.len() == count,
            "camera channel component count"
        );
        Ok(Self(curve))
    }
    pub fn key_count(&self) -> usize {
        self.0.channels.iter().map(Vec::len).sum()
    }
    pub fn sample(&self, frame: f64, base: &[f32]) -> Result<Vec<f32>> {
        components(&self.0.value_with_base(frame, base)?, &[], base.len())
    }
}
impl Animations {
    pub fn new(objects: &mut [Value]) -> Result<Self> {
        let mut curves = Vec::new();
        for (node, object) in objects.iter().enumerate() {
            collect(object, node, &mut Vec::new(), &mut curves)?;
        }
        ensure!(
            curves
                .iter()
                .flat_map(|c| &c.channels)
                .map(Vec::len)
                .sum::<usize>()
                <= 200_000,
            "property animation key budget exceeded"
        );
        let mut parents = Vec::new();
        for curve in &curves {
            let parent =
                value_at(&objects[curve.node], &curve.path)?["animation"]["options"]["parent"]
                    .as_str();
            parents.push(parent.and_then(|name| {
                curves.iter().position(|candidate| {
                    candidate.node == curve.node
                        && candidate.path.len() == curve.path.len()
                        && candidate.path[..candidate.path.len() - 1]
                            == curve.path[..curve.path.len() - 1]
                        && candidate.path.last().is_some_and(|p| p == name)
                })
            }));
        }
        for index in 0..curves.len() {
            let mut current = parents[index];
            let mut depth = 0;
            while let Some(i) = current {
                depth += 1;
                ensure!(depth < curves.len(), "property animation parent cycle");
                current = parents[i];
            }
            curves[index].parent = parents[index];
        }
        for object in objects.iter_mut() {
            object["__animations"] = json!([]);
        }
        for (index, curve) in curves.iter().enumerate() {
            let authored =
                value_at(&objects[curve.node], &curve.path)?["animation"]["options"].clone();
            let info = json!({"index":index,"path":curve.path,"parent":curve.parent,"name":authored["name"].as_str().unwrap_or(""),"fps":curve.fps,"frameCount":curve.length,"duration":curve.length/curve.fps,"mode":curve.mode,"rate":1,"paused":authored["startpaused"].as_bool().unwrap_or(false),"playing":true,"__revision":0,"__current":0,"__time":0});
            objects[curve.node]["__animations"]
                .as_array_mut()
                .unwrap()
                .push(info);
        }
        let samples = Samples(Rc::new(RefCell::new(curves.clone())));
        Ok(Self {
            curves,
            events: Vec::new(),
            samples,
        })
    }
    pub fn active(&self) -> bool {
        self.curves
            .iter()
            .any(|curve| curve.parent.is_none() && curve.clock.running)
    }
    pub fn present(&self) -> bool {
        !self.curves.is_empty()
    }
    pub fn properties(
        &mut self,
        authored: &[Value],
        properties: &crate::scene::bindings::Properties,
    ) -> Result<()> {
        self.import_samples();
        let bases = self
            .curves
            .iter()
            .map(|curve| {
                if authored
                    .get(curve.node)
                    .is_none_or(|object| object["__destroyed"] == true)
                {
                    return Ok(curve.base.clone());
                }
                components(
                    &crate::scene::bindings::resolve(
                        value_at(&authored[curve.node], &curve.path)?,
                        properties,
                    )?,
                    &[],
                    curve.channels.len(),
                )
            })
            .collect::<Result<Vec<_>>>()?;
        let mut samples = self.samples.0.borrow_mut();
        for ((curve, sample), base) in self.curves.iter_mut().zip(samples.iter_mut()).zip(bases) {
            curve.base = base.clone();
            sample.base = base;
        }
        Ok(())
    }
    pub fn advance(&mut self, time: f64, objects: &mut [Value]) -> Result<Vec<Value>> {
        self.import_samples();
        self.events.clear();
        let mut frames = vec![0.0; self.curves.len()];
        for (index, curve) in self.curves.iter_mut().enumerate() {
            if objects
                .get(curve.node)
                .is_none_or(|object| object["__destroyed"] == true)
            {
                curve.clock.running = false;
                continue;
            }
            let ctrl = objects[curve.node]["__animations"]
                .as_array()
                .context("missing property animation controls")?
                .iter()
                .find(|c| c["index"].as_u64() == Some(index as u64))
                .context("missing animation handle")?;
            let rate = components(&ctrl["rate"], &[1.0], 1)?[0] as f64;
            ensure!(rate.abs() <= 128.0, "invalid property animation rate");
            let paused = ctrl["paused"].as_bool().unwrap_or(false)
                || ctrl["playing"].as_bool() == Some(false);
            let revision = ctrl["__revision"].as_u64().unwrap_or(0);
            let rewind = curve.clock.time.is_some_and(|last| time < last);
            let mut previous = if curve.clock.time.is_none() || rewind {
                -0.5
            } else {
                curve.clock.frame
            };
            let seek = revision != curve.clock.revision;
            if seek {
                curve.clock.frame = ctrl["__frame"].as_f64().unwrap_or(0.0);
                ensure!(
                    curve.clock.frame.is_finite() && curve.clock.frame.abs() <= 1e12,
                    "invalid property animation frame"
                );
                curve.clock.revision = revision;
                previous = curve.clock.frame;
                if !paused {
                    curve.clock.frame += (time - ctrl["__time"].as_f64().unwrap_or(time)).max(0.0)
                        * curve.fps
                        * rate;
                }
            } else if curve.clock.time.is_none() || rewind {
                curve.clock.frame = if paused { 0.0 } else { time * curve.fps * rate };
            } else if !paused {
                curve.clock.frame += (time - curve.clock.time.unwrap()).max(0.0) * curve.fps * rate;
            }
            curve.clock.time = Some(time);
            if !paused && curve.parent.is_none() {
                for (frame, name) in &curve.events {
                    if crossed(
                        previous,
                        curve.clock.frame,
                        *frame,
                        curve.length,
                        &curve.mode,
                    ) {
                        self.events.push((
                            curve.node,
                            json!({"name":name,"frame":frame,"animation":ctrl["name"]}),
                        ));
                    }
                }
            }
            if curve.mode == "single" {
                curve.clock.frame = curve.clock.frame.clamp(0.0, curve.length);
            }
            curve.clock.running = !paused
                && rate != 0.0
                && (curve.mode != "single"
                    || (rate > 0.0 && curve.clock.frame < curve.length)
                    || (rate < 0.0 && curve.clock.frame > 0.0));
            frames[index] = curve.clock.frame;
        }
        let mut patches = Vec::new();
        for (index, curve) in self.curves.iter().enumerate() {
            if objects
                .get(curve.node)
                .is_none_or(|object| object["__destroyed"] == true)
            {
                continue;
            }
            let mut owner = index;
            while let Some(parent) = self.curves[owner].parent {
                owner = parent;
            }
            let frame = fold(frames[owner], curve.length, &curve.mode);
            let value = curve.value(frame)?;
            if value_at(&objects[curve.node], &curve.path)? != &value {
                crate::scene::runtime::set_value(
                    &mut objects[curve.node],
                    &curve.path,
                    value.clone(),
                )?;
                patches.push(json!([curve.node, curve.path, value]));
            }
            let ctrls = objects[curve.node]["__animations"].as_array_mut().unwrap();
            let local = ctrls
                .iter()
                .position(|c| c["index"].as_u64() == Some(index as u64))
                .unwrap();
            ctrls[local]["__current"] = json!(frame);
            ctrls[local]["__time"] = json!(time);
            patches.push(json!([
                curve.node,
                ["__animations", local.to_string(), "__current"],
                frame
            ]));
            patches.push(json!([
                curve.node,
                ["__animations", local.to_string(), "__time"],
                time
            ]));
        }
        Ok(patches)
    }
    fn import_samples(&mut self) {
        let samples = self.samples.0.borrow();
        self.curves.extend_from_slice(&samples[self.curves.len()..]);
    }
}

fn collect(
    value: &Value,
    node: usize,
    path: &mut Vec<String>,
    curves: &mut Vec<Curve>,
) -> Result<()> {
    if let Some(animation) = value.get("animation").filter(|a| a.is_object()) {
        ensure!(
            curves.len() < 512 && !path.is_empty() && path.len() <= 32,
            "property animation budget exceeded"
        );
        let opts = &animation["options"];
        let fps = opts["fps"].as_f64().unwrap_or(30.0);
        let length = opts["length"].as_f64().unwrap_or(1.0);
        ensure!(
            fps.is_finite()
                && fps > 0.0
                && fps <= 1000.0
                && length.is_finite()
                && (0.0..=200_000.0).contains(&length),
            "invalid property animation duration"
        );
        let mode = opts["mode"].as_str().unwrap_or("loop").to_owned();
        ensure!(
            ["loop", "mirror", "single"].contains(&mode.as_str()),
            "unknown property animation mode {mode}"
        );
        let mut channels = Vec::new();
        for index in 0..16 {
            let Some(channel) = animation.get(format!("c{index}")) else {
                break;
            };
            let keys = channel
                .as_array()
                .context("animation channel is not an array")?;
            ensure!(
                !keys.is_empty() && keys.len() <= 200_001,
                "invalid property animation key count"
            );
            let mut out = Vec::with_capacity(keys.len());
            let mut previous = -1.0;
            for key in keys {
                let frame = key["frame"].as_f64().context("animation key frame")?;
                let v = key["value"].as_f64().context("animation key value")?;
                ensure!(
                    frame.is_finite()
                        && frame > previous
                        && frame >= 0.0
                        && frame <= length
                        && v.is_finite(),
                    "invalid/non-increasing animation key"
                );
                previous = frame;
                let handle = |side: &str| -> Result<[f64; 2]> {
                    let h = &key[side];
                    let x = h["x"].as_f64().unwrap_or(1.0).abs();
                    let y = h["y"].as_f64().unwrap_or(0.0);
                    ensure!(x.is_finite() && y.is_finite(), "invalid animation tangent");
                    Ok(if h["enabled"].as_bool().unwrap_or(false) {
                        [x.min(1.5), y]
                    } else {
                        [f64::NAN, 0.0]
                    })
                };
                out.push(Key {
                    frame,
                    value: v,
                    front: handle("front")?,
                    back: handle("back")?,
                });
            }
            channels.push(out);
        }
        ensure!(!channels.is_empty(), "property animation has no channels");
        let base = components(&value["value"], &vec![0.0; channels.len()], channels.len())?;
        let mut events = Vec::new();
        if let Some(values) = opts["events"].as_array() {
            ensure!(values.len() <= 4096, "too many animation events");
            for event in values {
                let frame = event["frame"].as_f64().context("animation event frame")?;
                let name = event["name"].as_str().context("animation event name")?;
                ensure!(
                    frame.is_finite() && frame >= 0.0 && frame <= length && name.len() <= 4096,
                    "invalid animation event"
                );
                events.push((frame, name.into()));
            }
        }
        curves.push(Curve {
            node,
            path: path.clone(),
            channels,
            base,
            relative: animation["relative"].as_bool().unwrap_or(false),
            boolean: value["value"].is_boolean(),
            wrap: animation["wraploop"].as_bool().unwrap_or(false),
            parent: None,
            fps,
            length,
            mode,
            events,
            clock: Clock {
                running: !opts["startpaused"].as_bool().unwrap_or(false),
                ..Clock::default()
            },
        });
        return Ok(());
    }
    match value {
        Value::Object(values) => {
            for (key, value) in values {
                if [
                    "__model",
                    "__animations",
                    "__boneOverrides",
                    "__boneOverrideTime",
                    "__bonePhysics",
                ]
                .contains(&key.as_str())
                {
                    continue;
                }
                path.push(key.clone());
                collect(value, node, path, curves)?;
                path.pop();
            }
        }
        Value::Array(values) => {
            for (index, value) in values.iter().enumerate() {
                path.push(index.to_string());
                collect(value, node, path, curves)?;
                path.pop();
            }
        }
        _ => {}
    }
    Ok(())
}
fn value_at<'a>(value: &'a Value, path: &[String]) -> Result<&'a Value> {
    let mut value = value;
    for key in path {
        value = match value {
            Value::Array(values) => values.get(key.parse::<usize>()?),
            Value::Object(values) => values.get(key),
            _ => None,
        }
        .context("animation property no longer exists")?;
    }
    Ok(value)
}
fn fold(frame: f64, length: f64, mode: &str) -> f64 {
    if length == 0.0 {
        return 0.0;
    }
    match mode {
        "loop" => frame.rem_euclid(length),
        "mirror" => {
            let x = frame.rem_euclid(length * 2.0);
            if x > length { 2.0 * length - x } else { x }
        }
        _ => frame.clamp(0.0, length),
    }
}
fn bezier(p: [f64; 4], t: f64) -> f64 {
    let u = 1.0 - t;
    u * u * u * p[0] + 3.0 * u * u * t * p[1] + 3.0 * u * t * t * p[2] + t * t * t * p[3]
}
fn between(a: &Key, b: &Key, frame: f64) -> f64 {
    let span = b.frame - a.frame;
    if span <= 0.0 {
        return b.value;
    }
    let x = [
        a.frame,
        a.frame
            + if a.front[0].is_nan() {
                span / 3.0
            } else {
                a.front[0] * span / 3.0
            },
        b.frame
            - if b.back[0].is_nan() {
                span / 3.0
            } else {
                b.back[0] * span / 3.0
            },
        b.frame,
    ];
    let y = [
        a.value,
        if a.front[0].is_nan() {
            a.value + (b.value - a.value) / 3.0
        } else {
            a.value + a.front[1]
        },
        if b.back[0].is_nan() {
            b.value - (b.value - a.value) / 3.0
        } else {
            b.value + b.back[1]
        },
        b.value,
    ];
    let (mut lo, mut hi) = (0.0, 1.0);
    for _ in 0..32 {
        let t = (lo + hi) / 2.0;
        if bezier(x, t) < frame {
            lo = t;
        } else {
            hi = t;
        }
    }
    bezier(y, (lo + hi) / 2.0)
}
fn sample(keys: &[Key], frame: f64, wrap: Option<f64>) -> f64 {
    let last = keys.last().unwrap();
    if frame <= keys[0].frame {
        return keys[0].value;
    }
    if frame >= last.frame {
        if let Some(length) = wrap.filter(|l| *l > last.frame)
            && frame < length
        {
            let end = Key {
                frame: length,
                value: keys[0].value,
                front: keys[0].front,
                back: keys[0].back,
            };
            return between(last, &end, frame);
        }
        return last.value;
    }
    let next = keys.partition_point(|k| k.frame <= frame);
    between(&keys[next - 1], &keys[next], frame)
}
fn crossed(from: f64, to: f64, event: f64, length: f64, mode: &str) -> bool {
    if from == to {
        return false;
    }
    if mode == "single" || length == 0.0 {
        return if to > from {
            from < event && to >= event
        } else {
            to <= event && from > event
        };
    }
    let lane = |event: f64, period: f64| {
        if to > from {
            (to - event).div_euclid(period) > (from - event).div_euclid(period)
        } else {
            ((from - event) / period).ceil() > ((to - event) / period).ceil()
        }
    };
    if mode == "loop" {
        lane(event, length)
    } else {
        lane(event, 2.0 * length) || lane(2.0 * length - event, 2.0 * length)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::bindings::{Properties, resolve};
    #[test]
    #[ignore = "requires extracted real WE scene assets"]
    fn real_scene_timelines_parse_and_sample() {
        let root = std::env::var("WE_TIMELINE_ASSETS").expect("WE_TIMELINE_ASSETS scene directory");
        let mut scenes = 0;
        let mut total = 0;
        let mut failures = Vec::new();
        for path in std::fs::read_dir(root)
            .unwrap()
            .map(|p| p.unwrap().path())
            .map(|p| if p.is_dir() { p.join("scene.json") } else { p })
            .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "json"))
        {
            let value: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            let mut objects = value["objects"].as_array().unwrap().clone();
            objects.push(value["general"].clone());
            let mut animations = match Animations::new(&mut objects) {
                Ok(a) => a,
                Err(e) => {
                    failures.push(format!("{}: {e:#}", path.display()));
                    continue;
                }
            };
            total += animations.curves.len();
            scenes += 1;
            let mut objects = objects
                .iter()
                .map(|v| resolve(v, &Properties::new()).unwrap())
                .collect::<Vec<_>>();
            for time in [0., 0.25, 1., 10., 30.] {
                if let Err(e) = animations.advance(time, &mut objects) {
                    failures.push(format!("{} at {time}: {e:#}", path.display()));
                    break;
                }
            }
        }
        eprintln!("{scenes} real scenes / {total} property timelines");
        assert!(
            scenes > 0 && total > 0,
            "no real scene timelines were exercised"
        );
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }
    fn curve(relative: bool, mode: &str) -> Value {
        json!({"value":"10 20","animation":{"relative":relative,"c0":[{"frame":0,"value":0},{"frame":2,"value":8}],"c1":[{"frame":0,"value":0},{"frame":1,"value":2},{"frame":2,"value":4}],"options":{"fps":2,"length":2,"mode":mode,"name":"move","events":[{"frame":0,"name":"begin"},{"frame":2,"name":"end"}]}}})
    }
    #[test]
    fn independent_channels_relative_base_pause_seek_rate_rewind_and_events() {
        let mut authored = vec![json!({"origin":curve(true,"single")})];
        let mut animations = Animations::new(&mut authored).unwrap();
        let mut nodes = authored
            .iter()
            .map(|v| resolve(v, &Properties::new()).unwrap())
            .collect::<Vec<_>>();
        animations.advance(0., &mut nodes).unwrap();
        assert_eq!(nodes[0]["origin"], json!([10., 20.]));
        assert_eq!(animations.events[0].1["name"], json!("begin"));
        animations.advance(0.25, &mut nodes).unwrap();
        assert_eq!(nodes[0]["origin"], json!([12., 21.]));
        nodes[0]["__animations"][0]["paused"] = json!(true);
        animations.advance(1., &mut nodes).unwrap();
        assert_eq!(nodes[0]["origin"], json!([12., 21.]));
        assert!(!animations.active());
        nodes[0]["__animations"][0]["__revision"] = json!(1);
        nodes[0]["__animations"][0]["__frame"] = json!(1.5);
        animations.advance(1., &mut nodes).unwrap();
        assert_eq!(nodes[0]["origin"], json!([16., 23.]));
        assert!(animations.events.is_empty());
        nodes[0]["__animations"][0]["paused"] = json!(false);
        nodes[0]["__animations"][0]["rate"] = json!(2);
        animations.advance(1.125, &mut nodes).unwrap();
        assert_eq!(nodes[0]["origin"], json!([18., 24.]));
        assert!(!animations.active());
        assert_eq!(animations.events[0].1["name"], json!("end"));
        animations.advance(0., &mut nodes).unwrap();
        assert_eq!(nodes[0]["origin"], json!([10., 20.]));
    }
    #[test]
    fn bezier_handles_wrap_linked_timelines_and_invalid_input() {
        let keys = [
            Key {
                frame: 0.,
                value: 0.,
                front: [1., 0.],
                back: [1., 0.],
            },
            Key {
                frame: 1.,
                value: 1.,
                front: [1., 0.],
                back: [1., 0.],
            },
        ];
        assert!((sample(&keys, 0.25, None) - 0.15625).abs() < 1e-7);
        assert!((sample(&keys, 1.5, Some(2.)) - 0.5).abs() < 1e-7);
        assert_eq!(fold(3., 2., "mirror"), 1.);
        assert_eq!(fold(-0.5, 2., "loop"), 1.5);
        assert!(crossed(19., 23., 1., 20., "loop"));
        assert!(crossed(3., 0., 1., 2., "mirror"));
        let mut a = curve(false, "loop");
        a["animation"]["options"]["startpaused"] = json!(true);
        let mut b = curve(false, "loop");
        b["animation"]["options"]["parent"] = json!("origin");
        let mut nodes = vec![json!({"origin":a,"scale":b})];
        let mut animations = Animations::new(&mut nodes).unwrap();
        nodes[0] = resolve(&nodes[0], &Properties::new()).unwrap();
        animations.advance(1., &mut nodes).unwrap();
        assert_eq!(nodes[0]["origin"], nodes[0]["scale"]);
        assert!(!animations.active());
        let mut bad = curve(false, "single");
        bad["animation"]["c0"][1]["frame"] = json!(0);
        assert!(Animations::new(&mut [json!({"origin":bad})]).is_err());
        let mut a = curve(false, "loop");
        let mut b = a.clone();
        a["animation"]["options"]["parent"] = json!("scale");
        b["animation"]["options"]["parent"] = json!("origin");
        assert!(Animations::new(&mut [json!({"origin":a,"scale":b})]).is_err());
    }
}
