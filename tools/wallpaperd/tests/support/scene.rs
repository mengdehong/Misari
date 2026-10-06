use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
};

// Unpacked fixtures are self-contained and use the Rust renderer.
pub(crate) fn rust_scene_fixture(root: &Path) -> PathBuf {
    let project = root.join("rust-properties");
    for directory in ["models", "materials", "shaders", "effects"] {
        fs::create_dir_all(project.join(directory)).unwrap();
    }
    let write = |name: &str, value: Value| {
        fs::write(project.join(name), serde_json::to_vec(&value).unwrap()).unwrap();
    };
    write(
        "project.json",
        serde_json::json!({"type":"scene","file":"scene.json","general":{"properties":{
            "tint":{"type":"color","value":"1 0 0"},
            "enabled":{"type":"bool","value":true},
            "strength":{"type":"slider","min":0,"max":1,"fraction":true,"value":1},
            "style":{"type":"bool","value":false}
        }}}),
    );
    write(
        "scene.json",
        serde_json::json!({"general":{"orthogonalprojection":{"width":128,"height":64},"clearcolor":"0 0 0"},
        "objects":[{"image":"models/probe.json","size":"128 64","origin":"64 32 0",
        "visible":{"user":"enabled","value":true},"color":{"user":"tint","value":"1 0 0"},
        "effects":[{"file":"effects/graph.json","passes":[{"constantshadervalues":{"gain":{"user":"strength","value":1}},
            "combos":{"STYLE":{"user":{"name":"style","condition":"0"},"value":1}}}]}]}]}),
    );
    write(
        "models/probe.json",
        serde_json::json!({"material":"materials/probe.json"}),
    );
    write(
        "materials/probe.json",
        serde_json::json!({"passes":[{"shader":"probe","textures":["white"],"blending":"translucent"}]}),
    );
    write(
        "effects/graph.json",
        serde_json::json!({
            "fbos":[{"name":"_saved","format":"rgba_backbuffer","fit":64},{"name":"_rg","format":"rg88","scale":2}],
            "passes":[{"command":"copy","source":"previous","target":"_saved"},
                {"material":"materials/tone.json","target":"_rg","bind":[{"index":0,"name":"_saved"}]},
                {"material":"materials/finish.json","bind":[{"index":0,"name":"_rg"},{"index":1,"name":"_saved"}]}]
        }),
    );
    write(
        "materials/tone.json",
        serde_json::json!({"passes":[{"shader":"tone","blending":"translucent"}]}),
    );
    write(
        "materials/finish.json",
        serde_json::json!({"passes":[{"shader":"finish","blending":"normal",
        "constantshadervalues":{"gain":{"user":"strength","value":1}}}]}),
    );
    let vertex = b"attribute vec3 a_Position; attribute vec2 a_TexCoord; varying vec2 uv; uniform mat4 g_ModelViewProjectionMatrix; void main(){ uv=a_TexCoord; gl_Position=mul(vec4(a_Position,1),g_ModelViewProjectionMatrix); }";
    for name in ["probe", "tone", "finish"] {
        fs::write(project.join(format!("shaders/{name}.vert")), vertex).unwrap();
    }
    fs::write(project.join("shaders/probe.frag"),
        b"varying vec2 uv; uniform sampler2D g_Texture0; uniform vec4 g_Color4; void main(){gl_FragColor=texSample2D(g_Texture0,uv)*g_Color4;}").unwrap();
    fs::write(
        project.join("shaders/tone.frag"),
        r#"varying vec2 uv;
uniform sampler2D g_Texture0;
uniform float gain; // {"material":"gain","default":1}
void main(){
#if STYLE
gl_FragColor=vec4(0,gain,0,1);
#else
gl_FragColor=texSample2D(g_Texture0,uv)*gain;
#endif
}"#,
    )
    .unwrap();
    fs::write(project.join("shaders/finish.frag"),r#"varying vec2 uv;
uniform sampler2D g_Texture0;
uniform sampler2D g_Texture1;
uniform float gain; // {"material":"gain","default":1}
void main(){gl_FragColor=vec4(texSample2D(g_Texture0,uv).rg,texSample2D(g_Texture1,uv).b*gain,1);}"#).unwrap();
    let mut texture = b"TEXV0005\0TEXI0001\0".to_vec();
    for n in [0u32, 0, 4, 4, 4, 4, 0] {
        texture.extend(n.to_le_bytes());
    }
    texture.extend(b"TEXB0001\0");
    for n in [1u32, 1, 4, 4, 64] {
        texture.extend(n.to_le_bytes());
    }
    texture.extend([255; 64]);
    fs::write(project.join("materials/white.tex"), texture).unwrap();
    let mut package = 8u32.to_le_bytes().to_vec();
    package.extend(b"PKGV0001");
    package.extend(0u32.to_le_bytes());
    fs::write(project.join("scene.pkg"), package).unwrap();
    project
}
