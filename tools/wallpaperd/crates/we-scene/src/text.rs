//! Shaping/font fallback is shared per scene; layouts and glyph rasters survive frames.

use crate::{
    assets::{Assets, Texture},
    scene::bindings::components,
};
use anyhow::{Context, Result, ensure};
use cosmic_text::{
    Attrs, Buffer, CacheKey, Color, Family, FontSystem, Metrics, Shaping, Stretch, Style,
    SwashCache, Weight, Wrap,
};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};

pub(crate) struct TextSystem {
    fonts: Option<FontSystem>,
    pending_fonts: Vec<cosmic_text::fontdb::Source>,
    swash: SwashCache,
    project_fonts: HashMap<String, Face>,
    font_bytes: usize,
    generation: u64,
    glyph_bytes: usize,
    glyph_count: usize,
}
#[derive(Clone)]
struct Face {
    family: String,
    weight: Weight,
    style: Style,
    stretch: Stretch,
}
impl Face {
    fn named(family: String) -> Self {
        Self {
            family,
            weight: Weight::NORMAL,
            style: Style::Normal,
            stretch: Stretch::Normal,
        }
    }
    fn attrs(&self) -> Attrs<'_> {
        Attrs::new()
            .family(if self.family == "sans-serif" {
                Family::SansSerif
            } else {
                Family::Name(&self.family)
            })
            .weight(self.weight)
            .style(self.style)
            .stretch(self.stretch)
    }
}
impl TextSystem {
    pub fn new() -> Self {
        Self {
            fonts: None,
            pending_fonts: Vec::new(),
            swash: SwashCache::new(),
            project_fonts: HashMap::new(),
            font_bytes: 0,
            generation: 0,
            glyph_bytes: 0,
            glyph_count: 0,
        }
    }
    fn font_system(&mut self) -> &mut FontSystem {
        self.fonts.get_or_insert_with(|| {
            FontSystem::new_with_fonts(std::mem::take(&mut self.pending_fonts))
        })
    }
    fn family(&mut self, assets: &Assets, name: &str) -> Result<Face> {
        if name.is_empty() {
            return Ok(Face::named("sans-serif".into()));
        }
        if let Some(family) = self.project_fonts.get(name) {
            return Ok(family.clone());
        }
        if let Some(family) = name.strip_prefix("systemfont_") {
            return Ok(Face::named(family.replace('_', " ")));
        }
        if !name.contains('/') && !name.ends_with(".ttf") && !name.ends_with(".otf") {
            return Ok(Face::named(name.into()));
        }
        ensure!(
            self.project_fonts.len() < 256,
            "project font name cache exceeds 256 entries"
        );
        let Some(data) = assets.optional_read(name)? else {
            let face = Face::named("sans-serif".into());
            self.project_fonts.insert(name.into(), face.clone());
            return Ok(face);
        };
        ensure!(
            data.len() <= 32 * 1024 * 1024 && self.font_bytes + data.len() <= 64 * 1024 * 1024,
            "project font budget exceeded: {name}, bytes={}, used={}",
            data.len(),
            self.font_bytes
        );
        let bytes = data.len();
        let source = cosmic_text::fontdb::Source::Binary(std::sync::Arc::new(data));
        // Hidden text still validates its project face, without scanning or
        // retaining the installed system fonts until shaping is actually needed.
        let mut pending = cosmic_text::fontdb::Database::new();
        let db = self.fonts.as_mut().map_or(&mut pending, FontSystem::db_mut);
        let old = db.faces().map(|f| f.id).collect::<HashSet<_>>();
        db.load_font_source(source.clone());
        let family = db
            .faces()
            .find(|f| !old.contains(&f.id))
            .and_then(|f| {
                f.families.first().map(|family| Face {
                    family: family.0.clone(),
                    weight: f.weight,
                    style: f.style,
                    stretch: f.stretch,
                })
            })
            .context("project font contains no usable faces")?;
        if self.fonts.is_none() {
            self.pending_fonts.push(source);
        }
        self.project_fonts.insert(name.into(), family.clone());
        self.font_bytes += bytes;
        self.generation += 1;
        Ok(family)
    }
    fn trim_glyphs(&mut self, key: CacheKey) {
        if self.swash.image_cache.len() > self.glyph_count {
            self.glyph_bytes += self
                .swash
                .image_cache
                .get(&key)
                .and_then(Option::as_ref)
                .map_or(0, |image| image.data.len());
            self.glyph_count = self.swash.image_cache.len();
        }
        if self.glyph_bytes > 64 * 1024 * 1024 || self.glyph_count > 8192 {
            self.swash = SwashCache::new();
            self.glyph_bytes = 0;
            self.glyph_count = 0;
        }
    }
}
pub(crate) struct Raster {
    pub pixels: Texture,
    pub offset: [f32; 2],
}
#[derive(Default)]
pub(crate) struct TextCache {
    shape: Value,
    paint: Value,
    glyphs: Vec<(CacheKey, i32, i32)>,
    bounds: [i32; 4],
    pub rebuilds: usize,
}
impl TextCache {
    pub fn update(
        &mut self,
        system: &mut TextSystem,
        assets: &Assets,
        object: &Value,
        box_size: [f32; 2],
    ) -> Result<Option<Raster>> {
        self.prepare(system, assets, object, box_size, true)
    }

    pub fn prepare(
        &mut self,
        system: &mut TextSystem,
        assets: &Assets,
        object: &Value,
        box_size: [f32; 2],
        rasterize: bool,
    ) -> Result<Option<Raster>> {
        let text = object["text"]
            .as_str()
            .context("text property must be a string")?;
        ensure!(text.len() <= 64 * 1024, "text exceeds 64 KiB");
        let scalar = |key: &str, default: f32| -> Result<f32> {
            Ok(components(&object[key], &[default], 1)?[0])
        };
        let points = scalar("pointsize", 32.0)?;
        let px = points * 300.0 / 72.0;
        ensure!(px > 0.0 && px <= 2048.0, "invalid text point size");
        let spacing = components(&object["spacing"], &[0.0; 2], 2)?;
        let line_height = (px * 1.25 + spacing[1]).max(1.0);
        let font = object["font"].as_str().unwrap_or("");
        let family = system.family(assets, font)?;
        let halign = object["horizontalalign"].as_str().unwrap_or("center");
        let valign = object["verticalalign"].as_str().unwrap_or("center");
        ensure!(
            ["left", "center", "right"].contains(&halign)
                && ["top", "center", "bottom"].contains(&valign),
            "invalid text alignment"
        );
        let padding = crate::scene::bindings::components(&object["padding"], &[0.; 2], 2)?
            .into_iter()
            .map(|v| v.max(0.))
            .collect::<Vec<_>>();
        let width = if object["limitwidth"].as_bool().unwrap_or(false) {
            Some(scalar("maxwidth", box_size[0])?.max(1.0))
        } else {
            None
        };
        let rows = if object["limitrows"].as_bool().unwrap_or(false) {
            scalar("maxrows", 1.0)?.clamp(1.0, 4096.0) as usize
        } else {
            4096
        };
        let effects = raster::Effects::parse(object)?;
        if !rasterize {
            components(&object["color"], &[1.; 3], 3)?;
            scalar("brightness", 1.)?;
            if object["opaquebackground"].as_bool().unwrap_or(false) {
                components(&object["backgroundcolor"], &[0.; 3], 3)?;
                scalar("backgroundbrightness", 1.)?;
            }
            return Ok(None);
        }
        let shape = json!([
            text,
            font,
            px,
            line_height,
            spacing,
            halign,
            valign,
            width,
            rows,
            object["limituseellipsis"],
            box_size,
            padding,
            object["opaquebackground"],
            system.generation
        ]);
        let paint = json!([
            shape,
            object["color"],
            object["brightness"],
            object["opaquebackground"],
            object["backgroundcolor"],
            object["backgroundbrightness"],
            effects.key()
        ]);
        if self.paint == paint {
            return Ok(None);
        }
        system.font_system();
        if self.shape != shape {
            // Styles come from the selected face, including project Bold/Italic/Light files.
            let attrs = family.attrs();
            let mut buffer = Buffer::new(
                system.fonts.as_mut().unwrap(),
                Metrics::new(px, line_height),
            );
            buffer.set_size(width, None);
            buffer.set_wrap(if width.is_some() {
                Wrap::WordOrGlyph
            } else {
                Wrap::None
            });
            buffer.set_text(text, &attrs, Shaping::Advanced, None);
            buffer.shape_until_scroll(system.fonts.as_mut().unwrap(), false);
            let all = buffer.layout_runs().collect::<Vec<_>>();
            ensure!(all.len() <= 4096, "text exceeds 4096 rows");
            let count = all.len().min(rows);
            let total = count as f32 * line_height;
            let y = match valign {
                "top" => 0.0,
                "bottom" => -total,
                _ => -total / 2.0,
            };
            let mut glyphs = Vec::new();
            let mut min = [0.0_f32, y];
            let mut max = [0.0_f32, y + total];
            let mut ellipsis = Buffer::new(
                system.fonts.as_mut().unwrap(),
                Metrics::new(px, line_height),
            );
            ellipsis.set_text("…", &attrs, Shaping::Advanced, None);
            ellipsis.shape_until_scroll(system.fonts.as_mut().unwrap(), false);
            let ellipse = ellipsis.layout_runs().next();
            for (row, run) in all.iter().take(count).enumerate() {
                let extra = spacing[0] * run.glyphs.len().saturating_sub(1) as f32;
                let row_width = run.line_w + extra;
                let x = match halign {
                    "left" => 0.0,
                    "right" => -row_width,
                    _ => -row_width / 2.0,
                };
                min[0] = min[0].min(x);
                max[0] = max[0].max(x + row_width);
                let truncated = row + 1 == count
                    && all.len() > count
                    && object["limituseellipsis"].as_bool().unwrap_or(false);
                let ellipse_width = ellipse.as_ref().map_or(0.0, |r| r.line_w);
                let clip = if truncated {
                    width.unwrap_or(row_width) - ellipse_width
                } else {
                    f32::INFINITY
                };
                for (index, glyph) in run.glyphs.iter().enumerate() {
                    if glyph.x + glyph.w + spacing[0] * index as f32 > clip {
                        continue;
                    }
                    let physical =
                        glyph.physical((x + spacing[0] * index as f32, y + run.line_y), 1.0);
                    glyphs.push((physical.cache_key, physical.x, physical.y));
                }
                if truncated && let Some(ellipse) = &ellipse {
                    for glyph in ellipse.glyphs {
                        let physical = glyph.physical((x + clip, y + run.line_y), 1.0);
                        glyphs.push((physical.cache_key, physical.x, physical.y));
                    }
                }
            }
            // Include true glyph bearings, accents and descenders before allocating the bitmap.
            for &(key, x, y) in &glyphs {
                if let Some(image) = system.swash.get_image(system.fonts.as_mut().unwrap(), key) {
                    let left = x + image.placement.left;
                    let top = y - image.placement.top;
                    min[0] = min[0].min(left as f32);
                    min[1] = min[1].min(top as f32);
                    max[0] = max[0].max(left as f32 + image.placement.width as f32);
                    max[1] = max[1].max(top as f32 + image.placement.height as f32);
                }
                system.trim_glyphs(key);
            }
            if object["opaquebackground"].as_bool().unwrap_or(false) {
                min[0] = min[0].min(-box_size[0] / 2.0);
                min[1] = min[1].min(-box_size[1] / 2.0);
                max[0] = max[0].max(box_size[0] / 2.0);
                max[1] = max[1].max(box_size[1] / 2.0);
            }
            let bounds = [
                min[0] - padding[0],
                min[1] - padding[1],
                max[0] + padding[0],
                max[1] + padding[1],
            ];
            ensure!(
                bounds
                    .iter()
                    .all(|v| v.is_finite() && v.abs() < i32::MAX as f32 / 2.),
                "invalid text bounds"
            );
            self.bounds = [
                bounds[0].floor() as i32,
                bounds[1].floor() as i32,
                bounds[2].ceil() as i32,
                bounds[3].ceil() as i32,
            ];
            self.glyphs = glyphs;
            self.shape = shape;
            self.rebuilds += 1;
        }
        let [left, top, right, bottom] = effects.bounds(self.bounds)?;
        let size = [(right - left).max(1) as u32, (bottom - top).max(1) as u32];
        ensure!(
            size.iter().all(|v| *v <= 8192) && size[0] as u64 * size[1] as u64 <= 16 * 1024 * 1024,
            "text bitmap exceeds 64 MiB"
        );
        let color = components(&object["color"], &[1.0; 3], 3)?;
        let brightness = scalar("brightness", 1.0)?;
        let rgb = color
            .iter()
            .map(|v| (v * brightness * 255.0).clamp(0.0, 255.0) as u8)
            .collect::<Vec<_>>();
        let mut rgba = vec![0u8; size[0] as usize * size[1] as usize * 4];
        // Straight-alpha sampling must retain the glyph color even at zero coverage,
        // otherwise scaling and blur interpolate black into the glyph edges.
        for pixel in rgba.as_chunks_mut::<4>().0 {
            pixel[..3].copy_from_slice(&rgb);
        }
        let background = if object["opaquebackground"].as_bool().unwrap_or(false) {
            let color = components(&object["backgroundcolor"], &[0.0; 3], 3)?;
            let brightness = scalar("backgroundbrightness", 1.0)?;
            let background = [
                (color[0] * brightness * 255.0).clamp(0.0, 255.0) as u8,
                (color[1] * brightness * 255.0).clamp(0.0, 255.0) as u8,
                (color[2] * brightness * 255.0).clamp(0.0, 255.0) as u8,
                (effects.opacity() * 255.).round() as u8,
            ];
            Some(background)
        } else {
            None
        };
        if !effects.active()
            && let Some(background) = background
        {
            for pixel in rgba.as_chunks_mut::<4>().0 {
                *pixel = background;
            }
        }
        for &(key, x, y) in &self.glyphs {
            system.swash.with_pixels(
                system.fonts.as_mut().unwrap(),
                key,
                Color::rgb(rgb[0], rgb[1], rgb[2]),
                |gx, gy, color| {
                    let x = x + gx - left;
                    let y = y + gy - top;
                    if x < 0 || y < 0 || x >= size[0] as i32 || y >= size[1] as i32 {
                        return;
                    }
                    let offset = (y as usize * size[0] as usize + x as usize) * 4;
                    raster::over(&mut rgba[offset..offset + 4], color.as_rgba());
                },
            );
            system.trim_glyphs(key);
        }
        effects.paint(&mut rgba, size, &rgb)?;
        if effects.active()
            && let Some(background) = background
        {
            for pixel in rgba.as_chunks_mut::<4>().0 {
                let source = *pixel;
                *pixel = background;
                raster::over(pixel, source);
            }
        }
        self.paint = paint;
        Ok(Some(Raster {
            pixels: Texture {
                compressed: None,
                mipmaps: Vec::new(),
                flags: 2,
                video: None,
                frames: Vec::new(),
                width: size[0],
                height: size[1],
                content: size,
                rgba,
            },
            offset: [
                left as f32 + size[0] as f32 / 2.0,
                -(top as f32 + size[1] as f32 / 2.0),
            ],
        }))
    }
}

use glam::Vec4;
pub(super) fn material_color(object: &Value, color: Vec4) -> Vec4 {
    // The official shader applies layer alpha separately to glyph and shadow before blending.
    // Drop shadows bake it into the bitmap; the original plain-text material path stays intact.
    Vec4::new(
        1.,
        1.,
        1.,
        if object["dropshadow"].as_bool().unwrap_or(false) {
            1.
        } else {
            color.w
        },
    )
}

mod raster {
    //! CPU coverage follows the official `assets/shaders/font.frag` distance thresholds.
    //! Swash coverage supplies the contour; no extra font engine or per-frame atlas is needed.
    use crate::scene::bindings::components;
    use anyhow::{Result, ensure};
    use serde_json::{Value, json};

    pub(super) struct Effects {
        outline: f32,
        outline_color: [f32; 3],
        blur: f32,
        shadow: Option<Shadow>,
        opacity: f32,
    }
    struct Shadow {
        radius: f32,
        offset: [f32; 2],
        color: [f32; 3],
        opacity: f32,
    }
    impl Effects {
        pub fn parse(object: &Value) -> Result<Self> {
            let enabled = |key: &str| object[key].as_bool().unwrap_or(false);
            let scalar = |key: &str, default| {
                Ok::<_, anyhow::Error>(components(&object[key], &[default], 1)?[0])
            };
            let color = |key: &str| -> Result<[f32; 3]> {
                Ok(components(&object[key], &[0.; 3], 3)?.try_into().unwrap())
            };
            // Official constructor defaults: outline=4, blur/shadow size=6, shadow offset=(4,4).
            // Styles come from font faces. `castshadow` is a light/3D flag, not a text drop shadow.
            // Any of these effects selects the official distance-field path, even without `msdf`.
            Ok(Self {
                outline: if enabled("outline") {
                    scalar("outlinethickness", 4.)?.max(1.)
                } else {
                    0.
                },
                outline_color: if enabled("outline") {
                    color("outlinecolor")?
                } else {
                    [0.; 3]
                },
                blur: if enabled("blur") {
                    scalar("blursize", 6.)?.max(0.01)
                } else {
                    0.
                },
                shadow: if enabled("dropshadow") {
                    Some(Shadow {
                        radius: scalar("dropshadowsize", 6.)?.max(0.01),
                        offset: components(&object["dropshadowoffset"], &[4.; 2], 2)?
                            .try_into()
                            .unwrap(),
                        color: color("dropshadowcolor")?,
                        opacity: scalar("dropshadowopacity", 1.)?.max(0.),
                    })
                } else {
                    None
                },
                opacity: if enabled("dropshadow") {
                    scalar("alpha", 1.)?.clamp(0., 1.)
                } else {
                    1.
                },
            })
        }
        pub fn key(&self) -> Value {
            json!([
                self.outline,
                self.outline_color,
                self.blur,
                self.opacity,
                self.shadow
                    .as_ref()
                    .map(|s| json!([s.radius, s.offset, s.color, s.opacity]))
            ])
        }
        pub fn active(&self) -> bool {
            self.outline > 0. || self.blur > 0. || self.shadow.is_some()
        }
        pub fn opacity(&self) -> f32 {
            self.opacity
        }
        pub fn bounds(&self, bounds: [i32; 4]) -> Result<[i32; 4]> {
            let [left, top, right, bottom] = bounds.map(|v| v as f64);
            let margin = if self.active() {
                (self.outline as f64 + self.blur.max(0.5) as f64).ceil()
            } else {
                0.
            };
            let mut result = [left - margin, top - margin, right + margin, bottom + margin];
            if let Some(shadow) = &self.shadow {
                let extent = self.outline as f64 + shadow.radius.max(0.5) as f64;
                result[0] = result[0]
                    .min(left + shadow.offset[0] as f64 - extent)
                    .floor();
                result[1] = result[1]
                    .min(top + shadow.offset[1] as f64 - extent)
                    .floor();
                result[2] = result[2]
                    .max(right + shadow.offset[0] as f64 + extent)
                    .ceil();
                result[3] = result[3]
                    .max(bottom + shadow.offset[1] as f64 + extent)
                    .ceil();
            }
            ensure!(
                result.iter().all(|v| v.abs() < i32::MAX as f64 / 2.)
                    && result[2] - result[0] <= 8192.
                    && result[3] - result[1] <= 8192.,
                "text effect bounds exceed bitmap limit"
            );
            let bounds = result.map(|v| v as i32);
            if self.active() {
                check_workspace([
                    (bounds[2] - bounds[0]).max(1) as u32,
                    (bounds[3] - bounds[1]).max(1) as u32,
                ])?;
            }
            Ok(bounds)
        }
        pub fn paint(&self, rgba: &mut [u8], size: [u32; 2], rgb: &[u8]) -> Result<()> {
            if !self.active() {
                return Ok(());
            }
            let distances = distances(rgba, size)?;
            let smooth = self.blur > 0. || self.shadow.is_some();
            for (index, pixel) in rgba.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                let distance = distances[index];
                let fill = if smooth {
                    coverage(distance, 0., self.blur, true)
                } else {
                    pixel[3] as f32 / 255.
                };
                let alpha = if self.outline > 0. {
                    coverage(distance, self.outline, self.blur, smooth)
                } else {
                    fill
                };
                if pixel[3] == 0 {
                    pixel[..3].copy_from_slice(rgb);
                }
                if self.outline > 0. {
                    for (channel, value) in pixel.iter_mut().take(3).enumerate() {
                        *value = (self.outline_color[channel].clamp(0., 1.) * 255. * (1. - fill)
                            + *value as f32 * fill)
                            .round() as u8;
                    }
                }
                pixel[3] = (alpha * self.opacity * 255.).round() as u8;
                if let Some(shadow) = &self.shadow {
                    let x = (index % size[0] as usize) as f32 - shadow.offset[0];
                    let y = (index / size[0] as usize) as f32 - shadow.offset[1];
                    let alpha = (shadow.opacity
                        * coverage(
                            sample(&distances, size, x, y),
                            self.outline,
                            shadow.radius,
                            true,
                        ))
                    .clamp(0., 1.)
                        * self.opacity;
                    let glyph = *pixel;
                    *pixel = [
                        (shadow.color[0].clamp(0., 1.) * 255.).round() as u8,
                        (shadow.color[1].clamp(0., 1.) * 255.).round() as u8,
                        (shadow.color[2].clamp(0., 1.) * 255.).round() as u8,
                        (alpha * 255.).round() as u8,
                    ];
                    // Straight-alpha source-over: background, then shadow, then outlined glyph.
                    over(pixel, glyph);
                }
            }
            Ok(())
        }
    }

    fn sample(distances: &[f32], size: [u32; 2], x: f32, y: f32) -> f32 {
        let (left, top) = (x.floor() as i32, y.floor() as i32);
        let at = |x: i32, y: i32| {
            if x < 0 || y < 0 || x >= size[0] as i32 || y >= size[1] as i32 {
                -1e6
            } else {
                distances[y as usize * size[0] as usize + x as usize]
            }
        };
        let (fx, fy) = (x - x.floor(), y - y.floor());
        let a = at(left, top) * (1. - fx) + at(left + 1, top) * fx;
        let b = at(left, top + 1) * (1. - fx) + at(left + 1, top + 1) * fx;
        a * (1. - fy) + b * fy
    }

    pub(super) fn over(pixel: &mut [u8], source: [u8; 4]) {
        let a = source[3] as f32 / 255.;
        let old = pixel[3] as f32 / 255.;
        let out = a + old * (1. - a);
        if out > 0. {
            for channel in 0..3 {
                pixel[channel] =
                    ((source[channel] as f32 * a + pixel[channel] as f32 * old * (1. - a)) / out)
                        .round() as u8;
            }
        }
        pixel[3] = (out * 255.).round() as u8;
    }

    pub(super) fn coverage(distance: f32, width: f32, radius: f32, smooth: bool) -> f32 {
        if smooth {
            let radius = radius.max(0.5);
            let t = ((distance + width + radius) / (2. * radius)).clamp(0., 1.);
            t * t * (3. - 2. * t)
        } else {
            (distance + width + 0.5).clamp(0., 1.)
        }
    }

    pub(super) fn distances(rgba: &[u8], size: [u32; 2]) -> Result<Vec<f32>> {
        let count = rgba.len() / 4;
        check_workspace(size)?;
        let mut outer = Vec::with_capacity(count);
        let mut inner = Vec::with_capacity(count);
        for pixel in rgba.as_chunks::<4>().0 {
            let alpha = pixel[3] as f32 / 255.;
            outer.push(if alpha == 0. {
                1e12
            } else {
                (0.5 - alpha).max(0.).powi(2)
            });
            inner.push(if alpha == 1. {
                1e12
            } else {
                (alpha - 0.5).max(0.).powi(2)
            });
        }
        let mut scratch = DistanceScratch::new(size[0].max(size[1]) as usize);
        scratch.transform(&mut outer, size);
        scratch.transform(&mut inner, size);
        for (outside, inside) in outer.iter_mut().zip(inner) {
            *outside = inside.sqrt() - outside.sqrt();
        }
        Ok(outer)
    }

    fn check_workspace(size: [u32; 2]) -> Result<()> {
        // Both planes and the longest scanline share this ceiling; no per-layer distance cache.
        let planes = size[0] as u64 * size[1] as u64 * 8;
        let scanline =
            size[0].max(size[1]) as u64 * (8 + std::mem::size_of::<usize>() as u64 + 8) + 8;
        ensure!(
            planes + scanline <= 64 * 1024 * 1024,
            "text effect workspace exceeds 64 MiB"
        );
        Ok(())
    }

    // Separable squared Euclidean distance transform, linear in bitmap area rather than radius.
    struct DistanceScratch {
        line: Vec<f32>,
        input: Vec<f32>,
        sites: Vec<usize>,
        edges: Vec<f64>,
    }
    impl DistanceScratch {
        fn new(length: usize) -> Self {
            Self {
                line: vec![0.; length],
                input: vec![0.; length],
                sites: vec![0; length],
                edges: vec![0.; length + 1],
            }
        }
        fn transform(&mut self, grid: &mut [f32], size: [u32; 2]) {
            let [width, height] = size.map(|v| v as usize);
            for row in grid.chunks_exact_mut(width) {
                self.line[..width].copy_from_slice(row);
                self.transform_line(width);
                row.copy_from_slice(&self.line[..width]);
            }
            for x in 0..width {
                for y in 0..height {
                    self.line[y] = grid[y * width + x];
                }
                self.transform_line(height);
                for y in 0..height {
                    grid[y * width + x] = self.line[y];
                }
            }
        }
        fn transform_line(&mut self, length: usize) {
            self.input[..length].copy_from_slice(&self.line[..length]);
            let mut last = 0;
            self.sites[0] = 0;
            self.edges[0] = f64::NEG_INFINITY;
            self.edges[1] = f64::INFINITY;
            for q in 1..length {
                let mut edge;
                loop {
                    let p = self.sites[last];
                    edge = (self.input[q] as f64 + (q * q) as f64
                        - self.input[p] as f64
                        - (p * p) as f64)
                        / (2. * (q - p) as f64);
                    if edge > self.edges[last] {
                        break;
                    }
                    last -= 1;
                }
                last += 1;
                self.sites[last] = q;
                self.edges[last] = edge;
                self.edges[last + 1] = f64::INFINITY;
            }
            let mut site = 0;
            for q in 0..length {
                while self.edges[site + 1] < q as f64 {
                    site += 1;
                }
                let p = self.sites[site];
                self.line[q] = (q as f32 - p as f32).powi(2) + self.input[p];
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn distance_transform_matches_brute_force_and_preserves_soft_edges() {
            let size = [7, 5];
            let mut mask = vec![0; 7 * 5 * 4];
            mask[(2 * 7 + 3) * 4 + 3] = 255;
            mask[(2 * 7 + 4) * 4 + 3] = 128;
            let actual = distances(&mask, size).unwrap();
            for (index, distance) in actual.iter().enumerate() {
                let (x, y) = (index % 7, index / 7);
                let mut outer = 1e12_f32;
                let mut inner = 1e12_f32;
                for (other, pixel) in mask.as_chunks::<4>().0.iter().enumerate() {
                    let a = pixel[3] as f32 / 255.;
                    let d = (x as f32 - (other % 7) as f32).powi(2)
                        + (y as f32 - (other / 7) as f32).powi(2);
                    outer = outer.min(
                        d + if a == 0. {
                            1e12
                        } else {
                            (0.5 - a).max(0.).powi(2)
                        },
                    );
                    inner = inner.min(
                        d + if a == 1. {
                            1e12
                        } else {
                            (a - 0.5).max(0.).powi(2)
                        },
                    );
                }
                assert!((distance - (inner.sqrt() - outer.sqrt())).abs() < 1e-5);
            }
            assert!(actual[2 * 7 + 3] > 0.);
            assert!(coverage(actual[2 * 7 + 4], 0., 0.5, true) > 0.5);
            assert_eq!(coverage(actual[0], 1., 0., false), 0.);
            assert!(coverage(actual[0], 1., 6., true) > 0.);
            let effects = Effects::parse(&json!({"dropshadow":true})).unwrap();
            assert!(
                effects
                    .bounds([0, 0, 4096, 2048])
                    .unwrap_err()
                    .to_string()
                    .contains("workspace")
            );
            assert!(
                Effects::parse(&json!({}))
                    .unwrap()
                    .bounds([0, 0, 4096, 2048])
                    .is_ok()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hidden_text_validates_fonts_and_defers_the_system_font_scan() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("scene.json");
        std::fs::write(&path, b"{}").unwrap();
        let assets = Assets::open(root.path(), &path, None).unwrap();
        let mut system = TextSystem::new();
        let mut cache = TextCache::default();
        let mut object = json!({"text":"Hidden 中文", "pointsize":12});
        assert!(
            cache
                .prepare(&mut system, &assets, &object, [400., 200.], false)
                .unwrap()
                .is_none()
        );
        assert!(system.fonts.is_none());
        std::fs::write(root.path().join("bad.ttf"), b"invalid font").unwrap();
        object["font"] = json!("bad.ttf");
        assert!(
            cache
                .prepare(&mut system, &assets, &object, [400., 200.], false)
                .is_err()
        );
        object["font"] = json!("../font.ttf");
        assert!(
            cache
                .prepare(&mut system, &assets, &object, [400., 200.], false)
                .is_err()
        );
        assert!(system.fonts.is_none());
        object["font"] = json!("");
        assert!(
            cache
                .update(&mut system, &assets, &object, [400., 200.])
                .unwrap()
                .is_some()
        );
        assert!(system.fonts.is_some());
    }
    #[test]
    fn chinese_multiline_dynamic_paint_wrap_and_project_font_cache() {
        let temp = tempfile::tempdir().unwrap();
        let mut pkg = 8u32.to_le_bytes().to_vec();
        pkg.extend(b"PKGV0001");
        pkg.extend(0u32.to_le_bytes());
        std::fs::write(temp.path().join("scene.pkg"), pkg).unwrap();
        let assets = Assets::open(temp.path(), &temp.path().join("scene.pkg"), None).unwrap();
        let mut system = TextSystem::new();
        let mut cache = TextCache::default();
        let mut object = json!({"text":"中文 Hello\n第二行","pointsize":12,"horizontalalign":"left","verticalalign":"top","color":"1 0 0"});
        let first = cache
            .update(&mut system, &assets, &object, [400.0, 200.0])
            .unwrap()
            .unwrap();
        assert!(
            first
                .pixels
                .rgba
                .as_chunks::<4>()
                .0
                .iter()
                .any(|p| p[0] > 0 && p[1] == 0 && p[3] > 0)
        );
        assert!(
            cache
                .update(&mut system, &assets, &object, [400.0, 200.0])
                .unwrap()
                .is_none()
        );
        object["color"] = json!("0 1 0");
        let paint = cache
            .update(&mut system, &assets, &object, [400.0, 200.0])
            .unwrap()
            .unwrap();
        assert_eq!(cache.rebuilds, 1);
        assert!(
            paint
                .pixels
                .rgba
                .as_chunks::<4>()
                .0
                .iter()
                .any(|p| p[0] == 0 && p[1] > 0 && p[3] > 0)
        );
        object["text"] = json!("Hello Hello Hello Hello");
        object["limitwidth"] = json!(true);
        object["maxwidth"] = json!(160);
        let wrapped = cache
            .update(&mut system, &assets, &object, [400.0, 200.0])
            .unwrap()
            .unwrap();
        assert!(wrapped.pixels.height > first.pixels.height);
        assert!(wrapped.pixels.width <= 161);
        object["limitrows"] = json!(true);
        object["maxrows"] = json!(1);
        object["limituseellipsis"] = json!(true);
        let truncated = cache
            .update(&mut system, &assets, &object, [400.0, 200.0])
            .unwrap()
            .unwrap();
        assert!(truncated.pixels.height < wrapped.pixels.height);
        // Loading a project face invalidates fallback layouts once, then shares the face.
        let font_path = system
            .fonts
            .as_ref()
            .unwrap()
            .db()
            .faces()
            .find_map(|face| match &face.source {
                cosmic_text::fontdb::Source::File(path)
                | cosmic_text::fontdb::Source::SharedFile(path, _) => Some(path.clone()),
                _ => None,
            })
            .unwrap();
        std::fs::copy(font_path, temp.path().join("project.ttf")).unwrap();
        object["font"] = json!("project.ttf");
        cache
            .update(&mut system, &assets, &object, [400.0, 200.0])
            .unwrap();
        assert_eq!(system.project_fonts.len(), 1);
        let count = cache.rebuilds;
        assert!(
            cache
                .update(&mut system, &assets, &object, [400.0, 200.0])
                .unwrap()
                .is_none()
        );
        assert_eq!(cache.rebuilds, count);
        let bytes = system.font_bytes;
        assert_eq!(
            system.family(&assets, "missing.ttf").unwrap().family,
            "sans-serif"
        );
        assert_eq!(system.font_bytes, bytes);
        object["font"] = json!("../outside.ttf");
        assert!(
            cache
                .update(&mut system, &assets, &object, [400.0, 200.0])
                .is_err()
        );
    }
}

#[cfg(test)]
mod style_tests {
    use super::*;

    pub(super) fn assets() -> (tempfile::TempDir, Assets) {
        let root = tempfile::tempdir().unwrap();
        let mut pkg = 8u32.to_le_bytes().to_vec();
        pkg.extend(b"PKGV0001");
        pkg.extend(0u32.to_le_bytes());
        std::fs::write(root.path().join("scene.pkg"), pkg).unwrap();
        let assets = Assets::open(root.path(), &root.path().join("scene.pkg"), None).unwrap();
        (root, assets)
    }
    pub(super) fn render(
        cache: &mut TextCache,
        system: &mut TextSystem,
        assets: &Assets,
        object: &Value,
    ) -> Raster {
        cache
            .update(system, assets, object, [200., 100.])
            .unwrap()
            .unwrap()
    }
    pub(super) fn origin(raster: &Raster) -> [i32; 2] {
        [
            (raster.offset[0] - raster.pixels.width as f32 / 2.).round() as i32,
            (-raster.offset[1] - raster.pixels.height as f32 / 2.).round() as i32,
        ]
    }
    pub(super) fn pixel(raster: &Raster, x: i32, y: i32) -> [u8; 4] {
        let [left, top] = origin(raster);
        let (x, y) = (x - left, y - top);
        if x < 0 || y < 0 || x >= raster.pixels.width as i32 || y >= raster.pixels.height as i32 {
            return [0; 4];
        }
        let index = (y as usize * raster.pixels.width as usize + x as usize) * 4;
        raster.pixels.rgba[index..index + 4].try_into().unwrap()
    }
    pub(super) fn transparent_edges(raster: &Raster) {
        let [left, top] = origin(raster);
        let (right, bottom) = (
            left + raster.pixels.width as i32 - 1,
            top + raster.pixels.height as i32 - 1,
        );
        for x in left..=right {
            assert_eq!(pixel(raster, x, top)[3], 0);
            assert_eq!(pixel(raster, x, bottom)[3], 0);
        }
        for y in top..=bottom {
            assert_eq!(pixel(raster, left, y)[3], 0);
            assert_eq!(pixel(raster, right, y)[3], 0);
        }
    }

    #[test]
    fn official_outline_fields_expand_coverage_without_moving_multiline_alignment() {
        let (_root, assets) = assets();
        let mut system = TextSystem::new();
        for horizontal in ["left", "center", "right"] {
            for vertical in ["top", "center", "bottom"] {
                let mut cache = TextCache::default();
                let mut object = json!({"text":"中文 Ag\n第二行","font":"sans-serif","pointsize":10,"horizontalalign":horizontal,"verticalalign":vertical,"color":"1 0 0","padding":0});
                let base = render(&mut cache, &mut system, &assets, &object);
                object["outline"] = json!(true);
                object["outlinethickness"] = json!(3);
                object["outlinecolor"] = json!("0 1 0");
                let outlined = render(&mut cache, &mut system, &assets, &object);
                assert_eq!(cache.rebuilds, 1, "outline must reuse glyph layout");
                assert_eq!(outlined.pixels.width, base.pixels.width + 8);
                assert_eq!(outlined.pixels.height, base.pixels.height + 8);
                assert_eq!(
                    outlined.offset, base.offset,
                    "symmetric expansion moved the anchor"
                );
                let [left, top] = origin(&outlined);
                let mut added = 0;
                let mut body = 0;
                for y in top..top + outlined.pixels.height as i32 {
                    for x in left..left + outlined.pixels.width as i32 {
                        let old = pixel(&base, x, y);
                        let new = pixel(&outlined, x, y);
                        if old[3] == 255 {
                            assert_eq!(new, old);
                            body += 1;
                        }
                        if old[3] == 0 && new[3] > 0 {
                            assert_eq!(&new[..3], &[0, 255, 0]);
                            added += 1;
                        }
                    }
                }
                assert!(
                    body > 100 && added > 100,
                    "outline did not expand actual glyph pixels"
                );
                transparent_edges(&outlined);
                assert!(
                    cache
                        .update(&mut system, &assets, &object, [200., 100.])
                        .unwrap()
                        .is_none()
                );
                object["outlinecolor"] = json!("0 0 1");
                let blue = render(&mut cache, &mut system, &assets, &object);
                assert_eq!(cache.rebuilds, 1);
                assert!(
                    blue.pixels
                        .rgba
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .any(|p| p[0] == 0 && p[2] == 255 && p[3] > 0)
                );
                object["outline"] = json!(false);
                let plain = render(&mut cache, &mut system, &assets, &object);
                assert_eq!(plain.pixels.rgba, base.pixels.rgba);
                assert_eq!(plain.offset, base.offset);
                object["text"] = json!("动态内容\n变化");
                assert_ne!(
                    render(&mut cache, &mut system, &assets, &object)
                        .pixels
                        .rgba,
                    base.pixels.rgba
                );
                assert_eq!(cache.rebuilds, 2);
            }
        }
    }

    #[test]
    fn official_defaults_bindings_soft_style_and_bitmap_limits() {
        let (_root, assets) = assets();
        let mut system = TextSystem::new();
        let mut cache = TextCache::default();
        let mut object = json!({"text":"A中文","pointsize":10,"color":"1 0 0"});
        let base = render(&mut cache, &mut system, &assets, &object);
        object["outline"] = json!(true);
        let default_outline = render(&mut cache, &mut system, &assets, &object);
        assert!(
            default_outline
                .pixels
                .rgba
                .as_chunks::<4>()
                .0
                .iter()
                .any(|p| p == &[0, 0, 0, 255])
        );
        object["outlinethickness"] = json!(4);
        object["outlinecolor"] = json!("0 0 0");
        assert!(
            cache
                .update(&mut system, &assets, &object, [200., 100.])
                .unwrap()
                .is_none(),
            "explicit outline defaults should reuse the raster"
        );
        object["outline"] = json!(false);
        object["blur"] = json!(true);
        let soft = render(&mut cache, &mut system, &assets, &object);
        assert_eq!(cache.rebuilds, 1);
        let [left, top] = origin(&soft);
        let outside = (top..top + soft.pixels.height as i32)
            .flat_map(|y| (left..left + soft.pixels.width as i32).map(move |x| (x, y)))
            .filter(|&(x, y)| pixel(&base, x, y)[3] == 0 && pixel(&soft, x, y)[3] > 0)
            .count();
        assert!(
            outside > 100,
            "blursize default did not soften/expand the contour"
        );
        transparent_edges(&soft);
        object["blursize"] = json!(6);
        assert!(
            cache
                .update(&mut system, &assets, &object, [200., 100.])
                .unwrap()
                .is_none(),
            "explicit default should reuse the raster"
        );
        object["blur"] = json!(false);
        object["outline"] = json!({"value":false,"user":"border"});
        object["outlinethickness"] = json!({"value":4,"user":"width"});
        let resolved = crate::scene::bindings::resolve(
            &object,
            &crate::scene::bindings::Properties::from([
                ("border".into(), json!(true)),
                ("width".into(), json!(1.5)),
            ]),
        )
        .unwrap();
        assert_ne!(
            render(&mut cache, &mut system, &assets, &resolved)
                .pixels
                .rgba,
            base.pixels.rgba
        );
        assert_eq!(cache.rebuilds, 1);
        object["outline"] = json!(true);
        object["outlinethickness"] = json!(1e20);
        assert!(
            cache
                .update(&mut system, &assets, &object, [200., 100.])
                .is_err()
        );
        object["outline"] = json!(false);
        object["padding"] = json!(1e20);
        assert!(
            cache
                .update(&mut system, &assets, &object, [200., 100.])
                .is_err()
        );
    }

    #[test]
    fn project_font_style_switch_preserves_face_weight_and_italic_and_fallback() {
        let (root, assets) = assets();
        let mut system = TextSystem::new();
        // Installed family variants make the old family-only selection observably choose Regular.
        let faces = system.font_system().db().faces().collect::<Vec<_>>();
        let regular = faces
            .iter()
            .find(|f| {
                f.weight == Weight::NORMAL
                    && f.style == Style::Normal
                    && faces.iter().any(|b| {
                        b.families == f.families
                            && b.weight == Weight::BOLD
                            && b.style == Style::Normal
                    })
                    && faces
                        .iter()
                        .any(|i| i.families == f.families && i.style == Style::Italic)
            })
            .unwrap();
        let bold = faces
            .iter()
            .find(|f| {
                f.families == regular.families
                    && f.weight == Weight::BOLD
                    && f.style == Style::Normal
            })
            .unwrap();
        let italic = faces
            .iter()
            .find(|f| f.families == regular.families && f.style == Style::Italic)
            .unwrap();
        for (name, face) in [
            ("regular.ttf", regular),
            ("bold.ttf", bold),
            ("italic.ttf", italic),
        ] {
            let path = match &face.source {
                cosmic_text::fontdb::Source::File(p)
                | cosmic_text::fontdb::Source::SharedFile(p, _) => p,
                _ => panic!("test font needs a file"),
            };
            std::fs::copy(path, root.path().join(name)).unwrap();
        }
        let mut cache = TextCache::default();
        let mut object = json!({"text":"Hello 中文\nAg","font":"regular.ttf","pointsize":10});
        let plain = render(&mut cache, &mut system, &assets, &object);
        object["font"] = json!("bold.ttf");
        let bold = render(&mut cache, &mut system, &assets, &object);
        assert_eq!(system.project_fonts["bold.ttf"].weight, Weight::BOLD);
        assert_ne!(
            plain.pixels.rgba, bold.pixels.rgba,
            "font style change left glyph pixels unchanged"
        );
        let ink = |raster: &Raster| {
            raster
                .pixels
                .rgba
                .as_chunks::<4>()
                .0
                .iter()
                .map(|p| p[3] as u64)
                .sum::<u64>()
        };
        assert!(
            ink(&bold) > ink(&plain),
            "Bold must increase actual glyph coverage"
        );
        assert!(
            cache
                .glyphs
                .iter()
                .any(|(key, _, _)| key.font_weight == Weight::BOLD)
        );
        object["font"] = json!("italic.ttf");
        let italic = render(&mut cache, &mut system, &assets, &object);
        assert_eq!(system.project_fonts["italic.ttf"].style, Style::Italic);
        assert_ne!(plain.pixels.rgba, italic.pixels.rgba);
        assert!(
            cache.glyphs.iter().all(|(key, _, _)| system
                .fonts
                .as_ref()
                .unwrap()
                .db()
                .face(key.font_id)
                .is_some_and(|face| face.style != Style::Normal)
                || key.flags.contains(cosmic_text::CacheKeyFlags::FAKE_ITALIC)),
            "italic must also survive fallback shaping"
        );
        assert!(
            cache
                .update(&mut system, &assets, &object, [200., 100.])
                .unwrap()
                .is_none()
        );
        assert_eq!(cache.rebuilds, 3);
        assert_eq!(system.project_fonts.len(), 3);
        for index in 0..253 {
            system
                .family(&assets, &format!("missing-{index}.ttf"))
                .unwrap();
        }
        assert!(system.family(&assets, "another-missing.ttf").is_err());
        assert_eq!(
            system.family(&assets, "bold.ttf").unwrap().weight,
            Weight::BOLD
        );
    }
}

#[cfg(test)]
mod shadow_tests {
    use super::style_tests::{assets, origin, pixel, render, transparent_edges};
    use super::*;

    fn colored(raster: &Raster, channel: usize) -> usize {
        raster
            .pixels
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| {
                p[channel] == 255
                    && p[3] > 0
                    && p[(channel + 1) % 3] == 0
                    && p[(channel + 2) % 3] == 0
            })
            .count()
    }

    #[test]
    fn drop_shadow_signed_offset_color_opacity_and_fractional_sampling_pixels() {
        let (_root, assets) = assets();
        let mut system = TextSystem::new();
        let mut cache = TextCache::default();
        let mut object = json!({"text":"A","pointsize":10,"horizontalalign":"left","verticalalign":"top","color":"1 0 0"});
        let plain = render(&mut cache, &mut system, &assets, &object);
        object["dropshadow"] = json!(true);
        object["dropshadowoffset"] = json!("80 30");
        object["dropshadowcolor"] = json!("0 0 1");
        object["dropshadowsize"] = json!(0.01);
        object["dropshadowopacity"] = json!(0.4);
        let shadow = render(&mut cache, &mut system, &assets, &object);
        let [left, top] = origin(&plain);
        let mut checked = 0;
        for y in top..top + plain.pixels.height as i32 {
            for x in left..left + plain.pixels.width as i32 {
                if pixel(&plain, x, y)[3] == 255 {
                    assert_eq!(pixel(&shadow, x, y), [255, 0, 0, 255]);
                    assert_eq!(pixel(&shadow, x + 80, y + 30), [0, 0, 255, 102]);
                    checked += 1;
                }
            }
        }
        assert!(
            checked > 100,
            "expected opaque body and correctly translated 40% blue shadow"
        );
        assert_eq!(cache.rebuilds, 1);
        transparent_edges(&shadow);
        assert!(
            cache
                .update(&mut system, &assets, &object, [200., 100.])
                .unwrap()
                .is_none()
        );
        object["dropshadowoffset"] = json!([-60, -30]);
        let negative = render(&mut cache, &mut system, &assets, &object);
        assert!(colored(&negative, 2) > 100);
        for y in top..top + plain.pixels.height as i32 {
            for x in left..left + plain.pixels.width as i32 {
                if pixel(&plain, x, y)[3] == 255 {
                    assert_eq!(pixel(&negative, x - 60, y - 30), [0, 0, 255, 102]);
                }
            }
        }
        transparent_edges(&negative);
        object["dropshadowoffset"] = json!([80.5, 30.5]);
        let fractional = render(&mut cache, &mut system, &assets, &object);
        let changed = (top..top + plain.pixels.height as i32)
            .flat_map(|y| (left..left + plain.pixels.width as i32).map(move |x| (x, y)))
            .filter(|&(x, y)| pixel(&shadow, x + 80, y + 30) != pixel(&fractional, x + 80, y + 30))
            .count();
        assert!(changed > 20, "fractional offset was rounded away");
        assert_eq!(cache.rebuilds, 1);
    }

    #[test]
    fn shadow_blur_includes_outline_and_preserves_transparent_and_opaque_backgrounds() {
        let (_root, assets) = assets();
        let mut system = TextSystem::new();
        let mut cache = TextCache::default();
        let mut object = json!({"text":"中文 Ag\n第二行","pointsize":10,"color":"1 0 0","dropshadow":true,"dropshadowoffset":"220 0","dropshadowcolor":"0 0 1","dropshadowsize":0.01,"dropshadowopacity":0.5});
        let hard = render(&mut cache, &mut system, &assets, &object);
        object["dropshadowsize"] = json!(6);
        let soft = render(&mut cache, &mut system, &assets, &object);
        assert!(
            colored(&soft, 2) > colored(&hard, 2) + 100,
            "distance blur did not expand shadow coverage"
        );
        assert!(
            soft.pixels
                .rgba
                .as_chunks::<4>()
                .0
                .iter()
                .any(|p| p[2] == 255 && p[3] > 0 && p[3] < 64)
        );
        transparent_edges(&soft);
        object["outline"] = json!(true);
        object["outlinethickness"] = json!(3);
        object["outlinecolor"] = json!("0 1 0");
        let outlined = render(&mut cache, &mut system, &assets, &object);
        assert!(
            colored(&outlined, 2) > colored(&soft, 2) + 100,
            "shadow omitted outline silhouette"
        );
        assert!(colored(&outlined, 1) > 100);
        transparent_edges(&outlined);
        assert_eq!(cache.rebuilds, 1);
        // Opaque background belongs behind the text/shadow, never in their silhouette.
        object["opaquebackground"] = json!(true);
        object["backgroundcolor"] = json!("1 1 1");
        let opaque = render(&mut cache, &mut system, &assets, &object);
        assert!(
            opaque
                .pixels
                .rgba
                .as_chunks::<4>()
                .0
                .iter()
                .all(|p| p[3] == 255)
        );
        assert!(
            opaque
                .pixels
                .rgba
                .as_chunks::<4>()
                .0
                .iter()
                .any(|p| p[0] > 100 && p[0] < 255 && p[0] == p[1] && p[2] == 255)
        );
        assert_eq!(cache.rebuilds, 2);
        object["opaquebackground"] = json!(false);
        object["text"] = json!("");
        let empty = render(&mut cache, &mut system, &assets, &object);
        assert!(
            empty
                .pixels
                .rgba
                .as_chunks::<4>()
                .0
                .iter()
                .all(|p| p[3] == 0),
            "empty text cast a rectangular shadow"
        );
    }

    #[test]
    fn official_shadow_defaults_dynamic_bindings_disabled_restore_and_cache_limits() {
        let (_root, assets) = assets();
        let mut system = TextSystem::new();
        let mut cache = TextCache::default();
        let mut object = json!({"text":"A中文","pointsize":10,"color":"1 0 0"});
        let plain = render(&mut cache, &mut system, &assets, &object);
        object["dropshadow"] = json!(true);
        let default_shadow = render(&mut cache, &mut system, &assets, &object);
        assert_eq!(default_shadow.pixels.width, plain.pixels.width + 12);
        assert_eq!(default_shadow.pixels.height, plain.pixels.height + 12);
        assert_eq!(
            default_shadow.offset,
            [plain.offset[0] + 4., plain.offset[1] - 4.]
        );
        assert!(
            default_shadow
                .pixels
                .rgba
                .as_chunks::<4>()
                .0
                .iter()
                .any(|p| p[0] == 0 && p[1] == 0 && p[2] == 0 && p[3] > 0)
        );
        object["dropshadowoffset"] = json!("4 4");
        object["dropshadowcolor"] = json!("0 0 0");
        object["dropshadowopacity"] = json!(1);
        object["dropshadowsize"] = json!(6);
        assert!(
            cache
                .update(&mut system, &assets, &object, [200., 100.])
                .unwrap()
                .is_none(),
            "explicit defaults should reuse the raster"
        );
        let glyphs = system.glyph_count;
        for opacity in [0., 0.2, 0.6, 1.] {
            object["dropshadowopacity"] = json!({"value":1,"user":"opacity"});
            let resolved = crate::scene::bindings::resolve(
                &object,
                &crate::scene::bindings::Properties::from([("opacity".into(), json!(opacity))]),
            )
            .unwrap();
            render(&mut cache, &mut system, &assets, &resolved);
            assert_eq!(cache.rebuilds, 1);
            assert_eq!(system.glyph_count, glyphs);
        }
        object["alpha"] = json!(0.5);
        render(&mut cache, &mut system, &assets, &object);
        assert_eq!(cache.rebuilds, 1, "layer opacity must not reshape text");
        assert!(
            cache
                .update(&mut system, &assets, &object, [200., 100.])
                .unwrap()
                .is_none()
        );
        object["alpha"] = json!(1);
        object["dropshadow"] = json!(false);
        let restored = render(&mut cache, &mut system, &assets, &object);
        assert_eq!(restored.pixels.rgba, plain.pixels.rgba);
        assert_eq!(restored.offset, plain.offset);
        object["text"] = json!("动态内容\n两行");
        render(&mut cache, &mut system, &assets, &object);
        assert_eq!(cache.rebuilds, 2);
        object["dropshadow"] = json!(true);
        object["dropshadowopacity"] = json!(1);
        object["dropshadowoffset"] = json!("1e20 -1e20");
        assert!(
            cache
                .update(&mut system, &assets, &object, [200., 100.])
                .is_err()
        );
        object["dropshadowoffset"] = json!("4 4");
        object["dropshadowsize"] = json!(1e20);
        assert!(
            cache
                .update(&mut system, &assets, &object, [200., 100.])
                .is_err()
        );
    }

    #[test]
    fn shadow_is_below_outline_and_body_using_straight_alpha() {
        let effects=raster::Effects::parse(&json!({"outline":true,"outlinethickness":1,"outlinecolor":"0 1 0","dropshadow":true,"dropshadowoffset":"0 0","dropshadowcolor":"0 0 1","dropshadowopacity":0.5,"dropshadowsize":2})).unwrap();
        let mut rgba = vec![0; 7 * 7 * 4];
        rgba[(3 * 7 + 3) * 4..(3 * 7 + 3) * 4 + 4].copy_from_slice(&[255, 0, 0, 255]);
        effects.paint(&mut rgba, [7, 7], &[255, 0, 0]).unwrap();
        assert_eq!(
            &rgba[(3 * 7 + 3) * 4..(3 * 7 + 3) * 4 + 4],
            &[255, 0, 0, 255]
        );
        let edge = &rgba[(3 * 7 + 2) * 4..(3 * 7 + 2) * 4 + 4];
        // 50% green outline over 25% blue shadow: alpha=0.625; straight RGB=(0,.8,.2).
        assert_eq!(edge[0], 0);
        assert!(
            edge[1].abs_diff(204) <= 1 && edge[2].abs_diff(51) <= 1 && edge[3].abs_diff(159) <= 1,
            "unexpected blend {edge:?}"
        );
        let effects=raster::Effects::parse(&json!({"alpha":0.5,"dropshadow":true,"dropshadowoffset":"0 0","dropshadowcolor":"0 0 1","dropshadowopacity":1,"dropshadowsize":0.01})).unwrap();
        rgba.fill(0);
        rgba[(3 * 7 + 3) * 4..(3 * 7 + 3) * 4 + 4].copy_from_slice(&[255, 0, 0, 255]);
        effects.paint(&mut rgba, [7, 7], &[255, 0, 0]).unwrap();
        let body = &rgba[(3 * 7 + 3) * 4..(3 * 7 + 3) * 4 + 4];
        // 50% red over 50% blue: alpha=.75; straight RGB=(2/3,0,1/3).
        assert!(
            body[0].abs_diff(170) <= 1
                && body[1] == 0
                && body[2].abs_diff(85) <= 1
                && body[3].abs_diff(191) <= 1,
            "layer alpha was applied after composition: {body:?}"
        );
    }
}
