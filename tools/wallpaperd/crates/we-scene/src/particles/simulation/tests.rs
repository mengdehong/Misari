use super::*;
use serde_json::json;

#[test]
fn tick_motion_preserves_float_bits_with_world_gravity_and_angular_wraps() {
    let audio = AudioSnapshot::default();
    for world in [false, true] {
        for speed in [0., 0.1, 1., 3.] {
            for dt in [0., STEP as f32, 0.1, 2.] {
                let definition = json!({"maxcount":3,
                "emitter":[{"name":"boxrandom","instantaneous":3,"rate":0}],
                "initializer":[{"name":"lifetimerandom","min":4000,"max":4000}],
                "operator":[
                    {"name":"movement","gravity":"1.25 -9.81 0.3","drag":0.2,"worldspace":world},
                    {"name":"angularmovement","force":"0.12 -0.3 0.7","drag":0.1}
                ]});
                let mut system = System::new(&definition, &json!({"speed":speed}), 7).unwrap();
                system.space_inverse = Mat4::from_scale_rotation_translation(
                    Vec3::new(1.5, 0.7, 2.),
                    glam::Quat::from_rotation_z(0.3),
                    Vec3::new(2., -3., 1.),
                );
                system.step_tick(0., &audio);
                let rotations = [
                    Vec3::new(-0., 0., f32::from_bits(TAU.to_bits() - 1)),
                    Vec3::new(TAU, -TAU, 1e30),
                    Vec3::new(-1000., 1000., -f32::MIN_POSITIVE),
                ];
                for (particle, rotation) in system.particles.iter_mut().zip(rotations) {
                    particle.rotation = rotation;
                    particle.angular = Vec3::new(0.01, -0.02, -0.);
                    particle.velocity = Vec3::new(2.3, -0.7, 0.1);
                }
                let mut expected = system.particles.clone();
                for _ in 0..60 {
                    // The pre-optimization per-particle formula is the oracle.
                    for p in &mut expected {
                        for op in &system.operators {
                            match op {
                                Op::Movement {
                                    gravity,
                                    drag,
                                    world,
                                    ..
                                } => {
                                    p.position += p.velocity * dt;
                                    let gravity = if *world {
                                        system.space_inverse.transform_vector3(*gravity)
                                    } else {
                                        *gravity
                                    };
                                    p.velocity += gravity * dt * speed;
                                    p.velocity *= (1. - drag * dt).max(0.);
                                }
                                Op::Angular { force, drag, .. } => {
                                    p.rotation += p.angular * dt * speed;
                                    p.angular += *force * dt * speed;
                                    p.angular *= (1. - drag * dt).max(0.);
                                    p.rotation = p.rotation.rem_euclid(Vec3::splat(TAU));
                                }
                                _ => unreachable!(),
                            }
                        }
                    }
                    system.step_tick(dt, &audio);
                    assert_eq!(system.particles.len(), expected.len());
                    for (actual, expected) in system.particles.iter().zip(&expected) {
                        for (actual, expected) in [
                            (actual.position, expected.position),
                            (actual.velocity, expected.velocity),
                            (actual.rotation, expected.rotation),
                            (actual.angular, expected.angular),
                        ] {
                            assert_eq!(
                                actual.to_array().map(f32::to_bits),
                                expected.to_array().map(f32::to_bits),
                                "world={world}, speed={speed}, dt={dt}"
                            );
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn repeated_size_initializers_multiply_with_one_instance_scale_and_survive_ticks() {
    // Official WE: sizes 72 then 0.8 produce 57.6; no size initializer starts at 1.
    for (sizes, expected) in [
        (&[72., 0.8][..], 57.6),
        (&[0.8, 72.][..], 57.6),
        (&[72., 0.8, 2.][..], 115.2),
        (&[72.][..], 72.),
        (&[][..], 1.),
        (&[72., 0.][..], 0.),
    ] {
        for scale in [1., 2.] {
            let mut initializers = vec![json!({"name":"lifetimerandom","min":2,"max":2})];
            initializers.extend(
                sizes
                    .iter()
                    .map(|size| json!({"name":"sizerandom","min":size,"max":size})),
            );
            let definition = json!({"maxcount":1,
                "emitter":[{"name":"boxrandom","instantaneous":1,"rate":0}],
                "initializer":initializers});
            let mut system = System::new(&definition, &json!({"size":scale}), 7).unwrap();
            for time in [0., 0.25, 0.] {
                system.advance(time, Vec3::ZERO, Mat4::IDENTITY, &AudioSnapshot::default());
                let actual = system.particles[0].size;
                assert!(
                    (actual - expected * scale).abs() < 0.0001,
                    "sizes={sizes:?}, scale={scale}, time={time}: {actual}"
                );
            }
        }
    }
}

#[test]
fn snow_random_color_interpolates_whole_rgb_endpoints_with_one_weight() {
    let definition = json!({"maxcount":256,"emitter":[{"name":"boxrandom","instantaneous":256,"rate":0}],"initializer":[{"name":"lifetimerandom","min":20,"max":20},{"name":"colorrandom","min":"255 255 255","max":"95 98 100"},{"name":"alpharandom","min":0.5,"max":0.5}]});
    let mut system = System::new(&definition, &Value::Null, 7).unwrap();
    system.step_tick(0., &AudioSnapshot::default());
    let white = Vec3::splat(255.);
    let blue_gray = Vec3::new(95., 98., 100.);
    for particle in &system.particles {
        let rgb = particle.color.truncate() * 255.;
        let weight = (rgb.x - white.x) / (blue_gray.x - white.x);
        assert!(
            rgb.abs_diff_eq(white.lerp(blue_gray, weight), 0.0001),
            "snow must stay on the authored RGB gradient: {rgb:?}"
        );
        assert_eq!(particle.color.w, 0.5);
    }
}

#[test]
fn snow_emitter_omitted_rate_is_continuous_and_position_oscillation_has_amplitude() {
    let audio = AudioSnapshot::default();
    let snow = json!({"maxcount":360,"emitter":[{"name":"boxrandom"}],"initializer":[{"name":"lifetimerandom","min":20,"max":20}]});
    let mut system = System::new(&snow, &Value::Null, 7).unwrap();
    system.advance(0., Vec3::ZERO, Mat4::IDENTITY, &audio);
    assert!(
        system.particles.is_empty(),
        "omitting rate must not fill capacity immediately"
    );
    system.advance(1., Vec3::ZERO, Mat4::IDENTITY, &audio);
    assert_eq!(
        system.particles.len(),
        5,
        "default emission rate is five per second"
    );

    let oscillating = json!({"maxcount":1,"emitter":[{"name":"boxrandom","instantaneous":1,"rate":0}],"initializer":[{"name":"lifetimerandom","min":20,"max":20}],"operator":[{"name":"oscillateposition","frequencymin":1,"frequencymax":1,"scalemin":20,"scalemax":20,"phasemin":0,"phasemax":0,"mask":"1 0 0"}]});
    let mut system = System::new(&oscillating, &Value::Null, 7).unwrap();
    system.advance(0., Vec3::ZERO, Mat4::IDENTITY, &audio);
    assert_eq!(
        system.particles[0].position,
        Vec3::ZERO,
        "oscillation must not offset birth position"
    );
    system.advance(0.5, Vec3::ZERO, Mat4::IDENTITY, &audio);
    assert!(
        (system.particles[0].position.x - 20. * (0.5_f32.cos() - 1.)).abs() < 0.01,
        "a fixed scale is an oscillation amplitude, not a constant offset"
    );
}

#[test]
fn cached_position_phases_match_original_math_with_multiple_ranges() {
    let definition = json!({"maxcount":64,"emitter":[{"name":"boxrandom","instantaneous":64,"rate":0}],"initializer":[{"name":"lifetimerandom","min":20,"max":20}],"operator":[
        {"name":"oscillateposition","frequencymin":0.8,"frequencymax":1,"scalemin":20,"scalemax":35,"phasemin":0,"phasemax":1,"mask":"1 1 1"},
        {"name":"oscillateposition","frequencymin":-10,"frequencymax":20,"scalemin":-3,"scalemax":4,"phasemin":0,"phasemax":1,"mask":"1 1 1"},
        {"name":"oscillateposition","frequencymin":1,"frequencymax":2,"scalemin":2,"scalemax":5,"phasemin":-2,"phasemax":3,"mask":"1 1 1"}
    ]});
    let mut system = System::new(&definition, &Value::Null, 7).unwrap();
    system.step_tick(0., &AudioSnapshot::default());
    for (index, op) in system.operators.iter().enumerate() {
        let Op::OscPosition(osc) = op else {
            unreachable!()
        };
        assert_eq!(osc.cached_phase, index < 2);
        for particle in &system.particles {
            let mut particle = particle.clone();
            for age in [0., 0.01, 0.5, 3., 100., 3600.] {
                particle.age = age;
                for axis in 0..3 {
                    let frequency = lerp(osc.frequency[0], osc.frequency[1], particle.random[axis]);
                    let phase = lerp(osc.phase[0], osc.phase[1], particle.random[(axis + 1) % 3]);
                    let amplitude =
                        lerp(osc.scale[0], osc.scale[1], particle.random[(axis + 2) % 3]);
                    let expected = amplitude * ((age * frequency + phase).cos() - phase.cos());
                    assert_eq!(osc.position(&particle, axis).to_bits(), expected.to_bits());
                }
            }
        }
    }
}

#[test]
fn masked_position_oscillation_keeps_active_axes_and_overflow_cleanup() {
    let definition = json!({"maxcount":32,"emitter":[{"name":"boxrandom","instantaneous":32,"rate":0}],"initializer":[{"name":"lifetimerandom","min":20,"max":20}],"operator":[{"name":"oscillateposition","frequencymin":0.8,"frequencymax":1,"scalemin":20,"scalemax":35,"phasemin":0,"phasemax":1,"mask":"1 0.5 0"}]});
    let audio = AudioSnapshot::default();
    let mut masked = System::new(&definition, &Value::Null, 7).unwrap();
    let mut all_axes = definition.clone();
    all_axes["operator"][0]["mask"] = json!("1 0.5 1");
    let mut all_axes = System::new(&all_axes, &Value::Null, 7).unwrap();
    for frame in 0..=90 {
        let time = frame as f64 / 30.;
        masked.advance(time, Vec3::ZERO, Mat4::IDENTITY, &audio);
        all_axes.advance(time, Vec3::ZERO, Mat4::IDENTITY, &audio);
        assert_eq!(masked.particles.len(), all_axes.particles.len());
        for (masked, unmasked) in masked.particles.iter().zip(&all_axes.particles) {
            assert_eq!(masked.position.truncate(), unmasked.position.truncate());
            assert_eq!(masked.position.z, 0.);
            assert_eq!(masked.age, unmasked.age);
        }
    }

    // Finite authored frequencies can still overflow age * frequency. Zero
    // mask must not hide NaN and keep a formerly invalid particle alive.
    let mut overflow = definition;
    overflow["operator"][0]["frequencymin"] = json!(f32::MAX);
    overflow["operator"][0]["frequencymax"] = json!(f32::MAX);
    overflow["operator"][0]["mask"] = json!("0 0 0");
    let mut overflow = System::new(&overflow, &Value::Null, 7).unwrap();
    overflow.advance(0., Vec3::ZERO, Mat4::IDENTITY, &audio);
    assert_eq!(overflow.particles.len(), 32);
    overflow.advance(2., Vec3::ZERO, Mat4::IDENTITY, &audio);
    assert!(overflow.particles.is_empty());
}

#[test]
fn sphere_emitters_preserve_planar_radius_depth_sign_and_radial_speed() {
    let audio = AudioSnapshot::default();
    let base = json!({"maxcount":2048,"emitter":[{"name":"sphererandom","instantaneous":2048,"rate":0,"distancemin":64,"distancemax":64,"speedmin":32,"speedmax":32}],"initializer":[{"name":"lifetimerandom","min":2,"max":2}]});
    let mut system = System::new(&base, &Value::Null, 7).unwrap();
    system.step_tick(0., &audio);
    assert!(system.particles.iter().all(|p| p.position.z == 0.
        && p.velocity.z == 0.
        && (p.position.length() - 64.).abs() < 0.0001
        && (p.velocity.length() - 32.).abs() < 0.0001));
    let original = system.particles.clone();
    system.advance(0.5, Vec3::ZERO, Mat4::IDENTITY, &audio);
    system.advance(0., Vec3::ZERO, Mat4::IDENTITY, &audio);
    assert_eq!(system.particles, original);
    let mut signed = base.clone();
    signed["emitter"][0]["directions"] = json!("-1 2 0");
    signed["emitter"][0]["sign"] = json!("1 -1 0");
    let mut system = System::new(&signed, &Value::Null, 7).unwrap();
    system.step_tick(0., &audio);
    assert!(system.particles.iter().all(|p| p.position.x >= 0.
        && p.position.y <= 0.
        && p.velocity.x >= 0.
        && p.velocity.y <= 0.
        && p.velocity.cross(p.position).length() < 0.001));
    let mut depth = base.clone();
    depth["emitter"][0]["directions"] = json!("1 1 1");
    let mut system = System::new(&depth, &Value::Null, 7).unwrap();
    system.step_tick(0., &audio);
    assert!(
        system
            .particles
            .iter()
            .all(|p| (p.position.length() - 64.).abs() < 0.0001)
    );
    assert!(
        system.particles.iter().any(|p| p.position.z > 32.)
            && system.particles.iter().any(|p| p.position.z < -32.)
    );
    let mut disk = base.clone();
    disk["emitter"][0]["distancemin"] = json!(0);
    let mut system = System::new(&disk, &Value::Null, 7).unwrap();
    system.step_tick(0., &audio);
    let mean_area = system
        .particles
        .iter()
        .map(|p| p.position.length_squared() / (64. * 64.))
        .sum::<f32>()
        / system.particles.len() as f32;
    assert!(
        (mean_area - 0.5).abs() < 0.03,
        "disk samples must be uniform in area: {mean_area}"
    );
    let mut box_emitter = base;
    box_emitter["emitter"][0]["name"] = json!("boxrandom");
    let mut system = System::new(&box_emitter, &Value::Null, 7).unwrap();
    system.step_tick(0., &audio);
    assert!(system.particles.iter().all(|p| p.position.z == 0.
        && (p.velocity.length() - 32.).abs() < 0.0001
        && p.velocity.dot(p.position) > 0.));
}

#[test]
fn one_per_frame_cap_survives_fixed_steps_and_same_timestamp_catch_up() {
    let base = json!({"maxcount":1000,"emitter":[{"name":"boxrandom","flags":2,"rate":1000}],"initializer":[{"name":"lifetimerandom","min":20,"max":20}]});
    let audio = AudioSnapshot::default();
    for fps in [30, 60, 120] {
        let mut system = System::new(&base, &Value::Null, 7).unwrap();
        system.advance(0., Vec3::ZERO, Mat4::IDENTITY, &audio);
        for frame in 1..=fps {
            let time = frame as f64 / fps as f64;
            system.advance(time, Vec3::ZERO, Mat4::IDENTITY, &audio);
            system.advance(time, Vec3::ZERO, Mat4::IDENTITY, &audio);
        }
        assert_eq!(system.particles.len(), fps);
    }
    let mut burst = base.clone();
    burst["emitter"][0]["instantaneous"] = json!(100);
    let mut system = System::new(&burst, &Value::Null, 7).unwrap();
    system.advance(0., Vec3::ZERO, Mat4::IDENTITY, &audio);
    assert_eq!(system.particles.len(), 1);
    while !system.advance(10., Vec3::ZERO, Mat4::IDENTITY, &audio) {}
    assert_eq!(
        system.particles.len(),
        2,
        "same-time catch-up must not grant a new frame quota"
    );
    system.advance(0., Vec3::ZERO, Mat4::IDENTITY, &audio);
    assert_eq!(
        system.particles.len(),
        1,
        "rewinding must reset the frame quota"
    );
    system.forced = 3;
    system.step_tick(0., &audio);
    assert_eq!(
        system.particles.len(),
        4,
        "explicit SceneScript emissions retain their requested count"
    );
}

#[test]
fn size_change_without_an_end_value_shrinks_particle_trails_to_zero() {
    let audio = AudioSnapshot::default();
    let definition = json!({"maxcount":1,"emitter":[{"name":"boxrandom","instantaneous":1,"rate":0}],"initializer":[{"name":"lifetimerandom","min":1,"max":1},{"name":"sizerandom","min":4,"max":4}],"operator":[{"name":"sizechange"}]});
    let mut system = System::new(&definition, &Value::Null, 7).unwrap();
    for (time, size) in [(0., 4.), (0.5, 2.), (0.75, 1.)] {
        system.advance(time, Vec3::ZERO, Mat4::IDENTITY, &audio);
        assert!((system.particles[0].size - size).abs() < 0.00001);
    }
}

#[test]
fn alpha_fade_omitted_boundaries_use_the_native_half_lifetime_defaults() {
    let audio = AudioSnapshot::default();
    for (operator, birth, halfway, late) in [
        (json!({"name":"alphafade"}), 0., 1., 0.5),
        (json!({"name":"alphafade","fadeintime":0.1}), 0., 1., 0.5),
    ] {
        let definition = json!({"maxcount":1,"emitter":[{"name":"boxrandom","instantaneous":1,"rate":0}],"initializer":[{"name":"lifetimerandom","min":1,"max":1}],"operator":[operator]});
        let mut system = System::new(&definition, &Value::Null, 7).unwrap();
        for (time, alpha) in [(0., birth), (0.5, halfway), (0.75, late)] {
            system.advance(time, Vec3::ZERO, Mat4::IDENTITY, &audio);
            assert!((system.particles[0].color.w - alpha).abs() < 0.00001);
        }
    }
}

#[test]
fn instance_count_scales_continuous_emission_without_changing_capacity() {
    let mut definition = json!({"maxcount":100,"emitter":[{"name":"boxrandom","rate":20}],"initializer":[{"name":"lifetimerandom","min":10,"max":10}]});
    let audio = AudioSnapshot::default();
    for (flags, count, births) in [(0, 0.25, 5), (0, 2., 40), (16, 0.25, 20)] {
        definition["flags"] = json!(flags);
        let mut system = System::new(&definition, &json!({"count":count}), 7).unwrap();
        assert_eq!(system.max, 100, "maxcount is the authored hard limit");
        while !system.advance(1.01, Vec3::ZERO, Mat4::IDENTITY, &audio) {}
        assert_eq!(system.particles.len(), births);
        system.set_overrides(&json!({"count":0.5})).unwrap();
        while !system.advance(2.01, Vec3::ZERO, Mat4::IDENTITY, &audio) {}
        assert_eq!(
            system.particles.len(),
            births + if flags == 0 { 10 } else { 20 }
        );
    }
}

#[test]
fn instance_colors_replace_initializers_and_live_particles_without_losing_authored_rgb() {
    let mut definition = json!({"maxcount":2,"emitter":[{"name":"boxrandom","instantaneous":1,"rate":0}],"initializer":[{"name":"lifetimerandom","min":2,"max":2},{"name":"colorrandom","min":"136 255 80","max":"136 255 80"},{"name":"alpharandom","min":0.5,"max":0.5}]});
    let audio = AudioSnapshot::default();
    let authored = Vec3::new(136., 255., 80.) / 255.;
    for (overrides, expected) in [
        (Value::Null, authored),
        (
            json!({"colorn":"0.467 0.435 0.420"}),
            Vec3::new(0.467, 0.435, 0.420),
        ),
        (
            json!({"color":"119 111 107"}),
            Vec3::new(119., 111., 107.) / 255.,
        ),
        (json!({"colorn":"1 1 1"}), Vec3::ONE),
    ] {
        let mut system = System::new(&definition, &overrides, 7).unwrap();
        system.step_tick(0., &audio);
        assert_eq!(system.particles[0].color, expected.extend(0.5));
        system
            .set_overrides(&json!({"colorn":[0,0,1],"alpha":0.5}))
            .unwrap();
        assert_eq!(system.particles[0].color, Vec3::Z.extend(0.5));
        system.forced = 1;
        system.step_tick(STEP as f32, &audio);
        assert_eq!(system.particles[0].color, Vec3::Z.extend(0.5));
        assert_eq!(system.particles[1].color, Vec3::Z.extend(0.25));
        system.set_overrides(&Value::Null).unwrap();
        system.step_tick(STEP as f32, &audio);
        assert_eq!(system.particles[0].color, authored.extend(0.5));
    }
    definition["flags"] = json!(8);
    let mut disabled = System::new(&definition, &json!({"colorn":[0,0,1]}), 7).unwrap();
    disabled.step_tick(0., &audio);
    assert_eq!(disabled.particles[0].color, authored.extend(0.5));
}

#[test]
fn color_changes_multiply_initialized_rgb_in_operator_order_with_native_defaults() {
    let mut definition = json!({"maxcount":1,"emitter":[{"name":"boxrandom","instantaneous":1,"rate":0}],"initializer":[{"name":"lifetimerandom","min":2,"max":2},{"name":"colorrandom","min":"136 255 80","max":"136 255 80"},{"name":"alpharandom","min":0.25,"max":0.25}],"operator":[{"name":"colorchange","startvalue":"0.5 0.25 1","endvalue":"1 0.75 0.5","endtime":0.5}]});
    let audio = AudioSnapshot::default();
    let authored = Vec3::new(136., 255., 80.) / 255.;
    for (overrides, base) in [
        (Value::Null, authored),
        (json!({"colorn":[0.5,1,0.5]}), Vec3::new(0.5, 1., 0.5)),
    ] {
        let mut system = System::new(&definition, &overrides, 7).unwrap();
        for (time, factor) in [
            (0., Vec3::new(0.5, 0.25, 1.)),
            (0.5, Vec3::new(0.75, 0.5, 0.75)),
            (1., Vec3::new(1., 0.75, 0.5)),
        ] {
            system.advance(time, Vec3::ZERO, Mat4::IDENTITY, &audio);
            assert!((system.particles[0].color - (base * factor).extend(0.25)).length() < 0.00001);
        }
    }
    definition["operator"] = json!([{"name":"colorchange","endvalue":"0 0 1","endtime":0.5}]);
    let mut system = System::new(&definition, &Value::Null, 7).unwrap();
    for (time, blue) in [(0., 0.), (0.5, 40. / 255.), (1., 80. / 255.)] {
        system.advance(time, Vec3::ZERO, Mat4::IDENTITY, &audio);
        assert!((system.particles[0].color - Vec4::new(0., 0., blue, 0.25)).length() < 0.00001);
    }
    definition["operator"] = json!([{"name":"colorchange"}]);
    let mut system = System::new(&definition, &Value::Null, 7).unwrap();
    system.advance(0.5, Vec3::ZERO, Mat4::IDENTITY, &audio);
    assert_eq!(system.particles[0].color, Vec4::new(0., 0., 0., 0.25));
    definition["operator"] = json!([{"name":"colorchange","startvalue":"0.5 1 0.5","endvalue":"0.5 1 0.5"},{"name":"colorchange","startvalue":"1 0.5 0.5","endvalue":"1 0.5 0.5"}]);
    let mut system = System::new(&definition, &Value::Null, 7).unwrap();
    system.advance(0.5, Vec3::ZERO, Mat4::IDENTITY, &audio);
    assert!(
        (system.particles[0].color - (authored * Vec3::new(0.5, 0.5, 0.25)).extend(0.25)).length()
            < 0.00001
    );
}

#[test]
fn alpha_fade_overlap_keeps_native_fade_in_branch_priority() {
    let definition = json!({"maxcount":1,"emitter":[{"name":"boxrandom","instantaneous":1,"rate":0}],"initializer":[{"name":"lifetimerandom","min":1,"max":1}],"operator":[{"name":"alphafade","fadeintime":0.8,"fadeouttime":0.2}]});
    let audio = AudioSnapshot::default();
    let mut system = System::new(&definition, &Value::Null, 7).unwrap();
    for (time, alpha) in [(0., 0.), (0.5, 0.625), (0.75, 0.9375), (0.9, 0.125)] {
        system.advance(time, Vec3::ZERO, Mat4::IDENTITY, &audio);
        assert!(
            (system.particles[0].color.w - alpha).abs() < 0.00001,
            "time {time}: alpha {}, expected {alpha}",
            system.particles[0].color.w
        );
    }
}

#[test]
fn value_changes_keep_start_values_at_zero_width_lifetime_boundaries() {
    let audio = AudioSnapshot::default();
    for name in ["sizechange", "alphachange", "colorchange"] {
        let definition = json!({"maxcount":1,"emitter":[{"name":"boxrandom","instantaneous":1,"rate":0}],"initializer":[{"name":"lifetimerandom","min":1,"max":1},{"name":"sizerandom","min":4,"max":4}],"operator":[{"name":name,"startvalue":1,"endvalue":0,"starttime":0,"endtime":0}]});
        let mut system = System::new(&definition, &Value::Null, 7).unwrap();
        system.step_tick(0., &audio);
        assert_eq!(system.particles[0].size, 4.);
        assert_eq!(system.particles[0].color, Vec4::ONE);
        system.step_tick(STEP as f32, &audio);
        let p = &system.particles[0];
        match name {
            "sizechange" => assert_eq!(p.size, 0.),
            "alphachange" => assert_eq!(p.color.w, 0.),
            _ => assert_eq!(p.color, Vec4::new(0., 0., 0., 1.)),
        }
    }
}
fn definition() -> Value {
    json!({"maxcount":32,"emitter":[{"name":"boxrandom","rate":20,"instantaneous":2}],"initializer":[{"name":"lifetimerandom","min":2,"max":2},{"name":"sizerandom","min":4,"max":4},{"name":"velocityrandom","min":"10 0 0","max":"10 0 0"},{"name":"rotationrandom","min":0.2,"max":0.2}],"operator":[{"name":"movement"},{"name":"alphafade","fadeintime":0.25,"fadeouttime":0.75}]})
}
#[test]
fn disabled_overrides_apply_at_creation_and_live_changes_and_movement_has_its_own_world_flag() {
    let base = json!({"maxcount":8,"emitter":[{"name":"boxrandom","instantaneous":1,"rate":0}],"initializer":[{"name":"lifetimerandom","min":1,"max":1},{"name":"sizerandom","min":4,"max":4},{"name":"velocityrandom","min":"8 0 0","max":"8 0 0"}]});
    let overrides =
        json!({"count":0.25,"size":2,"lifetime":2,"speed":3,"colorn":[0,1,0],"alpha":0.5});
    for flags in [0, 8, 16, 32, 64, 128, 248] {
        let mut value = base.clone();
        value["flags"] = json!(flags);
        let mut system = System::new(&value, &overrides, 7).unwrap();
        system.step_tick(0., &AudioSnapshot::default());
        let p = &system.particles[0];
        assert_eq!(system.max, 8);
        assert_eq!(
            p.color,
            if flags & 8 != 0 {
                Vec3::ONE.extend(0.5)
            } else {
                Vec3::Y.extend(0.5)
            }
        );
        assert_eq!(p.life, if flags & 32 != 0 { 1. } else { 2. });
        assert_eq!(p.size, if flags & 64 != 0 { 4. } else { 8. });
        assert_eq!(p.velocity.x, if flags & 128 != 0 { 8. } else { 24. });
        system
            .set_overrides(&json!({"count":0.5,"size":3,"lifetime":3,"speed":2,"colorn":[1,0,0]}))
            .unwrap();
        system.forced = 1;
        system.step_tick(0., &AudioSnapshot::default());
        let p = &system.particles[1];
        assert_eq!(system.max, 8);
        assert_eq!(
            p.color,
            if flags & 8 != 0 {
                Vec4::ONE
            } else {
                Vec3::X.extend(1.)
            }
        );
        assert_eq!(p.life, if flags & 32 != 0 { 1. } else { 3. });
        assert_eq!(p.size, if flags & 64 != 0 { 4. } else { 12. });
        assert_eq!(p.velocity.x, if flags & 128 != 0 { 8. } else { 16. });
    }
    for world in [false, true] {
        let value = json!({"maxcount":1,"emitter":[{"name":"boxrandom","instantaneous":1,"rate":0}],"initializer":[{"name":"lifetimerandom","min":2,"max":2}],"operator":[{"name":"movement","gravity":"4 0 0","flags":u32::from(world)}]});
        let mut system = System::new(&value, &Value::Null, 7).unwrap();
        system.set_space(Mat4::from_rotation_z(std::f32::consts::FRAC_PI_2));
        system.step_tick(0., &AudioSnapshot::default());
        for _ in 0..30 {
            system.step_tick(STEP as f32, &AudioSnapshot::default());
        }
        let velocity = system.space.transform_vector3(system.particles[0].velocity);
        assert!(velocity.abs_diff_eq(if world { Vec3::X } else { Vec3::Y }, 1e-5));
    }
}
#[test]
fn world_particles_freeze_birth_transform_velocity_and_history_but_use_current_control_points() {
    let value = json!({"flags":1,"maxcount":3,"controlpoint":[{"id":0,"offset":"1 0 0"},{"id":1,"offset":"5 0 0"}],"emitter":[{"name":"boxrandom","instantaneous":1,"rate":0}],"initializer":[{"name":"lifetimerandom","min":2,"max":2},{"name":"velocityrandom","min":"12 0 0","max":"12 0 0"}],"operator":[{"name":"movement","gravity":"4 0 0"}],"renderer":[{"name":"ropetrail","length":0.5,"segments":8}]});
    let mut system = System::new(&value, &Value::Null, 7).unwrap();
    system.set_space(
        Mat4::from_translation(Vec3::new(10., 20., 0.))
            * Mat4::from_rotation_z(std::f32::consts::FRAC_PI_2),
    );
    system.update_points(Vec3::ZERO, Mat4::IDENTITY, Vec3::ZERO);
    system.step_tick(0., &AudioSnapshot::default());
    assert!(
        system.particles[0]
            .position
            .abs_diff_eq(Vec3::new(10., 21., 0.), 1e-5)
    );
    assert!(
        system.particles[0]
            .velocity
            .abs_diff_eq(Vec3::new(0., 12., 0.), 1e-5)
    );
    system.set_space(Mat4::from_translation(Vec3::new(30., 40., 0.)));
    for _ in 0..30 {
        system.step_tick(STEP as f32, &AudioSnapshot::default());
    }
    assert!(
        system.particles[0]
            .position
            .abs_diff_eq(Vec3::new(10.120833, 24., 0.), 1e-4)
    );
    assert!(
        system.particles[0]
            .velocity
            .abs_diff_eq(Vec3::new(1., 12., 0.), 1e-4)
    );
    let before = system.particles[0].clone();
    system.forced = 1;
    system.step_tick(0., &AudioSnapshot::default());
    assert_eq!(system.particles[0], before);
    assert_eq!(system.particles[1].position, Vec3::new(31., 40., 0.));
    assert_eq!(system.particles[1].velocity, Vec3::new(12., 0., 0.));
    let value = json!({"flags":1,"maxcount":1,"controlpoint":[{"id":1,"offset":"4 0 0"}],"emitter":[{"name":"boxrandom","instantaneous":1,"rate":0}],"initializer":[{"name":"lifetimerandom","min":2,"max":2}],"operator":[{"name":"controlpointattract","controlpoint":1,"scale":12,"threshold":1000}]});
    let mut attract = System::new(&value, &Value::Null, 1).unwrap();
    attract.set_space(Mat4::from_translation(Vec3::new(10., 20., 0.)));
    attract.update_points(Vec3::ZERO, Mat4::IDENTITY, Vec3::ZERO);
    attract.step_tick(0., &AudioSnapshot::default());
    attract.step_tick(STEP as f32, &AudioSnapshot::default());
    assert!(
        attract.particles[0]
            .velocity
            .abs_diff_eq(Vec3::new(0.0996, 0., 0.), 1e-6)
    );
}
#[test]
fn attract_force_falls_off_across_the_full_authored_threshold() {
    let mut value = json!({"maxcount":1,
        "controlpoint":[{"id":1,"offset":"0 0 -707"}],
        "emitter":[{"name":"boxrandom","instantaneous":1,"rate":0}],
        "operator":[{"name":"controlpointattract","controlpoint":1,"scale":600,"threshold":1000}]
    });
    let audio = AudioSnapshot::default();
    for (distance, expected) in [
        (1100., 0.),
        (1000., 0.),
        (950., -0.25),
        (707., -1.465),
        (700., -1.5),
        (350., -3.25),
    ] {
        value["controlpoint"][0]["offset"] = json!(format!("0 0 -{distance}"));
        let mut system = System::new(&value, &Value::Null, 7).unwrap();
        system.advance(0., Vec3::ZERO, Mat4::IDENTITY, &audio);
        system.step_tick(STEP as f32, &audio);
        assert!(
            system.particles[0]
                .velocity
                .abs_diff_eq(Vec3::Z * expected, 0.00001),
            "distance {distance} has incorrect attractor falloff"
        );
    }
}
#[test]
fn forced_births_are_reported_once_to_child_systems() {
    let mut system = System::new(&definition(), &Value::Null, 7).unwrap();
    system.emitting = false;
    system.forced = 2;
    system.step_tick(0., &AudioSnapshot::default());
    assert_eq!(system.particles.len(), 2);
    assert_eq!(system.events.born.len(), 2);
    assert_ne!(system.events.born[0].id, system.events.born[1].id);
    system.step_tick(STEP as f32, &AudioSnapshot::default());
    assert!(system.events.born.is_empty());
}
#[test]
fn trail_history_is_identical_at_different_frame_rates_pause_and_rewind() {
    let mut definition = definition();
    definition["renderer"] = json!([{"name":"ropetrail","length":0.5,"segments":8}]);
    definition["operator"]
        .as_array_mut()
        .unwrap()
        .push(json!({"name":"movement","gravity":"0 8 0"}));
    let mut a = System::new(&definition, &Value::Null, 7).unwrap();
    let mut b = a.clone();
    for frame in 0..=60 {
        a.advance(
            frame as f64 / 60.,
            Vec3::ZERO,
            glam::Mat4::IDENTITY,
            &AudioSnapshot::default(),
        );
    }
    for frame in 0..=24 {
        b.advance(
            frame as f64 / 24.,
            Vec3::ZERO,
            glam::Mat4::IDENTITY,
            &AudioSnapshot::default(),
        );
    }
    assert_eq!(a.particles, b.particles);
    let held = a.particles.clone();
    a.advance(
        1.,
        Vec3::ZERO,
        glam::Mat4::IDENTITY,
        &AudioSnapshot::default(),
    );
    assert_eq!(a.particles, held);
    a.advance(
        0.,
        Vec3::ZERO,
        glam::Mat4::IDENTITY,
        &AudioSnapshot::default(),
    );
    a.advance(
        1.,
        Vec3::ZERO,
        glam::Mat4::IDENTITY,
        &AudioSnapshot::default(),
    );
    assert_eq!(a.particles, held);
}
#[test]
fn fixed_seed_clock_fps_pause_lifecycle_and_pool_limit() {
    let v = definition();
    let mut a = System::new(&v, &Value::Null, 7).unwrap();
    let mut b = System::new(&v, &Value::Null, 7).unwrap();
    let audio = AudioSnapshot::default();
    for frame in 0..61 {
        a.advance(
            frame as f64 / 60.0,
            Vec3::ZERO,
            glam::Mat4::IDENTITY,
            &audio,
        );
    }
    for frame in 0..31 {
        b.advance(
            frame as f64 / 30.0,
            Vec3::ZERO,
            glam::Mat4::IDENTITY,
            &audio,
        );
    }
    assert_eq!(a.particles, b.particles);
    assert_eq!(a.tick, 120);
    assert_eq!(a.particles.len(), 22);
    let p = &a.particles[0];
    assert!((p.position.x - 10.0).abs() < 0.001);
    assert_eq!(p.rotation, Vec3::new(0.2, 0.0, 0.0));
    assert!((p.color.w - 1.0).abs() < 0.001);
    let paused = a.particles.clone();
    for _ in 0..10 {
        a.advance(1.0, Vec3::ZERO, glam::Mat4::IDENTITY, &audio);
    }
    assert_eq!(paused, a.particles);
    a.set_overrides(&json!({"count":0.25})).unwrap();
    assert_eq!(a.max, 32);
    assert_eq!(
        a.particles.len(),
        22,
        "changing emission must retain live particles"
    );
    let mut late = System::new(&v, &Value::Null, 7).unwrap();
    while !late.advance(4.0, Vec3::ZERO, glam::Mat4::IDENTITY, &audio) {}
    assert!(late.particles.iter().all(|p| p.age < p.life));
    late.advance(1.0, Vec3::ZERO, glam::Mat4::IDENTITY, &audio);
    assert_eq!(late.particles, b.particles);
}
#[test]
fn audio_channels_silence_controlpoints_and_bad_assets() {
    let mut v = definition();
    v["emitter"][0]["audioprocessingmode"] = json!(1);
    v["emitter"][0]["audioprocessingbounds"] = json!("0 1");
    v["controlpoint"] = json!([{"id":0,"flags":1}]);
    let mut s = System::new(&v, &Value::Null, 1).unwrap();
    let mut audio = AudioSnapshot::default();
    assert!(s.audio && s.pointer);
    s.advance(
        0.5,
        Vec3::new(100.0, 200.0, 0.0),
        glam::Mat4::IDENTITY,
        &audio,
    );
    assert!(s.particles.is_empty());
    audio.bands[0].right.fill(1.0);
    s.advance(
        1.0,
        Vec3::new(100.0, 200.0, 0.0),
        glam::Mat4::IDENTITY,
        &audio,
    );
    assert!(s.particles.is_empty());
    audio.bands[0].left.fill(1.0);
    s.advance(
        1.5,
        Vec3::new(100.0, 200.0, 0.0),
        glam::Mat4::IDENTITY,
        &audio,
    );
    assert!(!s.particles.is_empty());
    assert!(
        s.particles
            .iter()
            .all(|p| p.position.y == 200.0 && p.position.x >= 100.0)
    );
    v["initializer"][0]["exponent"] = json!(-1);
    assert!(System::new(&v, &Value::Null, 1).is_err());
    v["initializer"][0]["exponent"] = json!(1);
    v["maxcount"] = json!(1e12);
    assert_eq!(System::new(&v, &Value::Null, 1).unwrap().max, MAX_PARTICLES);
}

#[test]
fn prewarm_discards_only_trail_samples_that_expire_before_the_frame() {
    let audio = AudioSnapshot::default();
    let definition = json!({"maxcount":4,
        "emitter":[{"name":"boxrandom","rate":2}],
        "initializer":[{"name":"lifetimerandom","min":150,"max":300},
            {"name":"velocityrandom","min":"1 2 0","max":"3 4 0"}],
        "operator":[{"name":"movement"},{"name":"vortex"}]});
    for (duration, rate) in [0.07, 30., 3600.]
        .into_iter()
        .flat_map(|duration| [0., 0.1, 1., 5., 128.].map(|rate| (duration, rate)))
    {
        let mut expected = System::new(&definition, &json!({"rate":rate}), 7).unwrap();
        expected.histories = vec![super::super::history::Config {
            duration,
            segments: 16,
        }];
        let mut actual = expected.clone();
        actual.step_tick_to(0., &audio, 36_000);
        expected.step_tick(0., &audio);
        for tick in 1..=36_000 {
            actual.step_tick_to(STEP as f32, &audio, 36_000 - tick);
            expected.step_tick(STEP as f32, &audio);
        }
        assert_eq!(
            actual.particles, expected.particles,
            "duration={duration}, rate={rate}"
        );
        assert_eq!(actual.tick, 36_000);
        // Live playback must continue with the same histories after catch-up.
        for _ in 0..120 {
            actual.step_tick_to(STEP as f32, &audio, 0);
            expected.step_tick(STEP as f32, &audio);
        }
        assert_eq!(
            actual.particles, expected.particles,
            "live duration={duration}, rate={rate}"
        );
    }
}
