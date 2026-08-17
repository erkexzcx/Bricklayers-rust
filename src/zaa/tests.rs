use super::*;

/// A shallow cone: a square whose side loses [`RUN`] mm a side each layer,
/// which leaves a strip of that width exposed on every one of them. Only
/// the right-hand side of the strip is printed, which is all the surface
/// model needs and all a test can read.
const HALF: f64 = 30.0;
const RUN: f64 = 3.0;
const HEIGHT: f64 = 0.2;
/// Length of a bead laid along the strip, and what it is metered at.
const ALONG: f64 = 12.0;
const ALONG_E: f64 = 0.35;

/// The shipped settings, which is all of them: how wide a step to follow
/// and how finely to sample one are derived rather than given. The
/// fixture's 3 mm strip sits well inside the reach its 0.2 mm layers
/// derive — 11.5 mm — so a test reads the model and not the taper at the
/// edge of it.
fn config() -> Config {
    Config::default()
}

/// A wall stack as a slicer lays one: the hidden loop first, then the
/// visible loop on the layer's own outline.
fn ring(text: &mut String, half: f64) {
    for (label, half) in [
        (";TYPE:Perimeter\n", half - 0.45),
        (";TYPE:External perimeter\n", half),
    ] {
        text.push_str(label);
        text.push_str(&format!("G1 X{:.3} Y{:.3} F9000\n", -half, -half));
        for (x, y) in [(half, -half), (half, half), (-half, half), (-half, -half)] {
            text.push_str(&format!("G1 X{x:.3} Y{y:.3} E1.00000\n"));
        }
    }
}

/// Beads laid over the strip this layer leaves exposed: five running along
/// it at one height each, which is what a zigzag does where it turns, and
/// two crossing it, which is the climb itself.
fn surface(text: &mut String, half: f64, run: f64, label: &str) {
    text.push_str(label);
    let (inner, outer) = (half - run + 0.35, half - 0.3);
    for step in 0..5 {
        let x = inner + (outer - inner) * step as f64 / 4.0;
        let (from, to) = match step % 2 {
            0 => (-ALONG / 2.0, ALONG / 2.0),
            _ => (ALONG / 2.0, -ALONG / 2.0),
        };
        text.push_str(&format!("G1 X{x:.3} Y{from:.3} F9000\n"));
        text.push_str(&format!("G1 X{x:.3} Y{to:.3} E{ALONG_E:.5}\n"));
    }
    for (step, y) in [(0, -8.0), (1, 8.0)] {
        let (from, to) = match step % 2 {
            0 => (outer, inner),
            _ => (inner, outer),
        };
        text.push_str(&format!("G1 X{from:.3} Y{y:.3} F9000\n"));
        text.push_str(&format!("G1 X{to:.3} Y{y:.3} E0.10000\n"));
    }
}

fn cone(layers: usize) -> String {
    cone_of(layers, RUN)
}

/// The same cone at a stated slope: each layer loses `run` mm a side, so
/// the strip it leaves exposed is `run` wide and the slope is
/// `atan(HEIGHT / run)`.
fn cone_of(layers: usize, run: f64) -> String {
    let mut text = String::from("; layer_height = 0.2\nM83\nG1 Z0.200 F600\n");
    for layer in 0..layers {
        let half = HALF - run * layer as f64;
        text.push_str(";LAYER_CHANGE\n");
        text.push_str(&format!("G1 Z{:.3} F600\n", HEIGHT * (layer + 1) as f64));
        ring(&mut text, half);
        surface(&mut text, half, run, ";TYPE:Top surface\n");
    }
    text
}

/// The same shape with a vertical wall: nothing is ever exposed, so
/// nothing is followed.
fn box_(layers: usize) -> String {
    let mut text = String::from("; layer_height = 0.2\nM83\nG1 Z0.200 F600\n");
    for layer in 0..layers {
        text.push_str(";LAYER_CHANGE\n");
        text.push_str(&format!("G1 Z{:.3} F600\n", HEIGHT * (layer + 1) as f64));
        ring(&mut text, HALF);
        surface(&mut text, HALF, RUN, ";TYPE:Top surface\n");
    }
    text
}

/// One move of the output, with the axes it left unnamed carried forward.
#[derive(Clone, Debug)]
struct Step {
    x: f64,
    y: f64,
    z: f64,
    e: f64,
    feature: Feature,
    layer: usize,
    raw: String,
}

impl Step {
    fn length(&self, from: (f64, f64)) -> f64 {
        (self.x - from.0).hypot(self.y - from.1)
    }
}

fn steps(gcode: &str) -> Vec<Step> {
    let mut out = Vec::new();
    let (mut at, mut z) = ((0.0, 0.0), 0.0);
    let (mut feature, mut layer, mut started) = (Feature::Other, 0usize, false);
    let mut extruder = Extruder::new();
    for raw in gcode.lines() {
        let line = Line::parse(raw);
        if let Some(text) = line.marker() {
            if is_layer_marker(text) {
                layer += usize::from(std::mem::replace(&mut started, true));
                feature = Feature::Other;
            } else if let Some(read) = Feature::from_marker(text) {
                feature = read;
            }
            continue;
        }
        match line.code {
            Code::AbsoluteE | Code::RelativeE => extruder.set_mode(line.code),
            Code::SetPosition => {
                if let Some(e) = line.e {
                    extruder.set_position(e);
                }
            }
            _ => {}
        }
        if !line.draws() {
            continue;
        }
        if let Some(value) = line.z {
            z = value;
        }
        at = (line.x.unwrap_or(at.0), line.y.unwrap_or(at.1));
        let e = line.e.map_or(0.0, |value| extruder.observe(value));
        out.push(Step {
            x: at.0,
            y: at.1,
            z,
            e,
            feature,
            layer,
            raw: raw.to_owned(),
        });
    }
    out
}

/// The plane a layer is nominally printed at. Measured off the fixture
/// rather than off the output: the whole point of the transform is that
/// the output no longer sits on one height.
fn plane(layer: usize) -> f64 {
    HEIGHT * (layer + 1) as f64
}

/// The case this exists for. A layer's exposed strip is one tread of the
/// staircase, and the surface has to cross it from half a layer under its
/// own plane to half a layer over it — which is where the next layer's
/// strip picks it up.
#[test]
fn a_shallow_surface_is_followed_across_its_own_layer() {
    let out = apply(&cone(6), &config());
    assert!(out.stats.moves > 0, "something was followed");
    let steps = steps(&out.gcode);
    let plane = plane(3);

    let surface: Vec<&Step> = steps
        .iter()
        .filter(|step| step.layer == 3 && step.feature == Feature::TopSurface && step.e > 0.0)
        .collect();
    assert!(surface.len() >= 7, "the strip is printed");

    let lowest = surface.iter().map(|step| step.z).fold(f64::MAX, f64::min);
    let highest = surface.iter().map(|step| step.z).fold(f64::MIN, f64::max);
    assert!(lowest < plane - 0.03, "it dips: {lowest} against {plane}");
    assert!(highest > plane + 0.03, "and it rises: {highest}");
    // Never further than half a layer either way: past that it would be
    // printing into the layer above or into the one below.
    let half = HEIGHT / 2.0 + 1e-6;
    assert!(highest - plane <= half, "{highest} is over the layer above");
    assert!(plane - lowest <= half, "{lowest} is under the layer below");
    assert_eq!(out.stats.layers, 4, "the bed and the top stay flat");
}

/// A slicer's custom G-code switches to relative positioning to lift and
/// nudge without knowing where the toolhead is. It is never a top surface,
/// so it is measured and then left exactly as it was found.
///
/// Read as absolute, its `G1 Z-1` is a plane at minus one millimetre. The
/// layer's plane is the lowest height it commands, so the whole layer would
/// then be followed a millimetre under the bed.
#[test]
fn a_section_in_relative_positioning_is_written_back_exactly_as_it_was_found() {
    let block = concat!(
        "G91\n",
        "G1 Z1.000 F600\n",
        "G1 X1.000 Y1.000 F9000\n",
        "G1 Z-1.000 F600\n",
        "G90\n",
    );
    let source = cone(6).replace("G1 Z0.800 F600\n", &format!("G1 Z0.800 F600\n{block}"));
    assert!(source.contains(block), "the block went in");

    let out = apply(&source, &config());
    assert!(out.stats.moves > 0, "something was still followed");
    assert!(
        out.gcode.contains(block),
        "the block was rewritten:\n{}",
        out.gcode
    );
    let written: Vec<f64> = out
        .gcode
        .lines()
        .filter(|line| line.contains(ZAA_STAMP))
        .filter_map(|line| Line::parse(line).z)
        .collect();
    assert!(!written.is_empty(), "heights were written");
    for z in written {
        assert!(z > 0.0, "a height of {z} is under the bed:\n{}", out.gcode);
    }
}

/// The surface climbs as it goes inward, because the layer above starts
/// where the strip ends. A step of it must never go the other way.
#[test]
fn the_surface_climbs_toward_the_layer_printed_over_it() {
    let out = apply(&cone(6), &config());
    let steps = steps(&out.gcode);
    let inner = HALF - RUN * 3.0 - RUN + 0.35;

    let mut ranked: Vec<(f64, f64)> = steps
        .iter()
        .filter(|step| step.layer == 3 && step.feature == Feature::TopSurface && step.e > 0.0)
        .map(|step| (step.x, step.z))
        .collect();
    ranked.sort_by(|a, b| a.0.total_cmp(&b.0));
    assert!(ranked[0].0 < inner + 0.5, "the inner end is printed");
    for pair in ranked.windows(2) {
        assert!(
            pair[1].1 <= pair[0].1 + 1e-9,
            "{pair:?} rises going outward"
        );
    }
}

/// A stretch of surface standing above its plane crosses a taller gap than
/// the slicer metered it for, and one below it crosses a shorter one.
///
/// Read off the beads laid at one height, where the gap under the whole of
/// the bead is the same. A bead that climbs is metered for its own middle,
/// which is checked separately.
#[test]
fn each_stretch_is_metered_for_the_gap_it_crosses() {
    let out = apply(&cone(6), &config());
    let steps = steps(&out.gcode);
    let plane = plane(3);
    let sliced = ALONG_E / ALONG;

    let mut at = (0.0, 0.0);
    let mut was = plane;
    let mut rates: Vec<(f64, f64)> = Vec::new();
    for step in &steps {
        let length = step.length(at);
        at = (step.x, step.y);
        let level = was;
        was = step.z;
        if step.layer != 3 || step.feature != Feature::TopSurface || step.e <= 0.0 {
            continue;
        }
        if length > 1e-9 && level == step.z {
            rates.push((step.z - plane, step.e / length));
        }
    }
    assert!(rates.len() >= 5, "{} beads at one height", rates.len());

    for (rise, rate) in &rates {
        let wanted = sliced * (HEIGHT + rise) / HEIGHT;
        assert!(
            (rate - wanted).abs() < sliced * 0.02,
            "a stretch {rise} mm up took {rate} where {wanted} fills it"
        );
    }
    let low = rates.iter().map(|(_, rate)| *rate).fold(f64::MAX, f64::min);
    let high = rates.iter().map(|(_, rate)| *rate).fold(f64::MIN, f64::max);
    assert!(high > low * 1.4, "the two ends differ: {low} to {high}");
}

/// A bead that crosses a whole strip climbs from half a layer under its
/// plane to half a layer over it, so what it gains at one end it gives
/// back at the other and it ends up metered near enough as sliced.
#[test]
fn a_bead_that_crosses_a_whole_strip_costs_what_it_was_sliced_at() {
    let out = apply(&cone(6), &config());
    let steps = steps(&out.gcode);
    let plane = plane(3);

    // The crossings are the two beads laid at |y| = 8, away from the ones
    // that run along the strip.
    let mut stock = 0.0;
    let mut crossings = 0;
    let mut was = plane;
    for step in &steps {
        let level = was;
        was = step.z;
        if step.layer != 3 || step.feature != Feature::TopSurface || step.e <= 0.0 {
            continue;
        }
        if step.y.abs() < 7.0 {
            continue;
        }
        stock += step.e;
        crossings += usize::from(level != step.z);
    }
    assert!(crossings > 0, "the crossings do climb");
    let sliced = 0.2;
    assert!(
        (stock - sliced).abs() < sliced * 0.1,
        "{stock} against the {sliced} it was sliced at"
    );
}

/// Everything that is neither a surface nor a wall comes out exactly as it
/// went in. The infill belongs to neither transform.
#[test]
fn nothing_but_a_surface_or_a_wall_is_touched() {
    let source = cone(6);
    let out = apply(&source, &config());
    let (before, after) = (steps(&source), steps(&out.gcode));

    let rest = |steps: &[Step]| -> Vec<String> {
        steps
            .iter()
            .filter(|step| {
                !step.feature.is_surface()
                    && step.feature != Feature::ExternalPerimeter
                    && step.feature != Feature::InternalPerimeter
            })
            .map(|step| step.raw.clone())
            .collect()
    };
    assert_eq!(rest(&before), rest(&after));
}

/// A slope of more than about ten degrees leaves a tread narrower than the
/// wall stack standing on it, so the staircase is made of wall and there
/// is no top surface to follow at all. Both walls are exposed there, and
/// the hidden one carries as much of what the eye sees as the visible one:
/// measured over the layers of a 60 mm spherical cap that leave a tread
/// wider than a bead, 37% of the exposed path.
#[test]
fn both_walls_follow_the_surface_where_bricking_is_not_running() {
    let out = apply(&cone(6), &config());
    let steps = steps(&out.gcode);
    let plane = plane(3);

    let heights = |feature: Feature| -> Vec<f64> {
        steps
            .iter()
            .filter(|step| step.layer == 3 && step.feature == feature && step.e > 0.0)
            .map(|step| step.z - plane)
            .collect()
    };
    for feature in [Feature::ExternalPerimeter, Feature::InternalPerimeter] {
        let rises = heights(feature);
        assert!(!rises.is_empty(), "{feature:?} is printed");
        let lowest = rises.iter().copied().fold(f64::MAX, f64::min);
        assert!(
            lowest < -0.03,
            "{feature:?} drops toward the layer below: {lowest}"
        );
        for rise in &rises {
            assert!(
                *rise <= HEIGHT / 2.0 + 1e-6 && *rise >= -HEIGHT / 2.0 - 1e-6,
                "{rise} is more than half a layer"
            );
        }
    }
    for rise in &heights(Feature::ExternalPerimeter) {
        assert!(*rise <= 1e-9, "the wall that shows was raised by {rise}");
    }
}

/// The two transforms cannot both own the hidden walls. The bead under one
/// is a hidden loop of the layer below, offset by the tread rather than the
/// one directly beneath, and bricking may have raised that loop half a
/// layer; lowering onto it would close a gap the slicer metered open.
#[test]
fn the_hidden_wall_is_left_alone_where_bricking_owns_it() {
    let out = apply(
        &cone(6),
        &Config {
            bricked: true,
            ..config()
        },
    );
    let steps = steps(&out.gcode);
    let plane = plane(3);

    let mut moved = 0;
    for step in steps.iter().filter(|step| step.layer == 3 && step.e > 0.0) {
        match step.feature {
            Feature::InternalPerimeter => assert!(
                (step.z - plane).abs() < 1e-9,
                "the hidden wall moved by {}",
                step.z - plane
            ),
            Feature::ExternalPerimeter => moved += usize::from((step.z - plane).abs() > 1e-9),
            _ => {}
        }
    }
    assert!(moved > 0, "the visible wall still follows the surface");
}

/// The wall that shows is never commanded above its own plane, whichever
/// transforms are run. A bead of it standing proud is out of reach of the
/// nozzle's flat underside, so what would be ironed level is free to bulge
/// — and it does it on the face of the part.
#[test]
fn the_wall_that_shows_is_never_taken_above_its_plane() {
    // Sweeping the layer height sweeps the reach with it, since the reach
    // is that height over the tangent of the shallowest slope followed.
    for height in [0.06, 0.12, 0.2, 0.28, 0.6] {
        let out = apply(
            &cone(8),
            &Config {
                layer_height: Some(height),
                ..config()
            },
        );
        let steps = steps(&out.gcode);
        for step in steps
            .iter()
            .filter(|step| step.feature == Feature::ExternalPerimeter)
        {
            let plane = plane(step.layer);
            assert!(
                step.z <= plane + 1e-9,
                "a {height} mm layer raised the visible wall: {}",
                step.raw
            );
        }
    }
}

/// Bricking owns the height of any wall it raised, and it stamps that
/// height onto the file. A wall already standing off its plane is one
/// something else placed, so this leaves it exactly where it is.
#[test]
fn a_wall_already_standing_off_its_plane_is_left_where_it_is() {
    let source = cone(6);
    // As bricking leaves it: the travel into the visible wall carries a
    // height of its own.
    let raised = source
        .lines()
        .map(
            |line| match line.starts_with("G1 X-21.000 Y-21.000 F9000") {
                true => "G1 X-21.000 Y-21.000 F9000 Z0.900 ; corbel brick raised".to_owned(),
                false => line.to_owned(),
            },
        )
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    assert_ne!(raised, source, "the fixture was rewritten");

    let out = apply(&raised, &config());
    let steps = steps(&out.gcode);
    for step in steps
        .iter()
        .filter(|step| step.layer == 3 && step.feature == Feature::ExternalPerimeter)
    {
        assert_eq!(step.z, 0.9, "a raised wall moved: {}", step.raw);
    }
}

/// A vertical wall exposes nothing, so there is no staircase and nothing
/// to do. The file has to come back exactly as it went in.
#[test]
fn a_part_with_no_slope_comes_back_untouched() {
    let source = box_(6);
    let out = apply(&source, &config());
    assert_eq!(out.gcode, source);
    assert_eq!(out.stats.moves, 0);
    assert_eq!(out.stats.segments, 0);
    assert!(out.stats.rise.is_none());
}

/// A height change on a line of its own names no other axis, so the
/// planner brings the toolhead to a dead stop to run it. Every one of them
/// here rides a move the slicer was already making.
#[test]
fn a_height_change_rides_a_move_rather_than_stopping_the_toolhead() {
    let out = apply(&cone(6), &config());
    let stops: Vec<&str> = out
        .gcode
        .lines()
        .filter(|line| {
            let parsed = Line::parse(line);
            parsed.is_move() && parsed.z.is_some() && !parsed.is_xy_move()
        })
        .filter(|line| line.contains(ZAA_STAMP))
        .collect();
    assert!(stops.is_empty(), "{stops:?}");
}

/// Two heights the file cannot tell apart are one height, and levelling
/// from one to the other buys nothing while costing a dead stop.
#[test]
fn heights_the_file_cannot_tell_apart_are_the_same_height() {
    assert!(same_height(10.739, 10.739));
    assert!(same_height(10.7390001, 10.7389999));
    assert!(same_height(10.7391, 10.7394));
    assert!(!same_height(10.739, 10.740));
    assert!(!same_height(10.739, 10.744));
}

/// A levelling move must never command the height the nozzle already
/// holds. It writes nothing and costs a dead stop with a primed nozzle,
/// which is exactly what riding a move exists to avoid — and comparing the
/// full-precision heights rather than the three decimals the file carries
/// let 73 of them through on a real Benchy.
#[test]
fn levelling_to_a_height_already_commanded_writes_nothing() {
    let source = cone(6);
    let survey = Survey::of(&source);
    let mut out = Vec::new();
    let mut pass = Pass::new(&mut out, source.as_bytes(), &config(), &survey);

    // The height the file was given, and one a whisker off it: `write_fixed`
    // writes three decimals, so both reach the printer as `Z0.739`.
    pass.nozzle_z = Some(0.739);
    let _ = pass
        .level(0.7390004, false)
        .expect("writing to a Vec cannot fail");
    assert!(out.is_empty(), "{}", String::from_utf8_lossy(&out));

    // A micron further and it is a different command, so it is written.
    let mut out = Vec::new();
    let mut pass = Pass::new(&mut out, source.as_bytes(), &config(), &survey);
    pass.nozzle_z = Some(0.739);
    let _ = pass
        .level(0.740, false)
        .expect("writing to a Vec cannot fail");
    let written = String::from_utf8_lossy(&out);
    assert!(written.contains("Z0.740"), "{written}");
}

/// A file whose regions are never labelled says nothing about which layer
/// is which, and a surface that cannot be found must not be guessed at.
#[test]
fn a_file_with_no_layer_markers_is_left_alone() {
    let source = cone(6).replace(";LAYER_CHANGE\n", "");
    let out = apply(&source, &config());
    assert_eq!(out.gcode, source);
}

/// The layer on the plate has nothing under it to say which way its
/// surface went, and the last one has nothing over it.
#[test]
fn the_first_and_last_layers_of_a_part_stay_flat() {
    let out = apply(&cone(6), &config());
    let steps = steps(&out.gcode);
    for layer in [0usize, 5] {
        let plane = plane(layer);
        for step in steps.iter().filter(|step| step.layer == layer) {
            assert!(
                (step.z - plane).abs() < 1e-9,
                "layer {layer} moved: {}",
                step.raw
            );
        }
    }
}

/// Ironing runs over a surface that has already been laid, so it has to
/// follow it in Z or it scrapes what it is smoothing. It is deliberately
/// not re-metered: there is no gap under it to fill.
#[test]
fn ironing_follows_the_surface_without_being_re_metered() {
    let mut text = String::from("; layer_height = 0.2\nM83\nG1 Z0.200 F600\n");
    for layer in 0..6 {
        let half = HALF - RUN * layer as f64;
        text.push_str(";LAYER_CHANGE\n");
        text.push_str(&format!("G1 Z{:.3} F600\n", HEIGHT * (layer + 1) as f64));
        ring(&mut text, half);
        surface(&mut text, half, RUN, ";TYPE:Ironing\n");
    }
    let out = apply(&text, &config());
    let steps = steps(&out.gcode);
    let plane = plane(3);
    let sliced = ALONG_E / ALONG;

    let mut at = (0.0, 0.0);
    let mut was = plane;
    let (mut moved, mut checked) = (false, 0);
    for step in &steps {
        let length = step.length(at);
        at = (step.x, step.y);
        let level = was;
        was = step.z;
        if step.layer != 3 || step.feature != Feature::Ironing || step.e <= 0.0 {
            continue;
        }
        moved |= (step.z - plane).abs() > 1e-9;
        if length > 1e-9 && level == step.z {
            let rate = step.e / length;
            assert!(
                (rate - sliced).abs() < sliced * 0.02,
                "{rate} against {sliced}"
            );
            checked += 1;
        }
    }
    assert!(moved, "it follows the surface");
    assert!(checked >= 5, "and enough of it was checked");

    // The ironing's own stock is what has to come out unchanged; the wall
    // that shows is re-metered like any other surface.
    let stock = |gcode: &str| -> f64 {
        super::tests::steps(gcode)
            .iter()
            .filter(|step| step.feature == Feature::Ironing && step.e > 0.0)
            .map(|step| step.e)
            .sum()
    };
    let (before, after) = (stock(&text), stock(&out.gcode));
    assert!(
        (before - after).abs() < before * 1e-4,
        "{before} became {after}"
    );
}

/// An arc is followed round rather than cut across. Upstream leaves them
/// alone entirely, which quietly does nothing on a file sliced with arc
/// fitting on.
#[test]
fn an_arc_across_a_surface_is_followed_round_its_own_curve() {
    let mut text = String::from("; layer_height = 0.2\nM83\nG1 Z0.200 F600\n");
    for layer in 0..6 {
        let half = HALF - RUN * layer as f64;
        text.push_str(";LAYER_CHANGE\n");
        text.push_str(&format!("G1 Z{:.3} F600\n", HEIGHT * (layer + 1) as f64));
        ring(&mut text, half);
        // One shallow arc bowing across the strip, drawn about a centre
        // far off to the side so it stays inside the exposed band.
        text.push_str(";TYPE:Top surface\n");
        text.push_str(&format!("G1 X{:.3} Y-4.000 F9000\n", half - 0.3));
        text.push_str(&format!(
            "G2 X{:.3} Y4.000 I20.000 J4.000 E0.30000\n",
            half - 0.3
        ));
    }
    let out = apply(&text, &config());
    assert!(out.stats.moves > 0, "the arc was followed");
    let steps = steps(&out.gcode);
    let plane = plane(3);

    let curve: Vec<&Step> = steps
        .iter()
        .filter(|step| step.layer == 3 && step.feature == Feature::TopSurface && step.e > 0.0)
        .collect();
    assert!(curve.len() > 1, "it was cut into pieces that vary in Z");
    let centre = (HALF - RUN * 3.0 - 0.3 + 20.0, 0.0);
    let radius = 20.0f64.hypot(4.0);
    for step in &curve {
        let measured = (step.x - centre.0).hypot(step.y - centre.1);
        assert!(
            (measured - radius).abs() < 0.01,
            "{measured} is off the circle of {radius}"
        );
        assert!((step.z - plane).abs() <= HEIGHT / 2.0 + 1e-6);
    }
    assert!(
        curve.iter().any(|step| step.z != plane),
        "and it left the plane"
    );

    // Landing on the circle is not enough — what is printed is the chord
    // between two landings, and a simplifier that judges a stretch by its
    // climb alone will happily straighten the whole arc onto one of them,
    // because every sample of an arc at a steady height sits on the same
    // straight climb. Measured on a real 1000-wall Benchy before `span`
    // reached it: 74 arcs written as a single chord, the worst 1.27 mm
    // inside its own wall.
    let mut at = (HALF - RUN * 3.0 - 0.3, -4.0);
    for step in &curve {
        let middle = ((at.0 + step.x) / 2.0, (at.1 + step.y) / 2.0);
        let sag = radius - (middle.0 - centre.0).hypot(middle.1 - centre.1);
        // The written coordinates are on the file's own micron grid, so a
        // chord can sit half of one further in than it was planned to.
        assert!(
            sag <= SAG * 2.0,
            "a chord of {:.3} mm cuts {sag} mm inside the arc",
            step.length(at)
        );
        at = (step.x, step.y);
    }
}

/// In absolute mode every value downstream of a rescaled move shifts, so
/// the whole stream has to be renumbered rather than one word rewritten.
#[test]
fn absolute_extrusion_stays_continuous() {
    let relative = cone(6);
    let absolute = to_absolute(&relative);
    let from_relative = apply(&relative, &config());
    let from_absolute = apply(&absolute, &config());

    let (a, b) = (steps(&from_relative.gcode), steps(&from_absolute.gcode));
    assert_eq!(a.len(), b.len(), "the same moves come out either way");
    for (left, right) in a.iter().zip(&b) {
        assert!((left.x - right.x).abs() < 1e-9);
        assert!((left.z - right.z).abs() < 1e-9);
        // An absolute stream carries five decimals per line rather than
        // per move, so a delta read back out of it is the difference of
        // two roundings.
        assert!(
            (left.e - right.e).abs() < 5e-5,
            "{} of {} against {} of {}",
            left.e,
            left.raw,
            right.e,
            right.raw
        );
    }
    // And the absolute stream itself never runs backwards.
    let mut position = 0.0;
    for line in from_absolute.gcode.lines() {
        let parsed = Line::parse(line);
        if let Some(value) = parsed.e.filter(|_| parsed.draws()) {
            assert!(value >= position - 1e-9, "{line} runs backwards");
            position = value;
        }
    }
}

/// Rewrites a relative-extrusion file as an absolute one, which is what a
/// PrusaSlicer profile with `use_relative_e_distances` off produces.
fn to_absolute(source: &str) -> String {
    let mut out = String::from("M82\n");
    let mut position = 0.0;
    for line in source.lines() {
        if line == "M83" {
            continue;
        }
        let parsed = Line::parse(line);
        match parsed.e.filter(|_| parsed.draws()) {
            Some(delta) => {
                position += delta;
                let mut written = Vec::new();
                parsed
                    .write_e(&mut written, position)
                    .expect("writing to a Vec cannot fail");
                out.push_str(&String::from_utf8(written).expect("G-code is UTF-8"));
                out.push('\n');
            }
            None => {
                out.push_str(line);
                out.push('\n');
            }
        }
    }
    out
}

/// Running twice would measure a surface against a plane it is no longer
/// on, so the marks are left where a survey can find them.
#[test]
fn its_own_work_is_recognisable_afterwards() {
    let out = apply(&cone(6), &config());
    let survey = Survey::of(&out.gcode);
    assert!(survey.contoured);
    assert!(!Survey::of(&cone(6)).contoured);
}

/// The stamps are bare trailing comments on moves, never a region marker
/// of their own, or a second pass would read one as a change of region.
#[test]
fn a_stamp_is_never_read_as_a_region_marker() {
    let out = apply(&cone(6), &config());
    for line in out.gcode.lines().filter(|line| line.contains(ZAA_STAMP)) {
        let parsed = Line::parse(line);
        assert_eq!(
            parsed.marker().and_then(Feature::from_marker),
            None,
            "{line}"
        );
        assert!(!crate::gcode::feature::is_layer_change(line), "{line}");
    }
}

/// A ramp is one move however long it runs, because a printer interpolates
/// Z along a move already. Every bead here is sampled every [`STEP`], so a
/// 12 mm one is looked at 80 times; writing those out would multiply a
/// surface's line count for nothing.
#[test]
fn a_straight_climb_costs_one_move() {
    let out = apply(&cone(6), &config());
    let per_move = out.stats.segments as f64 / out.stats.moves as f64;
    assert!(
        per_move < 4.0,
        "{} moves for {} beads, each sampled {} times",
        out.stats.segments,
        out.stats.moves,
        (ALONG / STEP).ceil()
    );
}

/// How wide a step is worth following is a slope, so the reach it works
/// out to follows each layer's own height — which an adaptive slice
/// changes every layer. A figure in mm would mean a different slope on
/// every profile.
#[test]
fn the_reach_follows_the_layer_height() {
    for (height, expected) in [(0.08, 4.583), (0.2, 11.458), (0.28, 16.041)] {
        assert!(
            (reach_for(height) - expected).abs() < 0.001,
            "{height} mm layers reach {}",
            reach_for(height)
        );
    }
    // The slope it encodes, back out of the width, is the same everywhere.
    for height in [0.05, 0.1, 0.15, 0.2, 0.3, 0.4] {
        let slope = (height / reach_for(height)).atan().to_degrees();
        assert!((slope - SHALLOWEST_SLOPE).abs() < 1e-9, "{height}: {slope}");
    }
}

/// The bug the fixed 4 mm reach left behind: a 1.9° slope leaves a 6 mm
/// tread, which is a staircase at its most visible and was refused for
/// being wider than a number nothing derived.
#[test]
fn a_step_too_wide_for_the_old_fixed_reach_is_followed() {
    let run = 6.0;
    let slope = (HEIGHT / run).atan().to_degrees();
    assert!(slope < 2.0, "the fixture really is that shallow: {slope}");
    let out = apply(&cone_of(4, run), &config());
    assert!(
        out.stats.moves > 0,
        "a {slope:.1} degree slope leaves a {run} mm tread and has to be followed"
    );
    let (low, high) = out.stats.rise.expect("something was followed");
    assert!(
        low < -0.02 && high > 0.02,
        "it climbs the strip: {low}..{high}"
    );
}

/// An arc is replaced by the chords through its samples, so how finely it
/// is sampled has to follow its radius rather than a fixed length. A
/// tight radius is sampled finer than the grid asks for; nothing is ever
/// sampled coarser than the grid.
#[test]
fn an_arc_is_sampled_for_its_own_curvature() {
    let along = Grid::default().cell() * STEP;
    for radius in [0.5, 1.0, 5.0, 10.0, 100.0] {
        let step = chord_of(radius).min(along);
        assert!(step <= along, "radius {radius} sampled at {step}");
        // How far the chord through two samples sits from the arc itself.
        let sag = radius - (radius * radius - step * step / 4.0).max(0.0).sqrt();
        assert!(
            sag <= SAG + 1e-12,
            "radius {radius} is {sag} mm off its arc"
        );
    }
    // A radius tight enough that the grid's step would cut the corner off
    // is sampled finer than the grid, and a wide one is not.
    assert!(chord_of(1.0) < along);
    assert!(chord_of(10.0) > along);
}

/// The one setting left is held to what a printer can act on, wherever it
/// arrives from, since a library caller reaches it without passing the
/// command line's own checks. A height it cannot use falls back to the one
/// measured off the file rather than reaching the reach or the flow.
#[test]
fn a_setting_a_printer_cannot_act_on_is_refused() {
    for layer_height in [
        None,
        Some(f64::NAN),
        Some(f64::INFINITY),
        Some(-1.0),
        Some(0.0),
        Some(1e12),
    ] {
        let out = apply(
            &cone(6),
            &Config {
                layer_height,
                ..config()
            },
        );
        for line in out.gcode.lines() {
            let parsed = Line::parse(line);
            if let Some(z) = parsed.z {
                assert!(z.is_finite() && z >= 0.0, "{line}");
            }
            if let Some(e) = parsed.e.filter(|_| parsed.draws()) {
                assert!(e.is_finite(), "{line}");
            }
        }
    }
}

/// A surface with something printed over it is laid against, so it stays
/// on its plane however shallow the part is around it.
#[test]
fn a_surface_under_the_next_layer_is_left_on_its_plane() {
    // Cura labels both faces of a part `SKIN`, so the underside of a
    // sloping part arrives labelled exactly like the top of one.
    let mut text = String::from("; layer_height = 0.2\nM83\nG1 Z0.200 F600\n");
    for layer in 0..6 {
        let half = HALF - RUN * layer as f64;
        text.push_str(";LAYER:0\n");
        text.push_str(&format!("G1 Z{:.3} F600\n", HEIGHT * (layer + 1) as f64));
        ring(&mut text, half);
        // Laid well inside the layer above rather than in the strip.
        text.push_str(";TYPE:SKIN\n");
        text.push_str(&format!("G1 X{:.3} Y-4.000 F9000\n", half - RUN - 4.0));
        text.push_str(&format!("G1 X{:.3} Y4.000 E0.30000\n", half - RUN - 4.0));
    }
    let out = apply(&text, &config());
    let steps = steps(&out.gcode);
    for layer in 0..6 {
        let plane = plane(layer);
        for step in steps
            .iter()
            .filter(|step| step.layer == layer && step.feature == Feature::TopSurface)
        {
            assert!(
                (step.z - plane).abs() < 1e-9,
                "a covered surface moved: {}",
                step.raw
            );
        }
    }
}

/// A cone whose top surface is printed by beads that run off the exposed
/// strip and carry on under the layer above, which is what a zigzag over a
/// tread does at both ends of every pass.
fn crossing(layers: usize) -> String {
    let mut text = String::from("; layer_height = 0.2\nM83\nG1 Z0.200 F600\n");
    for layer in 0..layers {
        let half = HALF - RUN * layer as f64;
        text.push_str(";LAYER_CHANGE\n");
        text.push_str(&format!("G1 Z{:.3} F600\n", HEIGHT * (layer + 1) as f64));
        ring(&mut text, half);
        text.push_str(";TYPE:Top surface\n");
        for (index, y) in [-6.0, -2.0, 2.0, 6.0].into_iter().enumerate() {
            // Starts in the middle of this layer's strip and ends well
            // inside the footprint of the layer above it.
            let (from, to) = match index % 2 {
                0 => (half - RUN / 2.0, half - RUN - 4.0),
                _ => (half - RUN - 4.0, half - RUN / 2.0),
            };
            text.push_str(&format!("G1 X{from:.3} Y{y:.3} F9000\n"));
            text.push_str(&format!("G1 X{to:.3} Y{y:.3} E0.20000\n"));
        }
    }
    text
}

/// A bead does not have to sit wholly on the exposed strip to be worth
/// following. Testing both of its ends and giving up on the whole move
/// where either is covered throws away most of what there is to smooth:
/// measured on a stock Benchy, 1678 mm of exposed sloped path against the
/// 624 mm it kept, and on a 60 mm cap the top surface lost 126 mm of 184.
/// The stretch that runs under the layer above still goes back on the
/// plane — that part is decided sample by sample, not move by move.
#[test]
fn a_bead_that_runs_under_the_layer_above_still_follows_the_part_that_does_not() {
    let out = apply(&crossing(6), &config());
    let steps = steps(&out.gcode);
    let layer = 3;
    let plane = plane(layer);
    let surface: Vec<&Step> = steps
        .iter()
        .filter(|step| step.layer == layer && step.feature == Feature::TopSurface && step.e > 0.0)
        .collect();
    assert!(surface.len() > 4, "the crossing beads were split up");

    let off = surface
        .iter()
        .filter(|step| (step.z - plane).abs() > 1e-9)
        .count();
    assert!(off > 0, "the open half of a crossing bead is followed");

    // Where it ends is under the layer above, and that has to be flat.
    let half = HALF - RUN * layer as f64;
    for step in surface.iter().filter(|step| step.x < half - RUN - 0.5) {
        assert!(
            (step.z - plane).abs() < 1e-9,
            "a covered stretch moved: {}",
            step.raw
        );
    }
}

/// The surface is at its highest exactly where the layer above begins, and
/// a bead carrying on under that has to be back on its plane — a drop of
/// half a layer with a grid cell to do it in. Left alone that is not a path
/// a nozzle can take: on a 60 mm cap, 886 of 2207 written moves changed
/// height faster than one in two, the worst of them by 0.1 mm over 0.023 mm
/// of travel. The descent is spread out ahead of the edge instead.
#[test]
fn a_surface_never_drops_faster_than_a_slope_can() {
    let out = apply(&crossing(6), &config());
    let worst = steepest(&steps(&out.gcode));
    // The written height is rounded to a micron, so a very short move can
    // read a hair over the grade it was planned to.
    assert!(
        worst <= GRADE * 1.1,
        "a bead fell at {worst:.3} against a limit of {GRADE:.3}"
    );
}

/// One layer height per bead width: the steepest a surface can fall and
/// still be a slope rather than a wall.
const GRADE: f64 = HEIGHT / FALLBACK_WIDTH;

/// The steepest a bead **falls** anywhere, measured **across the joins
/// between moves** as well as along them.
///
/// Falls, and not climbs. What bounds a descent is the nozzle's flat
/// underside plowing back through material it laid a bead width ago; a climb
/// lifts away from that material, so it is held to the surface's own gradient
/// instead — see [`climbing`]. A height change made over no travel at all is
/// a dead stop whichever way it goes, and a `G1 Z` of its own between two
/// beads is exactly that, so one is counted here rather than skipped.
/// Skipping it while still carrying its height forward is the hole the older
/// gauge had: the drop went unseen and the bead after it then measured its
/// own slope from the height the drop had already reached. A layer change is
/// a `G1 Z` of its own too and is not a defect, so the reference is dropped
/// at one.
fn steepest(steps: &[Step]) -> f64 {
    let (mut worst, mut at, mut printing, mut layer) = (0.0f64, (0.0, 0.0, 0.0), false, 0usize);
    for step in steps {
        if step.layer != layer {
            layer = step.layer;
            printing = false;
        }
        let run = step.length((at.0, at.1));
        let change = step.z - at.2;
        if run > 0.0 {
            if step.e > 0.0 {
                worst = worst.max(-change / run);
            }
            printing = step.e > 0.0;
        } else if printing && change != 0.0 {
            worst = f64::INFINITY;
        }
        at = (step.x, step.y, step.z);
    }
    worst
}

/// The steepest a bead climbs anywhere, along a move or across a join.
fn climbing(steps: &[Step]) -> f64 {
    let (mut worst, mut at, mut layer) = (0.0f64, (0.0, 0.0, 0.0), 0usize);
    for step in steps {
        if step.layer != layer {
            layer = step.layer;
            at = (step.x, step.y, step.z);
            continue;
        }
        let run = step.length((at.0, at.1));
        if run > 0.0 && step.e > 0.0 {
            worst = worst.max((step.z - at.2) / run);
        }
        at = (step.x, step.y, step.z);
    }
    worst
}

/// A climb is not a fall seen backwards. Nothing stands in the way of one:
/// the material a climbing nozzle leaves behind is already laid and already
/// metered for the gap it fills, where a descending one drags its own flat
/// underside back through what it laid a bead width ago. So the bound on a
/// climb is the surface itself, which is box-blurred over a grid cell and
/// therefore cannot ask for more than one layer height per cell.
///
/// Held to the descent's figure instead, every bead leaving a covered stretch
/// was kept low for a further bead width — and the far edge of a strip is
/// exactly where the ramp has to reach half a layer for one layer's ramp to
/// meet the next one's, so a tread narrower than a bead was levelled outright
/// and the staircase came back.
#[test]
fn a_climb_is_held_to_the_surface_rather_than_to_the_descent_limit() {
    let out = apply(&crossing(6), &config());
    let steps = steps(&out.gcode);
    let worst = climbing(&steps);
    assert!(
        worst > GRADE * 1.5,
        "a bead leaving a covered stretch climbed at {worst:.3}, which is the \
         descent limit of {GRADE:.3} all over again"
    );

    let cell = Grid::for_span(HALF * 2.0, HALF * 2.0, MAX_WINDOW).cell();
    let limit = HEIGHT / cell;
    // The written height is rounded to a micron and the run to a micron
    // either end, so a sample-long piece can read a hair over its own bound.
    assert!(
        worst <= limit * 1.1,
        "a bead climbed at {worst:.3}, past the {limit:.3} the surface itself \
         can ask for on a {cell} mm grid"
    );
}

/// A cone whose top surface is laid by two beads chained end to end, the
/// way a zigzag runs: one across the open strip, and the next carrying on
/// from exactly where it ended and under the layer above a fraction of a
/// millimetre later. Nothing between them moves the nozzle, so there is no
/// travel for a height change to ride.
fn chained(layers: usize) -> String {
    let mut text = String::from("; layer_height = 0.2\nM83\nG1 Z0.200 F600\n");
    for layer in 0..layers {
        let half = HALF - RUN * layer as f64;
        text.push_str(";LAYER_CHANGE\n");
        text.push_str(&format!("G1 Z{:.3} F600\n", HEIGHT * (layer + 1) as f64));
        ring(&mut text, half);
        text.push_str(";TYPE:Top surface\n");
        // Where the layer above begins, which is where the strip ends.
        let edge = half - RUN;
        for y in [-6.0, -2.0, 2.0, 6.0] {
            text.push_str(&format!("G1 X{:.3} Y{y:.3} F9000\n", half - 0.3));
            text.push_str(&format!("G1 X{:.3} Y{y:.3} E0.20000\n", edge + 0.15));
            text.push_str(&format!("G1 X{:.3} Y{y:.3} E0.20000\n", edge - 4.0));
        }
    }
    text
}

/// The limit on how fast a surface may fall is a property of the path, not
/// of one move of it. A bead reaching a covered edge a fraction of a
/// millimetre in has to start its descent in the bead **before** it, which
/// is only possible while that one is still held back.
///
/// Enforced per move it is not enforced at all across the join: measured on
/// this fixture, the second bead's first sample was clipped from half a
/// layer to a third of it while the first bead had already been written
/// ending at half a layer, and the 33 µm left over came out as a `G1 Z` of
/// its own between two extrusions — a dead stop at a seam with a primed
/// nozzle, making the whole change over no travel whatever.
#[test]
fn the_descent_into_the_next_bead_starts_in_the_one_before_it() {
    let out = apply(&chained(6), &config());
    assert!(out.stats.moves > 0, "nothing was followed");

    let stops: Vec<&str> = out
        .gcode
        .lines()
        .filter(|line| {
            let parsed = Line::parse(line);
            parsed.is_move() && parsed.z.is_some() && !parsed.is_xy_move()
        })
        .filter(|line| line.contains(ZAA_STAMP))
        .collect();
    assert!(
        stops.is_empty(),
        "a height change stopped the toolhead: {stops:?}"
    );

    let worst = steepest(&steps(&out.gcode));
    assert!(
        worst <= GRADE * 1.1,
        "a bead fell at {worst:.3} across a join against a limit of {GRADE:.3}"
    );
}

/// A slicer labels an overhanging stretch of wall in place, partway round
/// the loop and with no travel between it and the wall it interrupts. The
/// wall is followed and the overhang is not, so the nozzle has to come back
/// to the plane between two extrusions with nothing held back to carry it
/// — and it rides the overhanging bead itself rather than stopping for a
/// move of its own.
#[test]
fn a_height_change_between_two_beads_rides_the_bead_after_it() {
    let mut text = String::from("; layer_height = 0.2\nM83\nG1 Z0.200 F600\n");
    for layer in 0..6 {
        let half = HALF - RUN * layer as f64;
        text.push_str(";LAYER_CHANGE\n");
        text.push_str(&format!("G1 Z{:.3} F600\n", HEIGHT * (layer + 1) as f64));
        ring(&mut text, half);
        // The wall carries on from the corner it was left at.
        text.push_str(";TYPE:Overhang perimeter\n");
        text.push_str(&format!("G1 X{:.3} Y{:.3} E0.30000\n", -half + 4.0, -half));
        surface(&mut text, half, RUN, ";TYPE:Top surface\n");
    }
    let out = apply(&text, &config());
    let steps = steps(&out.gcode);

    let overhangs: Vec<&Step> = steps
        .iter()
        .filter(|step| step.feature == Feature::Overhang && step.layer == 3)
        .collect();
    assert_eq!(overhangs.len(), 1, "{overhangs:?}");
    assert!(
        overhangs[0].raw.contains(" Z"),
        "the height did not ride the bead: {}",
        overhangs[0].raw
    );
    assert!(
        (overhangs[0].z - plane(3)).abs() < 1e-9,
        "the overhang was left off its plane: {}",
        overhangs[0].raw
    );

    let stops: Vec<&str> = out
        .gcode
        .lines()
        .filter(|line| {
            let parsed = Line::parse(line);
            parsed.is_move() && parsed.z.is_some() && !parsed.is_xy_move()
        })
        .filter(|line| line.contains(ZAA_STAMP))
        .collect();
    assert!(
        stops.is_empty(),
        "a height change stopped the toolhead: {stops:?}"
    );
    assert!(steepest(&steps).is_finite());
}

/// A move no printer makes cannot be rasterised, so the cells along its
/// path are never drawn — and an outline with a hole in it is not a
/// smaller outline. Read as one it says the layer above leaves exposed
/// what it really covers, and the surface is then reshaped to a model that
/// is not the part. The layer keeps the heights it was sliced with instead.
#[test]
fn a_layer_whose_outline_cannot_be_followed_keeps_the_heights_it_was_sliced_with() {
    let sliced = cone(6);
    let mut torn = String::new();
    for (index, block) in sliced.split(";LAYER_CHANGE\n").enumerate() {
        if index > 0 {
            torn.push_str(";LAYER_CHANGE\n");
        }
        torn.push_str(block);
        // Block 1 is layer 0, so this lands on layer 3. A coordinate past
        // what a double can hold names no cell at all, and it is put in a
        // region this transform never reshapes, so what is under test is
        // the outline the layer was measured from and not the move itself.
        if index == 4 {
            torn.push_str(
                ";TYPE:Solid infill\n\
                 G1 X0.000 Y0.000 F9000\n\
                 G1 X1e999 Y0.000 E1.00000\n",
            );
        }
    }
    let off_plane = |gcode: &str, layer: usize| {
        steps(gcode)
            .iter()
            .filter(|step| {
                step.layer == layer && step.feature == Feature::TopSurface && step.e > 0.0
            })
            .filter(|step| (step.z - plane(layer)).abs() > 1e-9)
            .count()
    };
    let whole = apply(&sliced, &config()).gcode;
    assert!(
        off_plane(&whole, 3) > 0,
        "the layer is followed where the file can be read"
    );
    let torn = apply(&torn, &config()).gcode;
    assert_eq!(
        off_plane(&torn, 3),
        0,
        "a layer that could not be read must keep its own plane"
    );
    // And it is the reading that stopped, not the transform: the layers
    // measured off outlines with nothing missing are followed as before.
    assert!(
        off_plane(&torn, 1) > 0,
        "nothing was followed at all, so the layer above proves nothing"
    );
}

/// Arcs laid five microns apart across the exposed strip, sweeping the place
/// where the surface passes through its own plane. Each one bows by half a
/// micron, so every sample of it stands the same distance off the plane.
const SWEEP: usize = 200;

fn swept(layers: usize) -> String {
    let mut text = String::from("; layer_height = 0.2\nM83\nG1 Z0.200 F600\n");
    for layer in 0..layers {
        let half = HALF - RUN * layer as f64;
        text.push_str(";LAYER_CHANGE\n");
        text.push_str(&format!("G1 Z{:.3} F600\n", HEIGHT * (layer + 1) as f64));
        ring(&mut text, half);
        text.push_str(";TYPE:Top surface\n");
        // Half a strip in from the outline, less the half bead the outline
        // is traced by, which is where the ramp crosses the plane.
        let crossing = half - (RUN / 2.0 - 0.225);
        for step in 0..SWEEP {
            let x = crossing - 0.5 + 0.005 * step as f64;
            text.push_str(&format!("G1 X{x:.3} Y-0.100 F9000\n"));
            text.push_str(&format!("G2 X{x:.3} Y0.100 I10.000 J0.100 E0.02000\n"));
        }
    }
    text
}

/// A height that reaches the file as the height the bead already has is not
/// a height change. The rise is quantised to a fraction of a layer and then
/// read between cells, so values well under the micron a `Z` word is written
/// to are ordinary wherever the ramp passes through its own plane — and a
/// bead there used to be taken apart and metered again to command exactly
/// where it already was.
///
/// An arc is the expensive case. It cannot carry a height, so it is
/// discarded and rewritten as the straight chords through its samples: a
/// curve the slicer fitted comes out as a run of `G1`s that all round back
/// to the same three decimals in X, Y **and** Z.
#[test]
fn an_arc_whose_rise_rounds_to_nothing_is_left_exactly_as_it_arrived() {
    let source = swept(6);
    let out = apply(&source, &config());
    let steps = steps(&out.gcode);
    let plane = plane(3);

    // One group per arc: the travel before each of them lays nothing, so it
    // closes the group before it.
    let mut groups: Vec<Vec<&Step>> = Vec::new();
    let mut drawing = false;
    for step in &steps {
        let laying = step.layer == 3 && step.feature == Feature::TopSurface && step.e > 0.0;
        match (laying, drawing) {
            (true, true) => groups.last_mut().expect("a group is open").push(step),
            (true, false) => groups.push(vec![step]),
            (false, _) => {}
        }
        drawing = laying;
    }
    assert_eq!(groups.len(), SWEEP, "one group of moves per arc");

    let (mut kept, mut followed) = (0usize, 0usize);
    for group in &groups {
        let moved = group
            .iter()
            .any(|step| (step.z * 1000.0).round() != (plane * 1000.0).round());
        followed += usize::from(moved);
        if group.len() == 1 && group[0].raw.starts_with("G2") {
            assert!(
                source.contains(&group[0].raw),
                "an arc came back altered: {}",
                group[0].raw
            );
            kept += 1;
            continue;
        }
        assert!(
            moved,
            "an arc was rewritten to command the height it already had: {:?}",
            group.iter().map(|step| &step.raw).collect::<Vec<_>>()
        );
    }
    assert!(
        kept > 0,
        "no arc landed where the surface rounds back onto the plane"
    );
    assert!(followed > 0, "and the strip either side of it is followed");
}

/// A radius at or under half of [`SAG`] leaves the chord an arc is sampled by
/// at zero: the length over it is infinite, `as usize` saturates, and the
/// clamp turns a half-micron arc into [`MAX_SAMPLES`] straight moves whose X,
/// Y and Z all round to the same three decimals, every one of them carrying
/// a positive `E`. An arc too small to sample is left exactly as the slicer
/// wrote it — there is nothing to follow across it either, since the rise is
/// blurred over a whole grid cell.
#[test]
fn an_arc_too_small_to_sample_is_written_as_the_slicer_wrote_it() {
    let tiny = |half: f64| {
        [
            format!("G2 X{:.3} Y0.002 I0.000500 J0.000 E0.00100", half - 1.0),
            format!("G3 X{:.3} Y0.002 I0.003000 J0.000 E0.00100", half - 1.0),
        ]
    };
    let mut text = String::from("; layer_height = 0.2\nM83\nG1 Z0.200 F600\n");
    for layer in 0..6 {
        let half = HALF - RUN * layer as f64;
        text.push_str(";LAYER_CHANGE\n");
        text.push_str(&format!("G1 Z{:.3} F600\n", HEIGHT * (layer + 1) as f64));
        ring(&mut text, half);
        surface(&mut text, half, RUN, ";TYPE:Top surface\n");
        for arc in tiny(half) {
            text.push_str(&format!("G1 X{:.3} Y0.000 F9000\n", half - 1.0));
            text.push_str(&arc);
            text.push('\n');
        }
    }
    let out = apply(&text, &config());
    assert!(out.stats.moves > 0, "the strip around them is followed");
    for layer in 0..6 {
        for arc in tiny(HALF - RUN * layer as f64) {
            assert!(out.gcode.contains(&arc), "{arc} was rewritten");
        }
    }
    let (before, after) = (text.lines().count(), out.gcode.lines().count());
    assert!(
        after < before * 3,
        "{after} lines came out of {before}, so an arc was sampled to the cap"
    );
}

/// The last point of a followed arc is the point the file commanded, not the
/// point the `I`/`J` offsets put on the circle. A slicer rounds an arc's end
/// to the micron and it lands a micron or two off its own radius; a
/// hand-edited or lossily re-encoded one lands anywhere. Rebuilding it from
/// the radius leaves the toolhead somewhere the file never asked for, and the
/// move after it then starts from a point the nozzle is not at.
#[test]
fn a_followed_arc_ends_on_the_point_the_file_commanded() {
    let mut text = String::from("; layer_height = 0.2\nM83\nG1 Z0.200 F600\n");
    for layer in 0..6 {
        let half = HALF - RUN * layer as f64;
        text.push_str(";LAYER_CHANGE\n");
        text.push_str(&format!("G1 Z{:.3} F600\n", HEIGHT * (layer + 1) as f64));
        ring(&mut text, half);
        text.push_str(";TYPE:Top surface\n");
        text.push_str(&format!("G1 X{:.3} Y-4.000 F9000\n", half - 0.3));
        // Ends 0.05 mm off the circle its own offsets describe, which is the
        // same fault a slicer's rounding leaves, writ large enough to read.
        text.push_str(&format!(
            "G2 X{:.3} Y4.000 I20.000 J4.000 E0.30000\n",
            half - 0.35
        ));
    }
    let out = apply(&text, &config());
    assert!(out.stats.moves > 0, "the arc was followed");

    let steps = steps(&out.gcode);
    let last = steps
        .iter()
        .rfind(|step| step.layer == 3 && step.feature == Feature::TopSurface && step.e > 0.0)
        .expect("the arc was written");
    let wanted = HALF - RUN * 3.0 - 0.35;
    assert!(
        (last.x - wanted).abs() < 1e-9 && (last.y - 4.0).abs() < 1e-9,
        "the arc was left at {}, {} instead of {wanted}, 4.0: {}",
        last.x,
        last.y,
        last.raw
    );
}

/// A stretch is metered for the material it has to lay, and the material lies
/// between a flat layer below and the tilted top written here: its volume is
/// the bead's width times the gap measured **vertically**, integrated over
/// the ground the stretch covers. So the run it is metered over is the one in
/// the plane, which is what the slicer's own rate is already stated per.
///
/// Metering the longer slanted path the nozzle takes instead would pour in a
/// further `1 / cos` of stock with nowhere to go — nine percent of it at the
/// steepest grade [`Pass::ease`] allows — and would break the identity a
/// stretch that is not re-metered keeps: its pieces sum to what the slicer
/// wrote for it.
#[test]
fn a_stretch_is_metered_for_the_volume_it_fills_not_the_path_it_travels() {
    // What the ironing over the strip is metered at, bead by bead: five
    // along it and two across, which are the ones that change height.
    let mut text = String::from("; layer_height = 0.2\nM83\nG1 Z0.200 F600\n");
    for layer in 0..6 {
        let half = HALF - RUN * layer as f64;
        text.push_str(";LAYER_CHANGE\n");
        text.push_str(&format!("G1 Z{:.3} F600\n", HEIGHT * (layer + 1) as f64));
        ring(&mut text, half);
        surface(&mut text, half, RUN, ";TYPE:Ironing\n");
    }
    let out = apply(&text, &config());
    let sliced = [ALONG_E, ALONG_E, ALONG_E, ALONG_E, ALONG_E, 0.1, 0.1];

    let mut beads: Vec<(f64, f64)> = Vec::new();
    let (mut at, mut drawing) = ((0.0, 0.0, 0.0), false);
    for step in steps(&out.gcode) {
        let laying = step.layer == 3 && step.feature == Feature::Ironing && step.e > 0.0;
        if laying {
            let climb = (step.z - at.2).abs();
            match drawing {
                true => {
                    let bead = beads.last_mut().expect("a bead is open");
                    bead.0 += step.e;
                    bead.1 += climb;
                }
                false => beads.push((step.e, climb)),
            }
        }
        drawing = laying;
        at = (step.x, step.y, step.z);
    }
    assert_eq!(beads.len(), sliced.len(), "one bead per bead sliced");
    for (bead, wrote) in beads.iter().zip(sliced) {
        assert!(
            (bead.0 - wrote).abs() < wrote * 1e-3,
            "a stretch metered at {wrote} came out as {}",
            bead.0
        );
    }
    assert!(
        beads.iter().any(|bead| bead.1 > 0.1),
        "and one of them really did cross the strip: {beads:?}"
    );

    // The re-metered case, where the gap is what changes. A piece is worth
    // the gap over its own ground, so its stock has to fall short of the
    // slant its nozzle travels — by nine percent where the descent into a
    // covered stretch is at its steepest.
    let out = apply(&crossing(6), &config());
    let plane = plane(3);
    let rate = 0.2 / (RUN / 2.0 + 4.0);
    let (mut at, mut checked, mut worst) = ((0.0, 0.0, 0.0), 0usize, 0.0f64);
    for step in steps(&out.gcode) {
        let run = step.length((at.0, at.1));
        let laying = step.layer == 3 && step.feature == Feature::TopSurface && step.e > 0.0;
        // Short pieces are left out: a run of a tenth of a millimetre is
        // written to three decimals either end, so its own length is only
        // known to a percent.
        if laying && run >= 0.15 {
            let middle = (step.z + at.2) / 2.0 - plane;
            let wanted = rate * run * (HEIGHT + middle) / HEIGHT;
            assert!(
                (step.e - wanted).abs() <= wanted * 0.02,
                "a piece {run:.3} mm long over a gap of {:.3} took {} where {wanted} fills it",
                HEIGHT + middle,
                step.e
            );
            checked += 1;
            worst = worst.max((step.z - at.2).abs() / run);
        }
        at = (step.x, step.y, step.z);
    }
    assert!(checked > 0, "some piece was long enough to read");
    assert!(
        worst > 0.25,
        "and one of them steep enough to tell the two rules apart: {worst:.3}"
    );
}

/// What [`simplify`] proves is that *a* straight climb covers every sample it
/// merges. What it then writes is a different line — the chord from the
/// anchor to the last sample that fitted — and that chord's slope can sit a
/// whole corridor away from the slope those samples were tested against. An
/// interior sample is one corridor from that slope and never further along
/// than the sample the chord ends at, so it can land two corridors from the
/// line that is printed: with the corridor kept at the whole tolerance, 10 µm
/// off a move promised to 5.
#[test]
fn a_written_stretch_stays_within_the_tolerance_of_every_sample_it_covers() {
    // The middle sample sits as far above the anchor as a corridor allows,
    // and the last one is back on the anchor's own height a hair further
    // along — so the line that gets written runs flat underneath it.
    let samples: Vec<Sample> = vec![
        (0.0, 0.0, 0.0, 0.0),
        (1.0, 0.0, 1.0, TOLERANCE * 1.98),
        (1.001, 0.0, 1.001, 0.0),
    ];
    let mut keep = Vec::new();
    simplify(&samples, TOLERANCE, f64::INFINITY, &mut keep);

    let (mut anchor, mut worst) = (0usize, 0.0f64);
    for &at in &keep {
        let run = samples[at].2 - samples[anchor].2;
        let rise = samples[at].3 - samples[anchor].3;
        assert!(run > 0.0, "a written move goes somewhere");
        for sample in &samples[anchor..=at] {
            let on_the_line = samples[anchor].3 + rise * (sample.2 - samples[anchor].2) / run;
            worst = worst.max((sample.3 - on_the_line).abs());
        }
        anchor = at;
    }
    assert!(
        worst <= TOLERANCE + 1e-12,
        "a written move ran {worst} mm from a sample it covers, against the \
         {TOLERANCE} mm it promises"
    );
}
