mod scene_tests {
    use super::*;

    pub(super) mod rust_scene_tests {
        use super::*;

        #[test]
        #[ignore = "requires EGL/GLES and a real WE scene"]
        fn real_scene_pixels_pause_and_resume() {
            let root = std::path::PathBuf::from(
                std::env::var_os("WALLPAPERD_TEST_RUST_SCENE").expect("scene path"),
            );
            let gpu = Gpu::headless(crate::pixels::Size::new(96, 54).unwrap()).unwrap();
            let common =
                crate::catalog::we_assets(&crate::store::WallpaperEngine::default()).unwrap();
            let mut renderer = we_scene::Renderer::load(
                gpu.gl.clone(),
                &root,
                &root.join("scene.pkg"),
                Some(&common),
            )
            .unwrap();
            let (_, wake) = UnixStream::pair().unwrap();
            wake.set_nonblocking(true).unwrap();
            renderer
                .attach_media(
                    mpv_context(&gpu, crate::domain::Fit::Stretch),
                    &wake,
                    wallpaper_media::Playback {
                        paused: false,
                        mute: true,
                        volume: 0.,
                    },
                )
                .unwrap();
            gpu.reset();
            // Prewarm after authored opening overlays, without advancing decoder time.
            let mut clock = Clock::new(false);
            clock.seek(Duration::from_secs(4));
            clock.pause(true);
            let mut content = Content {
                engine: Engine::RustScene {
                    renderer: Box::new(renderer),
                    clock,
                    fit: we_scene::Fit::Stretch,
                },
                target: None,
                dirty: true,
                paused: false,
                ready: false,
                failed: false,
                mouse: Mouse::default(),
            };
            let render_ready = |content: &mut Content| {
                let deadline = Instant::now() + Duration::from_secs(10);
                loop {
                    content.poll().unwrap();
                    content.render(&gpu).unwrap();
                    if content.ready() {
                        break;
                    }
                    assert!(Instant::now() < deadline, "real scene frame timeout");
                    std::thread::sleep(Duration::from_millis(5));
                }
            };
            render_ready(&mut content);
            if let Engine::RustScene { clock, .. } = &mut content.engine {
                clock.pause(false);
            }
            let first = read(&gpu, &content);
            assert!(first.as_chunks::<4>().0.iter().all(|p| p[3] == 255));
            assert!(
                first
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .any(|p| p[0] != p[1] || p[1] != p[2]),
                "scene must contain colored pixels"
            );
            content
                .playback(Playback {
                    paused: true,
                    ..Playback::default()
                })
                .unwrap();
            std::thread::sleep(Duration::from_millis(80));
            assert!(!content.render(&gpu).unwrap());
            assert_eq!(first, read(&gpu, &content));
            content.playback(Playback::default()).unwrap();
            std::thread::sleep(Duration::from_millis(500));
            if !content.animated() {
                assert!(!content.render(&gpu).unwrap());
                assert_eq!(first, read(&gpu, &content));
                return;
            }
            // A resumed embedded clip may need to decode its new timeline frame.
            if let Engine::RustScene { clock, .. } = &mut content.engine {
                clock.pause(true);
            }
            render_ready(&mut content);
            assert!(
                first != read(&gpu, &content),
                "scene animation did not advance"
            );
        }

        fn read(gpu: &Gpu, content: &Content) -> Vec<u8> {
            content.target().bind();
            let mut pixels = vec![0; (gpu.size.width * gpu.size.height * 4) as usize];
            unsafe {
                gpu.gl.read_pixels(
                    0,
                    0,
                    gpu.size.width as i32,
                    gpu.size.height as i32,
                    glow::RGBA,
                    glow::UNSIGNED_BYTE,
                    glow::PixelPackData::Slice(Some(&mut pixels)),
                );
            }
            gpu.reset();
            pixels
        }
    }

    pub(super) mod animation_tests {
        use super::*;

        #[test]
        #[ignore = "requires EGL/GLES and WE common shaders"]
        fn native_property_timelines_bezier_relative_script_seek_pause_and_values_pixels() {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("scene.pkg");
            let mut pkg = 8u32.to_le_bytes().to_vec();
            pkg.extend(b"PKGV0001");
            pkg.extend(0u32.to_le_bytes());
            std::fs::write(&path, pkg).unwrap();
            let timeline = json!({"value":"8 8 0","animation":{"relative":true,"c0":[{"frame":0,"value":0},{"frame":2,"value":4}],"c1":[{"frame":0,"value":0}],"c2":[{"frame":0,"value":0}],"options":{"name":"move","fps":1,"length":2,"mode":"single","startpaused":true}},"script":"export function init(v){const a=thisObject.getAnimation();if(a.name!=='move'||a.fps!==1||a.frameCount!==2||a.duration!==2||a.isPlaying())throw Error('animation metadata');a.play();return v;}export function update(v){const a=thisLayer.getAnimation('move');if(engine.runtime>=0.5&&engine.runtime<0.6)a.pause();if(engine.runtime>=1&&engine.runtime<1.1){a.setFrame(1);if(a.getFrame()!==1||v.x!==10)throw Error('seek must be immediate');a.play();}return v;}"});
            let scene = json!({"general":{"orthogonalprojection":{"width":16,"height":16}},"objects":[{"id":1,"image":"image.json","size":"2 2","origin":timeline}]});
            for (name, value) in [
                ("scene.json", scene),
                ("image.json", json!({"material":"material.json"})),
                (
                    "material.json",
                    json!({"passes":[{"shader":"genericimage","textures":["util/white"],"blending":"translucent"}]}),
                ),
            ] {
                write_json(root.path().join(name), &value);
            }
            test_support::white(root.path());
            let common =
                crate::catalog::we_assets(&crate::store::WallpaperEngine::default()).unwrap();
            let (gpu, target) = canvas(16, 16);
            let mut renderer =
                we_scene::Renderer::load(gpu.gl.clone(), root.path(), &path, Some(&common))
                    .unwrap();
            let draw = |renderer: &mut we_scene::Renderer, time| {
                renderer
                    .draw([16, 16], Some(target.fbo), we_scene::Fit::Stretch, time)
                    .unwrap();
                gpu.reset();
                gpu.read_image(&target).unwrap()
            };
            let first = draw(&mut renderer, 0.);
            assert_eq!(first.get_pixel(7, 8).0, [255; 4]);
            let moved = draw(&mut renderer, 0.5);
            assert_eq!(moved.get_pixel(7, 8).0, [0, 0, 0, 255]);
            assert_eq!(moved.get_pixel(9, 8).0, [255; 4]);
            assert_eq!(draw(&mut renderer, 0.9), moved);
            let sought = draw(&mut renderer, 1.);
            assert_eq!(sought.get_pixel(10, 8).0, [255; 4]);
            assert_eq!(sought.get_pixel(8, 8).0, [0, 0, 0, 255]);
            let end = draw(&mut renderer, 2.);
            assert_eq!(end.get_pixel(12, 8).0, [255; 4]);
            assert_eq!(end.get_pixel(10, 8).0, [0, 0, 0, 255]);
            renderer.pause(true);
            assert_eq!(draw(&mut renderer, 20.), end);
            assert!(
                renderer.diagnostics().is_empty(),
                "{}",
                renderer.diagnostics()
            );
        }
    }

    pub(super) mod camera_tests {
        use super::*;

        #[test]
        #[ignore = "requires EGL/GLES and WE common assets"]
        fn native_camera_path_fade_boundary_pause_rewind_and_dynamic_pixels() {
            let root = fixture(json!({
                "scene.json": {"general":{"orthogonalprojection":{"width":16,"height":16},"clearcolor":"0.2 0.4 0.6","camerafade":{"value":true,"user":"fade"}},"objects":[{"id":1,"image":"image.json","size":"16 16"},{"id":2,"camera":"default","path":"path.json"}]},
                "image.json": {"material":"material.json"},
                "material.json": {"passes":[{"shader":"genericimage","textures":["util/white"]}]},
                "path.json": {"paths":[{"options":{"fps":1,"length":2}},{"options":{"fps":1,"length":0.5}}]},
            }));
            let (gpu, target) = canvas(16, 16);
            let pixel = |scene: &mut we_scene::Renderer, t| {
                frame(scene, &gpu, &target, t).get_pixel(8, 8).0
            };
            let close = |actual: [u8; 4], expected: [u8; 4]| {
                assert!(
                    actual
                        .into_iter()
                        .zip(expected)
                        .all(|(a, b)| a.abs_diff(b) <= 1),
                    "{actual:?} != {expected:?}"
                )
            };
            let mut scene = rendered(&root, &gpu, &target);
            assert_eq!(pixel(&mut scene, 0.), [255; 4]);
            assert_eq!(pixel(&mut scene, 1.), [255; 4]);
            close(pixel(&mut scene, 2.), [51, 102, 153, 255]);
            close(pixel(&mut scene, 2.25), [83, 126, 169, 255]);
            let held = pixel(&mut scene, 2.25);
            scene.pause(true);
            assert_eq!(pixel(&mut scene, 100.), held);
            scene.pause(false);
            close(pixel(&mut scene, 2.5), [51, 102, 153, 255]);
            close(pixel(&mut scene, 3.), [153, 179, 204, 255]);
            assert_eq!(pixel(&mut scene, 3.5), [255; 4]);
            assert_eq!(pixel(&mut scene, 0.), [255; 4]);
            close(pixel(&mut scene, 2.), [51, 102, 153, 255]);
            set_properties(&mut scene, json!({"fade": false}));
            assert_eq!(pixel(&mut scene, 2.25), [255; 4]);
            set_properties(&mut scene, json!({"fade": true}));
            assert_eq!(
                pixel(&mut scene, 2.375),
                [255; 4],
                "enabling does not replay an old fade"
            );
            close(pixel(&mut scene, 2.5), [51, 102, 153, 255]);
            assert!(scene.errors().is_empty(), "{}", scene.errors());
            let mut off = rendered(&root, &gpu, &target);
            set_properties(&mut off, json!({"fade": false}));
            for t in [0., 2., 2.25, 2.5, 3., 5., 0.] {
                assert_eq!(pixel(&mut off, t), [255; 4]);
            }
        }
    }

    pub(super) mod text_tests {

        use super::*;

        #[test]
        #[ignore = "requires EGL/GLES, system fonts and WE common assets"]
        fn scaled_white_text_has_no_dark_fringes_with_or_without_blur() {
            let (gpu, target) = canvas(400, 160);
            for blurred in [false, true] {
                let mut text = json!({"id":1,"origin":"200 80 0","size":"400 160",
                "text":{"value":"6 OCT 2026","script":"export function update(){return engine.runtime>0.5?'12:34':'6 OCT 2026';}"},
                "font":"sans-serif","pointsize":36,"scale":"0.27716 0.27019 1","padding":32});
                if blurred {
                    text["effects"] = json!([{"file":"effects/blurprecise/effect.json","passes":[
                    {"constantshadervalues":{"scale":"1.33 1.33"}},
                    {"constantshadervalues":{"scale":"1.33 1.33"}}]}]);
                }
                let root = fixture(json!({
                    "scene.json": {"general":{"orthogonalprojection":{"width":400,"height":160},"clearcolor":"0.7 0.7 0.7"},"objects":[text]},
                }));
                let mut scene = rendered(&root, &gpu, &target);
                for time in [0., 1.] {
                    let pixels = scene.frame(time);
                    let background = pixels.get_pixel(0, 0)[0];
                    assert!(
                        pixels.pixels().filter(|p| p[0] > background + 10).count() > 100,
                        "text did not render: blurred={blurred}, time={time}"
                    );
                    let dark = pixels
                        .pixels()
                        .filter(|p| p.0[..3].iter().any(|v| *v < background - 1))
                        .count();
                    assert_eq!(
                        dark, 0,
                        "white glyphs darkened the background: blurred={blurred}, time={time}"
                    );
                }
                assert!(scene.errors().is_empty(), "{}", scene.diagnostics());
            }
        }

        #[test]
        #[ignore = "requires EGL/GLES and system fonts"]
        fn native_text_outline_soft_style_dynamic_vectors_and_anchor_pixels() {
            let text = json!({"id":1,"origin":"40 260 0","size":"2 2","text":{"value":"中文 Ag\n第二行","script":r#"
        export function applyUserProperties(p){
            p=engine.userProperties;
            thisLayer.outline=p.outline??false;
            thisLayer.outlinecolor.y=1;
            thisLayer.outlinethickness=p.width??4;
            thisLayer.blur=p.blur??false;
            thisLayer.blursize=p.radius??6;
            thisLayer.text=p.text??'中文 Ag\n第二行';
        }
    "#},"pointsize":10,"horizontalalign":"left","verticalalign":"top","color":"1 0 0","outlinecolor":"0 0 0"});
            let root = fixture(json!({
                "scene.json": {"general":{"orthogonalprojection":{"width":400,"height":300}},"objects":[text]},
            }));
            let (gpu, target) = canvas(400, 300);
            let mut actual = rendered(&root, &gpu, &target);
            let plain = actual.frame(0.);
            set_properties(&mut actual, json!({"outline": true, "width": 3}));
            let outlined = actual.frame(1.);
            let mut added = 0;
            let mut body = 0;
            for (old, new) in plain.pixels().zip(outlined.pixels()) {
                if old[0] > 253 {
                    assert!(new[0] > 253 && new[1] < 2);
                    body += 1;
                }
                if old[0] == 0 && new[1] > 32 {
                    added += 1;
                }
            }
            assert!(
                body > 100 && added > 100,
                "outline did not expand the glyph silhouette"
            );
            assert_eq!(outlined, actual.frame(1.5));
            actual.set_properties(&Default::default()).unwrap();
            assert_eq!(plain, actual.frame(2.));
            set_properties(&mut actual, json!({"blur": true, "radius": 6}));
            let soft = actual.frame(3.);
            assert!(
                plain
                    .pixels()
                    .zip(soft.pixels())
                    .filter(|(a, b)| a[0] == 0 && b[0] > 1)
                    .count()
                    > 100,
                "blur did not affect expected exterior pixels"
            );
            set_properties(
                &mut actual,
                json!({"outline": true, "text": "动态中文\nAg\n多行"}),
            );
            assert_ne!(outlined, actual.frame(4.));
            assert!(actual.errors().is_empty(), "{}", actual.diagnostics());
            drop(actual);
            assert_gl_clean(&gpu);
        }
    }
}

mod image_tests {
    use super::*;

    pub(super) mod blending_tests {
        use super::*;

        pub(in crate::content) fn base(root: &tempfile::TempDir) {
            std::fs::create_dir_all(root.path().join("shaders")).unwrap();
            std::fs::write(root.path().join("shaders/color.vert"), b"attribute vec3 a_Position;attribute vec2 a_TexCoord;varying vec2 uv;uniform mat4 g_ModelViewProjectionMatrix;void main(){uv=a_TexCoord;gl_Position=mul(vec4(a_Position,1),g_ModelViewProjectionMatrix);}").unwrap();
            std::fs::write(root.path().join("shaders/color.frag"), b"varying vec2 uv;uniform sampler2D g_Texture0;uniform vec4 g_Color4;uniform float g_Alpha;uniform float g_Brightness;void main(){gl_FragColor=texSample2D(g_Texture0,uv)*g_Color4*vec4(vec3(g_Brightness),g_Alpha);}").unwrap();
        }

        #[test]
        #[ignore = "requires EGL/GLES and WE common assets"]
        fn authored_uniforms_survive_program_switches_and_property_changes() {
            let root = fixture(json!({
                "scene.json": {"general":{"orthogonalprojection":{"width":4,"height":4}},"objects":[
                        {"id":1,"image":"left.json","origin":"1 2 0","size":"2 4"},
                        {"id":2,"image":"right.json","origin":"3 2 0","size":"2 4"}
                    ]},
                "left.json": {"material":"left-material.json"},
                "right.json": {"material":"right-material.json"},
                "left-material.json": {"passes":[{"shader":"constants","textures":["util/white"],"constantshadervalues":{"tint":{"user":"tint","value":"1 0 0"}}}]},
                "right-material.json": {"passes":[{"shader":"constants","textures":["util/white"],"constantshadervalues":{"tint":"0 0 1"}}]},
            }));
            base(&root);
            std::fs::rename(
                root.path().join("shaders/color.vert"),
                root.path().join("shaders/constants.vert"),
            )
            .unwrap();
            std::fs::write(root.path().join("shaders/constants.frag"), b"varying vec2 uv;uniform sampler2D g_Texture0;\nuniform vec3 g_Tint; // {\"material\":\"tint\",\"default\":\"1 0 0\"}\nuniform float g_Time; // {\"material\":\"time\",\"default\":1}\nvoid main(){gl_FragColor=texSample2D(g_Texture0,uv)*vec4(g_Tint*g_Time,1);}").unwrap();
            let (gpu, target) = canvas(4, 4);
            let mut scene = rendered(&root, &gpu, &target);
            for time in [0., 0.1, 0.2] {
                let pixels = scene.frame(time);
                assert_eq!(pixels.get_pixel(0, 2).0, [255, 0, 0, 255]);
                assert_eq!(pixels.get_pixel(3, 2).0, [0, 0, 255, 255]);
            }
            set_properties(&mut scene, json!({"tint": [0, 1, 0]}));
            for time in [0.3, 0.4] {
                let pixels = scene.frame(time);
                assert_eq!(pixels.get_pixel(0, 2).0, [0, 255, 0, 255]);
                assert_eq!(pixels.get_pixel(3, 2).0, [0, 0, 255, 255]);
            }
            assert!(
                scene
                    .set_properties(&we_scene::Properties::from([(
                        "tint".into(),
                        json!("invalid")
                    )]))
                    .is_err()
            );
            let pixels = scene.frame(0.5);
            assert_eq!(pixels.get_pixel(0, 2).0, [0, 255, 0, 255]);
            assert!(scene.diagnostics().is_empty(), "{}", scene.diagnostics());
        }

        #[test]
        #[ignore = "requires EGL/GLES and WE common assets"]
        fn native_padded_image_matches_effect_pass_pixels() {
            let root = fixture(json!({
                "scene.json": {"general":{"orthogonalprojection":{"width":8,"height":4}},"objects":[
                        {"id":1,"image":"image.json","origin":"2 2 0","size":"4 4"},
                        {"id":2,"image":"image.json","origin":"6 2 0","size":"4 4","effects":[{"file":"effect.json"}]}
                    ]},
                "image.json": {"material":"material.json"},
                "material.json": {"passes":[{"shader":"color","textures":["source"],"blending":"normal"}]},
                "effect.json": {"passes":[{"material":"copy.json"}]},
                "copy.json": {"passes":[{"shader":"color","blending":"normal"}]},
            }));
            base(&root);
            std::fs::write(
            root.path().join("shaders/color.frag"),
            b"varying vec2 uv;uniform sampler2D g_Texture0;void main(){gl_FragColor=texSample2D(g_Texture0,uv);}",
        )
        .unwrap();
            let mut texture = tex_header([0u32, 1, 8, 8, 4, 4, 0], [1u32, 1, 8, 8, 8 * 8 * 4]);
            for y in 0..8 {
                for x in 0..8 {
                    texture.extend(if x >= 4 || y >= 4 {
                        [0, 0, 255, 255]
                    } else if y < 2 {
                        [255, 0, 0, 255]
                    } else {
                        [0, 255, 0, 255]
                    });
                }
            }
            std::fs::write(root.path().join("materials/source.tex"), texture).unwrap();
            let (gpu, target) = canvas(8, 4);
            let mut scene = rendered(&root, &gpu, &target);
            let pixels = scene.frame(0.);
            for y in 0..4 {
                for x in 0..4 {
                    let expected = if y < 2 {
                        [255, 0, 0, 255]
                    } else {
                        [0, 255, 0, 255]
                    };
                    assert_eq!(pixels.get_pixel(x + 4, y).0, expected);
                    assert_eq!(
                        pixels.get_pixel(x, y).0,
                        expected,
                        "direct image must crop texture padding at ({x}, {y})"
                    );
                }
            }
        }

        #[test]
        #[ignore = "requires EGL/GLES and WE common assets"]
        fn native_layer_blending_all_we_modes_alpha_properties_and_pause_pixels() {
            let mut objects =
                vec![json!({"id":1000,"image":"image.json","size":"66 4","color":"0.2 0.6 0.8"})];
            for mode in 0..=32 {
                objects.push(json!({"id":mode+1,"image":"image.json","origin":format!("{} 2 0",mode*2+1),"size":"2 4","color":"0.65 0.25 0.5","alpha":0.37,"colorBlendMode":if mode==32 {json!({"user":"mode","value":32})} else {json!(mode)}}));
            }
            let root = fixture(json!({
                "scene.json": {"general":{"orthogonalprojection":{"width":66,"height":4}},"objects":objects},
                "image.json": {"material":"color.json"},
                "color.json": {"passes":[{"shader":"color","textures":["util/white"],"blending":"translucent"}]},
            }));
            base(&root);
            let reference = fixture(json!({}));
            std::fs::create_dir_all(reference.path().join("shaders")).unwrap();
            std::fs::copy(
                root.path().join("shaders/color.vert"),
                reference.path().join("shaders/reference.vert"),
            )
            .unwrap();
            // Independently compile the unmodified shipped header's constant-mode path.
            // Account for the scene's RGBA8 layer storage before applying its formula.
            std::fs::write(reference.path().join("shaders/reference.frag"),b"#include \"common_blending.h\"\nvoid main(){gl_FragColor=vec4(ApplyBlending(BLENDMODE,vec3(51,153,204)/255.0,vec3(166,64,128)/255.0,94.0/255.0),1);}").unwrap();
            let mut expected_objects = Vec::new();
            for mode in 0..=32 {
                let model = format!("reference-{mode}.json");
                let material = format!("material-{mode}.json");
                write_json(reference.path().join(&model), &json!({"material":material}));
                write_json(
                    reference.path().join(&material),
                    &json!({"passes":[{"shader":"reference","combos":{"BLENDMODE":mode},"textures":["util/white"]}]}),
                );
                expected_objects.push(
                json!({"id":mode+1,"image":model,"origin":format!("{} 2 0",mode*2+1),"size":"2 4"}),
            );
            }
            write_json(
                reference.path().join("scene.json"),
                &json!({"general":{"orthogonalprojection":{"width":66,"height":4}},"objects":expected_objects}),
            );
            let (gpu, target) = canvas(66, 4);
            let mut expected = renderer(&reference, &gpu);
            let reference_pixels = frame(&mut expected, &gpu, &target, 0.);
            let mut scene = rendered(&root, &gpu, &target);
            let pixels = scene.frame(0.);
            for mode in 0..=32 {
                let actual = pixels.get_pixel(mode * 2 + 1, 2).0;
                let expected = reference_pixels.get_pixel(mode * 2 + 1, 2).0;
                assert!(
                    actual.iter().zip(expected).all(|(a, b)| a.abs_diff(b) <= 2),
                    "WE blend mode {mode}: {actual:?}, reference {expected:?}"
                );
            }
            scene.pause(true);
            scene
                .set_properties(&std::collections::BTreeMap::from([(
                    "mode".into(),
                    json!(31),
                )]))
                .unwrap();
            assert_eq!(pixels, scene.frame(10.));
            scene.pause(false);
            let changed = scene.frame(0.1);
            assert_eq!(changed.get_pixel(65, 2), pixels.get_pixel(63, 2));
            assert!(
                scene
                    .set_properties(&std::collections::BTreeMap::from([(
                        "mode".into(),
                        json!(33)
                    )]))
                    .is_err()
            );
            assert_eq!(changed, scene.frame(0.2));
            assert!(scene.errors().is_empty(), "{}", scene.errors());
            drop(scene);
            drop(expected);
            assert_gl_clean(&gpu);
        }
    }
}

mod lighting_render_tests {
    use super::*;

    pub(super) mod lighting_tests {
        use super::*;

        #[test]
        #[ignore = "requires EGL/GLES and WE common shaders"]
        fn native_lighting_v1_official_pbr_image_properties_exponent_radius_and_pause_pixels() {
            let root = fixture(json!({
                "scene.json": {"general":{"orthogonalprojection":{"width":32,"height":32},"ambientcolor":"0 0 0","skylightcolor":"0 0 0","lightconfig":{"point":1}},"objects":[{"id":1,"image":"image.json","size":"8 8","origin":"16 16 0"},{"id":2,"light":"lpoint","origin":"16 16 8","color":"1 0 0","intensity":{"value":2,"user":"power"},"exponent":{"value":1,"user":"exponent"},"radius":{"value":16,"user":"radius"}}]},
                "image.json": {"material":"material.json"},
                "material.json": {"passes":[{"shader":"genericimage4","textures":["util/white"],"combos":{"LIGHTING":1,"REFLECTION":0},"blending":"normal"}]},
            }));
            let (gpu, target) = canvas(32, 32);
            let mut scene = rendered(&root, &gpu, &target);
            let first = scene.frame(0.);
            let color = first.get_pixel(16, 16).0;
            assert!(
                color[0] > 50 && color[1] < 3 && color[2] < 3,
                "PBR light did not reach the image: {color:?}"
            );
            set_properties(&mut scene, json!({"exponent": 2}));
            let falloff = scene.frame(0.1);
            assert!(
                falloff.get_pixel(16, 16)[0].abs_diff(color[0] / 2) <= 3,
                "falloff must use the light exponent, not inverse square"
            );
            scene.pause(true);
            assert_eq!(scene.frame(10.), falloff);
            scene.pause(false);
            set_properties(&mut scene, json!({"radius": 4}));
            assert_eq!(scene.frame(0.2).get_pixel(16, 16).0, [0, 0, 0, 255]);
            set_properties(&mut scene, json!({"radius": 16, "power": 0}));
            assert_eq!(scene.frame(0.3).get_pixel(16, 16).0, [0, 0, 0, 255]);
            assert!(scene.errors().is_empty(), "{}", scene.diagnostics());
            drop(scene);
            assert_gl_clean(&gpu);
        }
    }

    pub(super) mod shadow_tests {
        use super::*;

        fn shaders(root: &tempfile::TempDir) {
            std::fs::create_dir(root.path().join("shaders")).unwrap();
            for (name, code) in [
                (
                    "lit.vert",
                    r#"attribute vec3 a_Position;attribute vec3 a_Normal;uniform mat4 g_ModelMatrix;uniform mat4 g_ModelViewProjectionMatrix;uniform mat3 g_NormalModelMatrix;varying vec3 position;varying vec3 normal;void main(){position=mul(vec4(a_Position,1),g_ModelMatrix).xyz;normal=mul(a_Normal,g_NormalModelMatrix);gl_Position=mul(vec4(a_Position,1),g_ModelViewProjectionMatrix);}"#,
                ),
                (
                    "lit.frag",
                    "#include \"common_pbr_2.h\"\n#require LightingV1\nvarying vec3 position;varying vec3 normal;void main(){gl_FragColor=vec4(PerformLighting_V1(position,vec3(1),normalize(normal),vec3(0,0,1),vec3(1),vec3(0.04),1.0,0.0),1);}",
                ),
                (
                    "black.vert",
                    "attribute vec3 a_Position;uniform mat4 g_ModelViewProjectionMatrix;void main(){gl_Position=mul(vec4(a_Position,1),g_ModelViewProjectionMatrix);}",
                ),
                ("black.frag", "void main(){gl_FragColor=vec4(0,0,0,1);}"),
            ] {
                std::fs::write(root.path().join("shaders").join(name), code).unwrap();
            }
        }

        fn change(scene: &mut we_scene::Renderer, name: &str, value: serde_json::Value) {
            set_properties(scene, json!({(name): value}));
        }
        fn shadow_textures(gl: &glow::Context) -> [glow::Texture; 2] {
            // The model's lighting pass leaves the reserved shadow samplers bound.
            // Capture actual GLES objects to verify deletion, beyond Usage accounting.
            unsafe {
                let active = gl.get_parameter_i32(glow::ACTIVE_TEXTURE) as u32;
                let textures = [
                    (glow::TEXTURE8, glow::TEXTURE_BINDING_2D_ARRAY),
                    (glow::TEXTURE9, glow::TEXTURE_BINDING_2D),
                ]
                .map(|(unit, binding)| {
                    gl.active_texture(unit);
                    glow::NativeTexture(
                        std::num::NonZeroU32::new(gl.get_parameter_i32(binding) as u32)
                            .expect("allocated shadow sampler"),
                    )
                });
                gl.active_texture(active);
                textures
            }
        }

        fn transparent_tex(root: &tempfile::TempDir) {
            let mut tex = tex_header([0u32, 2, 2, 2, 2, 2, 0], [1u32, 1, 2, 2, 16]);
            tex.extend([0; 16]);
            std::fs::create_dir_all(root.path().join("materials")).unwrap();
            std::fs::write(root.path().join("materials/transparent.tex"), tex).unwrap();
        }
        fn fixture_shadow(kind: &str) -> tempfile::TempDir {
            std::fs::create_dir_all("/tmp/wallpaperd-shadow-native-20261005").unwrap();
            let script = r#"
        let data,caster,parent,light;
        function shape(r){return {vertexBuffer:new Float32Array([-r,-r,0,r,-r,0,-r,r,0,r,r,0]),indexBuffer:new Uint16Array([0,1,2,2,1,3]),vertexFormat:[IModelData.POSITION],material:engine.registerAsset('black.json'),isVertexBufferDynamic:true};}
        export function init(v){
            const receiver=thisScene.createModelData({shapes:[{...shape(32),material:engine.registerAsset('lit.json')}]});
            thisScene.createLayer({model:receiver,origin:new Vec3(32,32,0),castshadow:false});
            data=thisScene.createModelData({shapes:[shape(3)]});
            parent=thisScene.createLayer({origin:new Vec3(32,32,8)});
            caster=thisScene.createLayer({model:data,parent});
            light=thisScene.getLayer('light');
            return v;
        }
        export function applyUserProperties(v){
            if(typeof v.lightX==='number')light.origin.x=v.lightX;
            if(typeof v.lightShadow==='boolean')light.castshadow=v.lightShadow;
            if(typeof v.casterShadow==='boolean')caster.castshadow=v.casterShadow;
            if(typeof v.hide==='boolean')caster.visible=!v.hide;
            if(typeof v.parentX==='number')parent.origin.x=v.parentX;
            if(typeof v.angle==='number')light.angles.y=v.angle;
            if(typeof v.geometryX==='number'){let s=shape(3);for(let i=0;i<s.vertexBuffer.length;i+=3)s.vertexBuffer[i]+=v.geometryX;data.applyData({vertexBuffer:s.vertexBuffer});}
            if(v.releaseData)thisScene.destroyModelData(data);
            if(v.destroy)thisScene.destroyLayer(caster);
        }
    "#;
            let root = fixture(json!({
                "scene.json": {"general":{"orthogonalprojection":{"width":64,"height":64},"ambientcolor":"0 0 0","skylightcolor":"0 0 0","lightconfig":{kind:1,format!("{kind}shadow"):1}},"objects":[{"id":1,"origin":{"value":[0,0,0],"script":script}},{"id":2,"name":"light","light":format!("l{kind}"),"castshadow":true,"origin":"16 32 24","angles":if kind=="directional" {"0 -0.785398163 0"} else {"0 -0.3 0"},"radius":128,"intensity":1,"exponent":1,"innercone":60,"outercone":80}]},
                "lit.json": {"passes":[{"shader":"lit","depthtest":"enabled","depthwrite":"enabled"}]},
                "black.json": {"passes":[{"shader":"black","depthtest":"enabled","depthwrite":"enabled"}]},
            }));
            shaders(&root);
            root
        }

        #[test]
        #[ignore = "requires EGL/GLES and WE common shaders"]
        fn native_shadow_point_spot_directional_occlusion_movement_switches_geometry_parent_pause_release_pixels()
         {
            let (gpu, target) = canvas(64, 64);
            for kind in ["point", "spot", "directional"] {
                let root = fixture_shadow(kind);
                let mut scene = rendered(&root, &gpu, &target);
                let shadow = scene.frame(0.);
                let used = scene.resource_bytes();
                let textures = shadow_textures(&gpu.gl);
                assert!(textures.iter().all(|&t| unsafe { gpu.gl.is_texture(t) }));
                change(&mut scene, "lightShadow", json!(false));
                let lit = scene.frame(0.1);
                assert!(
                    textures.iter().all(|&t| unsafe { !gpu.gl.is_texture(t) }),
                    "{kind} shadow textures were not deleted"
                );

                let base = scene.resource_bytes();
                assert_eq!(
                    used - base,
                    (if kind == "point" { 6 } else { 1 }) * (512 * 512 * 4 + 64),
                    "{kind} resource accounting"
                );
                for (x, y) in [(40, 32), (41, 31)] {
                    let a = shadow.get_pixel(x, y)[0];
                    let b = lit.get_pixel(x, y)[0];

                    assert!(
                        b > 35 && a < b / 3,
                        "{kind} lacks an occluded receiver: {a}/{b}"
                    );
                }
                assert_eq!(
                    shadow.get_pixel(52, 32),
                    lit.get_pixel(52, 32),
                    "{kind} unoccluded receiver"
                );
                change(&mut scene, "lightShadow", json!(true));
                assert_eq!(scene.frame(0.2), shadow);
                change(&mut scene, "casterShadow", json!(false));
                assert_eq!(scene.frame(0.3), lit);
                assert_eq!(scene.resource_bytes(), base);
                change(&mut scene, "casterShadow", json!(true));
                if kind == "directional" {
                    change(&mut scene, "angle", json!(45));
                } else {
                    change(&mut scene, "lightX", json!(48));
                }
                let moved = scene.frame(0.4);

                assert!(
                    moved.get_pixel(40, 32)[0] > 35 && moved.get_pixel(24, 32)[0] < 10,
                    "{kind} light movement did not move shadow"
                );
                if kind == "directional" {
                    change(&mut scene, "angle", json!(-45));
                } else {
                    change(&mut scene, "lightX", json!(16));
                }
                change(&mut scene, "parentX", json!(24));
                let moved = scene.frame(0.5);
                assert!(
                    moved.get_pixel(40, 32)[0] > 35
                        && moved.get_pixel(if kind == "directional" { 32 } else { 28 }, 32)[0] < 10,
                    "{kind} parent movement"
                );
                change(&mut scene, "geometryX", json!(8));
                let dynamic = scene.frame(0.6);
                assert!(
                    dynamic.get_pixel(40, 32)[0] < 10,
                    "{kind} dynamic geometry not in shadow pass"
                );
                scene.pause(true);
                assert_eq!(scene.frame(20.), dynamic);
                scene.pause(false);
                change(&mut scene, "releaseData", json!(true));
                assert_eq!(
                    scene.frame(0.7),
                    dynamic,
                    "referenced ModelData stays alive"
                );
                change(&mut scene, "hide", json!(true));
                let hidden = scene.frame(0.8);
                assert!(hidden.get_pixel(40, 32)[0] > 35);
                assert_eq!(scene.resource_bytes(), base);
                change(&mut scene, "hide", json!(false));
                assert_eq!(scene.frame(0.9), dynamic);
                change(&mut scene, "destroy", json!(true));
                let destroyed = scene.frame(1.);
                assert!(destroyed.get_pixel(40, 32)[0] > 35);
                assert!(scene.resource_bytes() < base);
                assert!(scene.errors().is_empty(), "{kind}: {}", scene.errors());
                drop(scene);
                assert_gl_clean(&gpu);
            }
        }

        #[test]
        #[ignore = "requires EGL/GLES and WE common shaders"]
        fn native_shadow_alpha_coverage_material_default_and_replacement_pixels() {
            for named in [false, true] {
                let root = fixture_shadow("point");
                let path = root.path().join("scene.json");
                let mut value: serde_json::Value =
                    serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
                let code=value["objects"][0]["origin"]["script"].as_str().unwrap()
        .replace("[-r,-r,0,r,-r,0,-r,r,0,r,r,0]", "[-r,-r,0,0,0,r,-r,0,1,0,-r,r,0,0,1,r,r,0,1,1]")
        .replace("[IModelData.POSITION]","[IModelData.POSITION,IModelData.UV]")
        .replace("if(v.destroy)","if(v.opaque)data.replaceData({material:engine.registerAsset('opaque.json')});if(v.destroy)");
                value["objects"][0]["origin"]["script"] = json!(code);
                if named {
                    value["objects"].as_array_mut().unwrap().push(json!({"id":3,"name":"mask","image":"mask-image.json","size":"2 2","visible":false,"alpha":{"value":0,"user":"maskalpha"}}));
                }
                write_json(path, &value);
                std::fs::write(root.path().join("shaders/black.vert"),"attribute vec3 a_Position;attribute vec2 a_TexCoord;uniform mat4 g_ModelViewProjectionMatrix;varying vec2 uv;void main(){uv=a_TexCoord;gl_Position=mul(vec4(a_Position,1),g_ModelViewProjectionMatrix);}").unwrap();
                std::fs::write(root.path().join("shaders/black.frag"),"// [COMBO] {\"combo\":\"ALPHATOCOVERAGE\",\"default\":1}\nuniform sampler2D g_Texture0;varying vec2 uv;void main(){\n#if ALPHATOCOVERAGE\nif(texSample2D(g_Texture0,uv).a<0.5)discard;\n#endif\ngl_FragColor=vec4(0,0,0,1);}").unwrap();
                write_json(
                    root.path().join("black.json"),
                    &json!({"passes":[{"shader":"black","textures":[if named {"_rt_imageLayerComposite_3_a"} else {"transparent"}],"depthtest":"enabled","depthwrite":"enabled"}]}),
                );
                write_json(
                    root.path().join("opaque.json"),
                    &json!({"passes":[{"shader":"black","textures":["util/white"],"depthtest":"enabled","depthwrite":"enabled"}]}),
                );
                transparent_tex(&root);
                std::fs::write(
                    root.path().join("mask-image.json"),
                    br#"{"material":"mask.json"}"#,
                )
                .unwrap();
                std::fs::write(
                    root.path().join("mask.json"),
                    br#"{"passes":[{"shader":"genericimage","textures":["util/white"]}]}"#,
                )
                .unwrap();
                let (gpu, target) = canvas(64, 64);
                let mut scene = rendered(&root, &gpu, &target);
                let transparent = scene.frame(0.);

                transparent
                    .save("/tmp/wallpaperd-shadow-native-20261005/cutout-transparent.png")
                    .unwrap();
                assert!(
                    transparent.get_pixel(40, 32)[0] > 35,
                    "transparent caster left an opaque shadow"
                );
                change(
                    &mut scene,
                    if named { "maskalpha" } else { "opaque" },
                    if named { json!(1) } else { json!(true) },
                );
                let opaque = scene.frame(0.1);
                assert_eq!(
                    opaque.get_pixel(40, 32)[0],
                    0,
                    "replacement alpha material did not enter shadow pass"
                );
                assert!(scene.errors().is_empty(), "{}", scene.errors());
                drop(scene);
                assert_gl_clean(&gpu);
            }
        }

        #[test]
        #[ignore = "requires EGL/GLES and WE common shaders"]
        fn native_shadow_disabled_defaults_tube_wide_spot_budget_failure_keep_presentation_pixels()
        {
            let root = fixture_shadow("spot");
            let path = root.path().join("scene.json");
            let mut value: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            value["objects"][1]
                .as_object_mut()
                .unwrap()
                .remove("castshadow");
            write_json(&path, &value);
            let (gpu, target) = canvas(64, 64);
            let mut scene = rendered(&root, &gpu, &target);
            let lit = scene.frame(0.);
            let base = scene.resource_bytes();
            assert!(lit.get_pixel(40, 32)[0] > 35);
            change(&mut scene, "lightShadow", json!(true));
            assert_eq!(scene.frame(0.1).get_pixel(40, 32)[0], 0);
            assert_eq!(scene.resource_bytes() - base, 512 * 512 * 4 + 64);
            drop(scene);
            value["objects"][1]["castshadow"] = json!(true);
            value["objects"][1]["outercone"] = json!(120);
            write_json(&path, &value);
            let mut scene = rendered(&root, &gpu, &target);
            let accepted = scene.frame(0.);
            assert_eq!(accepted.get_pixel(40, 32)[0], 0);
            assert_eq!(scene.resource_bytes() - base, 6 * (512 * 512 * 4 + 64));
            let mut crowded = value.clone();
            crowded["general"]["lightconfig"] = json!({"point":15,"spot":15,"directional":15});
            let code=crowded["objects"][0]["origin"]["script"].as_str().unwrap().replace("light=thisScene.getLayer('light');", "for(let i=0;i<100;i++)thisScene.createLayer({model:data,parent});light=thisScene.getLayer('light');");
            crowded["objects"][0]["origin"]["script"] = json!(code);
            crowded["objects"].as_array_mut().unwrap().truncate(1);
            let template = value["objects"][1].clone();
            for (k, kind) in ["point", "spot", "directional"].into_iter().enumerate() {
                for n in 0..15 {
                    let mut l = template.clone();
                    l["id"] = json!(2 + k * 15 + n);
                    l["light"] = json!(format!("l{kind}"));
                    if k != 0 || n != 0 {
                        l["name"] = json!(format!("{kind}{n}"));
                    }
                    crowded["objects"].as_array_mut().unwrap().push(l);
                }
            }
            write_json(&path, &crowded);
            let common =
                crate::catalog::we_assets(&crate::store::WallpaperEngine::default()).unwrap();
            let failure = we_scene::Renderer::load(
                gpu.gl.clone(),
                root.path(),
                &root.path().join("scene.pkg"),
                Some(&common),
            )
            .err()
            .expect("unbounded shadow draws accepted");

            assert!(format!("{failure:#}").contains("shadow draw budget exceeded"));
            assert_eq!(
                scene.frame(0.1),
                accepted,
                "failed scene replacement disturbed accepted presentation"
            );
            drop(scene);
            value["general"]["lightconfig"] = json!({"tube":1});
            value["objects"][1]["light"] = json!("ltube");
            write_json(&path, &value);
            let mut tube = rendered(&root, &gpu, &target);
            let first = tube.frame(0.);
            assert_eq!(
                tube.resource_bytes(),
                base,
                "tube acquired undocumented shadow maps"
            );
            change(&mut tube, "lightShadow", json!(false));
            assert_eq!(tube.frame(0.1), first);
            assert!(tube.errors().is_empty(), "{}", tube.errors());
            drop(tube);
            assert_gl_clean(&gpu);
        }
    }
}
