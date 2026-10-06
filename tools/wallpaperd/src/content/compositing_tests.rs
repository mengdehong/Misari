mod compositing_tests {
    use super::*;

    pub(super) mod effect_tests {
        use super::*;

        #[test]
        #[ignore = "requires EGL/GLES and WE common assets"]
        fn native_transient_fbos_reuse_dead_ranges_and_output_without_aliasing_live_reads() {
            let chain = json!({
                "fbos":[
                    {"name":"a","format":"rgba_backbuffer"},
                    {"name":"b","format":"rgba_backbuffer"},
                    {"name":"c","format":"rgba_backbuffer"},
                    {"name":"d","format":"rgba_backbuffer"},
                    {"name":"unused","format":"rgba_backbuffer"}
                ],
                "passes":[
                    {"command":"copy","source":"previous","target":"a"},
                    {"command":"copy","source":"a","target":"b"},
                    {"command":"copy","source":"b","target":"c"},
                    {"command":"copy","source":"c","target":"d"},
                    {"command":"copy","source":"d","target":"_rt_default"}
                ]
            });
            let root = fixture(json!({
                "scene.json": {"general":{"orthogonalprojection":{"width":6,"height":6}},"objects":[
                        {"id":1,"image":"image.json","size":"6 6","color":"1 0.5 0.25","effects":[{"file":"chain.json"}]},
                        {"id":2,"image":"image.json","size":"6 6","color":"0.25 0.5 1","effects":[{"file":"chain.json"},{"file":"chain.json","visible":{"value":true,"user":"second"}}]}
                    ]},
                "image.json": {"material":"base.json"},
                "base.json": {"passes":[{"shader":"genericimage","textures":["util/white"]}]},
                "chain.json": chain.clone(),
            }));
            let (gpu, target) = canvas(6, 6);
            let mut scene = rendered(&root, &gpu, &target);
            write_json(
                root.path().join("chain.json"),
                &json!({"passes":[{"command":"copy","source":"previous","target":"_rt_default"}]}),
            );
            let mut direct = rendered(&root, &gpu, &target);
            // a/c use the not-yet-written effect output; b/d share one transient.
            // Both layers and all effects share that transient; unused declarations allocate nothing.
            assert_eq!(scene.resource_bytes(), direct.resource_bytes() + 6 * 6 * 4);
            for (time, visible) in [(0., true), (0.1, false), (0.2, true)] {
                let properties = we_scene::Properties::from([("second".into(), json!(visible))]);
                scene.set_properties(&properties).unwrap();
                direct.set_properties(&properties).unwrap();
                let expected = direct.frame(time);
                assert!(expected.get_pixel(3, 3)[2] > 240);
                assert_eq!(scene.frame(time), expected);
                assert_eq!(unsafe { gpu.gl.get_error() }, glow::NO_ERROR);
            }
            scene.pause(true);
            direct.pause(true);
            assert_eq!(scene.frame(10.), direct.frame(10.));
            assert!(scene.errors().is_empty(), "{}", scene.diagnostics());
            let mut mixed = chain;
            mixed["fbos"][1]["format"] = json!("rgba16f");
            write_json(root.path().join("chain.json"), &mixed);
            let mut mixed = rendered(&root, &gpu, &target);
            assert_eq!(
                mixed.resource_bytes(),
                direct.resource_bytes() + 6 * 6 * (8 + 4)
            );
            assert_eq!(mixed.frame(0.), direct.frame(10.));
        }
        #[test]
        #[ignore = "requires EGL/GLES and WE common assets"]
        fn native_output_feedback_cache_and_forward_output_reads_pixels() {
            let root = fixture(json!({
                "scene.json": {"general":{"orthogonalprojection":{"width":4,"height":4}},"objects":[{"id":1,"image":"image.json","size":"4 4","effects":[{"file":"effect.json"}]}]},
                "image.json": {"material":"base.json"},
                "base.json": {"passes":[{"shader":"genericimage","textures":["util/white"]}]},
                "effect.json": {"passes":[{"material":"accumulate.json"}]},
                "accumulate.json": {"passes":[{"shader":"accumulate","textures":["_rt_default"]}]},
            }));
            std::fs::create_dir(root.path().join("shaders")).unwrap();
            std::fs::write(root.path().join("shaders/accumulate.vert"), b"attribute vec3 a_Position;attribute vec2 a_TexCoord;varying vec2 uv;void main(){uv=a_TexCoord;gl_Position=vec4(a_Position,1);}").unwrap();
            std::fs::write(root.path().join("shaders/accumulate.frag"), b"varying vec2 uv;uniform sampler2D g_Texture0;void main(){gl_FragColor=vec4(texSample2D(g_Texture0,uv).rgb+vec3(0.125),1);}").unwrap();
            let (gpu, target) = canvas(4, 4);
            let mut feedback = rendered(&root, &gpu, &target);
            let bytes = feedback.resource_bytes();
            write_json(
                root.path().join("effect.json"),
                &json!({"fbos":[{"name":"temporary"}],"passes":[
                    {"command":"copy","source":"previous","target":"_rt_default"},
                    {"command":"copy","source":"_rt_default","target":"temporary"},
                    {"command":"copy","source":"temporary","target":"_rt_default"}
                ]}),
            );
            let mut forward = rendered(&root, &gpu, &target);
            // One retained output/cache equals one transient, not two history copies.
            assert_eq!(bytes, forward.resource_bytes());
            for (time, expected) in [(0., 32u8), (0.1, 64), (0.2, 96)] {
                assert!(feedback.frame(time).get_pixel(2, 2)[0].abs_diff(expected) <= 1);
                assert_eq!(forward.frame(time).get_pixel(2, 2).0, [255; 4]);
            }
            feedback.pause(true);
            let held = feedback.frame(1.);
            assert_eq!(held, feedback.frame(2.));
            feedback.pause(false);
            assert!(feedback.frame(0.3).get_pixel(2, 2)[0].abs_diff(128) <= 1);
            assert!(feedback.errors().is_empty() && forward.errors().is_empty());
        }

        #[test]
        #[ignore = "requires EGL/GLES and WE common assets"]
        fn native_sdr_hdr_bloom_tint_properties_and_resize_pixels() {
            for hdr in [false, true] {
                let root = fixture(json!({
                    "scene.json": {"general":{"orthogonalprojection":{"width":64,"height":64},"hdr":hdr,"bloom":{"value":true,"user":"bloom"},"bloomstrength":2.,"bloomthreshold":0.,"bloomhdrstrength":0.25,"bloomhdrthreshold":0.5,"bloomhdriterations":4,"bloomhdrscatter":0.7,"bloomtint":"1 0 0"},"objects":[{"id":1,"image":"image.json","size":"8 8","brightness":4.}]},
                    "image.json": {"material":"base.json"},
                    "base.json": {"passes":[{"shader":"genericimage","textures":["util/white"]}]},
                }));
                let (gpu, target) = canvas(64, 64);
                let mut scene = rendered(&root, &gpu, &target);
                let glow = scene.frame(0.);
                let bloom_bytes = scene.resource_bytes();
                assert!(
                    glow.get_pixel(20, 32)[0] > glow.get_pixel(20, 32)[2],
                    "hdr={hdr}: bloom must spread red light beyond the source"
                );
                set_properties(&mut scene, json!({"bloom": false}));
                let unlit = scene.frame(0.1);
                assert!(
                    scene.resource_bytes() < bloom_bytes,
                    "disabled bloom retains intermediate storage"
                );
                assert_eq!(unlit.get_pixel(20, 32).0, [0, 0, 0, 255]);
                assert_eq!(unlit.get_pixel(32, 32).0, [255; 4]);
                set_properties(&mut scene, json!({"bloom": true}));
                assert_eq!(glow, scene.frame(0.15));
                scene
                    .draw([32, 32], None, we_scene::Fit::Contain, 0.2)
                    .unwrap();
                assert_gl_clean(&gpu);
                assert!(scene.diagnostics().is_empty(), "{}", scene.diagnostics());
            }
        }

        #[test]
        #[ignore = "requires EGL/GLES and WE common assets"]
        fn native_media_current_previous_fallback_pause_and_release_pixels() {
            let root = fixture(json!({
                "scene.json": {"general":{"orthogonalprojection":{"width":16,"height":8}},"objects":[{"id":1,"image":"current.json","size":"8 8","origin":"4 4 0"},{"id":2,"image":"previous.json","size":"8 8","origin":"12 4 0"}]},
                "current.json": {"material":"current-material.json"},
                "previous.json": {"material":"previous-material.json"},
                "current-material.json": {"passes":[{"shader":"genericimage","textures":["util/white"],"usertextures":[{"name":"$mediaThumbnail","type":"system"}]}]},
                "previous-material.json": {"passes":[{"shader":"genericimage","textures":["util/white"],"usertextures":[{"name":"$mediaPreviousThumbnail","type":"system"}]}]},
            }));
            let red = root.path().join("red.png");
            let blue = root.path().join("blue.png");
            image::RgbaImage::from_pixel(2, 2, image::Rgba([255, 0, 0, 255]))
                .save(&red)
                .unwrap();
            image::RgbaImage::from_pixel(2, 2, image::Rgba([0, 0, 255, 255]))
                .save(&blue)
                .unwrap();
            let (gpu, target) = canvas(16, 8);
            let mut scene = rendered(&root, &gpu, &target);
            assert_eq!(scene.frame(0.).get_pixel(4, 4).0, [255; 4]);
            scene.set_media(
                json!({"enabled":true,"artwork":{"path":red},"previous_artwork":{"path":blue}}),
            );
            let first = scene.frame(0.1);
            assert_eq!(first.get_pixel(4, 4).0, [255, 0, 0, 255]);
            assert_eq!(first.get_pixel(12, 4).0, [0, 0, 255, 255]);
            scene.pause(true);
            scene.set_media(
                json!({"enabled":true,"artwork":{"path":blue},"previous_artwork":{"path":red}}),
            );
            assert_eq!(first, scene.frame(10.));
            scene.pause(false);
            let second = scene.frame(0.2);
            assert_eq!(second.get_pixel(4, 4).0, [0, 0, 255, 255]);
            assert_eq!(second.get_pixel(12, 4).0, [255, 0, 0, 255]);
            scene.set_media(json!({"enabled":false}));
            let fallback = scene.frame(0.3);
            assert_eq!(fallback.get_pixel(4, 4).0, [255; 4]);
            assert_eq!(fallback.get_pixel(12, 4).0, [255; 4]);
            assert!(scene.diagnostics().is_empty(), "{}", scene.diagnostics());
            drop(scene);
            assert_gl_clean(&gpu);
        }

        #[test]
        #[ignore = "requires EGL/GLES and system fonts"]
        fn native_hidden_text_defers_gpu_storage_and_renders_latest_value_on_show() {
            let root = fixture(json!({
                "scene.json": {"general":{"orthogonalprojection":{"width":400,"height":300}},"objects":[{
                        "id":1,"origin":"200 150 0","size":"400 300",
                        "text":{"user":"text","value":"HIDDEN WALLPAPER TEXT"},
                        "pointsize":84,"visible":{"user":"show","value":false},
                        "effects":[{"file":"copy.json"}]
                    }]},
                "copy.json": {"passes":[{"command":"copy","source":"previous","target":"_rt_default"}]},
            }));
            let (gpu, target) = canvas(400, 300);
            let mut actual = rendered(&root, &gpu, &target);
            let hidden_bytes = actual.resource_bytes();
            assert!(
                hidden_bytes < 64 * 1024,
                "hidden text allocated {hidden_bytes} bytes"
            );
            set_properties(&mut actual, json!({"text": "A"}));
            assert!(actual.frame(0.).pixels().all(|p| p[0] == 0));
            assert_eq!(actual.resource_bytes(), hidden_bytes);
            let properties = we_scene::Properties::from([
                ("text".into(), json!("A")),
                ("show".into(), json!(true)),
            ]);
            actual.set_properties(&properties).unwrap();
            let shown = actual.frame(1.);
            assert!(shown.pixels().any(|p| p[0] > 64));
            assert!(actual.resource_bytes() > hidden_bytes);
            let mut expected = rendered(&root, &gpu, &target);
            expected.set_properties(&properties).unwrap();
            assert_eq!(shown, expected.frame(1.));
        }
    }

    pub(super) mod compose_tests {
        use super::*;
        use serde_json::Value;

        fn composition(objects: Value, extra: Value) -> tempfile::TempDir {
            let mut files = json!({
                "scene.json": {"general":{"orthogonalprojection":{"width":16,"height":16}},"objects":objects},
                "image.json": {"material":"color.json"},
                "color.json": {"passes":[{"shader":"copycolor","blending":"translucent","textures":["util/white"]}]},
                "invert.json": {"passes":[{"material":"inverse.json"}]},
                "inverse.json": {"passes":[{"shader":"inverse"}]},
            });
            files
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            let root = fixture(files);
            std::fs::create_dir(root.path().join("shaders")).unwrap();
            std::fs::write(root.path().join("shaders/copycolor.vert"),b"attribute vec3 a_Position;attribute vec2 a_TexCoord;varying vec2 uv;uniform mat4 g_ModelViewProjectionMatrix;void main(){uv=a_TexCoord;gl_Position=mul(vec4(a_Position,1),g_ModelViewProjectionMatrix);}").unwrap();
            std::fs::write(root.path().join("shaders/copycolor.frag"),b"varying vec2 uv;uniform sampler2D g_Texture0;uniform vec4 g_Color4;uniform float g_Alpha;uniform float g_Brightness;void main(){gl_FragColor=texSample2D(g_Texture0,uv)*g_Color4*vec4(vec3(g_Brightness),g_Alpha);}").unwrap();
            std::fs::write(root.path().join("shaders/inverse.vert"),b"attribute vec3 a_Position;attribute vec2 a_TexCoord;varying vec2 uv;void main(){uv=a_TexCoord;gl_Position=vec4(a_Position,1);}").unwrap();
            std::fs::write(root.path().join("shaders/inverse.frag"),b"varying vec2 uv;uniform sampler2D g_Texture0;void main(){vec4 c=texSample2D(g_Texture0,uv);gl_FragColor=vec4(vec3(1)-c.rgb,c.a);}").unwrap();
            root
        }
        fn pixel(image: &image::RgbaImage, x: u32, y: u32, expected: [u8; 4]) {
            let actual = image.get_pixel(x, y).0;
            assert!(
                actual.iter().zip(expected).all(|(a, b)| a.abs_diff(b) <= 2),
                "({x},{y}): {actual:?}, expected {expected:?}"
            );
        }
        #[test]
        #[ignore = "requires EGL/GLES and WE common assets"]
        fn native_parented_fullscreen_passthrough_preserves_backdrop_pixels() {
            let root = composition(
                json!([
                    {"id":1,"image":"image.json","size":"16 8","origin":"8 12 0","color":"1 0 0"},
                    {"id":2,"image":"image.json","size":"16 8","origin":"8 4 0","color":"0 0 1"},
                    {"id":3,"origin":"10 11 0","scale":"1.5 0.5 1","angles":"0 0 0.3"},
                    {"id":4,"parent":3,"image":"postprocess.json","effects":[{"file":"copy.json","visible":{"user":"effect","value":true}}]}
                ]),
                json!({"postprocess.json": {"fullscreen":true,"material":"materials/util/fullscreenlayer.json"}, "copy.json": {"passes":[{"material":"copy-material.json"}]}, "copy-material.json": {"passes":[{"shader":"clipcopy"}]}}),
            );
            std::fs::write(root.path().join("shaders/clipcopy.vert"), b"attribute vec3 a_Position;attribute vec2 a_TexCoord;varying vec2 uv;void main(){uv=a_TexCoord;gl_Position=vec4(a_Position,1);}").unwrap();
            std::fs::write(root.path().join("shaders/clipcopy.frag"), b"varying vec2 uv;uniform sampler2D g_Texture0;void main(){gl_FragColor=texSample2D(g_Texture0,uv);}").unwrap();
            let mut gpu = Gpu::headless(crate::pixels::Size::new(16, 16).unwrap()).unwrap();
            let mut scene = renderer(&root, &gpu);
            // A zero-strength postprocess must preserve every backdrop pixel, with
            // the same coverage when its effect is bypassed or the output resizes.
            for (output, enabled) in [([16, 16], true), ([32, 16], true), ([32, 16], false)] {
                gpu.resize(crate::pixels::Size::new(output[0], output[1]).unwrap());
                let target = Target::new(gpu.gl.clone(), gpu.size).unwrap();
                set_properties(&mut scene, json!({"effect": enabled}));
                scene
                    .draw(output, Some(target.fbo), we_scene::Fit::Stretch, 4.)
                    .unwrap();
                gpu.reset();
                let pixels = gpu.read_image(&target).unwrap();
                for y in 0..output[1] {
                    for x in 0..output[0] {
                        pixel(
                            &pixels,
                            x,
                            y,
                            if y < 8 {
                                [255, 0, 0, 255]
                            } else {
                                [0, 0, 255, 255]
                            },
                        );
                    }
                }
            }
            assert!(scene.errors().is_empty(), "{}", scene.diagnostics());
        }

        #[test]
        #[ignore = "requires EGL/GLES and WE common assets"]
        fn native_compose_recycles_after_last_read_and_detaches_for_dynamic_child_pixels() {
            let script = r#"export function applyUserProperties(v){thisLayer.setParent(v.inside?thisScene.getLayer('group'):undefined,undefined,false);}"#;
            let root = composition(
                json!([
                    {"id":1,"name":"group","image":"models/util/composelayer.json","size":"6 6","origin":"8 8 0","alpha":0.5,"effects":[{"file":"invert.json"},{"file":"invert.json","visible":{"user":"second","value":false}}]},
                    {"id":2,"parent":1,"image":"image.json","size":"6 6","color":"1 0 0"},
                    {"id":3,"image":"image.json","size":"6 6","origin":{"value":[30,30,0],"script":script},"effects":[{"file":"copy.json"}]}
                ]),
                json!({"copy.json": {"passes":[{"command":"copy","source":"previous","target":"_rt_default"}]}}),
            );
            let (gpu, target) = canvas(16, 16);
            let mut scene = rendered(&root, &gpu, &target);
            let first = scene.frame(0.);
            pixel(&first, 8, 8, [0, 128, 128, 255]);
            let bytes = scene.resource_bytes();
            set_properties(&mut scene, json!({"second": true}));
            pixel(&scene.frame(0.1), 8, 8, [128, 0, 0, 255]);
            assert_eq!(scene.resource_bytes(), bytes);
            set_properties(&mut scene, json!({"inside": true, "second": true}));
            pixel(&scene.frame(0.2), 8, 8, [128, 0, 0, 255]);
            assert_eq!(
                scene.resource_bytes(),
                bytes + 6 * 6 * 4,
                "live child shares scratch; group must detach"
            );
            set_properties(&mut scene, json!({"inside": false, "second": false}));
            assert_eq!(scene.frame(0.3), first);
            assert_eq!(
                scene.resource_bytes(),
                bytes,
                "last child read permits reuse again"
            );
            scene.pause(true);
            assert_eq!(scene.frame(1.), first);
            assert!(scene.errors().is_empty(), "{}", scene.diagnostics());
        }

        #[test]
        #[ignore = "requires EGL/GLES, ffmpeg and libmpv"]
        fn native_compose_text_video_model_inputs_replacement_failure_and_release_pixels() {
            let code = r#"
      let data,m;
      export function init(v){
        data=thisScene.createModelData({shapes:[{vertexBuffer:new Float32Array([-3,-3,0,3,-3,0,-3,3,0,3,3,0]),indexBuffer:new Uint16Array([0,1,2,2,1,3]),vertexFormat:[IModelData.POSITION],material:engine.registerAsset('green.json')}]});
        m=thisScene.createLayer({model:data,parent:thisLayer,origin:new Vec3(-4,-4,0),alpha:0.5});
        return v;
      }
      export function applyUserProperties(v){
        if(v.replace)data.replaceData({material:engine.registerAsset('blue.json')});
        if(v.fail)data.replaceData({material:engine.registerAsset('missing-reference.json')});
        if(v.finished)for(const layer of thisScene.enumerateLayers())thisScene.destroyLayer(layer);
      }
    "#;
            let root = composition(
                json!([
                    {"id":1,"image":"models/util/composelayer.json","size":"16 16","origin":{"value":[8,8,0],"script":code},"effects":[{"file":"invert.json"}]},
                    {"id":2,"parent":1,"image":"movie.json","size":"6 6","origin":"4 4 0"},
                    {"id":3,"parent":1,"text":"A","size":"7 7","origin":"4 -4 0","pointsize":4,"font":"sans-serif","color":"1 0 0","horizontalalign":"center","verticalalign":"center"},
                    {"id":4,"parent":1,"image":"image.json","size":"6 6","origin":"-4 4 0","color":"1 0 0"}
                ]),
                json!({"movie.json": {"material":"movie-material.json"}, "movie-material.json": {"passes":[{"shader":"genericimage","textures":["movie"]}]}, "green.json": {"passes":[{"shader":"green"}]}, "blue.json": {"passes":[{"shader":"blue"}]}, "missing-reference.json": {"passes":[{"shader":"blue","textures":["_rt_imageLayerComposite_missing_a"]}]}}),
            );
            for (name, rgb) in [("green", "0,1,0"), ("blue", "0,0,1")] {
                std::fs::write(root.path().join(format!("shaders/{name}.vert")),b"attribute vec3 a_Position;uniform mat4 g_ModelViewProjectionMatrix;void main(){gl_Position=mul(vec4(a_Position,1),g_ModelViewProjectionMatrix);}").unwrap();
                std::fs::write(
                    root.path().join(format!("shaders/{name}.frag")),
                    format!(
                        "uniform vec4 g_Color4;void main(){{gl_FragColor=vec4({rgb},g_Color4.a);}}"
                    ),
                )
                .unwrap();
            }
            let movie = root.path().join("movie.mp4");
            assert!(
                std::process::Command::new("ffmpeg")
                    .args([
                        "-v",
                        "error",
                        "-f",
                        "lavfi",
                        "-i",
                        "color=red:size=16x16:rate=10",
                        "-t",
                        "0.5",
                        "-c:v",
                        "libx264",
                        "-crf",
                        "0",
                        "-pix_fmt",
                        "yuv444p",
                        "-y"
                    ])
                    .arg(&movie)
                    .status()
                    .unwrap()
                    .success()
            );
            let bytes = std::fs::read(movie).unwrap();
            let mut tex = tex_header(
                [0u32, 34, 16, 16, 16, 16, 0],
                [1u32, 1, 16, 16, bytes.len() as u32],
            );
            tex.extend(bytes);
            std::fs::create_dir_all(root.path().join("materials")).unwrap();
            std::fs::write(root.path().join("materials/movie.tex"), tex).unwrap();
            let (gpu, target) = canvas(16, 16);
            let mut scene = rendered(&root, &gpu, &target);
            let (_, wake) = UnixStream::pair().unwrap();
            wake.set_nonblocking(true).unwrap();
            scene
                .attach_media(
                    crate::content::mpv_context(&gpu, crate::domain::Fit::Stretch),
                    &wake,
                    wallpaper_media::Playback {
                        paused: true,
                        mute: true,
                        volume: 0.,
                    },
                )
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            let pixels = loop {
                scene.poll_media().unwrap();
                let pixels = scene.frame(0.);
                if scene.ready() && pixels.get_pixel(12, 4)[1] > 240 {
                    break pixels;
                }
                assert!(
                    Instant::now() < deadline,
                    "video group failed to become ready: {}",
                    scene.diagnostics()
                );
                std::thread::sleep(Duration::from_millis(5));
            };
            pixel(&pixels, 4, 4, [0, 255, 255, 255]);
            pixel(&pixels, 4, 12, [128, 0, 128, 255]);
            pixel(&pixels, 12, 4, [1, 255, 255, 255]);
            assert!(
                (8..16).any(|x| (8..16).any(|y| {
                    let p = pixels.get_pixel(x, y);
                    p[1] > 20 && p[2] > 20 && p[0] < 3
                })),
                "text did not enter the group effect"
            );
            set_properties(&mut scene, json!({"replace": true}));
            pixel(&scene.frame(0.1), 4, 12, [128, 128, 0, 255]);
            let valid = scene.resource_bytes();
            set_properties(&mut scene, json!({"fail": true}));
            pixel(&scene.frame(0.2), 4, 12, [128, 128, 0, 255]);
            assert_eq!(scene.resource_bytes(), valid);
            assert!(
                scene.errors().contains("ModelData replacement:"),
                "{}",
                scene.diagnostics()
            );
            set_properties(&mut scene, json!({"finished": true}));
            pixel(&scene.frame(0.3), 4, 4, [0, 0, 0, 255]);
            assert_eq!(scene.resource_bytes(), 80);
        }

        #[test]
        #[ignore = "requires EGL/GLES and WE common assets"]
        fn native_compose_invalid_buffer_alias_writer_and_group_rebuild_rollback_pixels() {
            let code = r#"
      export function applyUserProperties(v){if(v.bad)thisScene.createLayer({image:'models/util/composelayer.json',size:new Vec2(6),effects:[{file:'bad'+v.bad+'.json'}]});}
    "#;
            let root = composition(
                json!([
                    {"id":0,"origin":{"value":[0,0,0],"script":code}},
                    {"id":1,"image":"models/util/composelayer.json","size":"12 12","effects":[{"file":"invert.json"}]},
                    {"id":2,"parent":1,"image":"image.json","size":"12 12","color":"1 0 0"}
                ]),
                json!({"bad1.json": {"passes":[{"material":"unknown-alias.json"}]}, "unknown-alias.json": {"passes":[{"shader":"inverse","textures":["_alias_unknown"]}]}, "bad2.json": {"passes":[{"material":"inverse.json","target":"_rt_imageLayerComposite_1_a"},{"material":"inverse.json"}]}, "bad3.json": {"fbos":[{"name":"bad","scale":-1}],"passes":[{"material":"inverse.json","target":"bad"},{"material":"inverse.json"}]}}),
            );
            let (gpu, target) = canvas(16, 16);
            let mut scene = rendered(&root, &gpu, &target);
            let valid = scene.frame(0.);
            let bytes = scene.resource_bytes();
            for bad in 1..=3 {
                set_properties(&mut scene, json!({"bad": bad}));
                assert_eq!(valid, scene.frame(bad as f32 * 0.1));
                assert_eq!(scene.resource_bytes(), bytes);
            }
            assert!(
                scene
                    .errors()
                    .contains("unknown scene texture _alias_unknown"),
                "{}",
                scene.diagnostics()
            );
            assert!(
                scene.errors().contains("writing a shared scene texture"),
                "{}",
                scene.diagnostics()
            );
            assert!(
                scene.errors().contains("invalid FBO scale/fit"),
                "{}",
                scene.diagnostics()
            );
        }
    }
}

mod model_tests {
    use super::*;

    pub(super) mod mdl_tests {
        use super::*;

        fn u32s(data: &mut Vec<u8>, values: &[u32]) {
            for value in values {
                data.extend(value.to_le_bytes());
            }
        }
        fn floats(data: &mut Vec<u8>, values: &[f32]) {
            for value in values {
                data.extend(value.to_le_bytes());
            }
        }
        fn string(data: &mut Vec<u8>, value: &str) {
            data.extend(value.as_bytes());
            data.push(0);
        }
        pub(in crate::content) fn puppet() -> Vec<u8> {
            let mut data = b"MDLV0013\0".to_vec();
            u32s(&mut data, &[0x1800009, 1, 1]);
            string(&mut data, "material.json");
            u32s(&mut data, &[0, 4 * 52]);
            for (x, y, u, v) in [
                (-4., 4., 0., 0.),
                (4., 4., 1., 0.),
                (-4., -4., 0., 1.),
                (4., -4., 1., 1.),
            ] {
                floats(&mut data, &[x, y, 0.]);
                u32s(&mut data, &[1, 0, 0, 0]);
                floats(&mut data, &[1., 0., 0., 0., u, v]);
            }
            u32s(&mut data, &[12]);
            for i in [0u16, 2, 1, 1, 2, 3] {
                data.extend(i.to_le_bytes());
            }
            let skeleton = data.len();
            data.extend(b"MDLS0004\0");
            u32s(&mut data, &[0, 2]);
            for (name, parent) in [("root", u32::MAX), ("child", 0)] {
                string(&mut data, name);
                u32s(&mut data, &[0, parent, 64]);
                floats(
                    &mut data,
                    &[
                        1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1.,
                    ],
                );
                string(&mut data, "");
            }
            let animation = data.len();
            data[skeleton + 9..skeleton + 13].copy_from_slice(&(animation as u32).to_le_bytes());
            data.extend(b"MDLA0006\0");
            u32s(&mut data, &[0, 1, 17, 0]);
            string(&mut data, "move");
            string(&mut data, "single");
            floats(&mut data, &[1.]);
            u32s(&mut data, &[1, 0, 2]);
            for bone in 0..2 {
                u32s(&mut data, &[0, 72]);
                for time in [0., 1.] {
                    floats(
                        &mut data,
                        &[
                            if bone == 1 { time * 8. } else { 0. },
                            0.,
                            0.,
                            0.,
                            0.,
                            if bone == 1 {
                                time * std::f32::consts::FRAC_PI_2
                            } else {
                                0.
                            },
                            1.,
                            1.,
                            1.,
                        ],
                    );
                }
            }
            data.extend([0; 35]);
            let end = data.len() as u32;
            data[animation + 9..animation + 13].copy_from_slice(&end.to_le_bytes());
            data.extend(b"MDAT0001\0");
            u32s(&mut data, &[0]);
            data.extend(1u16.to_le_bytes());
            data.extend(1u16.to_le_bytes());
            string(&mut data, "hook");
            floats(
                &mut data,
                &[
                    1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1.,
                ],
            );
            data
        }

        #[test]
        #[ignore = "requires EGL/GLES"]
        fn native_puppet_euler_skinning_effects_bone_handles_pause_and_corruption_pixels() {
            skeletal_pixels(false);
        }

        #[test]
        #[ignore = "requires EGL/GLES"]
        fn native_3d_model_skeletal_poses_animation_layers_callbacks_and_stale_handles_pixels() {
            skeletal_pixels(true);
        }

        fn skeletal_pixels(model: bool) {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("scene.pkg");
            let mut pkg = 8u32.to_le_bytes().to_vec();
            pkg.extend(b"PKGV0001");
            pkg.extend(0u32.to_le_bytes());
            std::fs::write(&path, pkg).unwrap();
            let mut scene = json!({"general":{"orthogonalprojection":{"width":32,"height":32}},"objects":[{"id":1,"name":"puppet","image":"image.json","size":"8 8","origin":"16 16 0","animationlayers":[{"id":4,"animation":17,"name":"move"}],"alpha":{"value":1,"script":"export function init(){if(thisLayer.getBoneCount()!==2||thisLayer.getBoneIndex('child')!==1||thisLayer.getBoneParentIndex('child')!==0)throw Error('invalid bone metadata');const m=thisLayer.getLocalBoneTransform('child');m.translation(new Vec3(500,0,0));if(thisLayer.getLocalBoneOrigin('child').x!==0)throw Error('matrix is not detached');const temporary=thisLayer.createAnimationLayer('move',{name:'temporary',startpaused:true,visible:false});if(!thisLayer.destroyAnimationLayer(temporary))throw Error('destroy animation');let stale=false;try{temporary.play();}catch(e){stale=e instanceof ReferenceError;}if(!stale)throw Error('stale animation handle');shared.ended=0;shared.once=thisLayer.playSingleAnimation('move',{name:'once',visible:false});shared.once.addEndedCallback(()=>shared.ended++);}export function update(v){if(engine.runtime===1){if(thisLayer.getLocalBoneOrigin('child').x!==8)throw Error('script did not receive animated model pose');if(shared.ended!==1||thisLayer.getAnimationLayerCount()!==1)throw Error('single animation completion: ended='+shared.ended+' count='+thisLayer.getAnimationLayerCount());let stale=false;try{shared.once.getFrame();}catch(e){stale=e instanceof ReferenceError;}if(!stale)throw Error('completed handle still alive');}if(engine.runtime>=2)thisLayer.setLocalBoneOrigin('child',new Vec3(-8,0,0));return v;}"}}]});
            scene["objects"].as_array_mut().unwrap().push(json!({"id":2,"image":"flat.json","size":"2 2","origin":"0 12 0","parent":1,"attachment":"hook"}));
            if model {
                scene["objects"][0].as_object_mut().unwrap().remove("image");
                scene["objects"][0]["model"] = json!("puppet.mdl");
            }
            for (name, value) in [
                ("scene.json", scene),
                ("flat.json", json!({"material":"material.json"})),
                (
                    "image.json",
                    json!({"material":"material.json","puppet":"puppet.mdl"}),
                ),
                (
                    "material.json",
                    json!({"passes":[{"shader":"copy","textures":["white"],"blending":"translucent"}]}),
                ),
            ] {
                write_json(root.path().join(name), &value);
            }
            std::fs::write(root.path().join("puppet.mdl"), puppet()).unwrap();
            std::fs::create_dir(root.path().join("shaders")).unwrap();
            std::fs::write(root.path().join("shaders/copy.vert"),"attribute vec3 a_Position;attribute vec2 a_TexCoord;varying vec2 uv;uniform mat4 g_ModelViewProjectionMatrix;void main(){uv=a_TexCoord;gl_Position=mul(vec4(a_Position,1),g_ModelViewProjectionMatrix);}").unwrap();
            std::fs::write(root.path().join("shaders/copy.frag"),"varying vec2 uv;uniform sampler2D g_Texture0;void main(){gl_FragColor=texSample2D(g_Texture0,uv);}").unwrap();
            let mut tex = b"TEXV0005\0TEXI0001\0".to_vec();
            u32s(&mut tex, &[0, 0, 1, 1, 1, 1, 0]);
            tex.extend(b"TEXB0001\0");
            u32s(&mut tex, &[1, 1, 1, 1, 4]);
            tex.extend([255u8; 4]);
            std::fs::create_dir_all(root.path().join("materials")).unwrap();
            std::fs::write(root.path().join("materials/white.tex"), tex).unwrap();
            let (gpu, target) = canvas(32, 32);
            let mut renderer =
                we_scene::Renderer::load(gpu.gl.clone(), root.path(), &path, None).unwrap();
            let draw = |renderer: &mut we_scene::Renderer, time| {
                renderer
                    .draw([32, 32], Some(target.fbo), we_scene::Fit::Stretch, time)
                    .unwrap();
                gpu.reset();
                gpu.read_image(&target).unwrap()
            };
            let initial = draw(&mut renderer, 0.);
            assert_eq!(initial.get_pixel(16, 16).0, [255; 4]);
            assert_eq!(initial.get_pixel(24, 16).0, [0, 0, 0, 255]);
            assert_eq!(
                initial.get_pixel(16, 4).0,
                [255; 4],
                "attachment bind position"
            );
            let moved = draw(&mut renderer, 1.);
            assert_eq!(moved.get_pixel(24, 16).0, [255; 4]);
            assert_eq!(moved.get_pixel(16, 16).0, [0, 0, 0, 255]);
            assert_eq!(
                moved.get_pixel(12, 16).0,
                [255; 4],
                "child did not follow animated attachment"
            );
            assert_eq!(moved.get_pixel(16, 4).0, [0, 0, 0, 255]);
            renderer.pause(true);
            assert_eq!(draw(&mut renderer, 10.), moved);
            renderer.pause(false);
            let override_frame = draw(&mut renderer, 2.);
            assert!(
                renderer.diagnostics().is_empty(),
                "{}",
                renderer.diagnostics()
            );
            assert_eq!(override_frame.get_pixel(8, 16).0, [255; 4]);
            assert_eq!(override_frame.get_pixel(24, 16).0, [0, 0, 0, 255]);
            assert!(
                renderer.diagnostics().is_empty(),
                "{}",
                renderer.diagnostics()
            );
            drop(renderer);
            let bytes = puppet();
            for length in [0, 8, 20, bytes.len() / 2, bytes.len() - 40] {
                std::fs::write(root.path().join("puppet.mdl"), &bytes[..length]).unwrap();
                assert!(
                    we_scene::Renderer::load(gpu.gl.clone(), root.path(), &path, None).is_err(),
                    "accepted truncated MDL length {length}"
                );
            }
        }
    }

    pub(super) mod model_data_tests {
        use super::*;

        #[test]
        #[ignore = "requires EGL/GLES and WE common assets"]
        fn native_model_data_shared_buffers_apply_replace_pause_failure_destroy_and_reload_pixels()
        {
            let code = r#"
        const material=engine.registerAsset('material.json',true);
        const broken=engine.registerAsset('broken.json',true);
        const format=[IModelData.POSITION,IModelData.COLOR];
        let data,vertices,layers=[];
        function triangle(){return new Float32Array([-4,-4,0,0,0,1,1,4,-4,0,0,0,1,1,0,4,0,0,0,1,1]);}
        export function init(v){
            vertices=new Float32Array([-4,-4,0,1,0,0,1,4,-4,0,1,0,0,1,-4,4,0,1,0,0,1,4,4,0,1,0,0,1]);
            data=thisScene.createModelData({shapes:[{vertexBuffer:vertices,indexBuffer:new Uint16Array([0,1,2,2,1,3]),vertexFormat:format,material,isVertexBufferDynamic:true,isIndexBufferDynamic:true}]});
            for(const x of [8,24])layers.push(thisScene.createLayer({model:data,origin:new Vec3(x,16,0),name:'model-'+x}));
            if(layers[0].getAnimationLayerCount()!==0||layers[0].getBoneCount()!==0)throw Error('custom model host type');
            return v;
        }
        export function update(v){
            if(engine.runtime>=0.1&&!shared.green){shared.green=true;for(let i=0;i<4;i++){vertices[i*7+3]=0;vertices[i*7+4]=1;}data.applyData({vertexBuffer:vertices,indexBuffer:new Uint16Array([0,1,2,2,1,3])});}
            return v;
        }
        export function applyUserProperties(values){
            if(values.shape){data.replaceData({vertexBuffer:triangle(),indexBuffer:null,vertexFormat:format});}
            if(values.fault){data.replaceData({material:broken});}
            if(values.repair){data.replaceData({material});}
            if(values.destroydata&&!shared.destroyed){
                shared.destroyed=true;thisScene.destroyModelData(data);
                try{data.applyData({});throw Error('dead data accepted');}catch(e){if(!(e instanceof ReferenceError))throw e;}
            }
            if(values.finished&&!shared.finished){shared.finished=true;for(const layer of layers)thisScene.destroyLayer(layer);}
        }
    "#;
            let root = fixture(json!({
                "scene.json": {"general":{"orthogonalprojection":{"width":32,"height":32},"hdr":{"user":"hdr","value":false}},"objects":[{"id":1,"origin":{"value":[0,0,0],"script":code}}]},
                "material.json": {"passes":[{"shader":"geometry","cullmode":"normal"},{"shader":"geometry","cullmode":"normal"}]},
                "broken.json": {"passes":[{"shader":"broken"}]},
            }));
            std::fs::create_dir(root.path().join("shaders")).unwrap();
            let vertex=b"attribute vec3 a_Position;attribute vec4 a_Color;varying vec4 color;uniform mat4 g_ModelViewProjectionMatrix;void main(){color=a_Color;gl_Position=mul(vec4(a_Position,1),g_ModelViewProjectionMatrix);}";
            std::fs::write(root.path().join("shaders/geometry.vert"), vertex).unwrap();
            std::fs::write(
                root.path().join("shaders/geometry.frag"),
                b"varying vec4 color;void main(){gl_FragColor=color;}",
            )
            .unwrap();
            std::fs::write(root.path().join("shaders/broken.vert"), vertex).unwrap();
            std::fs::write(
                root.path().join("shaders/broken.frag"),
                b"#error broken geometry candidate",
            )
            .unwrap();
            let (gpu, target) = canvas(32, 32);
            let mut scene = rendered(&root, &gpu, &target);
            let assert_color = |scene: &mut we_scene::Renderer, time, color: [u8; 4]| {
                let pixels = frame(scene, &gpu, &target, time);
                for x in [8, 24] {
                    assert_eq!(
                        pixels.get_pixel(x, 16).0,
                        color,
                        "time {time}; {}",
                        scene.diagnostics()
                    );
                }
            };
            assert_color(&mut scene, 0., [255, 0, 0, 255]);
            assert_eq!(
                scene.resource_bytes(),
                80 + 136 + 32 * 32 * 8,
                "both layers and four material draws share one mesh"
            );
            assert_color(&mut scene, 0.1, [0, 255, 0, 255]);
            scene.pause(true);
            set_properties(&mut scene, json!({"shape": true}));
            assert_color(&mut scene, 10., [0, 255, 0, 255]);
            scene.pause(false);
            assert_color(&mut scene, 0.2, [0, 0, 255, 255]);
            assert_eq!(scene.resource_bytes(), 80 + 84 + 32 * 32 * 8);
            set_properties(&mut scene, json!({"fault": true}));
            assert_color(&mut scene, 0.3, [0, 0, 255, 255]);
            let errors = scene.errors();
            assert!(errors.contains("ModelData replacement:"), "{errors}");
            assert_color(&mut scene, 0.4, [0, 0, 255, 255]);
            assert_eq!(
                scene.errors(),
                errors,
                "failed replacement is diagnosed once"
            );
            set_properties(&mut scene, json!({"repair": true}));
            assert_color(&mut scene, 0.5, [0, 0, 255, 255]);
            set_properties(&mut scene, json!({"destroydata": true}));
            assert_color(&mut scene, 0.6, [0, 0, 255, 255]);
            // Destroying the data handle still leaves live layers owning its CPU/GPU buffers.
            set_properties(&mut scene, json!({"hdr": true}));
            assert_color(&mut scene, 0.7, [0, 0, 255, 255]);
            set_properties(&mut scene, json!({"hdr": false}));
            assert_color(&mut scene, 0.8, [0, 0, 255, 255]);
            set_properties(&mut scene, json!({"finished": true}));
            let pixels = scene.frame(0.9);
            assert_eq!(pixels.get_pixel(8, 16).0, [0, 0, 0, 255]);
            assert_eq!(
                scene.resource_bytes(),
                80,
                "last layer frees shared geometry and model depth/color target"
            );
            assert!(
                !scene.errors().contains("SceneScript"),
                "{}",
                scene.diagnostics()
            );
            drop(scene);
            assert_gl_clean(&gpu);
        }
    }

    pub(super) mod model_picking_tests {
        use super::*;
        use serde_json::Value;

        fn events(name: &str) -> String {
            let mut source = String::from("export function init(v){return v;}\n");
            for event in ["Enter", "Leave", "Move", "Down", "Up", "Click"] {
                source.push_str(&format!("export function cursor{event}(e){{console.log('PICK:'+JSON.stringify({{name:'{name}',event:'{event}',time:engine.runtime,p:[e.localPosition.x,e.localPosition.y,e.localPosition.z]}}));}}\n"));
            }
            source
        }
        fn at(scene: &we_scene::Renderer, time: f64, name: &str) -> Vec<Value> {
            scene
                .diagnostics()
                .lines()
                .filter_map(|line| {
                    let (_, trace) = line.split_once("PICK:")?;
                    let trace: Value = serde_json::from_str(trace).unwrap();
                    (trace["name"] == name && (trace["time"].as_f64().unwrap() - time).abs() < 1e-5)
                        .then_some(trace)
                })
                .collect()
        }
        fn sequence(scene: &we_scene::Renderer, time: f64, name: &str, expected: &[&str]) {
            let found = at(scene, time, name)
                .iter()
                .map(|v| v["event"].as_str().unwrap().to_owned())
                .collect::<Vec<_>>();
            assert_eq!(found, expected, "{}", scene.diagnostics());
            assert!(scene.errors().is_empty(), "{}", scene.diagnostics());
        }
        fn local(scene: &we_scene::Renderer, time: f64, name: &str, expected: [f64; 3]) {
            let trace = at(scene, time, name);
            assert!(!trace.is_empty(), "{}", scene.diagnostics());
            for event in trace {
                for (value, expected) in event["p"].as_array().unwrap().iter().zip(expected) {
                    assert!(
                        (value.as_f64().unwrap() - expected).abs() < 0.002,
                        "{event}"
                    );
                }
            }
        }
        fn geometry(root: &tempfile::TempDir) {
            std::fs::create_dir(root.path().join("shaders")).unwrap();
            std::fs::write(root.path().join("shaders/color.vert"),b"attribute vec3 a_Position;uniform mat4 g_ModelViewProjectionMatrix;void main(){gl_Position=mul(vec4(a_Position,1),g_ModelViewProjectionMatrix);}").unwrap();
            std::fs::write(
                root.path().join("shaders/color.frag"),
                b"uniform vec4 g_Color4;void main(){gl_FragColor=g_Color4;}",
            )
            .unwrap();
        }
        fn change(scene: &mut we_scene::Renderer, key: &str) {
            set_properties(scene, json!({(key): true}));
        }

        #[test]
        #[ignore = "requires EGL/GLES and WE common assets"]
        fn native_model_pointer_bounds_overlap_hierarchy_and_cancel_lifecycle_events() {
            let code = format!(
                r#"
        let b,data,parent;
        const sourceA={a},sourceB={b},sourceC={c};
        export function init(v){{
            parent=thisScene.createLayer({{name:'parent',origin:new Vec3(16,16,0),angles:new Vec3(0,0,90),scale:new Vec3(2,1,1)}});
            const shape={{vertexBuffer:new Float32Array([-2,-2,0,2,-2,0,-2,2,0,2,2,0]),indexBuffer:new Uint16Array([0,1,2,2,1,3]),vertexFormat:[IModelData.POSITION],material:engine.registerAsset('color.json')}};
            data=thisScene.createModelData({{shapes:[shape]}});
            const explicit=thisScene.createModelData({{shapes:[shape],boundingBoxMins:new Vec3(-4,-2,0),boundingBoxMaxs:new Vec3(4,2,0)}});
            thisScene.createLayer({{model:data,parent,solid:true,scale:new Vec3(1,3,1),origin:{{value:new Vec3(1,0,0),script:sourceA}}}});
            b=thisScene.createLayer({{model:explicit,parent,solid:true,perspective:true,scale:new Vec3(1,3,1),origin:{{value:new Vec3(1,0,0),script:sourceB}}}});
            thisScene.createLayer({{model:data,parent,solid:false,origin:{{value:new Vec3(1,0,0),script:sourceC}}}});
            return v;
        }}
        export function applyUserProperties(v){{
            if(v.hide)b.visible=false;
            if(v.show)b.visible=true;
            if(v.singular)b.scale=new Vec3(0,3,1);
            if(v.restore)b.scale=new Vec3(1,3,1);
            if(v.hideparent)parent.visible=false;
            if(v.showparent)parent.visible=true;
            if(v.destroydata)thisScene.destroyModelData(data);
            if(v.destroy)thisScene.destroyLayer(b);
            if(v.new)b=thisScene.createLayer({{model:thisScene.createModelData({{shapes:[{{vertexBuffer:new Float32Array([-4,-2,0,4,-2,0,-4,2,0]),vertexFormat:[IModelData.POSITION],material:engine.registerAsset('color.json')}}]}}),parent,solid:true,origin:{{value:new Vec3(1,0,0),script:sourceB}}}});
        }}
    "#,
                a = json!(events("a")),
                b = json!(events("b")),
                c = json!(events("non-solid"))
            );
            let root = fixture(json!({
                "scene.json": {"general":{"orthogonalprojection":{"width":32,"height":32}},"objects":[{"id":1,"origin":{"value":[0,0,0],"script":code}}]},
                "color.json": {"passes":[{"shader":"color"}]},
            }));
            geometry(&root);
            let (gpu, target) = canvas(32, 32);
            let mut scene = rendered(&root, &gpu, &target);
            scene.pointer(Some([0.5, 0.375]), Some(false));
            scene.frame(0.);
            for name in ["a", "b"] {
                sequence(&scene, 0., name, &["Enter", "Move"]);
                local(&scene, 0., name, [1., 0., 0.]);
            }
            sequence(&scene, 0., "non-solid", &[]);
            scene.pointer(Some([0.5, 0.375]), Some(true));
            scene.frame(0.1);
            for name in ["a", "b"] {
                sequence(&scene, 0.1, name, &["Down"]);
            }
            scene.pointer(Some([0.5, 0.375]), Some(false));
            scene.frame(0.2);
            for name in ["a", "b"] {
                sequence(&scene, 0.2, name, &["Up", "Click"]);
            }
            scene.pointer(Some([0.5, 0.125]), None);
            scene.frame(0.3);
            for name in ["a", "b"] {
                sequence(&scene, 0.3, name, &["Leave"]);
            }
            change(&mut scene, "destroydata");
            scene.pointer(Some([0.5, 0.25]), Some(true));
            scene.frame(0.4);
            sequence(&scene, 0.4, "a", &[]);
            sequence(&scene, 0.4, "b", &["Enter", "Move", "Down"]);
            local(&scene, 0.4, "b", [3., 0., 0.]);
            change(&mut scene, "hide");
            scene.frame(0.5);
            sequence(&scene, 0.5, "b", &["Leave"]);
            change(&mut scene, "show");
            scene.frame(0.6);
            sequence(&scene, 0.6, "b", &["Enter"]);
            scene.pointer(Some([0.5, 0.25]), Some(false));
            scene.frame(0.7);
            sequence(&scene, 0.7, "b", &["Up"]);
            scene.pointer(Some([0.5, 0.25]), Some(true));
            scene.frame(0.8);
            change(&mut scene, "singular");
            scene.frame(0.9);
            sequence(&scene, 0.9, "b", &["Leave"]);
            change(&mut scene, "restore");
            scene.pointer(Some([0.5, 0.25]), Some(false));
            scene.frame(1.);
            sequence(&scene, 1., "b", &["Enter", "Up"]);
            scene.pointer(Some([0.5, 0.25]), Some(true));
            scene.frame(1.1);
            scene.pointer(None, None);
            scene.frame(1.2);
            sequence(&scene, 1.2, "b", &["Leave"]);
            scene.pointer(Some([0.5, 0.25]), Some(true));
            scene.frame(1.3);
            sequence(&scene, 1.3, "b", &["Enter"]);
            scene.pointer(Some([0.5, 0.25]), Some(false));
            scene.frame(1.35);
            sequence(&scene, 1.35, "b", &[]);
            scene.pointer(Some([0.5, 0.25]), Some(true));
            scene.frame(1.4);
            scene.pause(true);
            scene.pointer(Some([0.5, 0.25]), Some(false));
            scene.frame(10.);
            scene.pause(false);
            scene.frame(1.5);
            sequence(&scene, 1.5, "b", &[]);
            scene.pointer(Some([0.5, 0.25]), Some(true));
            scene.frame(1.6);
            change(&mut scene, "destroy");
            scene.frame(1.7);
            sequence(&scene, 1.7, "b", &[]);
            change(&mut scene, "new");
            scene.frame(1.8);
            sequence(&scene, 1.8, "b", &["Enter"]);
            scene.pointer(Some([0.5, 0.25]), Some(false));
            scene.frame(1.9);
            sequence(&scene, 1.9, "b", &["Up"]);
            scene.pointer(Some([0.5, 0.375]), Some(true));
            scene.frame(2.);
            change(&mut scene, "hideparent");
            scene.frame(2.1);
            for name in ["a", "b"] {
                sequence(&scene, 2.1, name, &["Leave"]);
            }
            change(&mut scene, "showparent");
            scene.frame(2.2);
            for name in ["a", "b"] {
                sequence(&scene, 2.2, name, &["Enter"]);
            }
            scene.pointer(Some([0.5, 0.375]), Some(false));
            scene.frame(2.3);
            for name in ["a", "b"] {
                sequence(&scene, 2.3, name, &["Up"]);
            }
        }
    }
}

mod particle_render_tests {
    use super::*;

    pub(super) mod particle_tests {
        use super::*;

        #[test]
        #[ignore = "requires EGL/GLES and WE common shaders"]
        fn particle_child_prewarm_keeps_live_background_animation_advancing() {
            let root = fixture(json!({
                "scene.json": {"general":{"orthogonalprojection":{"width":4,"height":4}},"objects":[
                        {"id":1,"image":"image.json","origin":"2 2 0","size":"4 4"},
                        {"id":2,"particle":"parent.json","origin":"100 100 0"}
                    ]},
                "image.json": {"material":"background.json"},
                "background.json": {"passes":[{"shader":"clock"}]},
                "material.json": {"passes":[{"shader":"genericparticle","textures":["util/white"]}]},
                "parent.json": {"material":"material.json","maxcount":2,
                        "emitter":[{"name":"boxrandom","rate":60}],
                        "initializer":[{"name":"lifetimerandom","min":10,"max":10}],
                        "children":[{"name":"child.json","type":"eventfollow","maxcount":2}]
                    },
                "child.json": {"material":"material.json","starttime":3,"maxcount":1,
                        "emitter":[{"name":"boxrandom","instantaneous":1,"rate":0}],
                        "initializer":[{"name":"lifetimerandom","min":10,"max":10}]
                    },
            }));
            std::fs::create_dir_all(root.path().join("shaders")).unwrap();
            std::fs::write(root.path().join("shaders/clock.vert"),
            "attribute vec3 a_Position;uniform mat4 g_ModelViewProjectionMatrix;void main(){gl_Position=mul(vec4(a_Position,1),g_ModelViewProjectionMatrix);}").unwrap();
            std::fs::write(
                root.path().join("shaders/clock.frag"),
                "uniform float g_Time;void main(){gl_FragColor=vec4(g_Time,0,0,1);}",
            )
            .unwrap();
            let (gpu, target) = canvas(4, 4);
            let mut scene = rendered(&root, &gpu, &target);
            assert_eq!(scene.frame(0.).get_pixel(2, 2).0, [0, 0, 0, 255]);
            assert!(scene.ready());
            // Two eventfollow children each need 360 prewarm ticks plus normal ticks,
            // exceeding the per-call catch-up batch. The background still owes a frame.
            let time = 1. / 30.;
            let pixel = scene.frame(time).get_pixel(2, 2).0;
            assert!(
                (pixel[0] as f32 - time * 255.).abs() <= 1.,
                "background froze during child prewarm: {pixel:?}"
            );
            assert!(
                !scene.ready(),
                "child prewarm must retain its bounded catch-up batch"
            );
            scene.pause(true);
            assert_eq!(scene.frame(1.).get_pixel(2, 2).0, pixel);
            // Fresh loads and rewinds still wait for a complete first frame.
            let mut candidate = rendered(&root, &gpu, &target);
            for time in [0.5, 0.1] {
                let held = gpu.read_image(&target).unwrap().get_pixel(2, 2).0;
                assert_eq!(candidate.frame(time).get_pixel(2, 2).0, held);
                assert!(!candidate.ready());
                for _ in 0..4 {
                    candidate.frame(time);
                    if candidate.ready() {
                        break;
                    }
                }
                assert!(candidate.ready());
                let red = gpu.read_image(&target).unwrap().get_pixel(2, 2)[0];
                assert!((red as f32 - time * 255.).abs() <= 1.);
            }
            assert!(scene.errors().is_empty(), "{}", scene.diagnostics());
        }

        #[test]
        #[ignore = "requires EGL/GLES and WE common shaders"]
        fn native_particle_script_emission_pause_force_controlpoints_stop_resume_and_clock_pixels()
        {
            let code = r#"
        export function init(v){
            shared.point=thisLayer.instance.controlpoint0;
            if(shared.point.x!==4)throw Error('asset control point default');
            thisLayer.instance.colorn=new Vec3(0,1,0);
            thisLayer.stop();if(thisLayer.isPlaying())throw Error('stop');return v;
        }
        export function applyUserProperties(v){
            if(v.spawn)thisLayer.emitParticles(2);
            if(v.pause)thisLayer.pause();
            if(v.finished&&thisLayer.isPlaying())throw Error('expired system still playing');
            if(v.shift){shared.point.x=8;thisLayer.emitParticles();}
            if(v.stop){thisLayer.stop();if(thisLayer.isPlaying())throw Error('stop retains live state');}
            if(v.play)thisLayer.play();
        }
    "#;
            let root = fixture(json!({
                "scene.json": {"general":{"orthogonalprojection":{"width":32,"height":16}},"objects":[{"id":1,"particle":"particle.json","origin":{"value":[8,8,0],"script":code}}]},
                "particle.json": {"material":"material.json","maxcount":4,"controlpoint":[{"id":0,"offset":"4 0 0"}],"emitter":[{"name":"boxrandom","instantaneous":1,"rate":0}],"initializer":[{"name":"lifetimerandom","min":0.25,"max":0.25},{"name":"sizerandom","min":4,"max":4}],"renderer":[{"name":"sprite"}]},
                "material.json": {"passes":[{"shader":"genericparticle","textures":["util/white"],"blending":"translucent"}]},
            }));
            let (gpu, target) = canvas(32, 16);
            let mut scene = rendered(&root, &gpu, &target);
            let empty = |image: &image::RgbaImage| image.pixels().all(|p| p.0 == [0, 0, 0, 255]);
            assert!(empty(&scene.frame(0.)), "{}", scene.diagnostics());
            let update = |scene: &mut we_scene::Renderer, key: &str| {
                set_properties(scene, json!({(key): true}))
            };
            update(&mut scene, "spawn");
            let spawned = scene.frame(0.01);
            assert_eq!(
                spawned.get_pixel(12, 8).0,
                [0, 255, 0, 255],
                "{}",
                scene.diagnostics()
            );
            update(&mut scene, "pause");
            let held = scene.frame(0.2);
            assert_eq!(held.get_pixel(12, 8).0, [0, 255, 0, 255]);
            scene.pause(true);
            assert_eq!(scene.frame(10.), held, "wallpaper pause freezes simulation");
            scene.pause(false);
            assert!(
                empty(&scene.frame(0.3)),
                "emission pause must still age existing particles"
            );
            update(&mut scene, "finished");
            update(&mut scene, "shift");
            let shifted = scene.frame(0.31);
            assert_eq!(
                shifted.get_pixel(16, 8).0,
                [0, 255, 0, 255],
                "mutable control point survives property synchronization"
            );
            assert_eq!(shifted.get_pixel(12, 8).0, [0, 0, 0, 255]);
            update(&mut scene, "stop");
            assert!(empty(&scene.frame(0.4)));
            update(&mut scene, "play");
            assert_eq!(scene.frame(0.41).get_pixel(16, 8).0, [0, 255, 0, 255]);
            assert!(scene.errors().is_empty(), "{}", scene.diagnostics());
            drop(scene);
            assert_gl_clean(&gpu);
        }

        #[test]
        #[ignore = "requires EGL/GLES and WE common shaders"]
        fn native_particle_multiple_renderers_and_material_passes_share_one_simulation_pixels() {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("scene.json");
            let pass = json!({"shader":"genericparticle","textures":["util/white"],"blending":"translucent"});
            for (name, value) in [
                (
                    "scene.json",
                    json!({"general":{"orthogonalprojection":{"width":32,"height":32}},"objects":[{"id":1,"particle":"particle.json","origin":"16 16 0"}]}),
                ),
                (
                    "particle.json",
                    json!({"material":"material.json","maxcount":1,"emitter":[{"name":"boxrandom","instantaneous":1,"rate":0}],"initializer":[{"name":"lifetimerandom","min":2,"max":2},{"name":"sizerandom","min":8,"max":8},{"name":"alpharandom","min":0.5,"max":0.5}],"renderer":[{"name":"sprite"},{"name":"sprite"}]}),
                ),
                ("material.json", json!({"passes":[pass,pass]})),
            ] {
                write_json(root.path().join(name), &value);
            }
            test_support::white(root.path());
            let common =
                crate::catalog::we_assets(&crate::store::WallpaperEngine::default()).unwrap();
            let (gpu, target) = canvas(32, 32);
            let mut renderer =
                we_scene::Renderer::load(gpu.gl.clone(), root.path(), &path, Some(&common))
                    .unwrap();
            renderer
                .draw([32, 32], Some(target.fbo), we_scene::Fit::Stretch, 0.)
                .unwrap();
            gpu.reset();
            let first = gpu.read_image(&target).unwrap();
            let pixel = first.get_pixel(16, 16).0;
            assert!(
                pixel[..3].iter().all(|v| v.abs_diff(239) <= 2),
                "expected four shared-particle draws, got {pixel:?}"
            );
            renderer.pause(true);
            renderer
                .draw([32, 32], Some(target.fbo), we_scene::Fit::Stretch, 10.)
                .unwrap();
            gpu.reset();
            assert_eq!(first, gpu.read_image(&target).unwrap());
            assert!(renderer.errors().is_empty(), "{}", renderer.diagnostics());
        }
    }

    pub(super) mod particle_event_tests {
        use super::*;

        #[test]
        #[ignore = "requires EGL/GLES and WE common shaders"]
        fn native_particle_event_values_birth_live_death_alpha_pause_and_rewind_pixels() {
            let child = json!({"material":"material.json","maxcount":1,"emitter":[{"name":"boxrandom","instantaneous":1,"rate":0}],"initializer":[{"name":"lifetimerandom","min":1,"max":1},{"name":"sizerandom","min":4,"max":4},{"name":"inheritinitialvaluefromevent"}],"renderer":[{"name":"sprite"}]});
            let mut follow = child.clone();
            follow["operator"] = json!([{"name":"inheritvaluefromevent"}]);
            let mut opacity = child.clone();
            opacity["initializer"].as_array_mut().unwrap().pop();
            opacity["operator"] = json!([{"name":"inheritvaluefromevent","input":"opacity"}]);
            let root = fixture(json!({
                "scene.json": {"general":{"orthogonalprojection":{"width":48,"height":32}},"objects":[{"id":1,"particle":"parent.json","origin":"8 16 0"}]},
                "parent.json": {"maxcount":1,"emitter":[{"name":"boxrandom","instantaneous":1,"rate":0}],"initializer":[{"name":"lifetimerandom","min":0.125,"max":0.125},{"name":"alpharandom","min":0.25,"max":0.25}],"operator":[{"name":"colorchange","startvalue":"1 0 0","endvalue":"0 0 1","endtime":0.5}],"renderer":[],"children":[{"name":"birth.json","type":"eventspawn","origin":"8 0 0","instanceoverride":{"alpha":0.5}},{"name":"follow.json","type":"eventfollow","origin":"16 0 0"},{"name":"death.json","type":"eventdeath","origin":"24 0 0"},{"name":"opacity.json","type":"eventfollow","origin":"32 0 0"}]},
                "birth.json": child.clone(),
                "follow.json": follow,
                "death.json": child,
                "opacity.json": opacity,
                "material.json": {"passes":[{"shader":"genericparticle","textures":["util/white"],"blending":"translucent"}]},
            }));
            let (gpu, target) = canvas(48, 32);
            let mut scene = rendered(&root, &gpu, &target);
            let color = |image: &image::RgbaImage, x, expected: [u8; 4]| {
                let actual = image.get_pixel(x, 16).0;
                assert!(
                    actual.iter().zip(expected).all(|(a, b)| a.abs_diff(b) <= 1),
                    "event color at {x}: {actual:?}, expected {expected:?}"
                );
            };
            let first = scene.frame(0.);
            color(&first, 16, [128, 0, 0, 255]);
            color(&first, 24, [255, 0, 0, 255]);
            color(&first, 32, [0, 0, 0, 255]);
            color(&first, 40, [64, 64, 64, 255]);
            let changed = scene.frame(0.075);
            color(&changed, 16, [128, 0, 0, 255]);
            color(&changed, 24, [0, 0, 255, 255]);
            let died = scene.frame(0.125);
            color(&died, 32, [0, 0, 255, 255]);
            scene.pause(true);
            assert_eq!(scene.frame(10.), died);
            scene.pause(false);
            let held = scene.frame(0.5);
            color(&held, 16, [128, 0, 0, 255]);
            color(&held, 24, [0, 0, 255, 255]);
            color(&held, 32, [0, 0, 255, 255]);
            assert!(scene.frame(1.2).pixels().all(|p| p.0 == [0, 0, 0, 255]));
            assert_eq!(scene.frame(0.), first);
            assert!(scene.errors().is_empty(), "{}", scene.diagnostics());
            drop(scene);
            assert_gl_clean(&gpu);
        }
    }

    pub(super) mod particle_collision_tests {
        use super::*;

        #[test]
        #[ignore = "requires EGL/GLES and WE common shaders"]
        fn native_particle_model_collision_hidden_bones_motion_and_destroy_pixels() {
            let script = "export function applyUserProperties(p){if(p.destroy)thisScene.destroyLayer('collider');} export function update(v){thisLayer.setLocalBoneOrigin('child',new Vec3(0,engine.userProperties.move?24:0,0));return v;}";
            let root = fixture(json!({
                "scene.json": {"general":{"orthogonalprojection":{"width":64,"height":48}},"objects":[{"id":1,"name":"collider","image":"image.json","size":"8 8","origin":"32 24 0","visible":false,"alpha":{"value":1,"script":script}},{"id":2,"particle":"parent.json","origin":"16 24 0","dependencies":[{"id":1,"index":0,"type":"collisionmodel"}]}]},
                "image.json": {"material":"material.json","puppet":"puppet.mdl"},
                "material.json": {"passes":[{"shader":"genericparticle","textures":["util/white"],"blending":"translucent"}]},
                "parent.json": {"material":"material.json","maxcount":1,"emitter":[{"name":"boxrandom","instantaneous":1,"rate":0}],"initializer":[{"name":"lifetimerandom","min":2,"max":2},{"name":"sizerandom","min":4,"max":4},{"name":"colorrandom","min":"255 0 0","max":"255 0 0"},{"name":"velocityrandom","min":"32 0 0","max":"32 0 0"}],"operator":[{"name":"movement"},{"name":"collisionmodel","collisionbehavior":"delete"}],"children":[{"name":"child.json","type":"eventdeath","maxcount":1}],"renderer":[{"name":"sprite"}]},
                "child.json": {"material":"material.json","maxcount":1,"emitter":[{"name":"boxrandom","instantaneous":1,"rate":0}],"initializer":[{"name":"lifetimerandom","min":2,"max":2},{"name":"sizerandom","min":4,"max":4},{"name":"colorrandom","min":"0 255 0","max":"0 255 0"}],"renderer":[{"name":"sprite"}]},
            }));
            std::fs::write(root.path().join("puppet.mdl"), super::mdl_tests::puppet()).unwrap();
            let (gpu, target) = canvas(64, 48);
            let mut scene = rendered(&root, &gpu, &target);
            let first = scene.frame(0.);
            assert_eq!(first.get_pixel(16, 24).0, [255, 0, 0, 255]);
            let hit = scene.frame(0.5);
            assert_eq!(
                hit.get_pixel(26, 24).0,
                [0, 255, 0, 255],
                "hidden bind-pose capsule must dispatch death at the swept contact"
            );
            assert!(!hit.pixels().any(|p| p[0] > 200));
            scene.pause(true);
            assert_eq!(scene.frame(10.), hit);
            scene.pause(false);
            set_properties(&mut scene, json!({"move": true}));
            assert_eq!(scene.frame(0.), first);
            let avoided = scene.frame(0.5);
            assert_eq!(
                avoided.get_pixel(32, 24).0,
                [255, 0, 0, 255],
                "scripted bone motion must update collision connections"
            );
            set_properties(&mut scene, json!({"destroy": true}));
            assert_eq!(scene.frame(0.), first);
            assert_eq!(
                scene.frame(0.5),
                avoided,
                "destroyed source must release its collider snapshot"
            );
            assert!(scene.errors().is_empty(), "{}", scene.diagnostics());
            drop(scene);
            assert_gl_clean(&gpu);
        }

        #[test]
        #[ignore = "requires EGL/GLES and WE common shaders"]
        fn native_particle_collision_delete_child_swept_sphere_and_wallpaper_bounds_pixels() {
            let definition = |velocity: &str, operator: serde_json::Value| json!({"material":"material.json","maxcount":1,"emitter":[{"name":"boxrandom","instantaneous":1,"rate":0}],"initializer":[{"name":"lifetimerandom","min":2,"max":2},{"name":"sizerandom","min":4,"max":4},{"name":"colorrandom","min":"0 255 0","max":"0 255 0"},{"name":"velocityrandom","min":velocity,"max":velocity}],"operator":[{"name":"movement"},operator],"renderer":[{"name":"sprite"}]});
            let mut parent = definition(
                "8 -8 0",
                json!({"name":"collisionplane","distance":-2,"collisionbehavior":"delete"}),
            );
            parent["initializer"][2] =
                json!({"name":"colorrandom","min":"255 0 0","max":"255 0 0"});
            parent["children"] = json!([{"name":"child.json","type":"eventdeath","maxcount":1}]);
            let root = fixture(json!({
                "scene.json": {"general":{"orthogonalprojection":{"width":64,"height":32}},"objects":[{"id":1,"particle":"parent.json","origin":"16 16 0"},{"id":2,"particle":"sphere.json","origin":"8 8 0"},{"id":3,"particle":"bounds.json","origin":"60 24 0"}]},
                "material.json": {"passes":[{"shader":"genericparticle","textures":["util/white"],"blending":"translucent"}]},
                "parent.json": parent,
                "child.json": {"material":"material.json","maxcount":1,"emitter":[{"name":"boxrandom","origin":"8 0 0","instantaneous":1,"rate":0}],"initializer":[{"name":"lifetimerandom","min":2,"max":2},{"name":"sizerandom","min":4,"max":4},{"name":"colorrandom","min":"0 255 0","max":"0 255 0"}],"renderer":[{"name":"sprite"}]},
                "sphere.json": definition(
                        "64 0 0",
                        json!({"name":"collisionsphere","origin":"8 0 0","radius":2}),
                    ),
                "bounds.json": definition("8 0 0", json!({"name":"collisionbounds"})),
            }));
            let (gpu, target) = canvas(64, 32);
            let mut scene = rendered(&root, &gpu, &target);
            let first = scene.frame(0.);
            assert_eq!(first.get_pixel(16, 16).0, [255, 0, 0, 255]);
            assert_eq!(first.get_pixel(8, 24).0, [0, 255, 0, 255]);
            let collided = scene.frame(0.25);
            assert_eq!(
                collided.get_pixel(4, 24).0,
                [0, 255, 0, 255],
                "the sphere must reflect the remaining swept movement"
            );
            let deleted = scene.frame(0.3);
            assert_eq!(
                deleted.get_pixel(26, 18).0,
                [0, 255, 0, 255],
                "deleted particles must dispatch their final death position"
            );
            assert!(!deleted.pixels().any(|p| p[0] > 200));
            let bounds = scene.frame(0.75);
            assert_eq!(
                bounds.get_pixel(62, 8).0,
                [0, 255, 0, 255],
                "wallpaper bounds are independent of the emitter origin"
            );
            scene.pause(true);
            assert_eq!(scene.frame(10.), bounds);
            scene.pause(false);
            assert_eq!(scene.frame(0.), first);
            assert!(scene.errors().is_empty(), "{}", scene.diagnostics());
            drop(scene);
            assert_gl_clean(&gpu);
        }
    }
}
