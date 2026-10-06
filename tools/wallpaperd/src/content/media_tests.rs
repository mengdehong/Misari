use super::*;

pub(super) mod sound_tests {
    use super::*;

    use std::{f32::consts::TAU, thread};

    #[test]
    #[ignore = "requires EGL/GLES and an isolated PipeWire/Pulse server"]
    fn native_scene_sound_gain_script_pause_resume_stop_reconnect_and_release() {
        sound_lifecycle(false);
    }
    #[test]
    #[ignore = "requires EGL/GLES and an isolated PipeWire/Pulse server"]
    fn native_dynamic_scene_sound_create_clock_pause_resume_reconnect_and_release() {
        sound_lifecycle(true);
    }
    fn sound_lifecycle(dynamic: bool) {
        let mut server = crate::audio_capture::tests::Server::new();
        let root = tempfile::tempdir().unwrap();
        let mut pkg = 8u32.to_le_bytes().to_vec();
        pkg.extend(b"PKGV0001");
        pkg.extend(0u32.to_le_bytes());
        std::fs::write(root.path().join("scene.pkg"), pkg).unwrap();
        let sound = root.path().join("tone.wav");
        let samples = 48000 * 8;
        let bytes = samples * 8;
        let mut wave = b"RIFF".to_vec();
        wave.extend(((bytes + 36) as u32).to_le_bytes());
        wave.extend(b"WAVEfmt ");
        wave.extend(16u32.to_le_bytes());
        for value in [3u16, 2] {
            wave.extend(value.to_le_bytes());
        }
        wave.extend(48000u32.to_le_bytes());
        wave.extend(384000u32.to_le_bytes());
        wave.extend(8u16.to_le_bytes());
        wave.extend(32u16.to_le_bytes());
        wave.extend(b"data");
        wave.extend((bytes as u32).to_le_bytes());
        for sample in 0..samples {
            let frequency = if sample < 48000 {
                750.
            } else if sample < 96000 {
                3000.
            } else {
                6000.
            };
            let value = 0.4 * (TAU * frequency * sample as f32 / 48000.).sin();
            wave.extend(value.to_le_bytes());
            wave.extend(value.to_le_bytes());
        }
        std::fs::write(&sound, wave).unwrap();
        let code = if dynamic {
            "let sound;export function applyUserProperties(p){if(p.action==='create'){sound=thisScene.createLayer(engine.registerAsset('tone.wav'));sound.volume=0.5;sound.playbackmode='loop';return;}if(!sound)return;if(p.action==='pause')sound.pause();if(p.action==='resume'||p.action==='play')sound.play();if(p.action==='stop')sound.stop();if(p.action==='destroy')thisScene.destroyLayer(sound);}"
        } else {
            "export function applyUserProperties(p){if(p.action==='pause')thisLayer.pause();if(p.action==='resume'||p.action==='play')thisLayer.play();if(p.action==='stop')thisLayer.stop();}"
        };
        let mut object = json!({"id":1,"origin":{"value":[0,0,0],"script":code}});
        if !dynamic {
            object["sound"] = json!(["tone.wav"]);
            object["volume"] = json!(0.5);
            object["playbackmode"] = json!("loop");
        }
        write_json(
            root.path().join("scene.json"),
            &json!({"general":{"orthogonalprojection":{"width":8,"height":8}},"objects":[object]}),
        );
        let gpu = Gpu::headless(crate::pixels::Size::new(8, 8).unwrap()).unwrap();
        let mut scene = we_scene::Renderer::load(
            gpu.gl.clone(),
            root.path(),
            &root.path().join("scene.pkg"),
            None,
        )
        .unwrap();
        let (_reader, wake) = UnixStream::pair().unwrap();
        wake.set_nonblocking(true).unwrap();
        let mut context = crate::content::mpv_context(&gpu, crate::domain::Fit::Stretch);
        context.pulse_server = Some(server.address());
        let playback = wallpaper_media::Playback {
            paused: false,
            mute: false,
            volume: 50.,
        };
        let started = Instant::now();
        scene
            .synchronize_media(wallpaper_media::clock::Timeline {
                position_ns: 0,
                sampled_ns: wallpaper_media::clock::now(),
                running: true,
            })
            .unwrap();
        scene.attach_media(context, &wake, playback).unwrap();
        let mut capture =
            crate::audio_capture::Capture::start_pcm_with_server(Some(server.address())).unwrap();
        capture.set_active(true);
        if dynamic {
            scene
                .set_properties(&[("action".into(), json!("create"))].into())
                .unwrap();
        }
        let drive = |scene: &mut we_scene::Renderer| {
            scene
                .draw(
                    [8, 8],
                    None,
                    we_scene::Fit::Stretch,
                    started.elapsed().as_secs_f32(),
                )
                .unwrap();
            assert!(scene.errors().is_empty(), "{}", scene.diagnostics());
        };
        let peak = |s: &we_scene::audio::AudioSnapshot| {
            s.bands[2]
                .left
                .iter()
                .copied()
                .enumerate()
                .max_by(|a, b| a.1.total_cmp(&b.1))
                .unwrap()
        };
        let wait =
            |scene: &mut we_scene::Renderer,
             capture: &mut crate::audio_capture::Capture,
             predicate: &dyn Fn(&we_scene::audio::AudioSnapshot) -> bool| {
                let deadline = Instant::now() + Duration::from_secs(7);
                loop {
                    drive(scene);
                    let snapshot = capture.receive().unwrap();
                    if predicate(&snapshot) {
                        return snapshot;
                    }
                    assert!(
                        Instant::now() < deadline,
                        "sound spectrum did not converge: {:?}, {}",
                        peak(&snapshot),
                        scene.diagnostics()
                    );
                    thread::sleep(Duration::from_millis(10));
                }
            };
        // 0.4 PCM * 0.5 linear layer gain * (50/100)^3 mpv master gain = 0.025.
        let first = wait(&mut scene, &mut capture, &|s| {
            s.available && peak(s).1 > 0.02
        });
        assert!(
            (30..38).contains(&peak(&first).0) && peak(&first).1 < 0.03,
            "gain/frequency: {:?}",
            peak(&first)
        );
        let properties = |scene: &mut we_scene::Renderer, action: &str| {
            scene
                .set_properties(&[("action".into(), json!(action))].into())
                .unwrap();
            drive(scene);
        };
        properties(&mut scene, "pause");
        wait(&mut scene, &mut capture, &|s| {
            s.available && peak(s).1 < 0.005
        });
        thread::sleep(Duration::from_millis(1100));
        properties(&mut scene, "resume");
        let resumed = wait(&mut scene, &mut capture, &|s| peak(s).1 > 0.02);
        assert!(
            (30..38).contains(&peak(&resumed).0),
            "script resume jumped to scene time: {:?}",
            peak(&resumed)
        );
        properties(&mut scene, "stop");
        wait(&mut scene, &mut capture, &|s| {
            s.available && peak(s).1 < 0.005
        });
        properties(&mut scene, "play");
        let restarted = wait(&mut scene, &mut capture, &|s| peak(s).1 > 0.02);
        assert!(
            (30..38).contains(&peak(&restarted).0),
            "stop/play did not restart"
        );
        scene
            .media_playback(wallpaper_media::Playback {
                mute: true,
                ..playback
            })
            .unwrap();
        let mut pcm = Vec::new();
        for sample in 0..48000 * 6 {
            let value = 0.3 * (TAU * 12000. * sample as f32 / 48000.).sin();
            pcm.extend(value.to_le_bytes());
            pcm.extend(value.to_le_bytes());
        }
        let external = root.path().join("other.f32");
        std::fs::write(&external, pcm).unwrap();
        let other = server.play(&external);
        let muted = wait(&mut scene, &mut capture, &|s| peak(s).1 > 0.24);
        assert!(
            peak(&muted).0 > 55,
            "muted wallpaper suppressed another application's audio"
        );
        drop(other);
        scene.media_playback(playback).unwrap();
        server.disconnect();
        wait(&mut scene, &mut capture, &|s| !s.available);
        server.restart();
        wait(&mut scene, &mut capture, &|s| {
            s.available && peak(s).1 > 0.02
        });
        if dynamic {
            properties(&mut scene, "destroy");
        }
        let released = if dynamic {
            Some(scene)
        } else {
            drop(scene);
            None
        };
        let deadline = Instant::now() + Duration::from_secs(3);
        while server
            .pactl(&["--format=json", "list", "sink-inputs"])
            .trim()
            != "[]"
        {
            assert!(Instant::now() < deadline, "scene sound player leaked");
            thread::sleep(Duration::from_millis(20));
        }
        drop(released);
        drop(capture);
        assert_gl_clean(&gpu);
    }
}

pub(super) mod creation_tests {
    use super::*;

    #[test]
    #[ignore = "requires EGL/GLES and WE common assets"]
    fn native_dynamic_layer_assets_sort_parent_text_reload_failure_and_destroy_pixels() {
        let code = r#"
        let blue,green,failed,text;
        export function init(v){
            blue=thisScene.createLayer({image:engine.registerAsset('image.json'),name:'blue',parent:thisLayer,size:new Vec2(16),color:new Vec3(0,0,1)});
            thisScene.sortLayer(blue,0);
            return v;
        }
        export function update(v){
            if(engine.runtime>=0.1&&blue&&!shared.sorted){thisScene.sortLayer(blue,thisScene.getLayerCount()-1);shared.sorted=true;}
            if(engine.runtime>=0.2&&!text){text=thisScene.createLayer({name:'text',text:'动态中文',origin:new Vec3(100),size:new Vec2(40),font:'sans-serif'});}
            if(engine.runtime>=0.3&&blue){thisScene.destroyLayer(blue);thisScene.destroyLayer(text);shared.stale=blue;blue=undefined;}
            if(engine.runtime>=0.4&&!green){green=thisScene.createLayer({image:'image.json',name:'green',size:new Vec2(16),color:new Vec3(0,1,0)});}
            if(engine.runtime>=0.5&&!shared.churn){shared.churn=true;for(let i=0;i<8;i++)thisScene.destroyLayer(thisScene.createLayer({image:'image.json',size:new Vec2(32)}));}
            if(engine.runtime>=0.6&&!failed){failed=thisScene.createLayer({image:'broken.json',size:new Vec2(4)});}
            if(engine.runtime>=0.7&&!shared.checked){
                shared.checked=true;let caught=0;for(const handle of [shared.stale,failed])try{handle.visible;}catch(e){if(e instanceof ReferenceError)caught++;}
                if(caught!==2)throw Error('stale handles revived');
            }
            return v;
        }
    "#;
        let root = fixture(json!({
            "scene.json": {"general":{"orthogonalprojection":{"width":16,"height":16}},"objects":[{"id":1,"name":"controller","origin":{"value":[8,8,0],"script":code}},{"id":2,"image":"image.json","size":{"value":[16,16],"user":"size"},"color":[1,0,0]}]},
            "image.json": {"material":"material.json"},
            "material.json": {"passes":[{"shader":"copycolor","textures":["util/white"]}]},
            "broken.json": {"material":"broken-material.json"},
            "broken-material.json": {"passes":[{"shader":"broken","textures":["util/white"]}]},
        }));
        std::fs::create_dir(root.path().join("shaders")).unwrap();
        std::fs::write(root.path().join("shaders/copycolor.vert"),b"attribute vec3 a_Position;attribute vec2 a_TexCoord;varying vec2 uv;uniform mat4 g_ModelViewProjectionMatrix;void main(){uv=a_TexCoord;gl_Position=mul(vec4(a_Position,1),g_ModelViewProjectionMatrix);}").unwrap();
        std::fs::write(root.path().join("shaders/copycolor.frag"),b"varying vec2 uv;uniform sampler2D g_Texture0;uniform vec4 g_Color4;void main(){gl_FragColor=texSample2D(g_Texture0,uv)*g_Color4;}").unwrap();
        std::fs::write(
            root.path().join("shaders/broken.vert"),
            b"attribute vec3 a_Position;void main(){gl_Position=vec4(a_Position,1);}",
        )
        .unwrap();
        std::fs::write(
            root.path().join("shaders/broken.frag"),
            b"void main(){gl_FragColor=vec4(noSuchFunction());}",
        )
        .unwrap();
        let (gpu, target) = canvas(16, 16);
        let mut scene = rendered(&root, &gpu, &target);
        assert!(scene.diagnostics().is_empty(), "{}", scene.diagnostics());
        for (time, color) in [
            (0., [255, 0, 0, 255]),
            (0.1, [0, 0, 255, 255]),
            (0.2, [0, 0, 255, 255]),
        ] {
            let pixels = scene.frame(time);
            assert_eq!(
                pixels.get_pixel(8, 8).0,
                color,
                "time {time}; {}",
                scene.diagnostics()
            );
        }
        scene.pause(true);
        set_properties(&mut scene, json!({"size": 32}));
        assert_eq!(scene.frame(100.).get_pixel(8, 8).0, [0, 0, 255, 255]);
        scene.pause(false);
        assert_eq!(
            scene.frame(0.25).get_pixel(8, 8).0,
            [0, 0, 255, 255],
            "{}",
            scene.diagnostics()
        );
        for (time, color) in [
            (0.3, [255, 0, 0, 255]),
            (0.4, [0, 255, 0, 255]),
            (0.5, [0, 255, 0, 255]),
            (0.6, [0, 255, 0, 255]),
            (0.71, [0, 255, 0, 255]),
        ] {
            let pixels = scene.frame(time);
            assert_eq!(
                pixels.get_pixel(8, 8).0,
                color,
                "time {time}; {}",
                scene.diagnostics()
            );
        }
        assert!(
            scene.errors().contains("Scene layer creation:"),
            "{}",
            scene.diagnostics()
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
