//! WE's shipped SDR bloom shaders and floating-point HDR pyramid.
use crate::{
    assets::Assets,
    gpu::{Buffer, Frame, Mesh, Pass, Texture},
    scene::bindings::{Properties, components},
};
use anyhow::{Result, ensure};
use glam::{Mat4, Vec4};
use glow::HasContext;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    rc::Rc,
};
#[derive(Clone, PartialEq)]
pub(crate) struct Config {
    pub hdr: bool,
    pub bloom: bool,
    strength: f32,
    threshold: f32,
    tint: Vec4,
    iterations: usize,
    scatter: f32,
    feather: f32,
}
impl Config {
    pub fn prepare(value: &Value) -> Result<Self> {
        let scalar = |name: &str, default: f32| -> Result<f32> {
            Ok(components(&value[name], &[default], 1)?[0])
        };
        let hdr = value["hdr"].as_bool().unwrap_or(false);
        let prefix = if hdr { "bloomhdr" } else { "bloom" };
        let strength = scalar(&format!("{prefix}strength"), 2.)?;
        let threshold = scalar(&format!("{prefix}threshold"), if hdr { 1. } else { 0.65 })?;
        let iterations = scalar("bloomhdriterations", 8.)?;
        let scatter = scalar("bloomhdrscatter", 1.)?;
        let feather = scalar("bloomhdrfeather", 0.1)?;
        ensure!(
            (0.0..=64.).contains(&strength)
                && (0.0..=64.).contains(&threshold)
                && (1.0..=12.).contains(&iterations)
                && (0.0..=8.).contains(&scatter)
                && (0.0..=1.).contains(&feather),
            "invalid bloom configuration"
        );
        let color = components(&value["bloomtint"], &[1.; 3], 3)?;
        Ok(Self {
            hdr,
            bloom: value["bloom"].as_bool().unwrap_or(false),
            strength,
            threshold,
            tint: Vec4::new(color[0], color[1], color[2], 1.),
            iterations: iterations as usize,
            scatter,
            feather,
        })
    }
    pub fn enabled(&self) -> bool {
        self.hdr || self.bloom
    }
}
pub(crate) struct Postprocess {
    gl: Rc<glow::Context>,
    sdr: Vec<Pass>,
    hdr: Vec<Pass>,
    buffers: Vec<Buffer>,
    size: [u32; 2],
    format: u32,
    allocated: u64,
    configured: Option<Config>,
}
impl Postprocess {
    pub fn resources(&self, usage: &mut crate::gpu::Usage) {
        for buffer in &self.buffers {
            usage.buffer(buffer);
        }
        for pass in self.sdr.iter().chain(&self.hdr) {
            usage.pass(pass);
        }
    }
    pub fn load(gl: Rc<glow::Context>, assets: &Assets) -> Result<Self> {
        let mut cache = HashMap::new();
        let mut budget = 0;
        let mut load = |shader: &str, combos: Value| -> Result<Pass> {
            Pass::load(
                gl.clone(),
                assets,
                &json!({"shader":shader,"combos":combos,"textures":["previous","previous"],"blending":"normal"}),
                &Properties::new(),
                &HashSet::new(),
                &mut cache,
                &mut budget,
                crate::shader::MaterialDomain::Effect,
            )
        };
        let sdr = [
            "downsample_quarter_bloom",
            "downsample_eighth_blur_v",
            "blur_h_bloom",
            "combine",
        ]
        .into_iter()
        .map(|s| load(s, json!({})))
        .collect::<Result<Vec<_>>>()?;
        let hdr = vec![
            load("hdr_downsample", json!({"BLOOM":1}))?,
            load("hdr_downsample", json!({}))?,
            load("hdr_downsample", json!({"UPSAMPLE":1}))?,
            load("combine_hdr", json!({"LINEAR":1}))?,
        ];
        Ok(Self {
            gl,
            sdr,
            hdr,
            buffers: Vec::new(),
            size: [0; 2],
            format: 0,
            allocated: 0,
            configured: None,
        })
    }
    fn prepare(&mut self, config: &Config, size: [u32; 2], budget: &mut u64) -> Result<()> {
        if self.configured.as_ref() != Some(config) {
            let constants = json!({"bloomstrength":config.strength,"bloomthreshold":config.threshold,"bloomtint":[config.tint.x,config.tint.y,config.tint.z],"scatter":config.scatter,"blend":[config.threshold,config.threshold-config.threshold*config.feather,2.*config.threshold*config.feather,0.25/(config.threshold*config.feather+1e-5)]});
            for pass in self.sdr.iter_mut().chain(&mut self.hdr) {
                pass.apply_constants(&constants)?;
            }
        }
        let format = if config.hdr {
            glow::RGBA16F
        } else {
            glow::RGBA8
        };
        let bloom = config.bloom && config.strength > 0.;
        let levels = if !bloom {
            1
        } else if config.hdr {
            config
                .iterations
                .min(size[0].min(size[1]).ilog2().max(1) as usize)
        } else {
            3
        };
        if self.size != size
            || self.format != format
            || self.buffers.len() != levels
            || self
                .configured
                .as_ref()
                .is_some_and(|old| (old.bloom && old.strength > 0.) != bloom)
        {
            let mut candidate_budget = *budget;
            let mut buffers = Vec::with_capacity(levels);
            let mut allocated = 0;
            for level in 0..levels {
                let divisor = if config.hdr {
                    1 << (level + 1)
                } else {
                    if level == 0 { 4 } else { 8 }
                };
                let dimensions = if bloom {
                    size.map(|v| (v / divisor).max(1))
                } else {
                    [1; 2]
                };
                let buffer =
                    Buffer::formatted(self.gl.clone(), dimensions, format, &mut candidate_budget)?;
                allocated += buffer.texture.byte_size();
                buffers.push(buffer);
            }
            *budget = candidate_budget.saturating_sub(self.allocated);
            self.buffers = buffers;
            self.allocated = allocated;
            self.size = size;
            self.format = format;
        }
        self.configured = Some(config.clone());
        Ok(())
    }
    pub fn draw(
        &mut self,
        config: &Config,
        source: &Texture,
        target: Option<glow::Framebuffer>,
        quad: &Mesh,
        frame: &Frame<'_>,
        budget: &mut u64,
    ) -> Result<()> {
        self.prepare(config, frame.screen, budget)?;
        if config.bloom && config.strength > 0. {
            if config.hdr {
                let mut input = source;
                for (index, buffer) in self.buffers.iter().enumerate() {
                    let pass = &self.hdr[usize::from(index != 0)];
                    self.bind(Some(buffer.handle), buffer.texture.size, false);
                    Self::offsets(pass, input);
                    pass.draw(
                        quad,
                        Mat4::IDENTITY,
                        frame,
                        &[Some(input), None, None, None, None, None, None, None],
                        Vec4::ONE,
                    );
                    input = &buffer.texture;
                }
                for level in (1..self.buffers.len()).rev() {
                    let source = &self.buffers[level].texture;
                    let destination = &self.buffers[level - 1];
                    let pass = &self.hdr[2];
                    self.bind(Some(destination.handle), destination.texture.size, true);
                    Self::offsets(pass, source);
                    pass.draw(
                        quad,
                        Mat4::IDENTITY,
                        frame,
                        &[Some(source), None, None, None, None, None, None, None],
                        Vec4::ONE,
                    );
                }
            } else {
                for i in 0..3 {
                    self.bind(
                        Some(self.buffers[i].handle),
                        self.buffers[i].texture.size,
                        false,
                    );
                    let input = if i == 0 {
                        source
                    } else {
                        &self.buffers[i - 1].texture
                    };
                    self.sdr[i].draw(
                        quad,
                        Mat4::IDENTITY,
                        frame,
                        &[Some(input), None, None, None, None, None, None, None],
                        Vec4::ONE,
                    );
                }
            }
        }
        self.bind(target, frame.screen, false);
        // A disabled bloom samples one immutable black texel instead of retaining
        // and clearing a full pyramid. HDR tone mapping still runs normally.
        let bloom = &self.buffers[if config.hdr || !(config.bloom && config.strength > 0.) {
            0
        } else {
            2
        }]
        .texture;
        let pass = if config.hdr {
            &self.hdr[3]
        } else {
            &self.sdr[3]
        };
        pass.vector4("g_RenderVar0", Vec4::new(1., 0., 0., 0.));
        pass.draw(
            quad,
            Mat4::IDENTITY,
            frame,
            &[
                Some(source),
                Some(bloom),
                None,
                None,
                None,
                None,
                None,
                None,
            ],
            Vec4::ONE,
        );
        Ok(())
    }
    fn offsets(pass: &Pass, input: &Texture) {
        let x = 0.5 / input.size[0] as f32;
        let y = 0.5 / input.size[1] as f32;
        pass.vector4("g_RenderVar0", Vec4::new(x, y, -x, -y));
    }
    fn bind(&self, target: Option<glow::Framebuffer>, size: [u32; 2], additive: bool) {
        unsafe {
            self.gl.bind_framebuffer(glow::FRAMEBUFFER, target);
            self.gl.viewport(0, 0, size[0] as i32, size[1] as i32);
            self.gl.disable(glow::DEPTH_TEST);
            self.gl.disable(glow::SCISSOR_TEST);
            self.gl.color_mask(true, true, true, true);
            if additive {
                self.gl.enable(glow::BLEND);
                self.gl.blend_func(glow::ONE, glow::ONE);
            } else {
                self.gl.disable(glow::BLEND);
            }
        }
    }
}
