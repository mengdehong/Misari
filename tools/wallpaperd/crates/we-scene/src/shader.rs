use crate::assets::Assets;
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::{
    cell::RefCell,
    collections::{BTreeMap, HashSet},
};

/// Shader inputs are determined by the rendering path, never by injected JSON keys.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum MaterialDomain {
    #[default]
    Effect,
    Image,
    Model,
    Particle {
        rope: bool,
        frame_blend: bool,
    },
}

pub(crate) struct Uniform {
    pub name: String,
    pub kind: String,
    pub value: Value,
    pub binding: Option<String>,
}
impl Uniform {
    pub fn components(&self, value: &Value) -> Result<Vec<f32>> {
        let count = match self.kind.as_str() {
            "float" | "int" | "bool" => 1,
            "vec2" => 2,
            "vec3" => 3,
            "vec4" => 4,
            "mat3" => 9,
            "mat4" => 16,
            _ => anyhow::bail!("unsupported uniform type {}", self.kind),
        };
        crate::scene::bindings::components(value, &[], count)
            .with_context(|| format!("uniform {}", self.name))
    }
}
pub(crate) struct Sources {
    pub vertex: String,
    pub fragment: String,
    pub uniforms: Vec<Uniform>,
    pub defaults: BTreeMap<usize, String>,
    pub alpha_coverage: bool,
}
impl Sources {
    pub fn load(
        assets: &Assets,
        pass: &Value,
        named: &HashSet<String>,
        domain: MaterialDomain,
    ) -> Result<Self> {
        let shader = pass["shader"]
            .as_str()
            .context("material pass has no shader")?;
        let mut vertex = expand(
            assets,
            &format!("shaders/{shader}.vert"),
            &mut HashSet::new(),
            0,
        )?;
        let fragment = expand(
            assets,
            &format!("shaders/{shader}.frag"),
            &mut HashSet::new(),
            0,
        )?;
        let Metadata {
            mut combos,
            uniforms,
            defaults,
        } = metadata(&vertex, &fragment, pass);
        if let Some(values) = pass["combos"].as_object() {
            for (name, value) in values {
                combos.insert(
                    name.clone(),
                    value
                        .as_i64()
                        .or_else(|| value.as_bool().map(i64::from))
                        .or_else(|| value.as_str().and_then(|v| v.trim().parse().ok()))
                        .context("shader combo must be an integer")?,
                );
            }
        }
        let mut particle_frames = false;
        if domain == MaterialDomain::Model {
            combos.insert("SKINNING".into(), 0);
        }
        for index in 0..8 {
            if let Some(name) = pass["textures"][index]
                .as_str()
                .or_else(|| defaults.get(&index).map(String::as_str))
            {
                if name.starts_with("_rt_")
                    || name.starts_with("_alias_")
                    || name == "previous"
                    || named.contains(name)
                {
                    continue;
                }
                let (format, frames) = assets.texture_metadata(name)?;
                combos.insert(format!("TEX{index}FORMAT"), format as i64);
                if index == 0 {
                    particle_frames = !frames.is_empty();
                }
            }
        }
        if let MaterialDomain::Particle {
            rope: is_rope,
            frame_blend,
        } = domain
        {
            combos.insert("GS_ENABLED".into(), 0);
            combos.insert("THICKFORMAT".into(), 1);
            combos.insert("SPRITESHEET".into(), i64::from(particle_frames));
            combos.insert(
                "SPRITESHEETBLEND".into(),
                i64::from(particle_frames && frame_blend),
            );
            // Split corner UVs from particle rotation/size so all particle data is instanced.
            if is_rope {
                rope(&mut vertex)?;
            } else if vertex.contains("attribute vec4 a_TexCoordVec4;")
                && vertex.contains("ComputeParticlePosition(")
            {
                vertex=vertex.replace("attribute vec4 a_TexCoordVec4;", "attribute vec2 a_Corner;\nattribute vec4 a_ParticleRotationSize;\nattribute vec2 a_ParticleUVRange;\n#define a_TexCoordVec4 vec4(a_Corner.x,mix(a_ParticleUVRange.x,a_ParticleUVRange.y,a_Corner.y),a_ParticleRotationSize.zw)");
                vertex = vertex.replace(
                    "attribute vec2 a_TexCoordC2;",
                    "#define a_TexCoordC2 a_ParticleRotationSize.xy",
                );
                vertex=vertex.replace("attribute vec4 a_ParticleRotationSize;","attribute vec4 a_ParticleRotationSize;\nattribute vec4 a_ParticleFrame0;\nattribute vec4 a_ParticleFrame1;\nattribute vec4 a_ParticleFrame2;\nattribute float a_ParticleFrameMix;");
                vertex=vertex.replace("attribute vec2 a_Corner;", "attribute vec2 a_Corner;\nattribute vec4 a_InstanceRow0;\nattribute vec4 a_InstanceRow1;\nattribute vec4 a_InstanceRow2;");
                vertex=vertex.replace("gl_Position = mul(vec4(position, 1.0), g_ModelViewProjectionMatrix);", "position=vec3(dot(a_InstanceRow0,vec4(position,1)),dot(a_InstanceRow1,vec4(position,1)),dot(a_InstanceRow2,vec4(position,1)));\nright=vec3(dot(a_InstanceRow0.xyz,right),dot(a_InstanceRow1.xyz,right),dot(a_InstanceRow2.xyz,right));\nup=vec3(dot(a_InstanceRow0.xyz,up),dot(a_InstanceRow1.xyz,up),dot(a_InstanceRow2.xyz,up));\ngl_Position = mul(vec4(position, 1.0), g_ModelViewProjectionMatrix);");
                // A stopped particle has no trail axis; keep its degenerate quad finite.
                vertex = vertex
                    .replace(
                        "right = normalize(right);",
                        "right = dot(right,right)>1e-12 ? normalize(right) : g_OrientationRight;",
                    )
                    .replace(
                        "localVelocity /= trailLength;",
                        "localVelocity /= max(trailLength,1e-8);",
                    );
                if particle_frames {
                    let start = vertex
                        .find("\tvec2 uvOffsets;")
                        .context("WE particle sprite input block")?;
                    let end = vertex[start..]
                        .find("v_TexCoord += uvOffsets.xyxy;")
                        .context("WE particle sprite output block")?
                        + start
                        + "v_TexCoord += uvOffsets.xyxy;".len();
                    vertex.replace_range(start..end,"v_TexCoord.xy=a_ParticleFrame0.xy+a_TexCoordVec4.x*a_ParticleFrame0.zw+a_TexCoordVec4.y*a_ParticleFrame1.xy;\nv_TexCoord.zw=a_ParticleFrame1.zw+a_TexCoordVec4.x*a_ParticleFrame2.xy+a_TexCoordVec4.y*a_ParticleFrame2.zw;\nv_TexCoordBlend=a_ParticleFrameMix;");
                }
            }
            orientation(&mut vertex);
        }
        let mut definitions = String::new();
        let alpha_coverage = combos
            .get("ALPHATOCOVERAGE")
            .is_some_and(|value| *value != 0);
        for (name, value) in combos {
            ensure!(
                !name.is_empty()
                    && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                    && !name.starts_with(|c: char| c.is_ascii_digit()),
                "invalid combo name"
            );
            definitions.push_str(&format!("#define {name} {value}\n"));
        }
        let prelude = include_str!("shader_prelude.glsl");
        let (vertex, fragment) = if domain == MaterialDomain::Model {
            wrap_reflection(vertex, fragment)
        } else {
            (vertex, fragment)
        };
        let mut sources = Self {
            vertex: lower(
                &format!(
                    "{prelude}{definitions}#define attribute in\n#define varying out\n{vertex}"
                ),
                shaderc::ShaderKind::Vertex,
            ).with_context(|| format!("vertex shader {shader}"))?,
            fragment: lower(
                &format!(
                    "{prelude}{definitions}#define varying in\nout vec4 wallpaperColor;\n#define gl_FragColor wallpaperColor\n{fragment}"
                ),
                shaderc::ShaderKind::Fragment,
            ).with_context(|| format!("fragment shader {shader}"))?,
            uniforms,
            defaults,
            alpha_coverage,
        };
        sources.fragment = semantics::interfaces(&sources.vertex, &sources.fragment)?;
        Ok(sources)
    }
}
struct Metadata {
    combos: BTreeMap<String, i64>,
    uniforms: Vec<Uniform>,
    defaults: BTreeMap<usize, String>,
}
fn metadata(vertex: &str, fragment: &str, pass: &Value) -> Metadata {
    let mut combos = BTreeMap::new();
    let mut uniforms = Vec::new();
    let mut defaults = BTreeMap::new();
    for line in vertex.lines().chain(fragment.lines()) {
        let Some((code, comment)) = line.split_once("//") else {
            continue;
        };
        let Some(start) = comment.find('{') else {
            continue;
        };
        let Ok(meta) = serde_json::from_str::<Value>(&comment[start..]) else {
            continue;
        };
        if let Some(combo) = meta["combo"].as_str()
            && comment.contains("[COMBO]")
        {
            combos
                .entry(combo.to_owned())
                .or_insert(meta["default"].as_i64().unwrap_or(0));
        }
        let words: Vec<_> = code.split_whitespace().collect();
        if words.len() < 3 || words[0] != "uniform" {
            continue;
        }
        let kind = if matches!(words[1], "lowp" | "mediump" | "highp") {
            2
        } else {
            1
        };
        let Some(raw_name) = words.get(kind + 1) else {
            continue;
        };
        let name = raw_name.trim_end_matches(';').to_owned();
        if words[kind] == "sampler2D" {
            if let Some(index) = name
                .strip_prefix("g_Texture")
                .and_then(|index| index.parse::<usize>().ok())
            {
                if let Some(default) = meta["default"].as_str() {
                    defaults.insert(index, default.to_owned());
                }
                if let Some(combo) = meta["combo"].as_str() {
                    let assigned = pass["textures"][index].as_str().is_some();
                    combos.insert(combo.to_owned(), i64::from(assigned));
                }
            }
        } else {
            let mut value = meta["material"]
                .as_str()
                .and_then(|key| pass["constantshadervalues"].get(key))
                .unwrap_or(&meta["default"])
                .clone();
            if let Some(material) = meta["material"].as_str()
                && let Some(bindings) = pass["usershadervalues"].as_object()
                && let Some((property, _)) = bindings
                    .iter()
                    .find(|(_, binding)| binding.as_str() == Some(material))
            {
                value = serde_json::json!({"user":property,"value":value});
            }
            if !value.is_null() {
                uniforms.push(Uniform {
                    name,
                    kind: words[kind].to_owned(),
                    value,
                    binding: meta["material"].as_str().map(str::to_owned),
                });
            }
        }
    }
    Metadata {
        combos,
        uniforms,
        defaults,
    }
}
/// Shader properties exist before script initialization, including values omitted in JSON.
pub(crate) fn material_properties(assets: &Assets, pass: &mut Value) -> Result<()> {
    let Some(shader) = pass["shader"].as_str() else {
        return Ok(());
    };
    let vertex = expand(
        assets,
        &format!("shaders/{shader}.vert"),
        &mut HashSet::new(),
        0,
    )?;
    let fragment = expand(
        assets,
        &format!("shaders/{shader}.frag"),
        &mut HashSet::new(),
        0,
    )?;
    let uniforms = metadata(&vertex, &fragment, pass).uniforms;
    if !pass["constantshadervalues"].is_object() {
        pass["constantshadervalues"] = serde_json::json!({});
    }
    for uniform in uniforms {
        if let Some(binding) = uniform.binding {
            pass["constantshadervalues"]
                .as_object_mut()
                .unwrap()
                .entry(binding)
                .or_insert(uniform.value);
        }
    }
    Ok(())
}

fn expand(
    assets: &Assets,
    name: &str,
    included: &mut HashSet<String>,
    depth: usize,
) -> Result<String> {
    ensure!(depth < 32, "shader includes exceed 32 levels");
    if !included.insert(name.to_owned()) {
        return Ok(String::new());
    }
    let source = String::from_utf8(assets.read(name)?)?;
    ensure!(source.len() <= 1024 * 1024, "shader exceeds 1 MiB");
    let mut expanded = String::new();
    for line in source.lines() {
        let trimmed = line.trim();
        if let Some(include) = trimmed.strip_prefix("#include") {
            let include = include.trim();
            ensure!(
                include.starts_with('"') && include.ends_with('"'),
                "unsupported include: {line}"
            );
            let child = include.trim_matches('"');
            let child = format!("shaders/{child}");
            expanded.push_str(&expand(assets, &child, included, depth + 1)?);
        } else if trimmed.starts_with("#require ") {
            if trimmed.trim_start_matches("#require ").trim() == "LightingV1" {
                expanded.push_str(include_str!("lighting/v1.glsl"));
                expanded.push('\n');
                continue;
            }
            // WE capability annotations are not GLSL directives (same as native lowering).
            expanded.push_str("//");
            expanded.push_str(line);
            expanded.push('\n');
        } else {
            ensure!(
                !trimmed.starts_with("#version"),
                "embedded shader versions are not supported"
            );
            expanded.push_str(line);
            expanded.push('\n');
        }
        ensure!(
            expanded.len() <= 4 * 1024 * 1024,
            "expanded shader exceeds 4 MiB"
        );
    }
    Ok(expanded)
}

thread_local! {
    // Keep glslang's internal state alive across stages, material reloads and scene switches.
    // Each rendering thread owns one compiler and releases it when the thread exits.
    static SHADER_COMPILER: RefCell<Option<shaderc::Compiler>> = const { RefCell::new(None) };
}

fn lower(source: &str, stage: shaderc::ShaderKind) -> Result<String> {
    SHADER_COMPILER.with_borrow_mut(|compiler| {
        if compiler.is_none() {
            *compiler = Some(shaderc::Compiler::new()?);
        }
        lower_with_compiler(compiler.as_ref().unwrap(), source, stage)
    })
}

fn lower_with_compiler(
    compiler: &shaderc::Compiler,
    source: &str,
    stage: shaderc::ShaderKind,
) -> Result<String> {
    use spirv_cross2::{
        Compiler, Module,
        compile::{CompilableTarget, glsl::GlslVersion},
        targets::Glsl,
    };
    let mut options = shaderc::CompileOptions::new()?;
    options.set_target_env(
        shaderc::TargetEnv::OpenGL,
        shaderc::EnvVersion::OpenGL4_5 as u32,
    );
    options.set_auto_bind_uniforms(true);
    options.set_auto_map_locations(true);
    let source = compat::identifiers(&compat::directives(source));
    let preprocessed = compiler
        .preprocess(&source, "we.glsl", "main", Some(&options))
        .map_err(|e| shader_error(e, &source))?;
    let source = hoist_uniforms(&preprocessed.as_text());
    let binary =
        match compiler.compile_into_spirv(&source, stage, "we.glsl", "main", Some(&options)) {
            Ok(binary) => binary,
            Err(original) => {
                let converted = semantics::lower(&source).map_err(|error| {
                    anyhow::anyhow!("{}\n{error:#}", shader_error(original, &source))
                })?;
                compiler
                    .compile_into_spirv(&converted, stage, "we.glsl", "main", Some(&options))
                    .map_err(|error| shader_error(error, &converted))?
            }
        };
    let mut compiler = Compiler::<Glsl>::new(Module::from_words(binary.as_binary()))?;
    // WE includes declare inputs for several renderer variants. Their unused
    // locations must not exhaust GLES's attribute limit in the chosen variant.
    let active = compiler.active_interface_variables()?;
    if stage == shaderc::ShaderKind::Vertex {
        let resources = compiler.shader_resources_for_active_variables(active)?;
        let mut location = 0u32;
        for input in
            resources.resources_for_type(spirv_cross2::reflect::ResourceType::StageInput)?
        {
            let count = input_locations(&compiler, input.type_id, 0)?;
            compiler.set_decoration(
                input.id,
                spirv_cross2::spirv::Decoration::Location,
                Some(location),
            )?;
            location = location
                .checked_add(count)
                .context("vertex input locations overflow")?;
        }
    }
    compiler.set_enabled_interface_variables(compiler.active_interface_variables()?)?;
    let mut options = Glsl::options();
    options.version = GlslVersion::Glsl300Es;
    Ok(compiler.compile(&options)?.to_string())
}

fn input_locations(
    compiler: &spirv_cross2::Compiler<spirv_cross2::targets::Glsl>,
    id: spirv_cross2::handle::Handle<spirv_cross2::handle::TypeId>,
    depth: usize,
) -> Result<u32> {
    use spirv_cross2::reflect::{ArrayDimension, TypeInner};
    ensure!(depth < 8, "vertex input type nesting exceeds 8");
    Ok(match compiler.type_description(id)?.inner {
        TypeInner::Scalar(_) | TypeInner::Vector { .. } => 1,
        TypeInner::Matrix { columns, .. } => columns,
        TypeInner::Pointer { base, .. } => input_locations(compiler, base, depth + 1)?,
        TypeInner::Array {
            base, dimensions, ..
        } => {
            let mut count = input_locations(compiler, base, depth + 1)?;
            for dimension in dimensions {
                let length = match dimension {
                    ArrayDimension::Literal(length) => length,
                    ArrayDimension::Constant(id) => {
                        compiler.specialization_constant_value::<u32>(id)?
                    }
                };
                ensure!(length != 0, "unsized vertex input array");
                count = count
                    .checked_mul(length)
                    .context("vertex input array locations overflow")?;
            }
            count
        }
        other => anyhow::bail!("unsupported vertex input type {other:?}"),
    })
}

// Includes may use uniforms declared later in the main shader. Preprocess first
// so moving declarations preserves their combo conditions.
fn hoist_uniforms(source: &str) -> String {
    let mut headers = Vec::new();
    let mut uniforms = Vec::new();
    let mut body = Vec::new();
    for line in source.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("#version") || trimmed.starts_with("#extension") {
            headers.push(line);
        } else {
            let mut remaining = line;
            while remaining.trim_start().starts_with("uniform ") {
                let Some(end) = remaining.find(';') else {
                    break;
                };
                uniforms.push(&remaining[..=end]);
                remaining = &remaining[end + 1..];
            }
            body.push(remaining);
        }
    }
    format!(
        "{}\n{}\n{}",
        headers.join("\n"),
        uniforms.join("\n"),
        body.join("\n")
    )
}

fn shader_error(error: shaderc::Error, source: &str) -> anyhow::Error {
    let lines = source.lines().collect::<Vec<_>>();
    let mut indices = std::collections::BTreeSet::new();
    for line in error.to_string().lines() {
        if let Some(index) = line
            .strip_prefix("we.glsl:")
            .and_then(|s| s.split(':').next())
            .and_then(|n| n.parse::<usize>().ok())
        {
            indices.extend(index.saturating_sub(3)..(index + 2).min(lines.len()));
        }
    }
    let context = indices
        .into_iter()
        .take(30)
        .map(|i| format!("{}: {}", i + 1, lines[i]))
        .collect::<Vec<_>>()
        .join("\n");
    anyhow::anyhow!("{error}\n{context}")
}

pub(super) fn orientation(vertex: &mut String) {
    for (old, new) in [
        ("g_OrientationRight", "a_ParticleRight"),
        ("g_OrientationUp", "a_ParticleUp"),
        ("g_OrientationForward", "a_ParticleForward"),
    ] {
        *vertex = vertex
            .replace(
                &format!("uniform vec3 {old};"),
                &format!("attribute vec3 {new};"),
            )
            .replace(old, new);
    }
    vertex.insert_str(0, "attribute vec3 a_ParticleEye;\n");
    *vertex = vertex.replace(
        "localPosition - mul(vec4(g_EyePosition, 1.0), g_ModelMatrixInverse).xyz",
        "localPosition - a_ParticleEye",
    );
}

pub(super) fn rope(vertex: &mut String) -> Result<()> {
    let marker = vertex
        .find("// No geometry shader")
        .context("WE rope shader input section")?;
    let input = vertex[marker..]
        .find("attribute vec4 a_PositionVec4;")
        .context("WE rope attributes")?
        + marker;
    let varying = vertex[input..]
        .find("varying vec4 v_Color;")
        .context("WE rope varyings")?
        + input;
    vertex.replace_range(input..varying, INPUTS);
    let begin = vertex
        .rfind("void main() {")
        .context("WE rope entry point")?;
    let end = vertex.rfind("#endif").context("WE rope vertex branch")?;
    vertex.replace_range(begin..end, MAIN);
    Ok(())
}
const INPUTS: &str = r#"
attribute vec2 a_Corner;
attribute vec3 a_Position;
attribute vec4 a_ParticleRotationSize;
attribute vec4 a_Color;
attribute vec2 a_ParticleUVRange;
attribute vec4 a_RopeEnd;
attribute vec4 a_RopeEndColor;
attribute vec3 a_RopePrevious;
attribute vec3 a_RopeAfter;
attribute vec4 a_InstanceRow0;
attribute vec4 a_InstanceRow1;
attribute vec4 a_InstanceRow2;
#if SPRITESHEET
attribute vec4 a_ParticleFrame0;
attribute vec4 a_ParticleFrame1;
#endif
vec3 nativePoint(vec3 p) {
    vec4 v = vec4(p, 1.0);
    return vec3(dot(a_InstanceRow0, v), dot(a_InstanceRow1, v), dot(a_InstanceRow2, v));
}
vec3 nativeDirection(vec3 p) {
    return vec3(dot(a_InstanceRow0.xyz, p), dot(a_InstanceRow1.xyz, p), dot(a_InstanceRow2.xyz, p));
}
vec3 nativeUnit(vec3 p, vec3 fallback) {
    return dot(p,p) > 1e-12 ? normalize(p) : fallback;
}
"#;
const MAIN: &str = r#"
void main() {
    vec3 eyeDirection = a_ParticleForward;
    vec3 startRight = nativeUnit(cross(eyeDirection, a_RopeEnd.xyz - a_RopePrevious), g_OrientationRight) * a_ParticleRotationSize.w;
    vec3 endRight = nativeUnit(cross(eyeDirection, a_RopeAfter - a_Position), g_OrientationRight) * a_RopeEnd.w;
    vec3 right = nativeDirection(mix(startRight, endRight, a_Corner.y));
    vec3 position = nativePoint(mix(a_Position, a_RopeEnd.xyz, a_Corner.y)) + right * (a_Corner.x * 2.0 - 1.0);
    gl_Position = mul(vec4(position, 1.0), g_ModelViewProjectionMatrix);
    v_TexCoord = vec2(a_Corner.x, mix(a_ParticleUVRange.x, a_ParticleUVRange.y, a_Corner.y));
#if SPRITESHEET
    v_TexCoord = a_ParticleFrame0.xy + v_TexCoord.x * a_ParticleFrame0.zw + v_TexCoord.y * a_ParticleFrame1.xy;
#endif
    v_Color = mix(a_Color, a_RopeEndColor, a_Corner.y);
#if FOG_DIST || FOG_HEIGHT || LIGHTING
    vec3 worldPos = mul(vec4(position, 1.0), g_ModelMatrix).xyz;
    v_ViewDir.xyz = g_EyePosition - worldPos.xyz;
    v_ViewDir.w = worldPos.y;
#endif
#if LIGHTING
    v_WorldPos = worldPos;
    v_WorldRight = mul(right, CAST3X3(g_ModelMatrix));
#endif
#if REFRACT
    vec3 up = cross(right, eyeDirection);
    ComputeScreenRefractionTangents(gl_Position.xyw, right, up, v_ScreenCoord, v_ScreenTangents);
#endif
}

"#;

fn wrap_reflection(vertex: String, fragment: String) -> (String, String) {
    (
        format!(
            "uniform highp vec4 g_NativeReflectionClipPlane;\nvarying highp float v_NativeReflectionDistance;\n#define main wallpaperReflectionVertex\n{vertex}\n#undef main\nvoid main(){{wallpaperReflectionVertex();v_NativeReflectionDistance=dot(g_NativeReflectionClipPlane,gl_Position);}}"
        ),
        format!(
            "varying highp float v_NativeReflectionDistance;\n#define main wallpaperReflectionFragment\n{fragment}\n#undef main\nvoid main(){{wallpaperReflectionFragment();if(v_NativeReflectionDistance<0.0)discard;}}"
        ),
    )
}

mod compat {
    //! WE accepts HLSL identifiers which are reserved in the intermediate GLSL 450.
    //! Rewrite identifier tokens, retaining comments, literals and actual GLSL qualifiers.
    pub(super) fn identifiers(source: &str) -> String {
        let mut replacement = String::from("wallpaperCompatSample");
        while source.contains(&replacement) {
            replacement.push('_');
        }
        let bytes = source.as_bytes();
        let mut out = String::with_capacity(source.len());
        let mut i = 0;
        while i < bytes.len() {
            let start = i;
            if bytes[i..].starts_with(b"//") {
                i += bytes[i..]
                    .iter()
                    .position(|b| *b == b'\n')
                    .unwrap_or(bytes.len() - i);
            } else if bytes[i..].starts_with(b"/*") {
                i += 2;
                while i < bytes.len() && !bytes[i..].starts_with(b"*/") {
                    i += 1;
                }
                i = (i + 2).min(bytes.len());
            } else if bytes[i] == b'"' {
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    if bytes[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
                i = (i + 1).min(bytes.len());
            } else if bytes[i].is_ascii_alphabetic() || bytes[i] == b'_' {
                i += 1;
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
                let token = &source[start..i];
                let next = source[i..]
                    .trim_start()
                    .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                    .next()
                    .unwrap_or("");
                if token == "sample"
                    && !["in", "out", "uniform", "centroid", "flat", "smooth"].contains(&next)
                {
                    out.push_str(&replacement);
                    continue;
                }
            } else {
                // Copy UTF-8 comments/assets without slicing within a code point.
                i += source[i..].chars().next().unwrap().len_utf8();
            }
            out.push_str(&source[start..i]);
        }
        out
    }

    /// The WE editor emits trailing semicolons in conditional directives and some
    /// effects retain a closing directive after its opening condition was removed.
    /// Normalize these unambiguous annotations; preserve other invalid directives
    /// for compiler diagnostics instead of discarding arbitrary source.
    pub(super) fn directives(source: &str) -> String {
        // Preserve byte positions and newlines while hiding comments from directive
        // detection, including block comments spanning several lines.
        let mut code = source.as_bytes().to_vec();
        let mut at = 0;
        while at < code.len() {
            let start = at;
            if code[at..].starts_with(b"//") {
                while at < code.len() && code[at] != b'\n' {
                    at += 1;
                }
            } else if code[at..].starts_with(b"/*") {
                at += 2;
                while at < code.len() && !code[at..].starts_with(b"*/") {
                    at += 1;
                }
                at = (at + 2).min(code.len());
            } else if code[at] == b'"' {
                at += 1;
                while at < code.len() && code[at] != b'"' {
                    if code[at] == b'\\' {
                        at += 1;
                    }
                    at += 1;
                }
                at = (at + 1).min(code.len());
            } else {
                at += 1;
                continue;
            }
            for byte in &mut code[start..at] {
                if *byte != b'\n' {
                    *byte = b' ';
                }
            }
        }
        let code = String::from_utf8(code).expect("comments replaced with ASCII");
        let mut depth = 0usize;
        source
            .lines()
            .zip(code.lines())
            .map(|(line, code)| {
                let trimmed = code.trim_start();
                let word = trimmed
                    .strip_prefix('#')
                    .unwrap_or("")
                    .split_whitespace()
                    .next()
                    .unwrap_or("");
                if matches!(word, "if" | "ifdef" | "ifndef") {
                    depth += 1;
                }
                if word == "endif" {
                    if depth == 0 {
                        return "// WE editor: unmatched closing conditional".into();
                    }
                    depth -= 1;
                }
                if matches!(word, "if" | "elif") && code.trim_end().ends_with(';') {
                    let semicolon = code.rfind(';').unwrap();
                    return format!("{}{}", &line[..semicolon], &line[semicolon + 1..]);
                }
                line.to_owned()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn editor_directives_preserve_commented_conditionals_and_errors() {
            let source = "/*\n#if FALSE;\n*/\n#if VALUE; /* 注释 */\n#elif OTHER; // retained\n#endif\n#endif\n#else\n#if UNCLOSED";
            let result = super::directives(source);
            assert!(result.contains("/*\n#if FALSE;\n*/"));
            assert!(result.contains("#if VALUE /* 注释 */"));
            assert!(result.contains("#elif OTHER // retained"));
            assert_eq!(result.matches("WE editor: unmatched").count(), 1);
            assert!(result.ends_with("#else\n#if UNCLOSED"));
        }
        #[test]
        fn literals_comments_substrings_and_qualifiers_are_preserved() {
            let source = "// sample 样本\n#define x \"sample\"\nsample in vec4 multisample;\nvec4 sample=vec4(1); result=sample; /* sample */";
            let result = super::identifiers(source);
            assert!(result.contains("sample in vec4 multisample"));
            assert!(result.contains("// sample 样本"));
            assert!(result.contains("\"sample\""));
            assert!(result.contains("result=wallpaperCompatSample;"));
        }
    }
}

mod semantics {
    //! Explicit WE/HLSL numeric conversions on a parsed, preprocessed GLSL tree.
    //! Unknown types stay with shaderc's diagnostics; do not guess from spelling.

    use anyhow::{Result, ensure};
    use glsl_lang::{
        ast::*,
        parse::DefaultParse,
        transpiler::glsl,
        visitor::{HostMut, Visit, VisitorMut},
    };
    use std::collections::HashMap;

    struct Semantics {
        scopes: Vec<HashMap<String, Ty>>,
        functions: HashMap<String, Vec<Function>>,
        result: Ty,
        depth: usize,
        expressions: usize,
        exceeded: bool,
        remainder: String,
        uses_remainder: bool,
        uses_mod_assign: bool,
        interfaces: HashMap<String, (Ty, Ty)>,
        mutable_inputs: HashMap<String, String>,
    }
    impl Semantics {
        fn prototype(&mut self, prototype: &FunctionPrototype) {
            let parameters = prototype.parameters.iter().map(|p| {
            let (qualifier, ty) = match &**p {
                FunctionParameterDeclarationData::Named(q, p) => (q, array_ty(Ty::from_spec(&p.ty), &p.ident.array_spec)),
                FunctionParameterDeclarationData::Unnamed(q, ty) => (q, Ty::from_spec(ty)),
            };
            let output = qualifier.as_ref().is_some_and(|q| q.qualifiers.iter().any(|q| matches!(&**q, TypeQualifierSpecData::Storage(s) if matches!(&**s, StorageQualifierData::Out | StorageQualifierData::InOut))));
            Parameter { ty, output }
        }).collect();
            self.functions
                .entry(prototype.name.0.to_string())
                .or_default()
                .push(Function {
                    result: Ty::from_spec(&prototype.ty.ty),
                    parameters,
                });
        }
        fn lookup(&self, name: &str) -> Ty {
            self.scopes
                .iter()
                .rev()
                .find_map(|s| s.get(name).copied())
                .unwrap_or(match name {
                    "gl_Position" | "gl_FragCoord" | "gl_FragColor" => Ty::numeric(Kind::Float, 4),
                    "gl_FrontFacing" => Ty::scalar(Kind::Bool),
                    _ => Ty::UNKNOWN,
                })
        }
        fn declare(&mut self, name: &Identifier, ty: Ty, initializer: &mut Option<Initializer>) {
            if let Some(value) = initializer {
                self.initializer(value, ty);
            }
            self.scopes
                .last_mut()
                .unwrap()
                .insert(name.0.to_string(), ty);
        }
        fn initializer(&mut self, initializer: &mut Initializer, ty: Ty) {
            match &mut **initializer {
                InitializerData::Simple(value) => {
                    let actual = self.expression(value);
                    cast(value, actual, ty);
                }
                InitializerData::List(list) => {
                    let mut element = ty;
                    element.arrays = element.arrays.saturating_sub(1);
                    for item in list {
                        self.initializer(item, element);
                    }
                }
            }
        }
        fn expression(&mut self, expr: &mut Expr) -> Ty {
            self.depth += 1;
            self.expressions += 1;
            if self.depth > 256 || self.expressions > 200_000 {
                self.exceeded = true;
                self.depth -= 1;
                return Ty::UNKNOWN;
            }
            let result = self.expression_inner(expr);
            self.depth -= 1;
            result
        }
        fn expression_inner(&mut self, expr: &mut Expr) -> Ty {
            use ExprData::*;
            match &mut **expr {
                Variable(name) => {
                    let ty = self.lookup(&name.0);
                    if self
                        .scopes
                        .iter()
                        .skip(1)
                        .all(|s| !s.contains_key(name.0.as_str()))
                        && let Some(local) = self.mutable_inputs.get(name.0.as_str())
                    {
                        *name = IdentifierData::from(local.as_str()).into();
                        return ty;
                    }
                    if self
                        .scopes
                        .iter()
                        .skip(1)
                        .all(|s| !s.contains_key(name.0.as_str()))
                        && let Some((original, wide)) = self.interfaces.get(name.0.as_str())
                    {
                        let original = *original;
                        cast(expr, *wide, original);
                        return original;
                    }
                    ty
                }
                IntConst(_) => Ty::scalar(Kind::Int),
                UIntConst(_) => Ty::scalar(Kind::UInt),
                BoolConst(_) => Ty::scalar(Kind::Bool),
                FloatConst(_) => Ty::scalar(Kind::Float),
                DoubleConst(_) => Ty::UNKNOWN,
                Unary(op, value) => {
                    let ty = self.expression(value);
                    if **op == UnaryOpData::Not {
                        cast(value, ty, Ty::numeric(Kind::Bool, ty.width));
                        Ty::numeric(Kind::Bool, ty.width)
                    } else {
                        ty
                    }
                }
                PostInc(value) | PostDec(value) => self.expression(value),
                Dot(value, field) => {
                    let mut ty = self.expression(value);
                    if ty.number()
                        && !field.0.is_empty()
                        && field.0.len() <= 4
                        && field.0.chars().all(|c| "xyzwrgbastpq".contains(c))
                    {
                        ty.width = field.0.len() as u8;
                        ty
                    } else {
                        Ty::UNKNOWN
                    }
                }
                Comma(a, b) => {
                    self.expression(a);
                    self.expression(b)
                }
                Ternary(cond, a, b) => {
                    let c = self.expression(cond);
                    cast(cond, c, Ty::scalar(Kind::Bool));
                    let ta = self.expression(a);
                    let tb = self.expression(b);
                    let common = if ta == tb { ta } else { ta.common(tb) };
                    cast(a, ta, common);
                    cast(b, tb, common);
                    common
                }
                Assignment(a, op, b) => {
                    let ta = self.expression(a);
                    let tb = self.expression(b);
                    cast(b, tb, ta);
                    if **op == AssignmentOpData::Mod && ta.kind == Kind::Float && ta.number() {
                        self.uses_mod_assign = true;
                        *expr = call(
                            &format!("{}Assign", self.remainder),
                            vec![(**a).clone(), (**b).clone()],
                        );
                    }
                    ta
                }
                Binary(op, a, b) => {
                    let ta = self.expression(a);
                    let tb = self.expression(b);
                    if ta.kind == Kind::Matrix || tb.kind == Kind::Matrix {
                        if ta.kind == Kind::Matrix && tb.number() && tb.width == 1 {
                            cast(b, tb, Ty::scalar(Kind::Float));
                            return ta;
                        }
                        if tb.kind == Kind::Matrix && ta.number() && ta.width == 1 {
                            cast(a, ta, Ty::scalar(Kind::Float));
                            return tb;
                        }
                        return if ta.kind == Kind::Matrix && tb.kind == Kind::Matrix {
                            ta
                        } else {
                            Ty::numeric(Kind::Float, ta.width.max(tb.width))
                        };
                    }
                    if matches!(
                        **op,
                        BinaryOpData::And | BinaryOpData::Or | BinaryOpData::Xor
                    ) {
                        cast(a, ta, Ty::scalar(Kind::Bool));
                        cast(b, tb, Ty::scalar(Kind::Bool));
                        return Ty::scalar(Kind::Bool);
                    }
                    let common = ta.common(tb);
                    if !common.number() {
                        return common;
                    }
                    let ae = Ty::numeric(common.kind, if ta.width == 1 { 1 } else { common.width });
                    let be = Ty::numeric(common.kind, if tb.width == 1 { 1 } else { common.width });
                    cast(a, ta, ae);
                    cast(b, tb, be);
                    if matches!(
                        **op,
                        BinaryOpData::Equal
                            | BinaryOpData::NonEqual
                            | BinaryOpData::Lt
                            | BinaryOpData::Gt
                            | BinaryOpData::Lte
                            | BinaryOpData::Gte
                    ) {
                        return Ty::scalar(Kind::Bool);
                    }
                    if **op == BinaryOpData::Mod && common.kind == Kind::Float {
                        cast(a, ae, common);
                        cast(b, be, common);
                        self.uses_remainder = true;
                        *expr = call(&self.remainder, vec![(**a).clone(), (**b).clone()]);
                    }
                    common
                }
                Bracket(base, index) => {
                    // WE injects spectra as float4 vectors even when authored shaders
                    // spell the uniform float[N]. Only spectra permit packed access.
                    if let Bracket(array, group) = &mut ***base
                        && let Variable(name) = &***array
                        && name.0.starts_with("g_AudioSpectrum")
                        && self.lookup(&name.0)
                            == (Ty {
                                kind: Kind::Float,
                                width: 1,
                                arrays: 1,
                            })
                    {
                        let tg = self.expression(group);
                        let ti = self.expression(index);
                        cast(group, tg, Ty::scalar(Kind::Int));
                        cast(index, ti, Ty::scalar(Kind::Int));
                        let combined = Binary(
                            BinaryOpData::Add.into(),
                            Box::new(
                                Binary(
                                    BinaryOpData::Mult.into(),
                                    group.clone(),
                                    Box::new(IntConst(4).into()),
                                )
                                .into(),
                            ),
                            index.clone(),
                        )
                        .into();
                        *expr = Bracket(array.clone(), Box::new(combined)).into();
                        return Ty::scalar(Kind::Float);
                    }
                    let mut ty = self.expression(base);
                    let idx = self.expression(index);
                    cast(index, idx, Ty::scalar(Kind::Int));
                    if ty.arrays > 0 {
                        ty.arrays -= 1;
                    } else if ty.kind == Kind::Matrix {
                        ty.kind = Kind::Float;
                    } else if ty.number() {
                        ty.width = 1;
                    } else {
                        ty = Ty::UNKNOWN;
                    }
                    ty
                }
                FunCall(function, arguments) => {
                    let types = arguments
                        .iter_mut()
                        .map(|v| self.expression(v))
                        .collect::<Vec<_>>();
                    if let FunIdentifierData::TypeSpecifier(spec) = &**function {
                        return Ty::from_spec(spec);
                    }
                    let Some(name) = function.as_ident().map(|n| n.0.to_string()) else {
                        return Ty::UNKNOWN;
                    };
                    if let Some(overloads) = self.functions.get(&name) {
                        let selected = overloads
                            .iter()
                            .filter(|f| f.parameters.len() == types.len())
                            .min_by_key(|f| {
                                f.parameters
                                    .iter()
                                    .zip(&types)
                                    .map(|(p, t)| {
                                        if p.ty == *t {
                                            0
                                        } else if p.output
                                            || !p.ty.number()
                                            || !t.number()
                                            || (t.width > 1 && p.ty.width > t.width)
                                        {
                                            10_000
                                        } else {
                                            10 + t.width.abs_diff(p.ty.width) as u32
                                                + u32::from(t.kind != p.ty.kind)
                                        }
                                    })
                                    .sum::<u32>()
                            })
                            .cloned();
                        if let Some(f) = selected {
                            for ((arg, actual), parameter) in
                                arguments.iter_mut().zip(types).zip(f.parameters)
                            {
                                if !parameter.output {
                                    cast(arg, actual, parameter.ty);
                                }
                            }
                            return f.result;
                        }
                    }
                    self.builtin(&name, arguments, &types)
                }
            }
        }
        fn builtin(&mut self, name: &str, args: &mut [Expr], types: &[Ty]) -> Ty {
            let Some(first) = types.first().copied() else {
                return Ty::UNKNOWN;
            };
            if matches!(
                name,
                "texture" | "textureLod" | "textureGrad" | "textureProj"
            ) {
                if args.len() >= 2 {
                    cast(
                        &mut args[1],
                        types[1],
                        Ty::numeric(
                            Kind::Float,
                            if name == "textureProj" {
                                types[1].width
                            } else {
                                2
                            },
                        ),
                    );
                }
                return Ty::numeric(Kind::Float, 4);
            }
            let shared = types
                .iter()
                .copied()
                .reduce(|a, b| a.common(b))
                .unwrap_or(Ty::UNKNOWN);
            if matches!(name, "min" | "max" | "clamp") && shared.number() {
                for (arg, actual) in args.iter_mut().zip(types) {
                    cast(arg, *actual, shared);
                }
                return shared;
            }
            if name == "mix" && types.len() == 3 {
                let common = types[0].common(types[1]);
                if common.number() {
                    let common = Ty::numeric(Kind::Float, common.width);
                    cast(&mut args[0], types[0], common);
                    cast(&mut args[1], types[1], common);
                    cast(
                        &mut args[2],
                        types[2],
                        Ty::numeric(
                            if types[2].kind == Kind::Bool {
                                Kind::Bool
                            } else {
                                Kind::Float
                            },
                            if types[2].width == 1 { 1 } else { common.width },
                        ),
                    );
                    return common;
                }
            }
            if matches!(
                name,
                "pow"
                    | "step"
                    | "smoothstep"
                    | "mod"
                    | "atan"
                    | "distance"
                    | "dot"
                    | "cross"
                    | "reflect"
                    | "faceforward"
            ) && shared.number()
            {
                let ty = Ty::numeric(Kind::Float, shared.width);
                for (arg, actual) in args.iter_mut().zip(types) {
                    cast(arg, *actual, ty);
                }
                return if matches!(name, "distance" | "dot") {
                    Ty::scalar(Kind::Float)
                } else {
                    ty
                };
            }
            if matches!(
                name,
                "length"
                    | "normalize"
                    | "sin"
                    | "cos"
                    | "tan"
                    | "asin"
                    | "acos"
                    | "exp"
                    | "exp2"
                    | "log"
                    | "log2"
                    | "sqrt"
                    | "inversesqrt"
                    | "floor"
                    | "ceil"
                    | "round"
                    | "trunc"
                    | "fract"
                    | "radians"
                    | "degrees"
                    | "dFdx"
                    | "dFdy"
                    | "fwidth"
            ) && first.number()
            {
                let ty = Ty::numeric(Kind::Float, first.width);
                cast(&mut args[0], first, ty);
                return if name == "length" {
                    Ty::scalar(Kind::Float)
                } else {
                    ty
                };
            }
            if matches!(name, "abs" | "sign" | "transpose") {
                return first;
            }
            Ty::UNKNOWN
        }
    }
    impl VisitorMut for Semantics {
        fn visit_function_definition(&mut self, f: &mut FunctionDefinition) -> Visit {
            self.result = Ty::from_spec(&f.prototype.ty.ty);
            self.scopes.push(HashMap::new());
            for parameter in &f.prototype.parameters {
                if let FunctionParameterDeclarationData::Named(_, p) = &**parameter {
                    self.scopes.last_mut().unwrap().insert(
                        p.ident.ident.0.to_string(),
                        array_ty(Ty::from_spec(&p.ty), &p.ident.array_spec),
                    );
                }
            }
            f.statement.visit_mut(self);
            if f.prototype.name.0 == "main" {
                for (input, local) in &self.mutable_inputs {
                    let mut value: Expr =
                        ExprData::Variable(IdentifierData::from(input.as_str()).into()).into();
                    if let Some((original, wide)) = self.interfaces.get(input) {
                        cast(&mut value, *wide, *original);
                    }
                    let assignment = ExprData::Assignment(
                        Box::new(ExprData::variable(local.as_str()).into()),
                        AssignmentOpData::Equal.into(),
                        Box::new(value),
                    )
                    .into();
                    f.statement.statement_list.insert(
                        0,
                        StatementData::Expression(ExprStatementData(Some(assignment)).into())
                            .into(),
                    );
                }
            }
            self.scopes.pop();
            self.result = Ty::UNKNOWN;
            Visit::Parent
        }
        fn visit_compound_statement(&mut self, statement: &mut CompoundStatement) -> Visit {
            if self.scopes.len() > 256 {
                self.exceeded = true;
                return Visit::Parent;
            }
            self.scopes.push(HashMap::new());
            for child in &mut statement.statement_list {
                child.visit_mut(self);
            }
            self.scopes.pop();
            Visit::Parent
        }
        fn visit_iteration_statement(&mut self, statement: &mut IterationStatement) -> Visit {
            self.scopes.push(HashMap::new());
            match &mut **statement {
                IterationStatementData::For(init, rest, body) => {
                    init.visit_mut(self);
                    rest.visit_mut(self);
                    body.visit_mut(self);
                }
                IterationStatementData::While(cond, body) => {
                    cond.visit_mut(self);
                    body.visit_mut(self);
                }
                IterationStatementData::DoWhile(body, cond) => {
                    body.visit_mut(self);
                    cond.visit_mut(self);
                }
            }
            self.scopes.pop();
            Visit::Parent
        }
        fn visit_selection_statement(&mut self, statement: &mut SelectionStatement) -> Visit {
            let ty = self.expression(&mut statement.cond);
            cast(&mut statement.cond, ty, Ty::scalar(Kind::Bool));
            match &mut *statement.rest {
                SelectionRestStatementData::Statement(body) => {
                    self.scopes.push(HashMap::new());
                    body.visit_mut(self);
                    self.scopes.pop();
                }
                SelectionRestStatementData::Else(a, b) => {
                    for body in [a, b] {
                        self.scopes.push(HashMap::new());
                        body.visit_mut(self);
                        self.scopes.pop();
                    }
                }
            }
            Visit::Parent
        }
        fn visit_declaration(&mut self, declaration: &mut Declaration) -> Visit {
            if let DeclarationData::InitDeclaratorList(list) = &mut **declaration {
                let mut ty = Ty::from_spec(&list.head.ty.ty);
                if self.scopes.len() == 1
                    && let Some(name) = &list.head.name
                    && let Some((original, wide)) = self.interfaces.get(name.0.as_str())
                {
                    ty = *original;
                    list.head.ty.ty.ty = match wide.width {
                        2 => TypeSpecifierNonArrayData::Vec2,
                        3 => TypeSpecifierNonArrayData::Vec3,
                        4 => TypeSpecifierNonArrayData::Vec4,
                        _ => unreachable!(),
                    }
                    .into();
                }
                let head_ty = array_ty(ty, &list.head.array_specifier);
                if let Some(name) = list.head.name.clone() {
                    self.declare(&name, head_ty, &mut list.head.initializer);
                }
                for tail in &mut list.tail {
                    let name = tail.ident.ident.clone();
                    self.declare(
                        &name,
                        array_ty(ty, &tail.ident.array_spec),
                        &mut tail.initializer,
                    );
                }
            }
            Visit::Parent
        }
        fn visit_jump_statement(&mut self, statement: &mut JumpStatement) -> Visit {
            if let JumpStatementData::Return(Some(value)) = &mut **statement {
                let ty = self.expression(value);
                cast(value, ty, self.result);
            }
            Visit::Parent
        }
        fn visit_condition(&mut self, condition: &mut Condition) -> Visit {
            match &mut **condition {
                ConditionData::Expr(expr) => {
                    let ty = self.expression(expr);
                    cast(expr, ty, Ty::scalar(Kind::Bool));
                }
                ConditionData::Assignment(ty, name, init) => {
                    let t = Ty::from_spec(&ty.ty);
                    self.initializer(init, t);
                    self.scopes
                        .last_mut()
                        .unwrap()
                        .insert(name.0.to_string(), t);
                }
            }
            Visit::Parent
        }
        fn visit_expr(&mut self, expr: &mut Expr) -> Visit {
            self.expression(expr);
            Visit::Parent
        }
    }

    pub(super) fn lower(source: &str) -> Result<String> {
        lower_with_interfaces(source, HashMap::new())
    }
    fn lower_with_interfaces(
        source: &str,
        interfaces: HashMap<String, (Ty, Ty)>,
    ) -> Result<String> {
        let mut tree =
            TranslationUnit::parse(source).map_err(|e| anyhow::anyhow!("WE shader syntax: {e}"))?;
        let mut remainder = "wallpaperCompatRemainder".to_owned();
        while source.contains(&remainder) {
            remainder.push('_');
        }
        let mut semantics = Semantics {
            scopes: vec![HashMap::new()],
            functions: HashMap::new(),
            result: Ty::UNKNOWN,
            depth: 0,
            expressions: 0,
            exceeded: false,
            remainder,
            uses_remainder: false,
            uses_mod_assign: false,
            interfaces,
            mutable_inputs: HashMap::new(),
        };
        // Collect forward function/global references before each lexical body, so
        // parameters and shadowed locals never overwrite a global's type.
        for declaration in &tree.0 {
            match &**declaration {
                ExternalDeclarationData::FunctionDefinition(f) => semantics.prototype(&f.prototype),
                ExternalDeclarationData::Declaration(d) => match &**d {
                    DeclarationData::FunctionPrototype(f) => semantics.prototype(f),
                    DeclarationData::InitDeclaratorList(list) => {
                        let ty = Ty::from_spec(&list.head.ty.ty);
                        if let Some(name) = &list.head.name {
                            semantics.scopes[0].insert(
                                name.0.to_string(),
                                array_ty(ty, &list.head.array_specifier),
                            );
                        }
                        for tail in &list.tail {
                            semantics.scopes[0].insert(
                                tail.ident.ident.0.to_string(),
                                array_ty(ty, &tail.ident.array_spec),
                            );
                        }
                    }
                    _ => {}
                },
                _ => {}
            }
        }
        // Fragment inputs are HLSL value parameters. GLES exposes read-only globals;
        // copy only inputs which are written, before any authored main() code runs.
        let mut inputs = HashMap::new();
        for d in &tree.0 {
            if let ExternalDeclarationData::Declaration(d)=&**d && let DeclarationData::InitDeclaratorList(list)=&**d && let Some(name)=&list.head.name && list.head.ty.qualifier.as_ref().is_some_and(|q|q.qualifiers.iter().any(|q|matches!(&**q,TypeQualifierSpecData::Storage(s) if **s==StorageQualifierData::In))) {
        let ty=semantics.interfaces.get(name.0.as_str()).map_or(Ty::from_spec(&list.head.ty.ty),|(original,_)|*original);
        if ty.number() {inputs.insert(name.0.to_string(),ty);}
    }
        }
        let mut writes = InputWrites {
            inputs: &inputs,
            writes: std::collections::HashSet::new(),
            functions: &semantics.functions,
        };
        glsl_lang::visitor::Host::visit(&tree, &mut writes);
        for name in writes.writes {
            let mut local = format!("wallpaperMutable_{name}");
            while source.contains(&local) {
                local.push('_');
            }
            semantics.mutable_inputs.insert(name.clone(), local);
        }
        tree.visit_mut(&mut semantics);
        for (name, local) in &semantics.mutable_inputs {
            let declaration = StatementData::declare_var(
                numeric_spec(semantics.lookup(name)),
                local.as_str(),
                None,
                None,
            );
            if let StatementData::Declaration(d) = declaration {
                let position = tree
                    .0
                    .iter()
                    .position(|d| matches!(&**d, ExternalDeclarationData::FunctionDefinition(_)))
                    .unwrap_or(tree.0.len());
                tree.0
                    .insert(position, ExternalDeclarationData::Declaration(d).into());
            }
        }

        ensure!(!semantics.exceeded, "WE shader exceeds conversion budget");
        let mut output = String::new();
        glsl::show_translation_unit(&mut output, &tree, glsl::FormattingState::default())?;
        if semantics.uses_remainder || semantics.uses_mod_assign {
            let mut helpers = String::new();
            for ty in ["float", "vec2", "vec3", "vec4"] {
                helpers.push_str(&format!(
                    "{ty} {name}({ty} a,{ty} b){{return a-b*trunc(a/b);}}\n",
                    name = semantics.remainder
                ));
                if semantics.uses_mod_assign {
                    helpers.push_str(&format!(
                        "{ty} {name}Assign(inout {ty} a,{ty} b){{a={name}(a,b);return a;}}\n",
                        name = semantics.remainder
                    ));
                }
            }
            let position = output.find('\n').map_or(0, |n| n + 1);
            output.insert_str(position, &helpers);
        }
        Ok(output)
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(super) enum Kind {
        Unknown,
        Float,
        Int,
        UInt,
        Bool,
        Matrix,
    }
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(super) struct Ty {
        pub(super) kind: Kind,
        pub(super) width: u8,
        pub(super) arrays: u8,
    }
    impl Ty {
        pub(super) const UNKNOWN: Self = Self {
            kind: Kind::Unknown,
            width: 0,
            arrays: 0,
        };
        pub(super) fn numeric(kind: Kind, width: u8) -> Self {
            Self {
                kind,
                width,
                arrays: 0,
            }
        }
        pub(super) fn scalar(kind: Kind) -> Self {
            Self::numeric(kind, 1)
        }
        pub(super) fn number(self) -> bool {
            self.arrays == 0 && !matches!(self.kind, Kind::Unknown | Kind::Matrix)
        }
        pub(super) fn from_spec(spec: &TypeSpecifier) -> Self {
            use TypeSpecifierNonArrayData::*;
            let (kind, width) = match &*spec.ty {
                Float => (Kind::Float, 1),
                Vec2 => (Kind::Float, 2),
                Vec3 => (Kind::Float, 3),
                Vec4 => (Kind::Float, 4),
                Int => (Kind::Int, 1),
                IVec2 => (Kind::Int, 2),
                IVec3 => (Kind::Int, 3),
                IVec4 => (Kind::Int, 4),
                UInt => (Kind::UInt, 1),
                UVec2 => (Kind::UInt, 2),
                UVec3 => (Kind::UInt, 3),
                UVec4 => (Kind::UInt, 4),
                Bool => (Kind::Bool, 1),
                BVec2 => (Kind::Bool, 2),
                BVec3 => (Kind::Bool, 3),
                BVec4 => (Kind::Bool, 4),
                Mat2 | Mat22 => (Kind::Matrix, 2),
                Mat3 | Mat33 => (Kind::Matrix, 3),
                Mat4 | Mat44 => (Kind::Matrix, 4),
                _ => return Self::UNKNOWN,
            };
            Self {
                kind,
                width,
                arrays: spec
                    .array_specifier
                    .as_ref()
                    .map_or(0, |a| a.dimensions.len() as u8),
            }
        }
        pub(super) fn constructor(self) -> &'static str {
            match (self.kind, self.width) {
                (Kind::Float, 1) => "float",
                (Kind::Float, 2) => "vec2",
                (Kind::Float, 3) => "vec3",
                (Kind::Float, 4) => "vec4",
                (Kind::Int, 1) => "int",
                (Kind::Int, 2) => "ivec2",
                (Kind::Int, 3) => "ivec3",
                (Kind::Int, 4) => "ivec4",
                (Kind::UInt, 1) => "uint",
                (Kind::UInt, 2) => "uvec2",
                (Kind::UInt, 3) => "uvec3",
                (Kind::UInt, 4) => "uvec4",
                (Kind::Bool, 1) => "bool",
                (Kind::Bool, 2) => "bvec2",
                (Kind::Bool, 3) => "bvec3",
                (Kind::Bool, 4) => "bvec4",
                _ => unreachable!(),
            }
        }
        pub(super) fn common(self, other: Self) -> Self {
            if !self.number() || !other.number() {
                return Self::UNKNOWN;
            }
            let width = if self.width == 1 || other.width == 1 {
                self.width.max(other.width)
            } else {
                self.width.min(other.width)
            };
            let kind = if self.kind == Kind::Float || other.kind == Kind::Float {
                Kind::Float
            } else if self.kind == Kind::UInt || other.kind == Kind::UInt {
                Kind::UInt
            } else {
                Kind::Int
            };
            Self::numeric(kind, width)
        }
    }
    #[derive(Clone)]
    pub(super) struct Parameter {
        pub(super) ty: Ty,
        pub(super) output: bool,
    }
    #[derive(Clone)]
    pub(super) struct Function {
        pub(super) result: Ty,
        pub(super) parameters: Vec<Parameter>,
    }
    pub(super) fn call(name: &str, arguments: Vec<Expr>) -> Expr {
        ExprData::FunCall(FunIdentifierData::ident(name).into(), arguments).into()
    }
    pub(super) fn cast(expr: &mut Expr, actual: Ty, expected: Ty) {
        if actual == expected
            || !actual.number()
            || !expected.number()
            || (actual.width > 1 && expected.width > actual.width)
        {
            return;
        }
        let mut value = std::mem::replace(expr, ExprData::IntConst(0).into());
        if expected.width == 1 && actual.width > 1 {
            value = ExprData::Dot(Box::new(value), IdentifierData::from("x").into()).into();
        }
        *expr = call(expected.constructor(), vec![value]);
    }
    pub(super) fn array_ty(mut ty: Ty, array: &Option<ArraySpecifier>) -> Ty {
        ty.arrays = ty
            .arrays
            .saturating_add(array.as_ref().map_or(0, |a| a.dimensions.len() as u8));
        ty
    }
    pub(super) fn numeric_spec(ty: Ty) -> TypeSpecifierNonArrayData {
        use TypeSpecifierNonArrayData::*;
        match (ty.kind, ty.width) {
            (Kind::Float, 1) => Float,
            (Kind::Float, 2) => Vec2,
            (Kind::Float, 3) => Vec3,
            (Kind::Float, 4) => Vec4,
            (Kind::Int, 1) => Int,
            (Kind::Int, 2) => IVec2,
            (Kind::Int, 3) => IVec3,
            (Kind::Int, 4) => IVec4,
            (Kind::UInt, 1) => UInt,
            (Kind::UInt, 2) => UVec2,
            (Kind::UInt, 3) => UVec3,
            (Kind::UInt, 4) => UVec4,
            (Kind::Bool, 1) => Bool,
            (Kind::Bool, 2) => BVec2,
            (Kind::Bool, 3) => BVec3,
            (Kind::Bool, 4) => BVec4,
            _ => unreachable!(),
        }
    }

    // spirv-cross emits one interface declaration per line. Matching stages need no
    // AST compatibility pass. Ambiguous layouts still take the parsed path below.
    fn simple_declarations(source: &str, direction: &str) -> Option<HashMap<String, u8>> {
        let mut result = HashMap::new();
        for line in source.lines() {
            let words = line.split_whitespace().collect::<Vec<_>>();
            let Some(at) = words.iter().position(|w| *w == direction) else {
                continue;
            };
            let Some((type_at, width)) =
                words.iter().enumerate().skip(at + 1).find_map(|(i, w)| {
                    w.strip_prefix("vec")
                        .and_then(|n| n.parse::<u8>().ok())
                        .map(|n| (i, n))
                })
            else {
                continue;
            };
            let name = *words.get(type_at + 1)?;
            if line.matches(';').count() != 1
                || !line.trim_end().ends_with(';')
                || !name.ends_with(';')
                || name.contains('[')
                || !(2..=4).contains(&width)
            {
                return None;
            }
            let name = name.trim_end_matches(';');
            if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                return None;
            }
            result.insert(name.to_owned(), width);
        }
        Some(result)
    }
    // SPIR-V stages lower independently. DX permits a narrow consumer of a wider
    // varying; GLES requires matching declarations. Read only the authored lanes.
    pub(crate) fn interfaces(vertex: &str, fragment: &str) -> Result<String> {
        if let (Some(outputs), Some(inputs)) = (
            simple_declarations(vertex, "out"),
            simple_declarations(fragment, "in"),
        ) && inputs
            .iter()
            .all(|(name, width)| outputs.get(name).is_none_or(|w| w == width))
        {
            return Ok(fragment.to_owned());
        }
        let declarations =
            |source: &str, direction: StorageQualifierData| -> Result<HashMap<String, u8>> {
                let tree = TranslationUnit::parse(source)
                    .map_err(|e| anyhow::anyhow!("WE shader interface syntax: {e}"))?;
                let mut result = HashMap::new();
                for d in &tree.0 {
                    if let ExternalDeclarationData::Declaration(d) = &**d
                        && let DeclarationData::InitDeclaratorList(list) = &**d
                        && list.head.ty.qualifier.as_ref().is_some_and(|q| {
                            q.qualifiers.iter().any(
                        |q| matches!(&**q, TypeQualifierSpecData::Storage(s) if **s == direction),
                    )
                        })
                    {
                        let ty = Ty::from_spec(&list.head.ty.ty);
                        if ty.kind == Kind::Float && ty.width > 1 && ty.arrays == 0 {
                            if let Some(name) = &list.head.name
                                && list.head.array_specifier.is_none()
                            {
                                result.insert(name.0.to_string(), ty.width);
                            }
                            for tail in &list.tail {
                                if tail.ident.array_spec.is_none() {
                                    result.insert(tail.ident.ident.0.to_string(), ty.width);
                                }
                            }
                        }
                    }
                }
                Ok(result)
            };
        let outputs = declarations(vertex, StorageQualifierData::Out)?;
        let inputs = declarations(fragment, StorageQualifierData::In)?;
        let interfaces = inputs
            .into_iter()
            .filter_map(|(name, width)| {
                outputs.get(&name).filter(|v| **v > width).map(|v| {
                    (
                        name.clone(),
                        (
                            Ty::numeric(Kind::Float, width),
                            Ty::numeric(Kind::Float, *v),
                        ),
                    )
                })
            })
            .collect::<HashMap<_, _>>();
        let narrow = declarations(fragment, StorageQualifierData::In)?
            .into_iter()
            .filter_map(|(name, width)| {
                outputs
                    .get(&name)
                    .filter(|v| **v < width)
                    .map(|v| (name, *v))
            })
            .collect::<HashMap<_, _>>();
        let mut fragment = fragment.to_owned();
        if !narrow.is_empty() {
            let mut tree = TranslationUnit::parse(&fragment)
                .map_err(|e| anyhow::anyhow!("WE shader interface syntax: {e}"))?;
            let mut reads = NarrowReads {
                inputs: &narrow,
                invalid: None,
            };
            glsl_lang::visitor::Host::visit(&tree, &mut reads);
            ensure!(
                reads.invalid.is_none(),
                "WE varying {} reads lanes not provided by the vertex shader",
                reads.invalid.unwrap_or_default()
            );
            for d in &mut tree.0 {
                if let ExternalDeclarationData::Declaration(d) = &mut **d
                && let DeclarationData::InitDeclaratorList(list) = &mut **d
                && let Some(name) = &list.head.name
                && let Some(width) = narrow.get(name.0.as_str())
                && list.head.ty.qualifier.as_ref().is_some_and(|q| q.qualifiers.iter().any(|q| matches!(&**q, TypeQualifierSpecData::Storage(s) if **s == StorageQualifierData::In)))
            {
                list.head.ty.ty.ty = numeric_spec(Ty::numeric(Kind::Float, *width)).into();
            }
            }
            fragment.clear();
            glsl::show_translation_unit(&mut fragment, &tree, glsl::FormattingState::default())?;
        }
        if interfaces.is_empty() && narrow.is_empty() {
            return Ok(fragment);
        }
        lower_with_interfaces(&fragment, interfaces)
    }

    // A larger declaration is harmless only when every read explicitly selects
    // lanes actually written by the producer. Do not invent undefined Z/W values.
    struct NarrowReads<'a> {
        inputs: &'a HashMap<String, u8>,
        invalid: Option<String>,
    }
    impl glsl_lang::visitor::Visitor for NarrowReads<'_> {
        fn visit_expr(&mut self, expr: &Expr) -> Visit {
            let allowed = match &**expr {
                ExprData::Dot(base, field) => {
                    if let ExprData::Variable(name) = &***base
                        && let Some(width) = self.inputs.get(name.0.as_str())
                    {
                        field.0.chars().all(|c| {
                            ["xrs", "ygt", "zbp", "waq"]
                                .iter()
                                .position(|lane| lane.contains(c))
                                .is_some_and(|lane| lane < *width as usize)
                        })
                    } else {
                        false
                    }
                }
                ExprData::Bracket(base, index) => {
                    if let ExprData::Variable(name) = &***base
                        && let Some(width) = self.inputs.get(name.0.as_str())
                    {
                        match &***index {
                            ExprData::IntConst(n) => *n >= 0 && *n < i32::from(*width),
                            ExprData::UIntConst(n) => *n < u32::from(*width),
                            _ => false,
                        }
                    } else {
                        false
                    }
                }
                ExprData::Variable(name) if self.inputs.contains_key(name.0.as_str()) => {
                    self.invalid = Some(name.0.to_string());
                    false
                }
                _ => false,
            };
            if allowed {
                Visit::Parent
            } else {
                Visit::Children
            }
        }
    }

    pub(super) struct InputWrites<'a> {
        pub(super) inputs: &'a HashMap<String, Ty>,
        pub(super) writes: std::collections::HashSet<String>,
        pub(super) functions: &'a HashMap<String, Vec<Function>>,
    }
    impl InputWrites<'_> {
        fn write(&mut self, expr: &Expr) {
            match &**expr {
                ExprData::Variable(name) if self.inputs.contains_key(name.0.as_str()) => {
                    self.writes.insert(name.0.to_string());
                }
                ExprData::Dot(base, _) | ExprData::Bracket(base, _) => self.write(base),
                _ => {}
            }
        }
    }
    impl glsl_lang::visitor::Visitor for InputWrites<'_> {
        fn visit_expr(&mut self, expr: &Expr) -> Visit {
            match &**expr {
                ExprData::Assignment(lhs, _, _)
                | ExprData::PostInc(lhs)
                | ExprData::PostDec(lhs) => self.write(lhs),
                ExprData::Unary(op, lhs) if matches!(**op, UnaryOpData::Inc | UnaryOpData::Dec) => {
                    self.write(lhs)
                }
                ExprData::FunCall(name, args) => {
                    if let Some(name) = name.as_ident()
                        && let Some(functions) = self.functions.get(name.0.as_str())
                    {
                        let indices = functions
                            .iter()
                            .filter(|f| f.parameters.len() == args.len())
                            .flat_map(|f| {
                                f.parameters
                                    .iter()
                                    .enumerate()
                                    .filter(|(_, p)| p.output)
                                    .map(|(i, _)| i)
                            })
                            .collect::<Vec<_>>();
                        for i in indices {
                            self.write(&args[i]);
                        }
                    }
                }
                _ => {}
            }
            Visit::Children
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn worker_compiler_preserves_stages_and_recovers_after_errors() {
        std::thread::spawn(|| {
            let vertex = "#version 450\nvoid main(){gl_Position=vec4(0,0,0,1);}";
            let fragment = "#version 450\nout vec4 color;void main(){color=vec4(1);}";
            let expected = lower(vertex, shaderc::ShaderKind::Vertex).unwrap();
            assert!(lower(vertex, shaderc::ShaderKind::Fragment).is_err());
            let compiled = lower(fragment, shaderc::ShaderKind::Fragment).unwrap();
            assert!(compiled.contains("color = vec4(1.0)"), "{compiled}");
            assert_eq!(
                lower(vertex, shaderc::ShaderKind::Vertex).unwrap(),
                expected
            );
            assert_eq!(
                lower(fragment, shaderc::ShaderKind::Fragment).unwrap(),
                compiled
            );
        })
        .join()
        .unwrap();
    }
    #[test]
    fn unused_variant_inputs_do_not_exhaust_gles_attribute_locations() {
        let mut source = String::from("#version 450\n");
        for index in 0..24 {
            source.push_str(&format!("in vec4 unused{index};\n"));
        }
        source.push_str(
            "in mat4 transform;in vec4 position;void main(){gl_Position=transform*position;}",
        );
        let lowered = lower(&source, shaderc::ShaderKind::Vertex).unwrap();
        assert!(!lowered.contains("unused"), "{lowered}");
        assert!(
            lowered.contains("layout(location = 0) in mat4 transform"),
            "{lowered}"
        );
        assert!(
            lowered.contains("layout(location = 4) in vec4 position"),
            "{lowered}"
        );
    }
    #[test]
    fn precision_qualified_material_defaults_and_legacy_user_bindings_are_named_correctly() {
        let pass = serde_json::json!({"usershadervalues":{"accent":"tint"},"constantshadervalues":{"amount":0.5}});
        let parsed = metadata(
            "uniform lowp vec3 g_Tint; // {\"material\":\"tint\",\"default\":\"0.9 0.1 0.2\"}\n",
            "uniform mediump float g_Amount; // {\"material\":\"amount\",\"default\":0.8}\n",
            &pass,
        );
        assert_eq!(parsed.uniforms[0].name, "g_Tint");
        assert_eq!(parsed.uniforms[0].kind, "vec3");
        let properties = crate::scene::bindings::Properties::from([(
            "accent".into(),
            serde_json::json!([0.1, 0.2, 0.3]),
        )]);
        assert_eq!(
            crate::scene::bindings::resolve(&parsed.uniforms[0].value, &properties).unwrap(),
            serde_json::json!([0.1, 0.2, 0.3])
        );
        assert_eq!(parsed.uniforms[1].value, serde_json::json!(0.5));
    }
    #[test]
    fn hlsl_identifiers_and_direct_vector_assignments_compile_as_glsl() {
        let source = "#version 450\nin vec4 uv;\nout vec4 result;\nvoid main(){\nvec2 coord = uv;\nvec4 sample = vec4(coord,0,1);\nresult = sample;\n}";
        assert!(lower(source, shaderc::ShaderKind::Fragment).is_ok());
    }
    #[test]
    fn compressed_uniform_declarations_do_not_hoist_function_bodies() {
        let source = "#version 450\nout vec4 color;\nuniform vec4 tint;uniform float alpha;void main(){color=tint*alpha;}";
        lower(source, shaderc::ShaderKind::Fragment).unwrap();
    }
    #[test]
    fn hlsl_numeric_scope_packed_spectrum_and_overloads_compile() {
        let source = "#version 450\nin vec3 coord;\nuniform vec4 tint;\nuniform float start;\nuniform float g_AudioSpectrum64Left[64];\nout vec4 color;\nvec3 blend(int mode,vec3 a,vec3 b,float t){return mix(a,b,t);}\nvec3 narrow(){vec4 col=tint;return col;}\nfloat packed(float bar){return g_AudioSpectrum64Left[bar/4][bar%4];}\nvoid main(){vec2 p=abs(coord-vec2(0.5));float total=0;for(int i=start;i<16;i++){total+=g_AudioSpectrum64Left[i];}vec4 col=vec4(mix(narrow(),tint,0.5),1);float neg=-5.5;neg%=4;color=vec4(max(0,blend(31,col,tint,1)),packed(13.0)+neg+total+length(p));}";
        lower(source, shaderc::ShaderKind::Fragment).unwrap();
        // A float array has no general packed-vector semantics.
        assert!(lower("#version 450\nuniform float arbitrary[64];out vec4 color;void main(){color=vec4(arbitrary[1][2]);}",shaderc::ShaderKind::Fragment).is_err());
    }
    #[test]
    fn varying_alignment_requires_all_read_lanes_to_exist() {
        let vertex = "#version 300 es\nprecision highp float;\nout vec2 coord;void main(){coord=vec2(0.2);gl_Position=vec4(0);}";
        let fragment = "#version 300 es\nprecision highp float;\nin vec4 coord;out vec4 color;void main(){color=vec4(coord.xy,coord[1],1);}";
        let aligned = semantics::interfaces(vertex, fragment).unwrap();
        assert!(aligned.contains("in vec2 coord"));
        for read in ["coord.z", "coord[2]", "coord", "coord[index]"] {
            let source = format!(
                "#version 300 es\nprecision highp float;\nin vec4 coord;out vec4 color;void main(){{color=vec4({read});}}"
            );
            assert!(semantics::interfaces(vertex, &source).is_err(), "{read}");
        }
    }
}
