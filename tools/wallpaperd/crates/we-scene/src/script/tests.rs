use super::*;
#[test]
fn native_json_bridge_preserves_numbers_strings_prototypes_and_lifetime() {
    let runtime = Runtime::new().unwrap();
    let context = Context::full(&runtime).unwrap();
    context.with(|ctx| {
        let input = json!({
            "text":"壁纸\u{0}\"\\\n", "negativeZero":-0.0,
            "large":u64::MAX, "fraction":0.1_f32, "nonfinite":f64::INFINITY,
            "__proto__":{"own":true}, "array":[true,null,"é"]
        });
        let reference = ctx.json_parse(serde_json::to_vec(&input).unwrap()).unwrap();
        let parsed = Json(&input).into_js(&ctx).unwrap();
        drop(input);
        let compare: Function = ctx.eval(r#"(a,b)=>{
            function equal(x,y){
                if(x===null||typeof x!=='object')return Object.is(x,y);
                const keys=Object.keys(x);return Object.getPrototypeOf(x)===Object.getPrototypeOf(y)&&
                    keys.length===Object.keys(y).length&&keys.every(k=>Object.hasOwn(y,k)&&equal(x[k],y[k]));
            }
            return equal(a,b)&&Object.hasOwn(a,'__proto__')&&a.text.charCodeAt(2)===0&&
                Object.is(a.negativeZero,-0)&&a.nonfinite===null;
        }"#).unwrap();
        assert!(compare.call::<_, bool>((parsed, reference)).unwrap());
    });
}
#[test]
fn matrices_operators_transforms_inverse_decomposition_and_normals() {
    let (_temp, assets) = assets();
    let code = r#"
    function check(v,expected){if(!v.equals(expected))throw Error('matrix value '+v+' expected '+expected);}
    export function init(){
        const transform=Mat4.compose(new Vec3(3,4,5),new Vec3(0,0,90),new Vec3(2,4,8));
        check(new Vec3(transform*new Vec4(1,0,0,1)),new Vec3(3,6,5));
        check(transform.inverse().transformPoint(transform.transformPoint(new Vec3(1,2,3))),new Vec3(1,2,3));
        const decomposed=transform.decompose();
        if(!Mat4.compose(decomposed.translation,decomposed.rotation,decomposed.scale).equals(transform))throw Error('decompose');
        check(Mat4.fromScale(new Vec3(2,4,8)).normalMatrix()*new Vec3(2,4,8),new Vec3(1,1,1));
        check(Mat3.compose(new Vec2(2,3),90,new Vec2(2,4)).transformPoint(new Vec2(1,0)),new Vec2(2,5));
        check(Mat4.lookAt(new Vec3(0,0,10),new Vec3(0),new Vec3(0,1,0)).transformPoint(new Vec3(0)),new Vec3(0,0,-10));
        const moved=Mat4.identity();moved.translation(new Vec2(7,8));check(moved.translation(),new Vec3(7,8,0));
        if(!(transform+transform).equals(transform*2))throw Error('matrix addition');
        const copy=transform.copy();copy.m[12]=100;if(transform.translation().x!==3)throw Error('alias');
        let threw=false;try{Mat4.fromScale(0).inverse();}catch(e){threw=true;}if(!threw)throw Error('singular inverse');
        shared.matrixPassed=true;
    }
    export function update(v){return Mat4.fromTranslation(new Vec3(1,2,3)).transformPoint(v);}
    "#;
    let mut scripts = load(&assets, &[object(code)], &Properties::new(), [10.0; 2]);
    assert_clean(&scripts);
    scripts
        .context
        .with(|ctx| assert!(ctx.eval::<bool, _>("shared.matrixPassed").unwrap()));
    assert_eq!(
        scripts.frame(&frame(0.0, 0.0)).unwrap()[0][2],
        json!([2, 4, 6])
    );
}
use std::fs;

pub(super) fn assets() -> (tempfile::TempDir, Assets) {
    let temp = tempfile::tempdir().unwrap();
    let mut data = 8u32.to_le_bytes().to_vec();
    data.extend(b"PKGV0001");
    data.extend(0u32.to_le_bytes());
    fs::write(temp.path().join("scene.pkg"), data).unwrap();
    let assets = Assets::open(temp.path(), &temp.path().join("scene.pkg"), None).unwrap();
    (temp, assets)
}
fn object(source: &str) -> Value {
    json!({"id":1,"origin":{"value":"1 2 3","script":source},"scale":"1 1 1","angles":"0 0 0"})
}
pub(super) fn frame(time: f32, delta: f32) -> Value {
    json!({"time":time,"delta":delta,"timeOfDay":0.5,"screen":[100,100],"screenPointer":[0,0],"pointer":[0,0],"down":false,"focused":false,"hits":[],"audio":crate::audio::AudioSnapshot::default()})
}
#[test]
fn button_edges_between_frames_dispatch_one_click_before_update() {
    let (_temp, assets) = assets();
    let mut scripts = load(
        &assets,
        &[object(
            "export function init(v){shared.events=[];return v;} export function cursorDown(){shared.events.push('down');} export function cursorUp(){shared.events.push('up');} export function cursorClick(){shared.events.push('click');} export function update(v){shared.events.push(input.cursorLeftDown?'held':'released');return v;}",
        )],
        &Properties::new(),
        [100.; 2],
    );
    let mut input = frame(1., 1.);
    input["focused"] = json!(true);
    input["hits"] = json!([0]);
    input["eligible"] = json!([0]);
    let mut press = input.clone();
    press["down"] = json!(true);
    input["pointerEvents"] = json!([press, input.clone()]);
    scripts.frame(&input).unwrap();
    scripts.context.with(|ctx| {
        assert_eq!(
            ctx.eval::<String, _>("shared.events.join(',')").unwrap(),
            "down,up,click,released"
        );
    });
}
#[test]
fn feature_flags_follow_disabled_owners_and_remaining_timers() {
    let (_temp, assets) = assets();
    let mut scripts = load(
        &assets,
        &[
            object(
                "export function init(v){engine.registerAudioBuffers(16);return v;}export function update(v){if(engine.runtime>=1)throw Error('stop');return v;}export function cursorMove(){}export function mediaTimelineChanged(){}",
            ),
            object("export function init(v){engine.setTimeout(()=>{},2000);return v;}"),
        ],
        &Properties::new(),
        [100.; 2],
    );
    assert!(scripts.animated && scripts.audio && scripts.pointer && scripts.media_timeline);
    scripts.frame(&frame(1., 1.)).unwrap();
    assert!(scripts.animated);
    assert!(!scripts.audio && !scripts.pointer && !scripts.media_timeline);
    scripts.frame(&frame(2.1, 1.1)).unwrap();
    assert!(!scripts.animated && !scripts.audio && !scripts.pointer && !scripts.media_timeline);
}
#[test]
fn particle_host_commands_instance_vectors_bounds_and_destroyed_handles() {
    let (_temp, assets) = assets();
    let code = r#"
        export function init(v){
            shared.system=thisLayer;shared.point=thisLayer.instance.controlpoint0;
            if(!(shared.point instanceof Vec3)||shared.point.x!==4||thisLayer.instance.rate!==1)throw Error('instance defaults');
            thisLayer.stop();if(thisLayer.isPlaying())throw Error('stop state');
            thisLayer.emitParticles(2);if(!thisLayer.isPlaying())throw Error('forced state');
            shared.point.x=8;thisLayer.instance.colorn=new Vec3(0,1,0);
            let bad=false;try{thisLayer.emitParticles(20001);}catch(e){bad=e instanceof RangeError;}if(!bad)throw Error('particle bound');
            return v;
        }
        export function applyUserProperties(v){if(v.destroy)thisScene.destroyLayer(thisLayer);}
    "#;
    let objects = [
        json!({"id":1,"particle":"particle.json","__particle":{"playing":true,"live":0,"commands":[],"controlpoints":[[4,0,0]]},"origin":{"value":[0,0,0],"script":code}}),
    ];
    let mut scripts = load(&assets, &objects, &Properties::new(), [32.; 2]);
    assert_clean(&scripts);
    let patch = scripts.flush().unwrap();
    assert!(
        patch
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p[1] == json!(["__particle", "commands"])
                && p[2] == json!([["stop", 0], ["emit", 2]]))
    );
    assert!(
        patch
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p[1] == json!(["instanceoverride", "controlpoint0"])
                && p[2] == json!([8, 0, 0]))
    );
    scripts
        .set_properties(&Properties::from([("destroy".into(), json!(true))]))
        .unwrap();
    scripts.context.with(|ctx|{
        assert!(ctx.eval::<bool,_>("(()=>{try{shared.system.play();return false;}catch(e){return e instanceof ReferenceError;}})()").unwrap());
        assert!(ctx.eval::<bool,_>("(()=>{try{shared.point.x=1;return false;}catch(e){return e instanceof ReferenceError;}})()").unwrap());
    });
}
#[test]
fn initialization_precedes_properties_and_effect_materials_keep_vectors_and_handles() {
    let (_temp, assets) = assets();
    let objects = [
        json!({"id":1,"image":"image","name":"layer","effects":[{
            "name":"effect","visible":{"value":true,"script":r#"
            export function init(v){
                shared.ready=true;shared.effect=thisLayer.getEffect('effect');shared.material=thisObject.getMaterial(0);
                if(thisLayer.getEffectCount()!==1||thisObject.getMaterialCount()!==2||shared.effect!==thisObject)throw Error('effect identity');
                if(!(shared.material.tint instanceof Vec3)||shared.material.getAnimation('missing')!==undefined||thisObject.getMaterial(2)!==undefined)throw Error('material types');
                shared.tint=shared.material.tint;return v;
            }
            export function applyUserProperties(values){
                if(!shared.ready)throw Error('properties before init');
                shared.material.amount=values.amount;shared.tint.y=0.7;
                thisObject.setMaterialProperty('amount',values.amount);
                if(thisObject.getMaterial(1).amount!==values.amount||thisObject.getMaterial(1).other!==9)throw Error('matching material properties');
                shared.events=(shared.events||0)+1;
            }
        "#},"passes":[{"constantshadervalues":{"amount":0.1,"tint":"0.2 0.3 0.4"}},{"constantshadervalues":{"amount":0.4,"other":9}}]
        }]}),
        object(
            "export function init(v){if(!shared.ready)throw Error('ordered initialization');return v;} export function update(v){if(engine.runtime>1){thisScene.destroyLayer('layer');}return v;}",
        ),
    ];
    let mut scripts = Scripts::load(
        &assets,
        &objects,
        &Properties::from([("amount".into(), json!(0.6))]),
        [100.; 2],
        42,
    )
    .unwrap()
    .unwrap();
    assert_clean(&scripts);
    let patch = scripts.flush().unwrap();
    assert!(patch.as_array().unwrap().iter().any(|p| p[1]
        == json!([
            "effects",
            "0",
            "passes",
            "0",
            "constantshadervalues",
            "tint"
        ])
        && p[2] == json!([0.2, 0.7, 0.4])));
    scripts
        .set_properties(&Properties::from([("amount".into(), json!(0.8))]))
        .unwrap();
    scripts.context.with(|ctx|{
        assert!(ctx.eval::<bool,_>("shared.tint===shared.material.tint&&shared.material.amount===0.8&&shared.events===2").unwrap());
    });
    scripts.frame(&frame(2., 1.)).unwrap();
    scripts.context.with(|ctx|{
        assert!(ctx.eval::<bool,_>("(()=>{try{shared.material.amount=1;return false;}catch(e){return e instanceof ReferenceError;}})()").unwrap());
        assert!(ctx.eval::<bool,_>("(()=>{try{shared.effect.getMaterial(0);return false;}catch(e){return e instanceof ReferenceError;}})()").unwrap());
    });
}
#[test]
fn video_handles_share_controls_keep_pause_time_and_check_owners_and_callbacks() {
    let (_temp, assets) = assets();
    let objects = [
        json!({"id":1,"__videoMaster":0,"__texture":{"asset":"video","video":true,"loaded":true,"duration":4,"rate":1,"playing":true,"joined":true,"loop":true,"revision":0},"origin":{"value":[0,0,0],"script":r#"
            const video=thisLayer.getVideoTexture();shared.firstVideo=video;
            if(thisLayer.getTextureAnimation()!==undefined||video.duration!==4)throw Error('video metadata');
            export function init(v){video.loop=false;video.setCurrentTime(1);video.rate=2;video.pause();video.addEndedCallback(()=>shared.ended=(shared.ended||0)+1);return v;}
        "#}}),
        json!({"id":2,"__videoMaster":0,"__texture":{"video":true},"origin":{"value":[0,0,0],"script":r#"
            const video=thisLayer.getVideoTexture();shared.secondVideo=video;
            export function update(v){v.x=video.getCurrentTime();v.y=video.rate;v.z=video.isPlaying()?1:0;return v;}
        "#}}),
    ];
    let mut scripts = load(&assets, &objects, &Properties::new(), [32.; 2]);
    scripts.flush().unwrap();
    assert_eq!(
        scripts
            .frame(&frame(2., 2.))
            .unwrap()
            .as_array()
            .unwrap()
            .last()
            .unwrap()[2],
        json!([1, 2, 0])
    );
    let mut control: Value = scripts.context.with(|ctx| {
        serde_json::from_str(
            &ctx.eval::<String, _>("JSON.stringify(__weNodes[0].__texture)")
                .unwrap(),
        )
        .unwrap()
    });
    control["duration"] = json!(5);
    scripts
        .sync_values(&json!([[0, ["__texture"], control]]))
        .unwrap();
    scripts.context.with(|ctx| {
        assert!(
            ctx.eval::<bool, _>("shared.firstVideo.duration===5&&shared.secondVideo.duration===5")
                .unwrap()
        )
    });
    scripts
        .context
        .with(|ctx| ctx.eval::<(), _>("shared.secondVideo.play()").unwrap());
    assert_eq!(
        scripts
            .frame(&frame(2.5, 0.5))
            .unwrap()
            .as_array()
            .unwrap()
            .last()
            .unwrap()[2],
        json!([2, 2, 1])
    );
    scripts
        .context
        .with(|ctx| ctx.eval::<(), _>("shared.secondVideo.rate=0").unwrap());
    assert_eq!(
        scripts
            .frame(&frame(3., 0.5))
            .unwrap()
            .as_array()
            .unwrap()
            .last()
            .unwrap()[2],
        json!([2, 0, 0])
    );
    scripts
        .context
        .with(|ctx| ctx.eval::<(), _>("shared.secondVideo.rate=-1").unwrap());
    assert_eq!(
        scripts
            .frame(&frame(3.5, 0.5))
            .unwrap()
            .as_array()
            .unwrap()
            .last()
            .unwrap()[2],
        json!([1.5, -1, 1])
    );
    let mut event = frame(3.5, 0.);
    event["videoEvents"] = json!([{"node":0}]);
    scripts.frame(&event).unwrap();
    scripts.context.with(|ctx|{
        assert_eq!(ctx.eval::<u32,_>("shared.ended").unwrap(),1);
        assert!(ctx.eval::<bool,_>("(()=>{try{shared.secondVideo.rate=101;}catch(e){return e instanceof RangeError;}return false;})()").unwrap());
        ctx.eval::<(),_>("thisScene.destroyLayer(0)").unwrap();
    });
    scripts.flush().unwrap();
    scripts.context.with(|ctx|{
        assert!(ctx.eval::<bool,_>("(()=>{try{shared.firstVideo.play();}catch(e){return e instanceof ReferenceError;}return false;})()").unwrap());
        ctx.eval::<(),_>("shared.secondVideo.stop()").unwrap();
    });
    assert_eq!(
        scripts
            .frame(&frame(4., 0.5))
            .unwrap()
            .as_array()
            .unwrap()
            .last()
            .unwrap()[2],
        json!([0, -1, 0])
    );
    scripts.frame(&event).unwrap();
    scripts
        .context
        .with(|ctx| assert_eq!(ctx.eval::<u32, _>("shared.ended").unwrap(), 1));
    assert_clean(&scripts);
}
#[test]
fn non_finite_script_properties_are_diagnosed_before_native_commit() {
    let (_temp, assets) = assets();
    let mut scripts = load(
        &assets,
        &[object("export function update(v){v.x=NaN;return v;}")],
        &Properties::new(),
        [1.; 2],
    );
    assert!(scripts.frame(&frame(0., 0.)).is_err());
    assert!(scripts.exhausted);
    assert!(
        scripts
            .diagnostics()
            .iter()
            .any(|message| message.contains("Non-finite"))
    );
    assert_eq!(scripts.frame(&frame(1., 1.)).unwrap(), json!([]));
}
#[test]
fn user_colors_are_vectors_in_modules_initialization_and_property_events() {
    let (_temp, assets) = assets();
    let source = r#"
        const initial=engine.userProperties.tint;
        if(!(initial instanceof Vec3)||!initial.equals(new Vec3(0.1,0.2,0.3)))throw Error('module color');
        export function applyUserProperties(values){
            if('tint' in values&&(!(values.tint instanceof Vec3)||!values.tint.equals(engine.userProperties.tint)))throw Error('event color');
            shared.events=(shared.events||0)+1;shared.removed=values.removed;
        }
        export function init(v){return initial*2;}
        export function update(v){
            if(initial!==engine.userProperties.tint)throw Error('color alias');
            if(engine.userProperties.scalar!==7||engine.userProperties.text!=='0.1 0.2 0.3')throw Error('scalar conversion');
            return initial*2;
        }
    "#;
    let values = Properties::from([
        ("tint".into(), json!([0.1, 0.2, 0.3])),
        ("scalar".into(), json!(7)),
        ("text".into(), json!("0.1 0.2 0.3")),
        ("removed".into(), json!(true)),
    ]);
    let mut scripts = Scripts::load(&assets, &[object(source)], &values, [100.; 2], 42)
        .unwrap()
        .unwrap();
    assert_eq!(scripts.flush().unwrap()[0][2], json!([0.2, 0.4, 0.6]));
    let mut changed = values;
    changed.insert("tint".into(), json!([0.4, 0.5, 0.6]));
    changed.remove("removed");
    scripts.set_properties(&changed).unwrap();
    assert_eq!(
        scripts.frame(&frame(1., 0.016)).unwrap()[0][2],
        json!([0.8, 1, 1.2])
    );
    scripts.context.with(|ctx| {
        assert_eq!(ctx.eval::<u32, _>("shared.events").unwrap(), 2);
        assert!(ctx.eval::<bool, _>("shared.removed===null").unwrap());
    });
    assert_clean(&scripts);
}
#[test]
fn storage_vectors_layer_world_and_texture_clock_have_independent_checked_state() {
    let (_temp, assets) = assets();
    let source = r#"
    export function init(value){
        localStorage.set('vector',new Vec3(3,4,5));
        const vector=localStorage.get('vector');if(!(vector instanceof Vec3)||vector.y!==4)throw Error('stored vector');
        vector.y=99;if(localStorage.get('vector').y!==4)throw Error('storage alias');
        localStorage.set('../key',{answer:42},'global');if(localStorage.get('../key')!==undefined)throw Error('storage scope');
        if(!localStorage.delete('../key','global')||localStorage.delete('../key','global'))throw Error('delete result');
        localStorage.clear();if(localStorage.get('vector')!==undefined)throw Error('clear');
        const world=thisLayer.getTransformMatrix();if(!world.translation().equals(new Vec3(5,8,0)))throw Error('layer world');
        world.m[12]=100;if(thisLayer.getTransformMatrix().translation().x!==5)throw Error('world alias');
        const a=thisLayer.getTextureAnimation();a.stop();a.setFrame(1);shared.animation=a;return value;
    }
    export function update(v){shared.frame=shared.animation.getFrame();return v;}
    "#;
    let objects = [
        json!({"id":1,"origin":[3,4,0],"scale":[2,2,1]}),
        json!({"id":2,"parent":1,"origin":{"value":[1,2,0],"script":source},"__texture":{"loaded":true,"durations":[0.25,0.75],"frameCount":2,"duration":1,"rate":1,"position":0,"anchor":0,"playing":true,"joined":true}}),
    ];
    let mut scripts = load(&assets, &objects, &Properties::new(), [10.; 2]);
    assert_clean(&scripts);
    scripts.frame(&frame(10., 10.)).unwrap();
    scripts.context.with(|ctx| {
        assert_eq!(ctx.eval::<f64, _>("shared.frame").unwrap(), 1.);
        ctx.eval::<(), _>("shared.animation.play()").unwrap();
    });
    scripts.frame(&frame(10.375, 0.375)).unwrap();
    scripts.context.with(|ctx| {
        assert_eq!(ctx.eval::<f64, _>("shared.frame").unwrap(), 1.5);
        ctx.eval::<(), _>("shared.animation.pause()").unwrap();
    });
    scripts.frame(&frame(20., 9.625)).unwrap();
    scripts.context.with(|ctx| {
        assert_eq!(ctx.eval::<f64, _>("shared.frame").unwrap(), 1.5);
        ctx.eval::<(), _>("shared.animation.join()").unwrap();
    });
    scripts.frame(&frame(20.125, 0.125)).unwrap();
    assert_eq!(
        scripts
            .context
            .with(|ctx| ctx.eval::<f64, _>("shared.frame").unwrap()),
        0.5
    );
}
#[test]
fn returning_tracked_vectors_keeps_identity_across_many_frames() {
    let (_temp, assets) = assets();
    let source = "let original; export function init(v){original=v;return v;} export function update(v){if(v!==original)throw Error('vector identity changed');v.x+=1;return v;}";
    let node = json!({"id":1,"origin":{"value":"1 2 3","script":source},"effects":[{"passes":[{"constantshadervalues":{"color1":{"value":"4 5 6","script":source}}}]}]});
    let mut scripts = Scripts::load(&assets, &[node], &Properties::new(), [100.0; 2], 42)
        .unwrap()
        .unwrap();
    for index in 0..1024 {
        let patch = scripts.frame(&frame(index as f32 / 60., 1. / 60.)).unwrap();
        for (path, expected) in [
            (json!(["origin"]), json!([index + 2, 2, 3])),
            (
                json!([
                    "effects",
                    "0",
                    "passes",
                    "0",
                    "constantshadervalues",
                    "color1"
                ]),
                json!([index + 5, 5, 6]),
            ),
        ] {
            assert!(
                patch
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|p| p[1] == path && p[2] == expected),
                "frame {index}: {patch}; {:?}",
                scripts.diagnostics()
            );
        }
    }
    assert_clean(&scripts);
}

#[test]
fn animation_sync_keeps_vector_handles_and_does_not_emit_script_writes() {
    let (_temp, assets) = assets();
    let source = "export function init(){shared.origin=thisLayer.origin;shared.angles=thisLayer.angles;} export function update(){if(shared.origin!==thisLayer.origin||shared.angles!==thisLayer.angles)throw Error('animation replaced vector handle');}";
    let node =
        json!({"id":1,"origin":[1,2,3],"angles":[0,0,0],"alpha":{"value":1,"script":source}});
    let mut scripts = Scripts::load(&assets, &[node], &Properties::new(), [100.0; 2], 42)
        .unwrap()
        .unwrap();
    scripts.flush().unwrap();
    for (index, raw, expected) in [
        (1, json!([4, 5, 6]), json!([4, 5, 6])),
        (2, json!([7, 8]), json!([7, 8, 0])),
        (3, json!([9]), json!([9, 9, 9])),
        (4, json!("10 11 12"), json!([10, 11, 12])),
    ] {
        let mut input = frame(index as f32, 1.);
        input["values"] = json!([
            [0, ["origin"], raw],
            [
                0,
                ["angles"],
                [
                    std::f64::consts::FRAC_PI_2,
                    -std::f64::consts::FRAC_PI_4,
                    0.
                ]
            ]
        ]);
        assert_eq!(scripts.frame(&input).unwrap(), json!([]));
        scripts.context.with(|ctx| {
            let origin: String = ctx.eval("JSON.stringify(shared.origin)").unwrap();
            assert_eq!(serde_json::from_str::<Value>(&origin).unwrap(), expected);
            let angles: String = ctx.eval("JSON.stringify(shared.angles)").unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(&angles).unwrap(),
                json!([90, -45, 0])
            );
        });
    }
    assert_clean(&scripts);
}

#[test]
fn scalar_host_sync_retains_nested_handles_and_tracks_later_script_writes() {
    let (_temp, assets) = assets();
    let node = json!({"id":1,"custom":{"number":1,"list":[1,2],"nested":{"active":true}},"alpha":{"value":1,"script":"export function init(){shared.custom=thisLayer.custom;shared.list=thisLayer.custom.list;shared.nested=thisLayer.custom.nested;} export function update(){if(shared.custom!==thisLayer.custom||shared.list!==thisLayer.custom.list||shared.nested!==thisLayer.custom.nested)throw Error('host replaced handle');}"}});
    let mut scripts = Scripts::load(&assets, &[node], &Properties::new(), [100.; 2], 42)
        .unwrap()
        .unwrap();
    scripts.flush().unwrap();
    let mut input = frame(1., 1.);
    input["values"] = json!([
        [0, ["custom", "number"], null],
        [0, ["custom", "list", "0"], "sample"],
        [0, ["custom", "nested", "active"], false]
    ]);
    assert_eq!(scripts.frame(&input).unwrap(), json!([]));
    scripts.context.with(|ctx| {
        assert!(ctx.eval::<bool,_>("shared.custom.number===null&&shared.list[0]==='sample'&&shared.nested.active===false").unwrap());
        ctx.eval::<(),_>("shared.custom.number=12;shared.list[0]=30;shared.nested.active=true;undefined").unwrap();
    });
    assert_eq!(
        scripts.flush().unwrap(),
        json!([
            [0, ["custom", "number"], 12],
            [0, ["custom", "list"], [30, 2]],
            [0, ["custom", "nested", "active"], true]
        ])
    );
    assert_clean(&scripts);
}

#[test]
fn canonical_properties_preserve_vector_identity_and_nested_property_vectors() {
    let (_temp, assets) = assets();
    let node = json!({"id":1,"scale":"1 1 1","origin":{"value":"1 2 3","script":"const scale=thisLayer.scale;let ticks=0;export function update(v){v.x=scale.x;v.z=++ticks;return v;}"}});
    let mut scripts = Scripts::load(&assets, &[node], &Properties::new(), [100.0; 2], 42)
        .unwrap()
        .unwrap();
    assert_eq!(
        scripts.frame(&frame(1.0, 0.016)).unwrap()[0][2],
        json!([1, 2, 1])
    );
    scripts
        .sync_nodes(&[json!({"id":1,"scale":[3,4,5],"origin":[10,20,30]})])
        .unwrap();
    assert_eq!(
        scripts.frame(&frame(2.0, 0.016)).unwrap()[0][2],
        json!([3, 20, 2])
    );
    let node = json!({"id":1,"effects":[{"passes":[{"constantshadervalues":{"color1":{"value":"1 2 3","script":"export function update(v){v.x+=4;return v;}"}}}]}]});
    let mut nested = Scripts::load(&assets, &[node], &Properties::new(), [100.0; 2], 42)
        .unwrap()
        .unwrap();
    let patch = nested.frame(&frame(1.0, 0.016)).unwrap();
    assert!(
        patch.as_array().unwrap().iter().any(|p| p[1]
            == json!([
                "effects",
                "0",
                "passes",
                "0",
                "constantshadervalues",
                "color1"
            ])
            && p[2] == json!([5, 2, 3])),
        "{patch}"
    );
}
#[test]
fn media_types_pointer_local_coordinates_and_press_ownership() {
    let (_temp, assets) = assets();
    let node = object(
        "export function mediaPlaybackChanged(e){thisLayer.origin.z=e.state===MediaPlaybackEvent.PLAYBACK_PLAYING?1:0;}export function mediaThumbnailChanged(e){if(!(e.primaryColor instanceof Vec3))throw Error('untyped palette');thisLayer.color=e.primaryColor.copy();}export function cursorDown(e){thisLayer.origin.x=e.localPosition.x;}export function cursorClick(){thisLayer.origin.z++;}",
    );
    let mut scripts = Scripts::load(&assets, &[node], &Properties::new(), [100.0; 2], 42)
        .unwrap()
        .unwrap();
    let patch = scripts
        .event("mediaPlaybackChanged", &json!({"state":1}))
        .unwrap();
    assert_eq!(patch[0][2][2], 1);
    let patch=scripts.event("mediaThumbnailChanged",&json!({"primaryColor":[0.2,0.3,0.4],"secondaryColor":[0,0,0],"tertiaryColor":[0,0,0],"textColor":[1,1,1],"highContrastColor":[1,1,1]})).unwrap();
    assert_eq!(patch[0][2], json!([0.2, 0.3, 0.4]));
    let mut input = frame(1.0, 0.016);
    input["focused"] = json!(true);
    input["down"] = json!(true);
    input["locals"] = json!([[10, 20, 0]]);
    scripts.frame(&input).unwrap();
    input["hits"] = json!([0]);
    scripts.frame(&input).unwrap();
    input["down"] = json!(false);
    assert!(
        scripts
            .frame(&input)
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty()
    );
    input["down"] = json!(true);
    let patch = scripts.frame(&input).unwrap();
    assert_eq!(patch[0][2][0], 10);
    input["down"] = json!(false);
    let patch = scripts.frame(&input).unwrap();
    assert_eq!(patch[0][2][2], 2);
    assert_clean(&scripts);
}
#[test]
fn vector_operators_methods_modules_shared_mutation_and_degree_units() {
    let (_temp, assets) = assets();
    let objects = [
        object(
            "import {mix} from 'WEMath'; export function init(v){shared.base=v.copy();thisLayer.angles=new Vec3(0,0,90);return v;} export function update(v){return shared.base + new Vec3(mix(0,4,0.5),2,3)*2;}",
        ),
        json!({"id":2,"origin":{"value":"0 0 0","script":"export function update(v){v.x=shared.base.x;return v;}"}}),
    ];
    let mut scripts = Scripts::load(&assets, &objects, &Properties::new(), [100.0; 2], 42)
        .unwrap()
        .unwrap();
    assert!(scripts.animated);
    let initial = scripts.flush().unwrap();
    assert!(
        initial
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e[1] == json!(["angles"])
                && (e[2][2].as_f64().unwrap() - std::f64::consts::FRAC_PI_2).abs() < 0.00001)
    );
    let patch = scripts.frame(&frame(1.0, 0.016)).unwrap();
    assert!(
        patch
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e[0] == 0 && e[1] == json!(["origin"]) && e[2] == json!([5, 6, 9]))
    );
    assert!(
        patch
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e[0] == 1 && e[2] == json!([1, 0, 0]))
    );
    assert_clean(&scripts);
}
#[test]
fn compound_assignment_order_strings_comments_templates_regex_and_standard_js() {
    let (_temp, assets) = assets();
    let source = r#"export function update(value){
        const events=[];let reads=0,keys=0,rhs=0;
        const obj={get v(){events.push('get');reads++;return new Vec3(1,2,3);},set v(x){events.push('set');value.x=x.x;}};
        function object(){events.push('object');return obj;}
        function key(){keys++;events.push('key');return 'v';}
        function right(){rhs++;events.push('rhs');return new Vec3(4,5,6);}
        object()[key()]+=right();
        if(events.join(',')!=='object,key,get,rhs,set'||reads!==1||keys!==1||rhs!==1)throw Error(events);
        const literal='a+b * c';const regex=/a+b/;const text=`prefix ${1+2*3}`;
        if(literal!=='a+b * c'||text!=='prefix 7'||'a'+1!=='a1'||2+3*4!==14||2n+3n!==5n)throw Error('JS changed');
        value.y=(-new Vec3(2,3,4)).y;return value;
    }"#;
    let mut scripts = Scripts::load(
        &assets,
        &[object(source)],
        &Properties::new(),
        [100.0; 2],
        42,
    )
    .unwrap()
    .unwrap();
    let patch = scripts.frame(&frame(1.0, 0.016)).unwrap();
    assert_clean(&scripts);
    assert_eq!(patch[0][2], json!([5, -3, 3]));
}
#[test]
fn assets_are_scoped_audio_buffers_keep_identity_properties_and_pointer_cancel() {
    let (temp, assets) = assets();
    fs::create_dir(temp.path().join("scripts")).unwrap();
    fs::write(
        temp.path().join("scripts/helper.js"),
        "export const increment = v => v+1;",
    )
    .unwrap();
    let mut node = object(
        "import {increment} from './helper.js'; const audio=engine.registerAudioBuffers(16);const left=audio.left;export var scriptProperties=createScriptProperties().addSlider({name:'speed',value:1}).finish(); export function update(v){v.x=left[0]*scriptProperties.speed;v.z=increment(v.z);return v;} export function cursorClick(){thisLayer.origin.y++;}",
    );
    node["origin"]["scriptproperties"] = json!({"speed":{"user":"speed","value":1}});
    let mut scripts = Scripts::load(&assets, &[node], &Properties::new(), [100.0; 2], 42)
        .unwrap()
        .unwrap();
    assert!(scripts.audio && scripts.pointer);
    scripts
        .set_properties(&Properties::from([("speed".into(), json!(2))]))
        .unwrap();
    let mut input = frame(1.0, 0.016);
    input["audio"]["bands"][0]["left"][0] = json!(0.25);
    input["focused"] = json!(true);
    input["hits"] = json!([0]);
    input["down"] = json!(true);
    let pressed = scripts.frame(&input).unwrap();
    assert_eq!(pressed[0][2][0], json!(0.5));
    input["focused"] = json!(false);
    input["hits"] = json!([]);
    input["down"] = json!(false);
    let cancelled = scripts.frame(&input).unwrap();
    assert_eq!(cancelled[0][2][1], json!(2));
    assert_clean(&scripts);
    let escaping =
        object("import value from '../../outside.js';export function update(v){return v;}");
    assert!(Scripts::load(&assets, &[escaping], &Properties::new(), [1.0; 2], 0).is_err());
}
#[test]
fn infinite_loop_exceptions_destroyed_handles_and_scene_isolation() {
    let (_temp, assets) = assets();
    let mut scripts = load(
        &assets,
        &[object("export function update(v){while(true){}return v;}")],
        &Properties::new(),
        [1.0; 2],
    );
    let start = Instant::now();
    let _ = scripts.frame(&frame(1.0, 0.016));
    assert!(start.elapsed() < Duration::from_secs(1));
    assert!(!scripts.diagnostics().is_empty());
    let mut other = load(
        &assets,
        &[object("export function update(v){v.x=7;return v;}")],
        &Properties::new(),
        [1.0; 2],
    );
    assert_eq!(other.frame(&frame(1.0, 0.016)).unwrap()[0][2][0], json!(7));
    let mut destroyed = load(
        &assets,
        &[object(
            "const layer=thisLayer;export function update(v){thisScene.destroyLayer(layer);return v;}export function destroy(){shared.done=true;}",
        )],
        &Properties::new(),
        [1.0; 2],
    );
    let patch = destroyed.frame(&frame(1.0, 0.016)).unwrap();
    assert!(
        patch
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e[1] == json!(["visible"]) && e[2] == false)
    );
    destroyed.context.with(|ctx| {
        assert!(ctx.eval::<(), _>("thisLayer.origin.x=1").is_err());
        let _ = ctx.catch();
    });
}

#[test]
fn skeletal_animation_handles_clock_creation_callbacks_removal_and_limits() {
    let (_temp, assets) = assets();
    let source = r#"
    export function init(v){
        const h=thisLayer.getAnimationLayer('base');shared.h=h;shared.ended=0;
        if(h.fps!==2||h.frameCount!==4||h.duration!==2||h.name!=='base'||thisLayer.getAnimationLayerCount()!==1)throw Error('animation metadata');
        h.setFrame(1);h.pause();h.addEndedCallback(()=>{if(thisLayer.id!==1)throw Error('callback owner');shared.ended++;});
        return v;
    }
    export function update(v){shared.frame=shared.h.getFrame();return v;}
    "#;
    let node = json!({"id":1,"origin":{"value":[0,0,0],"script":source},"animationlayers":[{"name":"base","animation":4}],"__model":{"bones":[],"clips":[{"id":4,"name":"turn","mode":"single","fps":2,"frames":4,"duration":2}]}});
    let mut scripts = load(&assets, &[node], &Properties::new(), [10.; 2]);
    scripts.frame(&frame(10., 10.)).unwrap();
    scripts.context.with(|ctx| {
        assert_eq!(ctx.eval::<f64, _>("shared.frame").unwrap(), 1.);
        ctx.eval::<(), _>("shared.h.play()").unwrap();
    });
    scripts.frame(&frame(10.25, 0.25)).unwrap();
    scripts.context.with(|ctx| {
        assert_eq!(ctx.eval::<f64, _>("shared.frame").unwrap(), 1.5);
        ctx.eval::<(), _>("shared.h.rate=2").unwrap();
    });
    scripts.frame(&frame(10.5, 0.25)).unwrap();
    let data=scripts.context.with(|ctx|{
        assert_eq!(ctx.eval::<f64,_>("shared.frame").unwrap(),2.5);
        let encoded=ctx.eval::<String,_>("JSON.stringify({key:__weNodes[0].animationlayers[0].__key,revision:__weNodes[0].animationlayers[0].__revision})").unwrap();
        serde_json::from_str::<Value>(&encoded).unwrap()
    });
    let mut f = frame(11., 0.5);
    f["poses"] = json!([{"node":0,"time":11,"local":[],"layers":[{"key":data["key"],"revision":data["revision"],"frame":4,"playing":false}]}]);
    f["animationEvents"] = json!([{"node":0,"event":{"key":data["key"],"revision":data["revision"],"frame":4,"ended":true}}]);
    scripts.frame(&f).unwrap();
    scripts.context.with(|ctx|{
        assert_eq!(ctx.eval::<u32,_>("shared.ended").unwrap(),1);
        checked(&ctx,ctx.eval::<(),_>(r#"
{
            if(shared.h.isPlaying())throw Error('ended animation still playing');
            shared.h.play();if(shared.h.getFrame()!==0)throw Error('ended animation did not restart');
            const once=thisLayer.playSingleAnimation('turn',{name:'once',blendin:true,blendtime:0.1});shared.once=once;
            if(thisLayer.getAnimationLayerCount()!==2)throw Error('create');
            if(!thisLayer.destroyAnimationLayer('base'))throw Error('destroy');
            let stale=false;try{shared.h.getFrame();}catch(e){stale=e instanceof ReferenceError;}if(!stale)throw Error('stale handle');
            shared.h=thisLayer.getAnimationLayer('once');
            let invalid=false;try{thisLayer.createAnimationLayer('unknown');}catch(e){invalid=e instanceof RangeError;}if(!invalid)throw Error('unknown clip accepted');
}
        "#)).unwrap();
    });
    let data = scripts.context.with(|ctx| {
        serde_json::from_str::<Value>(
            &ctx.eval::<String, _>(
                "JSON.stringify(__weNodes[0].animationlayers.find(l=>l.name==='once'))",
            )
            .unwrap(),
        )
        .unwrap()
    });
    let mut f = frame(13., 2.);
    f["animationEvents"] = json!([{"node":0,"event":{"key":data["__key"],"revision":data["__revision"],"ended":true}}]);
    scripts.frame(&f).unwrap();
    scripts.deadline.set(Instant::now() + LOAD_BUDGET);
    scripts.context.with(|ctx|{
        checked(&ctx,ctx.eval::<(),_>(r#"
{
            if(thisLayer.getAnimationLayerCount()!==0)throw Error('single animation not removed');
            let stale=false;try{shared.once.play();}catch(e){stale=e instanceof ReferenceError;}if(!stale)throw Error('single handle still alive');
            for(let i=0;i<128;i++)thisLayer.createAnimationLayer('turn');
            let limit=false;try{thisLayer.createAnimationLayer('turn');}catch(e){limit=e instanceof RangeError;}if(!limit)throw Error('unbounded layers');
            const h=thisLayer.getAnimationLayer(0);for(let i=0;i<128;i++)h.addEndedCallback(()=>{});
            limit=false;try{h.addEndedCallback(()=>{});}catch(e){limit=e instanceof RangeError;}if(!limit)throw Error('unbounded callbacks');
}
        "#)).unwrap();
    });
    // The intentionally stale update binding disables only itself after single-shot destruction.
    assert!(
        scripts
            .diagnostics()
            .iter()
            .any(|s| s.contains("handle has been destroyed"))
    );
}

#[test]
fn attachment_bone_pose_world_parent_adjustment_and_cycles_preserve_values() {
    let (_temp, assets) = assets();
    let source = r#"
    export function init(v){
        const parent=thisScene.getLayer('parent'),child=thisScene.getLayer('child');
        if(parent.getAttachmentIndex('hook')!==0||parent.getAttachmentIndex('missing')!==-1)throw Error('attachment lookup');
        if(!parent.getAttachmentOrigin('hook').equals(new Vec3(17,4,0)))throw Error('attachment bind world');
        let cycle=false;try{parent.setParent(child);}catch(e){cycle=e instanceof RangeError;}if(!cycle)throw Error('parent cycle accepted');
        const before=child.getTransformMatrix();child.setParent(parent,'hook',true);
        if(!child.getTransformMatrix().equals(before)||!child.origin.equals(new Vec3(-6,0,0)))throw Error('adjust parent changed pose');
        const detached=parent.getAttachmentMatrix('hook');detached.m[12]=500;if(parent.getAttachmentOrigin('hook').x!==17)throw Error('attachment alias');
        let invalid=false;try{child.setParent(parent,'missing',true);}catch(e){invalid=e instanceof RangeError;}if(!invalid||child.attachment!=='hook')throw Error('invalid attachment was not atomic');
        return v;
    }
    export function update(v){
        const child=thisScene.getLayer('child');shared.position=child.getTransformMatrix().translation().x;
        if(engine.runtime===1){if(shared.position!==9)throw Error('animated attachment');child.setParent(undefined,undefined,true);if(!child.origin.equals(new Vec3(9,4,0)))throw Error('unparent adjustment');}
        return v;
    }
    "#;
    let matrix = |x| glam::Mat4::from_translation(glam::Vec3::new(x, 0., 0.)).to_cols_array();
    let nodes = [
        json!({"id":1,"name":"parent","origin":{"value":[3,4,0],"script":source},"scale":[2,2,1],"__model":{"bones":[{"name":"root","parent":null,"local":matrix(2.)}],"clips":[],"attachments":[{"name":"hook","bone":0,"matrix":matrix(5.)}]} }),
        json!({"id":2,"name":"child","parent":1,"origin":[1,0,0]}),
    ];
    let mut scripts = load(&assets, &nodes, &Properties::new(), [32.; 2]);
    assert_clean(&scripts);
    let mut f = frame(1., 1.);
    f["poses"] = json!([{"node":0,"time":1,"local":[matrix(4.)],"layers":[]}]);
    scripts.frame(&f).unwrap();
    assert_clean(&scripts);
    assert_eq!(
        scripts
            .context
            .with(|ctx| ctx.eval::<f64, _>("shared.position").unwrap()),
        9.
    );
    let pose = [glam::Mat4::from_translation(glam::Vec3::splat(0.1))];
    scripts.set_poses(std::iter::once((0, pose.as_slice())), &nodes);
    f["time"] = json!(2.);
    f["poses"] = json!([{"node":0,"time":2,"layers":[]}]);
    scripts.frame(&f).unwrap();
    scripts.context.with(|ctx| {
        let x: f64 = ctx.eval("thisScene.getLayer('parent').getLocalBoneOrigin(0).x").unwrap();
        assert_eq!(x, f64::from(0.1_f32));
        let x: f64 = ctx.eval("const m=thisScene.getLayer('parent').getLocalBoneTransform(0);m.m[12]=500;thisScene.getLayer('parent').getLocalBoneOrigin(0).x").unwrap();
        assert_eq!(x, f64::from(0.1_f32));
    });
    assert_clean(&scripts);
}

mod creation_tests {
    use super::{assets, frame, *};
    #[test]
    fn creation_modules_owner_initial_configuration_order_and_deferred_destroy() {
        let (_root, assets) = assets();
        let code = r#"
        shared.created=0;
        shared.parent=thisScene.createLayer({name:'parent',origin:new Vec3(3,4,0),angles:new Vec3(0,0,90)});
        export function init(v){
            const source="export function init(v){shared.created++;if(thisObject!==thisLayer||thisLayer.name!=='child')throw Error('dynamic owner');return v+'!';} export function update(v){return v;} export function destroy(){shared.destroyed=(shared.destroyed||0)+1;}";
            shared.child=thisScene.createLayer({name:'child',parent:shared.parent,text:{value:'中文',script:source},size:new Vec2(20),font:'sans-serif'});
            if(thisLayer.name!=='controller')throw Error('owner was not restored');
            if(shared.child.text!=='中文!'||shared.created!==1)throw Error('created module lifecycle');
            if(shared.child.getParent()!==shared.parent||shared.parent.getChildren()[0]!==shared.child)throw Error('dynamic hierarchy');
            if(thisScene.getInitialLayerConfig(thisLayer).origin.script===undefined)throw Error('original binding was resolved');
            if(thisScene.getInitialLayerConfig(shared.child).text.script!==source)throw Error('created initial configuration');
            if(thisScene.getLayerCount()!==3||!thisScene.sortLayer(shared.child,0)||thisScene.getLayer(0)!==shared.child||thisScene.getLayerIndex(shared.child)!==0)throw Error('dynamic sort');
            return v;
        }
        export function update(v){
            if(engine.runtime>=1&&!shared.requested){
                shared.requested=true;shared.position=shared.child.origin;thisScene.destroyLayer(shared.child);
                if(shared.child.text!=='中文!')throw Error('premature destruction');
            }
            return v;
        }
    "#;
        let objects =
            [json!({"id":1,"name":"controller","origin":{"value":[1,2,3],"script":code}})];
        let mut scripts = Scripts::load(&assets, &objects, &Properties::new(), [100.; 2], 42)
            .unwrap()
            .unwrap();
        assert_clean(&scripts);
        let patch = scripts.flush().unwrap();
        let created = patch
            .as_array()
            .unwrap()
            .iter()
            .filter(|p| p[1] == json!([]))
            .collect::<Vec<_>>();
        assert_eq!(created.len(), 2);
        assert_eq!(
            created[0][2]["values"]["angles"],
            json!([0, 0, std::f64::consts::FRAC_PI_2])
        );
        assert_eq!(created[1][2]["values"]["text"], json!("中文!"));
        scripts.frame(&frame(1., 1.)).unwrap();
        scripts.context.with(|ctx| {
        assert!(ctx.eval::<bool,_>("shared.created===1&&shared.destroyed===1&&thisScene.getLayerCount()===2").unwrap());
        assert!(ctx.eval::<bool,_>("(()=>{try{shared.child.text;return false;}catch(e){return e instanceof ReferenceError;}})()").unwrap());
        assert!(ctx.eval::<bool,_>("(()=>{try{shared.position.x=9;return false;}catch(e){return e instanceof ReferenceError;}})()").unwrap());
    });
        assert_clean(&scripts);
    }

    #[test]
    fn created_script_properties_bind_by_owner_after_interleaved_module_loading() {
        let (_root, assets) = assets();
        let source = r#"
        const childSource="export const scriptProperties=createScriptProperties().addSlider({name:'amount',value:1}).finish();export function applyUserProperties(){thisLayer.alpha=scriptProperties.amount;}";
        shared.dynamic=thisScene.createLayer({name:'dynamic',alpha:{value:1,script:childSource,scriptproperties:{amount:{user:'gain',value:1}}}});
        export function init(v){return v;}
    "#;
        let mut scripts = Scripts::load(
            &assets,
            &[json!({"id":1,"origin":{"value":[0,0,0],"script":source}})],
            &Properties::from([("gain".into(), json!(0.4))]),
            [100.; 2],
            42,
        )
        .unwrap()
        .unwrap();
        assert_clean(&scripts);
        scripts
            .context
            .with(|ctx| assert_eq!(ctx.eval::<f64, _>("shared.dynamic.alpha").unwrap(), 0.4));
        scripts.flush().unwrap();
        scripts
            .set_properties(&Properties::from([("gain".into(), json!(0.7))]))
            .unwrap();
        scripts
            .context
            .with(|ctx| assert_eq!(ctx.eval::<f64, _>("shared.dynamic.alpha").unwrap(), 0.7));
        assert_clean(&scripts);
    }
}

mod model_data_tests {
    use super::{assets, frame, *};
    fn geometry_assets() -> (tempfile::TempDir, Assets) {
        let (root, assets) = assets();
        std::fs::write(
            root.path().join("material.json"),
            b"{\"passes\":[{\"shader\":\"mesh\"}]}",
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
        (root, assets)
    }
    #[test]
    fn custom_typed_arrays_atomic_updates_formats_reentrancy_and_lifetimes() {
        let (_root, assets) = geometry_assets();
        let code = r#"
        const material=engine.registerAsset('material.json',true);
        const vertices=new Float32Array([0,0,0,1,0,0,0,1,0]);
        const shape={vertexBuffer:vertices,vertexFormat:[IModelData.POSITION],material,isVertexBufferDynamic:true};
        export function init(v){
            shared.data=thisScene.createModelData({shapes:[shape,{...shape,indexBuffer:new Uint16Array([0,1,2]),isIndexBufferDynamic:true}],boundingBoxMins:new Vec3(0),boundingBoxMaxs:new Vec3(1)});
            shared.layer=thisScene.createLayer({model:shared.data});
            let rejected=0;
            for(const patch of [
                {vertexBuffer:new Float64Array(9)},
                {vertexBuffer:new Float32Array(12)},
                {vertexBuffer:new Float32Array([NaN,0,0,1,0,0,0,1,0])},
                {vertexFormat:[IModelData.UV,IModelData.POSITION]},
                {indexBuffer:new Uint16Array([0,1,9])}
            ])try{shared.data.applyData(patch);}catch(e){if(e instanceof TypeError)rejected++;}
            try{shared.data.applyData([{vertexBuffer:new Float32Array([9,0,0,1,0,0,0,1,0])},{indexBuffer:new Uint32Array([0,1,2])}]);}catch(e){if(e instanceof TypeError)rejected++;}
            try{shared.data.applyData({get vertexBuffer(){shared.data.replaceData({vertexBuffer:new Float32Array([2,0,0,1,0,0,0,1,0])});return vertices;}});}catch(e){if(e instanceof TypeError)rejected++;}
            if(rejected!==7)throw Error('missing ModelData validation: '+rejected);
            shared.data.replaceData([{},null]);
            return v;
        }
        export function update(v){try{shared.data.replaceData({});throw Error('replaceData was allowed');}catch(e){if(!(e instanceof TypeError))throw e;}return v;}
    "#;
        let mut scripts = Scripts::load(
            &assets,
            &[json!({"id":1,"origin":{"value":[0,0,0],"script":code}})],
            &Properties::new(),
            [32.; 2],
            42,
        )
        .unwrap()
        .unwrap();
        assert_clean(&scripts);
        let data = scripts.model_data.borrow().get(0).unwrap();
        assert_eq!(data.borrow().shapes[0].as_ref().unwrap().vertices[0], 2.);
        assert!(data.borrow().shapes[1].is_none());
        assert_eq!(data.borrow().bounds, Some([[0.; 3], [1.; 3]]));
        scripts.frame(&frame(1., 1.)).unwrap();
        assert_clean(&scripts);
        scripts.context.with(|ctx| {
        assert!(ctx.eval::<bool,_>("(()=>{thisScene.destroyModelData(shared.data);try{shared.data.applyData({});return false;}catch(e){return e instanceof ReferenceError;}})()").unwrap());
        assert!(ctx.eval::<bool,_>("(()=>{try{thisScene.createLayer({model:shared.data});return false;}catch(e){return e instanceof ReferenceError;}})()").unwrap());
    });
        assert!(scripts.model_data.borrow().get(0).is_err());
        assert!(Rc::ptr_eq(
            &data,
            &scripts.model_data.borrow().retained(0).unwrap()
        ));
        drop(data);
        assert!(scripts.model_data.borrow().retained(0).is_err());
    }
    #[test]
    fn custom_static_buffers_and_asset_scope_are_enforced() {
        let (_root, assets) = geometry_assets();
        let code = r#"
        export function init(v){
            const configuration={shapes:[{vertexBuffer:new Float32Array([0,0,0,1,0,0,0,1,0]),vertexFormat:[IModelData.POSITION],material:engine.registerAsset('material.json')}]};
            shared.data=thisScene.createModelData(configuration);
            let caught=0;
            try{shared.data.applyData({vertexBuffer:configuration.shapes[0].vertexBuffer});}catch(e){if(e instanceof TypeError)caught++;}
            try{thisScene.createModelData({shapes:[{...configuration.shapes[0],material:{file:'../outside.json'}}]});}catch(e){if(e instanceof TypeError)caught++;}
            if(caught!==2)throw Error('static buffers or material scope were not enforced');return v;
        }
    "#;
        let scripts = Scripts::load(
            &assets,
            &[json!({"id":1,"origin":{"value":[0,0,0],"script":code}})],
            &Properties::new(),
            [32.; 2],
            42,
        )
        .unwrap()
        .unwrap();
        assert_clean(&scripts);
    }
}

mod texture_tests {
    use super::*;
    use super::{assets, frame};

    #[test]
    fn texture_frame_controls_preserve_duration_and_independent_instances() {
        let (_root, assets) = assets();
        let texture = json!({"loaded":true,"video":false,"durations":[0.25,0.75],"frameCount":2,"duration":1,"rate":1,"position":0,"anchor":0,"playing":true,"joined":true});
        let objects = [
            json!({"id":1,"origin":{"value":[0,0,0],"script":"export function init(){shared.a=thisLayer.getTextureAnimation();shared.b=thisScene.getLayer(1).getTextureAnimation();}"},"__texture":texture}),
            json!({"id":2,"origin":[0,0,0],"__texture":texture}),
        ];
        let mut scripts = load(&assets, &objects, &Properties::new(), [16.; 2]);
        assert_clean(&scripts);
        scripts.context.with(|ctx|ctx.eval::<(),_>("shared.a.setFrame(1);if(shared.a.frameCount!==2||shared.a.duration!==1||shared.a.getFrame()!==1||shared.b.getFrame()!==0)throw Error('frame jump/instance leak');").unwrap());
        scripts.frame(&frame(0.5, 0.5)).unwrap();
        scripts.context.with(|ctx| {
        ctx.eval::<(), _>(
            "if(Math.abs(shared.a.getFrame()-5/3)>1e-8)throw Error('frame duration');shared.a.pause();",
        )
        .unwrap()
    });
        scripts.frame(&frame(10., 9.5)).unwrap();
        scripts.context.with(|ctx| {
        ctx.eval::<(), _>(
            "if(Math.abs(shared.a.getFrame()-5/3)>1e-8||shared.a.isPlaying())throw Error('paused');shared.a.play();shared.a.rate=-1;",
        )
        .unwrap()
    });
        scripts.frame(&frame(10.5, 0.5)).unwrap();
        scripts.context.with(|ctx|ctx.eval::<(),_>("if(shared.a.getFrame()!==1)throw Error('reverse playback');shared.a.setFrame(1.5);if(shared.a.duration!==1||Math.abs(shared.a.getFrame()-1.5)>1e-8)throw Error('fractional frame jump');for(const f of [-1,2,NaN]){let rejected=false;try{shared.a.setFrame(f);}catch(e){rejected=true;}if(!rejected)throw Error('invalid frame');}shared.a.stop();if(shared.a.getFrame()!==0||shared.a.isPlaying())throw Error('stop');shared.a.join();if(shared.a.duration!==1||!shared.a.isPlaying())throw Error('join');").unwrap());
        scripts.frame(&frame(10.625, 0.125)).unwrap();
        scripts.context.with(|ctx|ctx.eval::<(),_>("if(Math.abs(shared.a.getFrame()-7/6)>1e-8)throw Error('join shared clock with negative rate');").unwrap());
        assert_clean(&scripts);
    }
}

mod media_budget_tests {
    //! Check the cumulative handle guard separately from renderer serialization cost.
    use super::*;

    #[test]
    fn destroyed_layers_still_count_toward_the_4096_handle_sound_creation_budget() {
        let (temp, assets) = super::assets();
        std::fs::write(temp.path().join("short.wav"), b"RIFF").unwrap();
        let scripts = load(
            &assets,
            &[json!({"id":1,"origin":{"value":[0,0,0],"script":"export function init(){}"}})],
            &Properties::new(),
            [1.; 2],
        );
        let initial = scripts
            .context
            .with(|ctx| ctx.eval::<usize, _>("__weNodes.length").unwrap());
        // This checks the handle count, not latency under concurrent test load.
        // Infinite-loop tests separately exercise the production 20ms budget.
        for _ in initial..4096 {
            scripts.deadline.set(Instant::now() + LOAD_BUDGET);
            scripts.context.with(|ctx| {
                checked(
                &ctx,
                ctx.eval::<(), _>(
                    "thisScene.destroyLayer(thisScene.createLayer({name:'released'}));__weFlush();",
                ),
            )
            .unwrap();
            });
        }
        scripts.deadline.set(Instant::now() + LOAD_BUDGET);
        scripts.context.with(|ctx| {
            assert_eq!(ctx.eval::<usize, _>("__weNodes.length").unwrap(), 4096);
            assert_eq!(
                ctx.eval::<usize, _>("thisScene.getLayerCount()").unwrap(),
                1
            );
            assert!(
                ctx.eval::<bool, _>(
                    r#"(()=>{
            let rejected=false;
            try{thisScene.createLayer(engine.registerAsset('short.wav'));}
            catch(e){rejected=e instanceof RangeError&&String(e).includes('Scene object budget');}
            return rejected&&__weNodes.length===4096&&thisScene.getLayerCount()===1;
        })()"#
                )
                .unwrap()
            );
        });
        assert_clean(&scripts);
    }
}

fn load(assets: &Assets, objects: &[Value], properties: &Properties, screen: [f32; 2]) -> Scripts {
    Scripts::load(assets, objects, properties, screen, 0)
        .unwrap()
        .unwrap()
}

fn assert_clean(scripts: &Scripts) {
    assert!(
        scripts.diagnostics().is_empty(),
        "{:?}",
        scripts.diagnostics()
    );
}
