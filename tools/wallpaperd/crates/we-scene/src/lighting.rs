//! WE's fixed four-light uniforms use scene world coordinates, including parents.
use crate::{
    scene::bindings::components,
    scene::{Node, State},
};
use anyhow::{Result, ensure};
use glam::Vec3;
use serde_json::Value;
mod modern {
    //! LightingV1 keeps modern light slots separate from legacy inverse-square lights.
    use super::*;
    use glam::Vec3;

    pub(crate) const PER_KIND: usize = 15;
    pub(crate) const LIGHTS: usize = PER_KIND * 4;
    pub(crate) struct Modern {
        pub origin: [f32; LIGHTS * 4],
        pub color: [f32; LIGHTS * 4],
        pub direction: [f32; LIGHTS * 4],
        pub extra: [f32; LIGHTS * 4],
        pub count: usize,
        pub casts: [bool; LIGHTS],
    }
    impl Default for Modern {
        fn default() -> Self {
            Self {
                origin: [0.; LIGHTS * 4],
                color: [0.; LIGHTS * 4],
                direction: [0.; LIGHTS * 4],
                extra: [0.; LIGHTS * 4],
                count: 0,
                casts: [false; LIGHTS],
            }
        }
    }
    pub(crate) fn capacities(config: &Value) -> Result<[usize; 4]> {
        ensure!(
            config.is_null() || config.is_object(),
            "invalid scene light configuration"
        );
        let mut counts = [0; 4];
        for (i, name) in ["point", "spot", "tube", "directional"].iter().enumerate() {
            counts[i] = match config.get(*name) {
                None => 0,
                Some(v) => v
                    .as_u64()
                    .filter(|v| *v <= PER_KIND as u64)
                    .ok_or_else(|| anyhow::anyhow!("invalid scene {name} light capacity"))?
                    as usize,
            };
            if *name != "tube"
                && let Some(value) = config.get(format!("{name}shadow"))
            {
                ensure!(
                    value.as_u64().is_some_and(|n| n <= PER_KIND as u64),
                    "invalid scene {name} shadow light capacity"
                );
            }
        }
        Ok(counts)
    }
    impl Modern {
        pub fn collect(nodes: &[Node], states: &[State], capacities: [usize; 4]) -> Result<Self> {
            let mut result = Self::default();
            let mut counts = [0; 4];
            for (node, state) in nodes.iter().zip(states) {
                let Some(light) = node.light else { continue };
                let Some(kind) = light.kind.channel() else {
                    continue;
                };
                if counts[kind] >= capacities[kind] {
                    continue;
                }
                counts[kind] += 1;
                let slot = result.count;
                result.count += 1;
                if !state.visible {
                    continue;
                }
                let radius = light.radius;
                let exponent = light.exponent;
                if kind != 3 && radius == 0. {
                    continue;
                }
                let color = state.tint * light.intensity;
                ensure!(color.is_finite(), "modern light color overflows");
                let origin = state.transform.transform_point3(Vec3::ZERO);
                let direction = state
                    .transform
                    .transform_vector3(-Vec3::Z)
                    .normalize_or(-Vec3::Z);
                let (origin, extra) = if kind == 2 {
                    (
                        state.transform.transform_point3(light.endpoints[0]),
                        state.transform.transform_point3(light.endpoints[1]),
                    )
                } else if kind == 1 {
                    // Degrees are normalized to cone cosines at the input boundary.
                    (origin, Vec3::new(light.cones[0], light.cones[1], 0.))
                } else {
                    (origin, Vec3::ZERO)
                };
                result.origin[slot * 4..slot * 4 + 4]
                    .copy_from_slice(&origin.extend(exponent).to_array());
                result.color[slot * 4..slot * 4 + 4]
                    .copy_from_slice(&color.extend(radius).to_array());
                result.direction[slot * 4..slot * 4 + 4]
                    .copy_from_slice(&direction.extend(kind as f32).to_array());
                result.extra[slot * 4..slot * 4 + 4].copy_from_slice(&extra.extend(0.).to_array());
                // WE models cast by default; lights require an explicit switch. Tube
                // shadows are not part of LightingV1's documented feature set.
                result.casts[slot] = kind != 2 && light.casts;
            }
            Ok(result)
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::{scene::hierarchy::Hierarchy, scene::local_state};
        use serde_json::json;
        fn collect(objects: &[Value], states: &[State], config: &Value) -> Modern {
            let nodes = objects
                .iter()
                .map(|o| Node::decode(o, [32.; 2]).unwrap())
                .collect::<Vec<_>>();
            Modern::collect(&nodes, states, capacities(config).unwrap()).unwrap()
        }
        #[test]
        fn script_cast_defaults_and_tube_vectors_are_mutable() {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("scene.pkg");
            let mut pkg = 8u32.to_le_bytes().to_vec();
            pkg.extend(b"PKGV0001");
            pkg.extend(0u32.to_le_bytes());
            std::fs::write(&path, pkg).unwrap();
            let assets = crate::assets::Assets::open(root.path(), &path, None).unwrap();
            let objects = [
                json!({"id":1,"light":"ltube","point0":[-2,0,0],"point1":[2,0,0],"origin":{"value":[0,0,0],"script":"export function init(v){if(thisLayer.castshadow!==false||thisScene.getLayerByID(2).castshadow!==true)throw Error('Cast shadow defaults');thisLayer.point0.x=-4;thisLayer.point1=new Vec3(7,0,0);thisLayer.castshadow=true;return v;}"}}),
                json!({"id":2,"model":"not-loaded.mdl"}),
            ];
            let scripts = crate::script::Scripts::load(
                &assets,
                &objects,
                &crate::Properties::new(),
                [32.; 2],
                0,
            )
            .unwrap()
            .unwrap();
            assert!(
                scripts.diagnostics().is_empty(),
                "{:?}",
                scripts.diagnostics()
            );
            let patch = scripts.flush().unwrap();
            let values = patch.as_array().unwrap();
            assert!(
                values
                    .iter()
                    .any(|v| v[1] == json!(["point0"]) && v[2] == json!([-4, 0, 0]))
            );
            assert!(
                values
                    .iter()
                    .any(|v| v[1] == json!(["point1"]) && v[2] == json!([7, 0, 0]))
            );
            assert!(
                values
                    .iter()
                    .any(|v| v[1] == json!(["castshadow"]) && v[2] == true)
            );
        }
        #[test]
        fn shadow_switches_spot_defaults_and_tube_endpoints() {
            let objects = vec![
                json!({"light":"lpoint"}),
                json!({"light":"lspot","castshadow":true}),
                json!({"light":"ltube","castshadow":true,"origin":"0 0 0"}),
            ];
            let states = objects
                .iter()
                .map(|o| local_state(o, [32.; 2], false).unwrap())
                .collect::<Vec<_>>();
            let lights = collect(&objects, &states, &json!({"point":1,"spot":1,"tube":1}));
            assert_eq!(&lights.casts[..3], &[false, true, false]);
            assert_eq!(lights.extra[4], 2f32.to_radians().cos());
            assert_eq!(lights.extra[5], 20f32.to_radians().cos());
            assert_eq!(&lights.origin[8..11], &[-50., 0., 0.]);
            assert_eq!(&lights.extra[8..11], &[50., 0., 0.]);
        }
        #[test]
        fn capacities_visibility_parent_transform_and_modern_channel_are_independent() {
            let objects = vec![
                json!({"id":1,"origin":"4 5 6","angles":"0 0 1.57079632679"}),
                json!({"id":2,"parent":1,"light":"lpoint","origin":"2 0 0","color":"1 0.5 0","intensity":2,"radius":20,"exponent":3}),
                json!({"id":3,"light":"point"}),
                json!({"id":4,"light":"lpoint","visible":false}),
                json!({"id":5,"light":"lpoint","color":"0 1 0"}),
                json!({"id":6,"light":"ltube","point0":"-2 0 0","point1":"2 0 0","origin":"8 0 0"}),
                json!({"id":7,"light":"ldirectional","origin":"100 100 100"}),
            ];
            let mut states = objects
                .iter()
                .map(|o| local_state(o, [32.; 2], !o["parent"].is_null()).unwrap())
                .collect::<Vec<_>>();
            Hierarchy::new(&objects)
                .unwrap()
                .compose(&mut states, &Default::default())
                .unwrap();
            assert_eq!(collect(&objects, &states, &Value::Null).count, 0);
            let lights = collect(
                &objects,
                &states,
                &json!({"point":2,"tube":1,"directional":1}),
            );
            assert_eq!(lights.count, 4);
            assert!(
                (Vec3::from_slice(&lights.origin[..3]) - Vec3::new(4., 7., 6.)).length() < 1e-5
            );
            assert_eq!(&lights.origin[3..4], &[3.]);
            assert_eq!(&lights.color[..4], &[2., 1., 0., 20.]);
            assert_eq!(&lights.color[4..8], &[0.; 4]);
            assert_eq!(&lights.origin[8..11], &[6., 0., 0.]);
            assert_eq!(&lights.extra[8..11], &[10., 0., 0.]);
            assert_eq!(lights.direction[15], 3.);
            assert!(capacities(&json!({"point":16})).is_err());
            assert!(capacities(&json!({"point":1.5})).is_err());
        }
    }
}
pub(crate) mod shadow;
pub(crate) use modern::{LIGHTS as MODERN_LIGHTS, capacities as validate_config};

pub(crate) const LEGACY_LIGHTS: usize = 4;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    LegacyPoint,
    Point,
    Spot,
    Tube,
    Directional,
}
impl Kind {
    fn channel(self) -> Option<usize> {
        match self {
            Self::LegacyPoint => None,
            Self::Point => Some(0),
            Self::Spot => Some(1),
            Self::Tube => Some(2),
            Self::Directional => Some(3),
        }
    }
}
#[derive(Clone, Copy)]
pub(crate) struct Light {
    pub kind: Kind,
    intensity: f32,
    radius: f32,
    exponent: f32,
    endpoints: [Vec3; 2],
    cones: [f32; 2],
    casts: bool,
}
impl Light {
    /// Dynamic JSON is decoded once when effective values change, never while drawing.
    pub fn decode(object: &Value) -> Result<Option<Self>> {
        validate(object)?;
        let kind = match object["light"].as_str() {
            Some("point") => Kind::LegacyPoint,
            Some("lpoint") => Kind::Point,
            Some("lspot") => Kind::Spot,
            Some("ltube") => Kind::Tube,
            Some("ldirectional") => Kind::Directional,
            _ => return Ok(None),
        };
        let scalar = |name: &str, default| -> Result<f32> {
            Ok(components(&object[name], &[default], 1)?[0])
        };
        let exponent = if kind == Kind::LegacyPoint {
            2.
        } else {
            scalar("exponent", 2.)?
        };
        ensure!(
            (0. ..=128.).contains(&exponent),
            "invalid light falloff exponent"
        );
        let mut endpoints = [-Vec3::X * 50., Vec3::X * 50.];
        if kind == Kind::Tube {
            for (endpoint, name) in endpoints.iter_mut().zip(["point0", "point1"]) {
                *endpoint = Vec3::from_slice(&components(&object[name], &endpoint.to_array(), 3)?);
            }
        }
        let mut cones = [2., 20.];
        if kind == Kind::Spot {
            cones = [scalar("innercone", 2.)?, scalar("outercone", 20.)?];
            ensure!(
                (0. ..=180.).contains(&cones[0]) && (cones[0]..=180.).contains(&cones[1]),
                "invalid spot light cones"
            );
        }
        Ok(Some(Self {
            kind,
            intensity: scalar("intensity", 1.)?,
            radius: scalar("radius", 1000.)?,
            exponent,
            endpoints,
            cones: cones.map(|angle| angle.to_radians().cos()),
            casts: object["castshadow"] == true,
        }))
    }
}
pub(crate) struct Snapshot {
    pub position: [f32; LEGACY_LIGHTS * 3],
    pub color_radius: [f32; LEGACY_LIGHTS * 4],
    pub premultiplied: [f32; LEGACY_LIGHTS * 3],
    pub modern: modern::Modern,
    pub shadows: Option<std::rc::Rc<shadow::Target>>,
}
impl Default for Snapshot {
    fn default() -> Self {
        let mut result = Self {
            position: [0.; 12],
            color_radius: [0.; 16],
            premultiplied: [0.; 12],
            modern: Default::default(),
            shadows: None,
        };
        for i in 0..LEGACY_LIGHTS {
            // Disabled slots must remain finite inside shaders that normalize
            // the direction or divide by radius even when the color is zero.
            result.position[i * 3 + 2] = 1e8;
            result.color_radius[i * 4 + 3] = 1.;
        }
        result
    }
}
pub(crate) fn validate(object: &Value) -> Result<()> {
    if object["light"].is_null() {
        return Ok(());
    }
    let intensity = components(&object["intensity"], &[1.], 1)?[0];
    let radius = components(&object["radius"], &[1000.], 1)?[0];
    ensure!(
        object["castshadow"].is_null() || object["castshadow"].is_boolean(),
        "light Cast shadow must be boolean"
    );
    ensure!(
        (0. ..=1e6).contains(&intensity) && (0. ..=1e6).contains(&radius),
        "invalid light intensity/radius"
    );
    Ok(())
}
impl Snapshot {
    pub fn collect(nodes: &[Node], states: &[State], capacities: [usize; 4]) -> Result<Self> {
        let mut result = Self {
            modern: modern::Modern::collect(nodes, states, capacities)?,
            ..Self::default()
        };
        let mut slot = 0;
        for (node, state) in nodes.iter().zip(states) {
            let Some(light) = node.light.filter(|light| light.kind == Kind::LegacyPoint) else {
                continue;
            };
            if slot >= LEGACY_LIGHTS {
                break;
            }
            let current = slot;
            slot += 1;
            let radius = light.radius;
            let color = state.tint * light.intensity;
            let premultiplied = color * radius * radius;
            ensure!(
                color.is_finite() && premultiplied.is_finite(),
                "light color overflows"
            );
            if !state.visible || radius == 0. || color == glam::Vec3::ZERO {
                continue;
            }
            let position = state.transform.transform_point3(glam::Vec3::ZERO);
            result.position[current * 3..current * 3 + 3].copy_from_slice(&position.to_array());
            result.color_radius[current * 4..current * 4 + 4]
                .copy_from_slice(&color.extend(radius).to_array());
            result.premultiplied[current * 3..current * 3 + 3]
                .copy_from_slice(&premultiplied.to_array());
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{scene::hierarchy::Hierarchy, scene::local_state};
    use serde_json::json;
    #[test]
    fn light_slots_keep_order_and_follow_parent_visibility_transform_and_intensity() {
        let objects = vec![
            json!({"id":1,"origin":[3,4,5]}),
            json!({"id":2,"parent":1,"light":"point","origin":[1,2,3],"color":[1,0.5,0],"intensity":2,"radius":10}),
            json!({"id":3,"light":"point","visible":false}),
            json!({"id":4,"light":"point","origin":[9,8,7],"radius":0}),
        ];
        let mut states = objects
            .iter()
            .map(|o| local_state(o, [100.; 2], !o["parent"].is_null()).unwrap())
            .collect::<Vec<_>>();
        Hierarchy::new(&objects)
            .unwrap()
            .compose(&mut states, &Default::default())
            .unwrap();
        let nodes = objects
            .iter()
            .map(|o| Node::decode(o, [100.; 2]).unwrap())
            .collect::<Vec<_>>();
        let lights = Snapshot::collect(&nodes, &states, [0; 4]).unwrap();
        assert_eq!(&lights.position[..3], &[4., 6., 8.]);
        assert_eq!(&lights.color_radius[..4], &[2., 1., 0., 10.]);
        assert_eq!(&lights.premultiplied[..3], &[200., 100., 0.]);
        assert_eq!(&lights.color_radius[4..8], &[0., 0., 0., 1.]);
        assert_eq!(&lights.color_radius[8..12], &[0., 0., 0., 1.]);
        assert!(validate(&json!({"light":"point","radius":-1})).is_err());
    }
}
