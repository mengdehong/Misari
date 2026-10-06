use super::*;
use serde_json::json;
use std::rc::Rc;
pub(crate) fn fixture() -> Rc<Model> {
    let key = |x, z| Key {
        translation: Vec3::new(x, 0., 0.),
        angles: Vec3::new(0., 0., z),
        scale: Vec3::ONE,
    };
    Rc::new(Model {
        meshes: vec![Geometry {
            materials: vec!["mat.json".into()],
            vertices: vec![Vertex {
                position: Vec3::new(8., 0., 0.),
                normal: Vec3::X,
                tangent: Vec4::X,
                uv: Vec2::ZERO,
                uv2: Vec2::ZERO,
                bones: [0; 4],
                weights: Vec4::X,
            }],
            indices: vec![],
            parts: vec![],
        }],
        bones: vec![
            Bone {
                name: "child".into(),
                parent: Some(1),
                local: Mat4::from_translation(Vec3::new(2., 0., 0.)),
                simulation: json!(null),
            },
            Bone {
                name: "root".into(),
                parent: None,
                local: Mat4::from_translation(Vec3::new(5., 0., 0.)),
                simulation: json!(null),
            },
        ],
        clips: vec![Clip {
            id: 4,
            name: "turn".into(),
            mode: "single".into(),
            fps: 1.,
            frames: 1,
            tracks: vec![
                vec![key(2., 0.), key(4., std::f32::consts::FRAC_PI_2)],
                vec![key(5., 0.), key(5., 0.)],
            ],
            events: vec![(0.5, "half".into())],
        }],
        rest: None,
        attachments: vec![],
    })
}
#[test]
fn bind_pose_forward_parent_euler_motion_normals_pause_rate_and_rewind() {
    let mut rig = Rig::new(fixture()).unwrap();
    let mut object = json!({"animationlayers":[{"id":1,"animation":4}]});
    rig.advance(0., &object).unwrap();
    assert!(Vec3::from_slice(&rig.vertices(0)).abs_diff_eq(Vec3::new(8., 0., 0.), 1e-5));
    rig.advance(0.5, &object).unwrap();
    assert_eq!(rig.events.len(), 1);
    object["animationlayers"][0]["paused"] = json!(true);
    rig.advance(0.8, &object).unwrap();
    let paused = rig.vertices(0);
    assert!((paused[0] - 8.707107).abs() < 1e-5);
    object["animationlayers"][0]["paused"] = json!(false);
    object["animationlayers"][0]["rate"] = json!(2);
    rig.advance(1.05, &object).unwrap();
    let vertices = rig.vertices(0);
    assert!(Vec3::from_slice(&vertices).abs_diff_eq(Vec3::new(9., 1., 0.), 1e-5));
    assert!(Vec3::from_slice(&vertices[3..]).abs_diff_eq(Vec3::Y, 1e-5));
    rig.advance(0., &object).unwrap();
    assert!(Vec3::from_slice(&rig.vertices(0)).abs_diff_eq(Vec3::new(8., 0., 0.), 1e-5));
    object["__boneOverrides"] =
        json!({"0":Mat4::from_translation(Vec3::new(-2.,0.,0.)).to_cols_array()});
    rig.advance(0., &object).unwrap();
    assert!(Vec3::from_slice(&rig.vertices(0)).abs_diff_eq(Vec3::new(4., 0., 0.), 1e-5));
}

#[test]
fn skeletal_commands_anchor_elapsed_time_stop_cache_reverse_events_and_layer_removal() {
    let mut rig = Rig::new(fixture()).unwrap();
    let mut object = json!({"animationlayers":[{"id":1,"animation":4}]});
    rig.advance(0., &object).unwrap();
    object["animationlayers"][0] =
        json!({"id":1,"animation":4,"__frame":0.25,"__time":10,"__revision":1});
    rig.advance(10.25, &object).unwrap();
    assert!(
        (rig.local[0].w_axis.x - 3.).abs() < 1e-6,
        "seek ignored elapsed time"
    );
    assert_eq!(rig.events[0]["name"], "half");
    assert_eq!(rig.events[0]["frame"], 0.5);
    object["animationlayers"][0] =
        json!({"id":1,"animation":4,"__frame":0.5,"__time":10.25,"__revision":2,"paused":true});
    rig.advance(10.4, &object).unwrap();
    assert!(!rig.animated());
    let held = rig.local.clone();
    assert!(
        !rig.advance(11., &object).unwrap(),
        "paused rig reskinned unchanged vertices"
    );
    assert_eq!(held, rig.local);
    object["animationlayers"][0] =
        json!({"id":1,"animation":4,"__frame":0.5,"__time":11,"__revision":3});
    rig.advance(11.5, &object).unwrap();
    assert_eq!(rig.local[0].w_axis.x, 4.);
    assert!(!rig.animated());
    assert!(
        rig.events
            .iter()
            .any(|e| e["ended"] == true && e["frame"] == 1.)
    );
    assert!(!rig.advance(12., &object).unwrap());
    object["animationlayers"][0] =
        json!({"id":1,"animation":4,"__frame":0.75,"__time":15,"__revision":4,"rate":-1});
    rig.advance(15.3, &object).unwrap();
    assert!(rig.events.iter().any(|e| e["name"] == "half"));
    object["animationlayers"] = json!([]);
    rig.advance(16., &object).unwrap();
    assert_eq!(
        rig.local[0], rig.model.bones[0].local,
        "empty layer list restarted default clip"
    );
    assert!(!rig.animated());
    assert!(rig.layers.is_empty());
}
#[test]
fn mirror_skeletal_events_follow_both_directions_and_large_steps_are_bounded() {
    let mut model = fixture();
    Rc::get_mut(&mut model).unwrap().clips[0].mode = "mirror".into();
    let mut rig = Rig::new(model).unwrap();
    let object = json!({"animationlayers":[{"id":1,"animation":4}]});
    rig.advance(0., &object).unwrap();
    rig.advance(0.6, &object).unwrap();
    assert_eq!(rig.events.len(), 1);
    rig.advance(1.6, &object).unwrap();
    assert_eq!(rig.events.iter().filter(|e| e["name"] == "half").count(), 1);
    assert_eq!(rig.events.iter().filter(|e| e["ended"] == true).count(), 1);
    rig.advance(100000., &object).unwrap();
    assert_eq!(rig.events.len(), 128);
}
