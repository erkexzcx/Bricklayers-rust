use super::*;

fn layer(z: f64) -> String {
    format!(";LAYER_CHANGE\nG1 Z{z:.2} F600\n")
}

fn relative(body: &str) -> String {
    format!("; layer_height = 0.2\nM83\n{body}")
}

/// A file whose middle layer carries `body`, so neither the layers a
/// column climbs over nor the one that caps it applies.
///
/// The same wall runs the whole height of the file, as it does in a real
/// one. Without a copy above, the body would be the last layer holding a
/// wall, which caps it; without copies below, it would be a column that
/// begins out of nowhere, which starts the climb instead. Either way the
/// body measures something other than the steady state. The copies are
/// stripped of their tags so they stay out of [`loop_states`], which does
/// mean a body's loops are counted five times in `Stats::loops`.
fn middle_layer(body: &str) -> String {
    let same = untagged(body);
    relative(&format!(
        "{}{same}{}{same}{}{same}{}{body}{}{same}",
        layer(0.2),
        layer(0.4),
        layer(0.6),
        layer(0.8),
        layer(1.0),
    ))
}

/// `body` with its trailing comments removed, so the same geometry can be
/// emitted twice without the tags being counted twice. Marker lines, which
/// are nothing but a comment, are kept whole.
fn untagged(body: &str) -> String {
    let mut text = String::new();
    for line in body.lines() {
        let kept = match line.trim_start().starts_with(';') {
            true => line,
            false => line.split(';').next().unwrap_or(line).trim_end(),
        };
        text.push_str(kept);
        text.push('\n');
    }
    text
}

/// One wall's internal perimeter loops, the way a slicer emits them:
/// concentric squares printed innermost first, since every mainstream
/// slicer lays the external perimeter down last. Each loop is an extrusion
/// width out from the one before and reached by its own travel, and each
/// one's first extrusion is labelled `<tag><number>` in print order, so
/// the highest number is the loop against the visible wall.
fn wall(loops: usize, tag: &str) -> String {
    wall_of(loops, tag, 0.0, 10.0, 0.5)
}

fn wall_of(loops: usize, tag: &str, origin: f64, size: f64, flow: f64) -> String {
    let mut text = String::new();
    for index in 0..loops {
        let step = 0.45 * (loops - 1 - index) as f64;
        let near = origin + step;
        let far = origin + size - step;
        text.push_str(&format!("G1 X{near:.2} Y{near:.2} F9000\n"));
        text.push_str(&format!(
            "G1 X{far:.2} Y{near:.2} E{flow} ; {tag}{}\n",
            index + 1
        ));
        for (x, y) in [(far, far), (near, far), (near, near)] {
            text.push_str(&format!("G1 X{x:.2} Y{y:.2} E{flow}\n"));
        }
    }
    text
}

fn run(source: &str, config: &Config) -> String {
    apply(source, config).gcode
}

/// The shipped settings with the flow left alone, for a test that measures
/// what the geometry asks for rather than what the multiplier adds to it.
fn plain() -> Config {
    Config {
        wall_flow: Some(1.0),
        ..Config::default()
    }
}

/// A file that states the width its visible wall was metered at, which is
/// what turns a multiplier into a distance to draw that wall in by.
///
/// 0.4 mm at a multiplier of 1.3 gives an offset of exactly 0.06, so the
/// moved coordinates are readable rather than rounded.
fn with_skin_width(body: &str) -> String {
    format!("; external_perimeter_extrusion_width = 0.4\n{body}")
}

fn drawn_in() -> Config {
    Config {
        wall_flow: Some(1.3),
        ..Config::default()
    }
}

/// A 10 mm square of visible wall, printed anticlockwise as a slicer emits
/// an island's boundary.
fn skin() -> String {
    format!(
        ";TYPE:External perimeter\n{}",
        wall_of(1, "skin", 0.0, 10.0, 1.0)
    )
}

/// The same square, stopped `gap` mm short of its own seam, which is what
/// a slicer actually emits so the two ends of a ring do not pile up.
fn seamed_skin(gap: f64) -> String {
    format!(
        ";TYPE:External perimeter\n\
         G1 X0.00 Y0.00 F9000\n\
         G1 X10.00 Y0.00 E1 ; skin1\n\
         G1 X10.00 Y10.00 E1\n\
         G1 X0.00 Y10.00 E1\n\
         G1 X0.00 Y{gap:.3} E1\n"
    )
}

/// Each tagged loop in the output, paired with whether the nozzle was
/// raised when it printed.
fn loop_states(out: &str) -> Vec<(String, bool)> {
    let mut raised = false;
    let mut states = Vec::new();
    for line in out.lines() {
        let Some((body, tag)) = line.rsplit_once("; ") else {
            continue;
        };
        if let Some(note) = tag.strip_prefix(BRICK_STAMP) {
            match note {
                "raised" => raised = true,
                "reset" => raised = false,
                _ => {}
            }
        } else if !body.trim().is_empty() {
            states.push((tag.to_owned(), raised));
        }
    }
    states
}

/// The same file with its layer-change markers taken out, which is what a
/// hand-written file or a machine profile that emits none hands this pass:
/// the layer's own `G1 Z` is all there is to go on.
fn without_layer_markers(source: &str) -> String {
    source
        .lines()
        .filter(|line| !line.starts_with(";LAYER_CHANGE"))
        .map(|line| format!("{line}\n"))
        .collect()
}

/// The same file with the nozzle lifted 2 mm over every travel and put back
/// before the bead that follows it, which is what a slicer emits with Z-hop
/// on. Nothing about the part changes: every bead is laid at exactly the
/// height it was laid at before.
fn with_a_hop_before_every_travel(source: &str) -> String {
    let mut text = String::new();
    let mut plane = None;
    for line in source.lines() {
        let parsed = Line::parse(line);
        if let Some(z) = parsed.z.filter(|_| parsed.is_move()) {
            plane = Some(z);
        }
        let travels =
            parsed.is_move() && parsed.is_xy_move() && parsed.e.is_none() && parsed.z.is_none();
        match plane.filter(|_| travels) {
            Some(plane) => {
                let hop = plane + 2.0;
                text.push_str(&format!("G1 Z{hop:.2} F600\n{line}\nG1 Z{plane:.2} F600\n"));
            }
            None => text.push_str(&format!("{line}\n")),
        }
    }
    text
}

/// Each tagged loop in the output, paired with whether the nozzle stood off
/// the plane its layer was sliced at when it printed.
///
/// [`loop_states`] reads the stamps this pass leaves, and a file with no
/// layer markers does not always leave a closing one: where the layer's own
/// `G1 Z` follows the last raised loop the file takes the nozzle to the next
/// plane itself, so there is nothing to reset and none is written. This reads
/// the height that was commanded instead, against the planes of the file that
/// went in — never against the output, whose heights are what is under test.
fn raised_loops(out: &str, planes: &[f64]) -> Vec<(String, bool)> {
    let mut z = 0.0;
    let mut states = Vec::new();
    for line in out.lines() {
        let parsed = Line::parse(line);
        if let Some(height) = parsed.z.filter(|_| parsed.is_move()) {
            z = height;
        }
        let Some((body, tag)) = line.rsplit_once("; ") else {
            continue;
        };
        if tag.starts_with(BRICK_STAMP) || body.trim().is_empty() {
            continue;
        }
        let on_a_plane = planes.iter().any(|plane| (z - plane).abs() < 1e-9);
        states.push((tag.to_owned(), !on_a_plane));
    }
    states
}

/// Every height this pass raised the nozzle to, in the order it wrote them,
/// however it wrote them: a raise rides whatever move the file offers, which
/// is a travel in one file and a Z-hop's own restore in another.
fn raised_to(out: &str) -> Vec<String> {
    out.lines()
        .filter(|line| line.ends_with("raised"))
        .filter_map(|line| Line::parse(line).z)
        .map(|z| format!("{z:.3}"))
        .collect()
}

#[test]
fn raises_every_other_internal_loop() {
    let source = middle_layer(&format!(";TYPE:Perimeter\n{}", wall(2, "loop")));
    let out = run(&source, &Config::default());
    assert!(
        out.contains("G1 X0.00 Y0.00 F9000 Z0.900 ; corbel brick raised"),
        "{out}"
    );
    assert!(out.contains("G1 Z0.800 F600 ; corbel brick reset"), "{out}");
}

#[test]
fn a_height_change_rides_the_travel_that_reaches_the_loop() {
    // A `G1 Z` of its own names no other axis, so the planner stops the
    // toolhead to run it and the nozzle sits primed over the loop's start
    // point while the axis crawls. Every loop starts at the seam, so an
    // aligned seam stacks the ooze from all of them into one line.
    let source = middle_layer(&format!(";TYPE:Perimeter\n{}", wall(3, "loop")));
    let out = run(&source, &Config::default());
    let halts = |note: &str| {
        out.lines()
            .filter(|line| line.starts_with("G1 Z") && line.ends_with(note))
            .count()
    };
    let ridden = out
        .lines()
        .filter(|line| line.contains(" X") && line.contains(BRICK_STAMP))
        .count();
    assert!(ridden > 0, "no height change rode a travel:\n{out}");
    assert_eq!(
        halts("raised"),
        0,
        "every raise has the travel that reaches its loop to ride:\n{out}"
    );
    // Riding a travel must not move the bead: every loop is still laid at
    // the height it was laid at before.
    assert!(
        out.contains("G1 X0.00 Y0.00 F9000 Z0.900 ; corbel brick raised"),
        "{out}"
    );
    assert!(
        !out.contains("G1 Z0.900"),
        "the raise still stopped the toolhead:\n{out}"
    );
}

#[test]
fn a_height_change_never_rides_a_z_hop_down() {
    // Pulling a hop down to printing height would drag the nozzle through
    // exactly what the slicer lifted it to clear. The restore rides the
    // first extrusion here, so the hop is the last move of the lead and is
    // the one a careless rewrite would land on.
    let source = middle_layer(
        ";TYPE:Perimeter\n\
         G1 X0.45 Y0.45 F9000\n\
         G1 X9.55 Y0.45 E0.5\n\
         G1 X9.55 Y9.55 E0.5\n\
         G1 X0.45 Y9.55 E0.5\n\
         G1 X0.45 Y0.45 E0.5\n\
         G1 X0 Y0 Z1.4 F9000\n\
         G1 X10 Y0 Z0.8 E0.5\n\
         G1 X10 Y10 E0.5\n\
         G1 X0 Y10 E0.5\n\
         G1 X0 Y0 E0.5\n",
    );
    let out = run(&source, &Config::default());
    assert!(
        out.contains("G1 X0 Y0 Z1.4 F9000\n"),
        "the hop was flattened onto the layer:\n{out}"
    );
}

#[test]
fn a_height_change_never_rides_a_travel_a_lift_is_already_holding_up() {
    // A lift is usually a line of its own, and the travel that runs under
    // it names no `Z` at all — it inherits the height. Reading only the
    // candidate's own words finds nothing to object to, accepts the travel
    // and writes the printing height onto it, which pulls the nozzle down
    // through whatever the lift was for and keeps it there for the whole
    // move. The height in force has to be read off the range, not off the
    // line. Nothing restores the lift here, so the layer's own floor stays
    // where the slicer put it.
    let source = middle_layer(
        ";TYPE:Perimeter\n\
         G1 X0.45 Y0.45 F9000\n\
         G1 X9.55 Y0.45 E0.5\n\
         G1 X9.55 Y9.55 E0.5\n\
         G1 X0.45 Y9.55 E0.5\n\
         G1 X0.45 Y0.45 E0.5\n\
         G1 Z1.4 F600\n\
         G1 X0 Y0 F9000 ; to the outer loop\n\
         G1 X10 Y0 E0.5\n\
         G1 X10 Y10 E0.5\n\
         G1 X0 Y10 E0.5\n\
         G1 X0 Y0 E0.5\n",
    );
    let out = run(&source, &Config::default());
    let travel = out
        .lines()
        .find(|line| line.contains("to the outer loop"))
        .unwrap_or_else(|| panic!("the travel was lost:\n{out}"));
    assert!(
        Line::parse(travel).z.is_none(),
        "the travel was dragged down out of the lift: {travel}"
    );
    assert!(
        out.contains("G1 Z1.4 F600\n"),
        "the lift was flattened onto the layer:\n{out}"
    );
    assert!(
        out.contains("Z0.900"),
        "the loop was not raised at all:\n{out}"
    );
}

#[test]
fn a_height_change_rides_a_travel_that_already_carries_a_comment() {
    // Slicers that annotate their moves put a comment on every travel, and
    // refusing those left every loop of such a file with a `G1 Z` of its
    // own, on the seam, primed. The stamp goes in front of what the line
    // already said: a comment is everything past the first `;`, so a stamp
    // appended behind the slicer's note is no longer the start of one and
    // the survey would not see this file had been processed.
    let source = middle_layer(
        ";TYPE:Perimeter\n\
         G1 X0.45 Y0.45 F9000\n\
         G1 X9.55 Y0.45 E0.5\n\
         G1 X9.55 Y9.55 E0.5\n\
         G1 X0.45 Y9.55 E0.5\n\
         G1 X0.45 Y0.45 E0.5\n\
         G1 X0 Y0 F9000 ; travel to the outer loop\n\
         G1 X10 Y0 E0.5\n\
         G1 X10 Y10 E0.5\n\
         G1 X0 Y10 E0.5\n\
         G1 X0 Y0 E0.5\n",
    );
    let out = run(&source, &Config::default());
    let ridden = out
        .lines()
        .find(|line| line.contains("travel to the outer loop"))
        .unwrap_or_else(|| panic!("the slicer's own note was lost:\n{out}"));
    assert!(
        ridden.starts_with("G1 X0 Y0 F9000") && ridden.contains("Z0.900"),
        "the raise did not ride the commented travel:\n{out}"
    );
    let parsed = Line::parse(ridden);
    assert!(
        crate::scan::is_stamp(parsed.comment().unwrap()),
        "the stamp is no longer where a reader looks for it: {ridden}"
    );
    assert!(
        parsed.marker().is_none(),
        "the ridden travel now reads as a region marker: {ridden}"
    );
    assert!(
        !out.lines().any(|line| line.starts_with("G1 Z")
            && line.ends_with(&format!("{BRICK_STAMP}raised"))),
        "the raise still stopped the toolhead:\n{out}"
    );
}

#[test]
fn inserted_z_moves_carry_a_feedrate_and_hand_the_print_speed_back() {
    // A bare `G1 Z` inherits whatever `F` came last, which after a travel
    // slews the Z axis at travel speed. `F` is modal, so the print speed
    // has to be restored before the loop resumes. The lead here ends on a
    // lift, which a raise must never ride down, so the fallback is what
    // runs.
    let source = middle_layer(
        ";TYPE:Perimeter\n\
         G1 X0.45 Y0.45 F9000\n\
         G1 X9.55 Y0.45 E0.5\n\
         G1 X9.55 Y9.55 E0.5\n\
         G1 X0.45 Y9.55 E0.5\n\
         G1 X0.45 Y0.45 E0.5\n\
         G1 X0 Y0 Z1.4 F9000\n\
         G1 F1800\n\
         G1 X10 Y0 E0.5\n\
         G1 X10 Y10 E0.5\n\
         G1 X0 Y10 E0.5\n\
         G1 X0 Y0 E0.5\n",
    );
    let out = run(&source, &Config::default());
    assert!(
        out.contains("G1 Z0.900 F600 ; corbel brick raised"),
        "{out}"
    );
    assert!(out.contains("G1 F1800 ; corbel brick resume"), "{out}");
}

#[test]
fn an_inserted_feedrate_hands_back_the_rate_the_file_asked_for() {
    // Rounding the restore to whole mm/min hands the print a speed it
    // never asked for, and anything under half a unit comes back as `F0`.
    let source = middle_layer(
        ";TYPE:Perimeter\n\
         G1 X0.45 Y0.45 F9000\n\
         G1 X9.55 Y0.45 E0.5\n\
         G1 X9.55 Y9.55 E0.5\n\
         G1 X0.45 Y9.55 E0.5\n\
         G1 X0.45 Y0.45 E0.5\n\
         G1 X0 Y0 Z1.4 F9000\n\
         G1 F1799.5\n\
         G1 X10 Y0 E0.5\n\
         G1 X10 Y10 E0.5\n\
         G1 X0 Y10 E0.5\n\
         G1 X0 Y0 E0.5\n",
    );
    let out = run(&source, &Config::default());
    assert!(
        out.contains("G1 F1799.5 ; corbel brick resume"),
        "the restored feedrate was rounded:\n{out}"
    );
}

/// A slicer drops progress, fan, acceleration, tool and origin codes
/// between the layer's `G1 Z` and the wall that follows it. Ending the
/// held tail on one of those wrote the travel out before the raise could
/// ride it, so the raise fell back to a `G1 Z` of its own — on the loop's
/// start point, primed, which is the seam. Measured on a stock OrcaSlicer
/// file it cost 2 of 132 raises, and 132 of 132 once an `M73` followed
/// every layer's `G1 Z`.
#[test]
fn a_height_change_still_rides_a_travel_across_an_interrupting_command() {
    for interruption in ["M73 P1 R1", "M106 S255", "M204 S500", "T0", "G92 E0"] {
        // The loop's travel has to sit before the interruption, as it does
        // in a real file: the slicer reaches the wall, then declares the
        // region, and the first loop has no move of its own left.
        let loops = wall(3, "loop");
        let (travel, rest) = loops.split_once('\n').expect("a wall opens with a travel");
        let body = format!("{travel}\n{interruption}\n;TYPE:Perimeter\n{rest}");
        let same = untagged(&body);
        let source = relative(&format!(
            "{}{same}{}{same}{}{same}{}{body}{}{same}",
            layer(0.2),
            layer(0.4),
            layer(0.6),
            layer(0.8),
            layer(1.0),
        ));

        let out = run(&source, &Config::default());
        let halts = out
            .lines()
            .filter(|line| line.starts_with("G1 Z") && line.ends_with("raised"))
            .count();
        assert_eq!(halts, 0, "{interruption} cost a raise its carrier:\n{out}");
        assert!(
            out.contains("Z0.900 ; corbel brick raised"),
            "{interruption}: nothing was raised at all:\n{out}"
        );
        assert!(
            out.contains(&format!("\n{interruption}\n")),
            "{interruption} was dropped:\n{out}"
        );
    }
}

/// The same, in absolute mode, where a `G92` also has to keep the stream
/// honest: the origin is read when the line is parsed but only reaches the
/// output when the buffered tail is written, so the two halves move apart.
#[test]
fn a_g92_between_the_layer_and_the_wall_keeps_the_carrier_and_the_origin() {
    let loops = wall_of(3, "loop", 0.0, 10.0, 1.0);
    let (travel, rest) = loops.split_once('\n').expect("a wall opens with a travel");
    let body = format!("{travel}\nG92 E0\n;TYPE:Perimeter\n{rest}");
    let same = untagged(&body);
    let source = format!(
        "; layer_height = 0.2\nM82\n{}{same}{}{same}{}{same}{}{body}{}{same}",
        layer(0.2),
        layer(0.4),
        layer(0.6),
        layer(0.8),
        layer(1.0),
    );

    let out = run(&source, &Config::default());
    let halts = out
        .lines()
        .filter(|line| line.starts_with("G1 Z") && line.ends_with("raised"))
        .count();
    assert_eq!(halts, 0, "the reset cost the raise its carrier:\n{out}");

    // Every absolute value after the last reset is measured from the new
    // zero, so none of them may jump or run backwards.
    let after = out.rsplit_once("\nG92 E0\n").expect("the reset is kept").1;
    let mut position = 0.0;
    for line in after.lines() {
        let parsed = Line::parse(line);
        let Some(e) = parsed.e.filter(|_| parsed.draws()) else {
            continue;
        };
        assert!(
            e >= position && e - position <= 1.5,
            "{line} asks for {} mm in one move:\n{out}",
            e - position
        );
        position = e;
    }
}

/// The same again for `M82`/`M83`, which move the convention rather than
/// the origin. A mode change names no coordinate and no filament, so it is
/// held with the region around it and only reaches the printer when that
/// region is written — but it was being applied the moment it was read,
/// which metered the beads still buffered ahead of it back out in the
/// convention that follows them. A wall standing at 12.5 mm of filament
/// came out asking for 0.5.
#[test]
fn a_mode_change_inside_a_region_reaches_the_beads_in_replay_order() {
    let mut source = String::from("; layer_height = 0.2\nM82\n");
    let mut position = 0.0;
    let mut relative = false;
    for (index, z) in [0.2, 0.4, 0.6, 0.8, 1.0].into_iter().enumerate() {
        let tested = index == 3;
        source.push_str(&layer(z));
        source.push_str(";TYPE:Perimeter\n");
        for (number, near, far) in [(1, 0.45, 9.55), (2, 0.0, 10.0)] {
            // The switch lands between the two loops of the layer under
            // test, so one region holds beads written in both conventions.
            if tested && number == 2 {
                source.push_str("M83\n");
                relative = true;
            }
            source.push_str(&format!("G1 X{near:.2} Y{near:.2} F9000\n"));
            let corners = [(far, near), (far, far), (near, far), (near, near)];
            for (step, (x, y)) in corners.into_iter().enumerate() {
                position += 0.5;
                let word = if relative { 0.5 } else { position };
                let tag = match (tested, step) {
                    (true, 0) => format!(" ; loop{number}"),
                    _ => String::new(),
                };
                source.push_str(&format!("G1 X{x:.2} Y{y:.2} E{word:.4}{tag}\n"));
            }
        }
    }

    let out = run(&source, &plain());
    assert!(
        out.contains("\nM83\n"),
        "the mode change was dropped:\n{out}"
    );
    let before = out
        .lines()
        .find(|line| line.ends_with("; loop1"))
        .unwrap_or_else(|| panic!("the wall ahead of the switch was lost:\n{out}"));
    let after = out
        .lines()
        .find(|line| line.ends_with("; loop2"))
        .unwrap_or_else(|| panic!("the wall behind the switch was lost:\n{out}"));
    assert!(
        Line::parse(before).e.is_some_and(|e| e > 10.0),
        "a bead written before the switch was metered out as a delta: {before}"
    );
    assert!(
        Line::parse(after).e.is_some_and(|e| e < 2.0),
        "a bead written after the switch was metered out as a position: {after}"
    );
}

/// A slicer's custom G-code — a colour change, a timelapse frame, an MMU
/// swap — switches to relative positioning so it can lift and nudge without
/// knowing where the toolhead is, then switches back. It is never a
/// perimeter, so the right answer is to measure it and leave it exactly as
/// it was found.
///
/// Read as absolute, its restoring `G1 Z-2` is a plane at minus two
/// millimetres, the layer's floor follows it down, and every raise on that
/// layer is then written under the bed.
#[test]
fn a_section_in_relative_positioning_is_written_back_exactly_as_it_was_found() {
    let block = concat!(
        "G91\n",
        "G1 Z2.000 F600\n",
        "G1 X1.000 Y1.000 F9000\n",
        "G1 Z-2.000 F600\n",
        "G90\n",
    );
    let custom = format!(";TYPE:Custom\n{block}");
    let body = format!(";TYPE:Perimeter\n{}", wall(2, "loop"));
    let same = untagged(&body);
    let source = relative(&format!(
        "{}{same}{}{same}{}{same}{}{custom}{body}{}{same}",
        layer(0.2),
        layer(0.4),
        layer(0.6),
        layer(0.8),
        layer(1.0),
    ));

    let out = run(&source, &Config::default());
    assert!(out.contains(block), "the block was rewritten:\n{out}");
    for z in raised_to(&out) {
        assert!(
            z.parse::<f64>().expect("a height") > 0.0,
            "a raise to {z} is under the bed:\n{out}"
        );
    }
    // And the layer it sits in is still bricked, against its real plane.
    assert!(
        out.contains("Z0.900 ; corbel brick raised"),
        "the block switched the transform off:\n{out}"
    );
}

#[test]
fn inserted_z_moves_fall_back_when_the_file_never_moves_z_alone() {
    let source = relative(&format!(
        ";LAYER_CHANGE\n;TYPE:Perimeter\nG1 X0 Y0 Z0.2 F9000\n{}\
         ;LAYER_CHANGE\n;TYPE:Perimeter\nG1 X0 Y0 Z0.4 F9000\n{}\
         ;LAYER_CHANGE\n;TYPE:Perimeter\nG1 X0 Y0 Z0.6 F9000\n{}\
         ;LAYER_CHANGE\n;TYPE:Perimeter\nG1 X0 Y0 Z0.8 F9000\n{}\
         ;LAYER_CHANGE\n;TYPE:Solid infill\nG1 X0 Y0 Z1.0 F9000\nG1 X10 Y0 E0.5\n",
        wall(2, "loop"),
        wall(2, "second"),
        wall(2, "third"),
        wall(2, "above")
    ));
    let out = run(&source, &Config::default());
    // Nothing here moves Z on its own, so the closing reset — which has no
    // travel left to ride — falls back to the built-in feedrate.
    assert!(out.contains("G1 Z0.600 F720 ; corbel brick reset"), "{out}");
}

#[test]
fn a_wall_that_starts_partway_up_climbs_from_where_it_starts() {
    // The mirror of a wall that ends: a column beginning on solid infill —
    // the underside of a shelf, the roof of a bridged hole — has no seam
    // under its first bead. Raising that bead by the full offset asks it
    // to span a layer and a half of gap the slicer metered for one, which
    // leaves a void. It has to climb from where it begins, exactly as a
    // column standing on the bed does.
    let body = format!(";TYPE:Perimeter\n{}", wall(2, "loop"));
    let mut source = String::new();
    // Two layers of solid infill, so the wall above them is supported
    // material but stands on no column of its own.
    for z in [0.2, 0.4] {
        source.push_str(&layer(z));
        source.push_str(";TYPE:Solid infill\nG1 X0 Y0 F9000\nG1 X10 Y0 E0.5\n");
    }
    for z in [0.6, 0.8, 1.0, 1.2, 1.4] {
        source.push_str(&layer(z));
        source.push_str(&untagged(&body));
    }
    let out = run(&relative(&source), &plain());
    let raises: Vec<&str> = out
        .lines()
        .filter(|line| line.ends_with("raised"))
        .map(|line| {
            let height = line.find(" Z").expect("a height") + 2;
            let stamp = line.find(" ; ").expect("a stamp");
            &line[height..stamp]
        })
        .collect();
    // Nothing on the layer the column starts at, then a quarter and a half
    // of the layer as it climbs, then the offset it keeps.
    assert_eq!(raises, ["0.850", "1.100", "1.300"], "{out}");
    // The climbing beads are metered for the ground they gained; the one
    // above them spans exactly its own layer.
    assert!(out.contains("E0.62500"), "a climbing bead: {out}");
    assert!(
        out.contains("G1 X10.00 Y0.00 E0.5"),
        "the bead that starts the column is left as sliced: {out}"
    );
}

#[test]
fn a_bead_laid_on_a_raise_is_metered_for_it_even_where_its_own_parity_says_otherwise() {
    // A loop's parity is not its column's history. Numbering runs from the
    // visible wall inward, so a wall that thickens outward — a flaring hull —
    // renumbers, and a loop raised on one layer is laid on the plane on the
    // next, directly over a bead standing half a layer proud. Metered for a
    // whole layer it pours twice what that gap can hold. Measured on a stock
    // 2-wall Benchy, 188 mm of internal wall path was fed at exactly 2.00x,
    // most of it between Z8 and Z15 where the hull flares hardest.
    let thin = format!(";TYPE:Perimeter\n{}", wall_of(2, "thin", 0.0, 10.0, 0.5));
    // The same two loops with a third added outside them, so the loop at the
    // old outer edge keeps its coordinates and changes its number.
    let thick = format!(";TYPE:Perimeter\n{}", wall_of(3, "flip", -0.45, 10.9, 0.5));
    let mut source = String::new();
    for z in [0.2, 0.4, 0.6, 0.8] {
        source.push_str(&layer(z));
        source.push_str(&untagged(&thin));
    }
    source.push_str(&layer(1.0));
    source.push_str(&thick);
    // Two more, so the layer under test is neither the top of the wall nor
    // the one the file caps.
    for z in [1.2, 1.4] {
        source.push_str(&layer(z));
        source.push_str(&untagged(&thick));
    }

    let out = run(&relative(&source), &plain());
    let states = loop_states(&out);
    assert_eq!(
        states,
        [
            ("flip1".to_owned(), true),
            ("flip2".to_owned(), false),
            ("flip3".to_owned(), false),
        ],
        "{out}"
    );
    let flip2 = out
        .lines()
        .find(|line| line.ends_with("; flip2"))
        .expect("the loop that changed number");
    // Its own column stood 0.1 mm proud, so half of this layer is already
    // filled and the bead crosses half a layer, not one.
    assert!(flip2.contains("E0.25000"), "{out}");
    let flip1 = out
        .lines()
        .find(|line| line.ends_with("; flip1"))
        .expect("the loop behind it");
    // The loop behind it was on the plane below and is raised here, so it
    // spans a layer and a half.
    assert!(flip1.contains("E0.75000"), "{out}");
}

#[test]
fn a_wall_that_ends_partway_up_is_capped_while_its_neighbour_carries_on() {
    // A shoulder: one column of wall runs on to the top of the part and
    // another stops here, closed by a surface printed at the next plane.
    // That surface is metered for a whole layer, so a bead left raised
    // under it fills half the gap with twice the material. Measured on the
    // real slice this came from, 293.8 mm of a 399.0 mm top surface sat on
    // a bead 0.1 mm proud.
    let on = wall_of(2, "on", 0.0, 10.0, 0.5);
    let ends = wall_of(2, "end", 20.0, 10.0, 0.5);
    let both = untagged(&format!(";TYPE:Perimeter\n{on}{ends}"));
    let mut source = String::new();
    // Both columns run up from the bed, or the layer under test would be
    // where they begin rather than where one of them ends.
    for z in [0.2, 0.4, 0.6, 0.8] {
        source.push_str(&layer(z));
        source.push_str(&both);
    }
    source.push_str(&layer(1.0));
    source.push_str(&format!(";TYPE:Perimeter\n{on}{ends}"));
    source.push_str(&layer(1.2));
    source.push_str(&format!(";TYPE:Perimeter\n{}", untagged(&on)));
    source.push_str(";TYPE:Solid infill\nG1 X20 Y20 F9000\nG1 X30 Y20 E0.5\nG1 X30 Y30 E0.5\n");
    let source = relative(&source);
    let out = run(&source, &plain());
    assert_eq!(
        loop_states(&out),
        vec![
            ("on1".to_owned(), false),
            ("on2".to_owned(), true),
            ("end1".to_owned(), false),
            ("end2".to_owned(), false),
        ],
        "only the wall that stops is capped: {out}"
    );
    assert!(
        out.contains("E0.25000 ; end2"),
        "and it gives back the half layer its column took: {out}"
    );
}

/// The same shoulder, in a file that never says where its layers begin.
///
/// This is the defect the layout fixes: with no marker to hang a layer on,
/// the survey reported no coverage at all, so nothing was ever capped and
/// every column read as fully aged from its first bead. The wall that stops
/// here stood half a layer proud under the surface that closes it, and that
/// surface was metered for a whole layer.
#[test]
fn a_wall_that_ends_partway_up_is_capped_though_the_file_states_no_layers() {
    let on = wall_of(2, "on", 0.0, 10.0, 0.5);
    let ends = wall_of(2, "end", 20.0, 10.0, 0.5);
    let both = untagged(&format!(";TYPE:Perimeter\n{on}{ends}"));
    let mut source = String::new();
    for z in [0.2, 0.4, 0.6, 0.8] {
        source.push_str(&layer(z));
        source.push_str(&both);
    }
    source.push_str(&layer(1.0));
    source.push_str(&format!(";TYPE:Perimeter\n{on}{ends}"));
    source.push_str(&layer(1.2));
    source.push_str(&format!(";TYPE:Perimeter\n{}", untagged(&on)));
    source.push_str(";TYPE:Solid infill\nG1 X20 Y20 F9000\nG1 X30 Y20 E0.5\nG1 X30 Y30 E0.5\n");
    let source = relative(&source);
    let planes = [0.2, 0.4, 0.6, 0.8, 1.0, 1.2];
    let out = run(&without_layer_markers(&source), &plain());
    assert_eq!(
        raised_loops(&out, &planes),
        vec![
            ("on1".to_owned(), false),
            ("on2".to_owned(), true),
            ("end1".to_owned(), false),
            ("end2".to_owned(), false),
        ],
        "only the wall that stops is capped: {out}"
    );
    assert!(
        out.contains("E0.25000 ; end2"),
        "and it gives back the half layer its column took: {out}"
    );
    // And it is the same part the marked file prints: the two passes lay a
    // file out by the same rule, so the same beads are raised either way.
    let marked = run(&source, &plain());
    assert_eq!(
        raised_loops(&out, &planes),
        raised_loops(&marked, &planes),
        "the markers changed which loops were raised:\n{out}"
    );
    assert_eq!(
        out.lines()
            .filter(|line| line.contains("; on") || line.contains("; end"))
            .collect::<Vec<_>>(),
        marked
            .lines()
            .filter(|line| line.contains("; on") || line.contains("; end"))
            .collect::<Vec<_>>(),
        "the markers changed what a bead was metered for:\n{out}"
    );
}

/// A hop lifts the nozzle and puts it back before anything is extruded
/// again, so a layer rule that read the Z move itself counted every hop as a
/// layer — which walked this pass's layer number away from the survey's and
/// read every column's coverage off the wrong layer. A layer is confirmed by
/// the first bead laid off the plane instead, so hopping changes nothing.
#[test]
fn a_z_hop_opens_no_layer_where_the_file_states_none() {
    let body = format!(";TYPE:Perimeter\n{}", wall(2, "loop"));
    let mut source = String::new();
    for z in [0.2, 0.4, 0.6] {
        source.push_str(&layer(z));
        source.push_str(&untagged(&body));
    }
    source.push_str(&layer(0.8));
    source.push_str(&body);
    source.push_str(&layer(1.0));
    source.push_str(&untagged(&body));
    let source = without_layer_markers(&relative(&source));
    let planes = [0.2, 0.4, 0.6, 0.8, 1.0];

    let flat = run(&source, &plain());
    let hopped = run(&with_a_hop_before_every_travel(&source), &plain());
    assert_eq!(
        raised_loops(&flat, &planes),
        vec![("loop1".to_owned(), false), ("loop2".to_owned(), true)],
        "{flat}"
    );
    assert_eq!(
        raised_loops(&hopped, &planes),
        raised_loops(&flat, &planes),
        "hopping changed which loops were raised:\n{hopped}"
    );
    assert_eq!(
        raised_to(&hopped),
        raised_to(&flat),
        "hopping changed how far a loop was raised:\n{hopped}"
    );
    let beads = |out: &str| -> Vec<String> {
        out.lines()
            .filter(|line| line.contains("; loop"))
            .map(str::to_owned)
            .collect()
    };
    assert_eq!(
        beads(&hopped),
        beads(&flat),
        "hopping changed what a bead was metered for:\n{hopped}"
    );
}

/// Two perimeter regions are deliberately kept in one buffer, since a wall a
/// slicer split across them is one wall and has to be numbered as one. Where
/// the file states no layers, that is exactly where a layer boundary falls,
/// so the buffer has to be settled there too: left alone, the layer above
/// joins the wall below it, is numbered with it, metered against its height,
/// and written out after the `G1 Z` that has already left it behind.
#[test]
fn a_layer_that_ends_inside_a_perimeter_region_is_written_before_the_next_begins() {
    let body = format!(";TYPE:Perimeter\n{}", wall(2, "loop"));
    let mut source = String::new();
    for z in [0.2, 0.4, 0.6] {
        source.push_str(&layer(z));
        source.push_str(&untagged(&body));
    }
    source.push_str(&layer(0.8));
    source.push_str(&body);
    source.push_str(&layer(1.0));
    source.push_str(&untagged(&body));
    let out = run(&without_layer_markers(&relative(&source)), &plain());

    let tagged = out.find("; loop2").expect("the tagged loop was written");
    let next = out.find("G1 Z1.00").expect("the next layer's own Z");
    assert!(
        tagged < next,
        "a layer's loops were written after the layer above it began:\n{out}"
    );
    // And they were raised from their own plane, not from the one above.
    assert!(
        out.contains("Z0.900 ; corbel brick raised"),
        "the raise was measured from the wrong plane:\n{out}"
    );
    assert_eq!(
        raised_loops(&out, &[0.2, 0.4, 0.6, 0.8, 1.0]),
        vec![("loop1".to_owned(), false), ("loop2".to_owned(), true)],
        "{out}"
    );
}

/// And the lines the slicer wrote after that layer's last bead stay behind
/// with the buffer, because they are the lead the next layer's first loop
/// rides.
///
/// A file that states no layers only confirms a boundary at the first bead
/// off the previous plane, by which time the `G1 Z` that reached the new
/// plane and the travel that reaches that bead are already buffered with
/// the layer that is ending. Writing all of it out left that first loop
/// nothing to carry its height, so a raised one fell back to a `G1 Z` of
/// its own — a dead stop on the loop's start point, with the nozzle primed,
/// which is the seam this transform exists to stagger.
#[test]
fn a_layer_a_file_states_no_marker_for_still_has_a_travel_to_raise_on() {
    let source = without_layer_markers(&middle_layer(&format!(
        ";TYPE:Perimeter\n{}",
        wall(3, "loop")
    )));
    let out = run(&source, &plain());
    let halts: Vec<&str> = out
        .lines()
        .filter(|line| line.starts_with("G1 Z") && line.ends_with(&format!("{BRICK_STAMP}raised")))
        .collect();
    assert!(
        halts.is_empty(),
        "{halts:?} stopped the toolhead on a seam:\n{out}"
    );
    // With no external perimeter the alternation starts at the outermost
    // loop, so the first loop the layer prints is a raised one — which is
    // the loop that had nothing left to ride.
    assert_eq!(
        raised_loops(&out, &[0.2, 0.4, 0.6, 0.8, 1.0]),
        vec![
            ("loop1".to_owned(), true),
            ("loop2".to_owned(), false),
            ("loop3".to_owned(), true),
        ],
        "{out}"
    );
}

#[test]
fn the_top_layer_caps_the_wall_flat() {
    // Raising here would stand a bead half a layer proud of the top
    // surface beside it, and meter it for half a gap that is a whole one.
    let body = format!(";TYPE:Perimeter\n{}", wall(2, "loop"));
    let same = untagged(&body);
    let source = relative(&format!(
        "{}{same}{}{same}{}{same}{}{body}{}",
        layer(0.2),
        layer(0.4),
        layer(0.6),
        layer(0.8),
        ";TYPE:Top solid infill\nG1 X40 Y0 E0.5\n"
    ));
    let out = run(&source, &plain());
    assert_eq!(
        loop_states(&out),
        vec![("loop1".to_owned(), false), ("loop2".to_owned(), false)],
        "the top layer must stay on the plane: {out}"
    );
    assert!(
        out.contains("E0.25000 ; loop2"),
        "and still meter the half gap the raised loop below left: {out}"
    );
}

#[test]
fn the_layer_that_tops_a_wall_caps_it_though_the_file_goes_on() {
    // What closes a part is solid infill laid over its walls, so the last
    // wall is a layer or more below the last layer. Measured on six real
    // slices: one to five layers below, so testing the layer count capped
    // nothing at all and left every part's topmost wall standing proud.
    let body = format!(";TYPE:Perimeter\n{}", wall(2, "loop"));
    let same = untagged(&body);
    let source = relative(&format!(
        "{}{same}{}{same}{}{same}{}{body}{}{}",
        layer(0.2),
        layer(0.4),
        layer(0.6),
        layer(0.8),
        layer(1.0),
        ";TYPE:Top solid infill\nG1 X40 Y0 E0.5\n"
    ));
    let out = run(&source, &plain());
    assert_eq!(
        loop_states(&out),
        vec![("loop1".to_owned(), false), ("loop2".to_owned(), false)],
        "the wall's top layer must stay on the plane: {out}"
    );
    assert!(
        out.contains("E0.25000 ; loop2"),
        "and still meter the half gap the raised loop below left: {out}"
    );
}

#[test]
fn tracks_a_layer_z_that_arrives_inside_a_perimeter_region() {
    // Klipper flavour, and Orca with Z-hop off, fold the layer's Z into the
    // travel that reaches the first loop rather than emitting it alone.
    let source = relative(&format!(
        ";LAYER_CHANGE\n;TYPE:Perimeter\nG1 X0 Y0 Z0.2 F9000\n{}\
         ;LAYER_CHANGE\n;TYPE:Perimeter\nG1 X0 Y0 Z0.4 F9000\n{}\
         ;LAYER_CHANGE\n;TYPE:Perimeter\nG1 X0 Y0 Z0.6 F9000\n{}\
         ;LAYER_CHANGE\n;TYPE:Perimeter\nG1 X0 Y0 Z0.8 F9000\n{}\
         ;LAYER_CHANGE\n;TYPE:Solid infill\nG1 X0 Y0 Z1.0 F9000\nG1 X10 Y0 E0.5\n",
        wall(2, "first"),
        wall(2, "second"),
        wall(2, "third"),
        wall(2, "above"),
    ));
    let out = run(&source, &Config::default());
    assert!(
        out.contains("G1 X0.00 Y0.00 F9000 Z0.450 ; corbel brick raised"),
        "{out}"
    );
    assert!(
        out.contains("G1 X0.00 Y0.00 F9000 Z0.700 ; corbel brick raised"),
        "{out}"
    );
    assert!(
        !out.contains("G1 Z0.000"),
        "drove the nozzle into the bed: {out}"
    );
    assert!(
        !out.contains("G1 Z0.100"),
        "shifted off a stale layer Z: {out}"
    );
}

#[test]
fn a_loop_that_does_not_touch_the_last_one_starts_a_new_contour() {
    // One region holding a three-loop wall and then a two-loop hole well
    // away from it. The wall's loops run beside each other and keep the
    // alternation going; the hole touches nothing, so it opens raised
    // despite following an odd loop count.
    let source = middle_layer(&format!(
        ";TYPE:Perimeter\n{}{}",
        wall_of(3, "wall", 0.0, 10.0, 0.5),
        wall_of(2, "hole", 50.0, 4.0, 0.5),
    ));
    let out = run(&source, &Config::default());
    assert_eq!(
        loop_states(&out),
        vec![
            ("wall1".to_owned(), true),
            ("wall2".to_owned(), false),
            ("wall3".to_owned(), true),
            ("hole1".to_owned(), false),
            ("hole2".to_owned(), true),
        ],
        "{out}"
    );
}

#[test]
fn an_open_wall_alternates_though_its_loops_do_not_nest() {
    // Most of a wall is not a closed ring. Where a slicer follows a curved
    // surface the loops are arcs, each one offset sideways from the last
    // and often longer than it, so neither encloses the other. They are
    // still the same wall, and grouping them by what they enclose leaves
    // almost every loop of a real print on its own.
    let mut arcs = String::new();
    for index in 0..4 {
        let x = 0.45 * index as f64;
        let reach = 4.0 + index as f64;
        arcs.push_str(&format!("G1 X{x:.2} Y0 F9000\n"));
        arcs.push_str(&format!("G1 X{x:.2} Y{reach:.2} E0.5 ; arc{}\n", index + 1));
    }
    let source = middle_layer(&format!(";TYPE:Perimeter\n{arcs}"));
    let outcome = apply(&source, &Config::default());
    // Four per layer, on each of the five layers the fixture repeats the
    // wall over so that this one stands on a column and carries one.
    assert_eq!(outcome.stats.loops, 20);
    assert_eq!(
        outcome.stats.raised, 6,
        "one wall, so every other arc: {}",
        outcome.gcode
    );
}

#[test]
fn a_retraction_between_a_wall_s_own_loops_does_not_split_it() {
    // Slicers retract and hop between neighbouring loops of one wall
    // whenever the seams are far apart, which must not read as a new
    // contour: the two loops still have to alternate.
    let source = middle_layer(
        ";TYPE:Perimeter\n\
         G1 X0.45 Y0.45 F9000\n\
         G1 X9.55 Y0.45 E0.5 ; inner\n\
         G1 X9.55 Y9.55 E0.5\n\
         G1 X0.45 Y9.55 E0.5\n\
         G1 X0.45 Y0.45 E0.5\n\
         G1 E-0.8 F2100\n\
         G1 X20 Y20 F9000\n\
         G1 X0 Y0 F9000\n\
         G1 E0.8 F2100\n\
         G1 X10 Y0 E0.5 ; outer\n\
         G1 X10 Y10 E0.5\n\
         G1 X0 Y10 E0.5\n\
         G1 X0 Y0 E0.5\n",
    );
    let config = Config {
        wall_flow: Some(2.0),
        ..Config::default()
    };
    let out = run(&source, &config);
    assert_eq!(
        loop_states(&out),
        vec![("inner".to_owned(), false), ("outer".to_owned(), true)],
        "the retraction must not split the wall: {out}"
    );
}

#[test]
fn a_layer_change_ends_the_region_it_interrupts() {
    // OrcaSlicer opens the next layer's wall with a stray segment before it
    // re-declares the region. Carrying the old region across the layer
    // change would buffer that segment as a perimeter loop of its own, at
    // the new layer's Z.
    let source = relative(&format!(
        ";LAYER_CHANGE\nG1 Z0.20 F600\n;TYPE:Perimeter\n{}\
         ;LAYER_CHANGE\nG1 Z0.40 F600\nG1 X20 Y20 E0.01 ; stray\n\
         ;TYPE:Perimeter\n{}",
        wall(2, "first"),
        wall(2, "second"),
    ));
    let outcome = apply(&source, &Config::default());
    assert!(
        outcome.gcode.contains("G1 X20 Y20 E0.01 ; stray\n"),
        "{}",
        outcome.gcode
    );
    assert_eq!(outcome.stats.loops, 4, "the stray segment is not a loop");
}

#[test]
fn a_loop_that_opens_with_an_arc_is_raised_whole() {
    // Arc fitting replaces a run of short segments with one G2/G3, often at
    // the very start of a loop. Treating that as part of the travel would
    // print it before the nozzle rises, at the height of the loop below.
    let source = middle_layer(
        ";TYPE:Perimeter\n\
         G1 X0.45 Y0.45 F9000\n\
         G1 X9.55 Y0.45 E0.5\n\
         G1 X9.55 Y9.55 E0.5\n\
         G1 X0.45 Y9.55 E0.5\n\
         G1 X0.45 Y0.45 E0.5\n\
         G1 X0 Y0 F9000\n\
         G2 X10 Y0 I5 J1 E0.5 ; outer opens\n\
         G1 X10 Y10 E0.5\n\
         G1 X0 Y10 E0.5\n\
         G1 X0 Y0 E0.5\n",
    );
    let out = run(&source, &Config::default());
    let raise = out
        .find("Z0.900 ; corbel brick raised")
        .expect("the wall is raised");
    let arc = out.find("; outer opens").expect("arc kept");
    assert!(
        raise < arc,
        "the arc opens the loop, so it rises with it:\n{out}"
    );
}

#[test]
fn a_contour_holding_one_loop_is_raised_against_the_wall_that_shows() {
    // A lone loop has no internal neighbour, but it was inset from an
    // external perimeter, so the visible wall is what it staggers against.
    let source = middle_layer(&format!(";TYPE:Perimeter\n{}", wall(1, "lone")));
    let outcome = apply(&source, &Config::default());
    assert_eq!(
        loop_states(&outcome.gcode),
        vec![("lone1".to_owned(), true)]
    );
    // The other four are the fixture's own wall on the layers around it.
    assert_eq!(outcome.stats.loops, 5);
    // The bed layer stays flat and the top one is capped, so the three in
    // between are raised.
    assert_eq!(outcome.stats.raised, 3);
}

#[test]
fn a_solid_wall_three_beads_thick_bricks_its_single_inner_bead() {
    // The case a thin rib produces: the visible wall wraps both faces and
    // one internal loop runs down the middle. Raising it keys the rib to
    // the wall on either side, and the wall itself must stay on the plane.
    let source = middle_layer(
        ";TYPE:Perimeter\n\
         G1 X0.70 Y0.68 F9000\n\
         G1 X39.30 Y0.68 E1.0 ; rib1\n\
         ;TYPE:External perimeter\n\
         G1 X0.22 Y0.22 F9000\n\
         G1 X39.78 Y0.22 E1.0 ; skin1\n\
         G1 X39.78 Y1.13 E0.1\n\
         G1 X0.22 Y1.13 E1.0\n\
         G1 X0.22 Y0.22 E0.1\n",
    );
    let outcome = apply(&source, &Config::default());
    assert_eq!(
        loop_states(&outcome.gcode),
        vec![("rib1".to_owned(), true), ("skin1".to_owned(), false)],
        "{}",
        outcome.gcode
    );
    assert_eq!(outcome.stats.raised, 3);
}

#[test]
fn a_lone_hole_is_bricked_beside_a_wall_in_the_same_region() {
    // Numbering restarts per contour, so a two-loop wall and the
    // single-loop hole beside it are each anchored on their own
    // external-adjacent end.
    let source = middle_layer(&format!(
        ";TYPE:Perimeter\n{}{}",
        wall_of(2, "wall", 0.0, 10.0, 0.5),
        wall_of(1, "hole", 50.0, 4.0, 0.5),
    ));
    let config = Config {
        wall_flow: Some(2.0),
        ..Config::default()
    };
    let outcome = apply(&source, &config);
    assert!(
        outcome.gcode.contains("E1.00000 ; wall2"),
        "{}",
        outcome.gcode
    );
    assert!(
        outcome.gcode.contains("E1.00000 ; hole1"),
        "{}",
        outcome.gcode
    );
    assert_eq!(outcome.stats.loops, 15);
    assert_eq!(outcome.stats.raised, 6);
}

#[test]
fn restores_z_before_leaving_the_perimeter_region() {
    let source = middle_layer(&format!(
        ";TYPE:Perimeter\n{};TYPE:Solid infill\nG1 X40 Y0 E0.5\n",
        wall(2, "loop")
    ));
    let out = run(&source, &Config::default());
    // The fixture repeats the wall on every layer, so anchor on the
    // tagged one rather than on the file's first solid infill.
    let tail = &out[out.find("; loop2").expect("tagged loop kept")..];
    let reset = tail.find("corbel brick reset").expect("reset emitted");
    let infill = tail.find(";TYPE:Solid infill").expect("marker kept");
    assert!(reset < infill, "Z must drop before infill starts:\n{out}");
}

/// A region is buffered whole and settled at the marker that ends it, so a
/// retract-and-lift written after its last bead is read before any of its
/// loops are written out. The plane every raise is measured from is the
/// LOWEST height the layer commands, never the last one: taking the last
/// would hand the hop to every loop of the region and print the lot in air.
#[test]
fn a_lift_after_the_last_bead_is_not_the_plane_the_wall_is_raised_from() {
    let source = middle_layer(&format!(
        ";TYPE:Perimeter\n{}G1 E-0.8 F2400\nG1 Z1.20 F600\n",
        wall(2, "loop")
    ));
    let out = run(&source, &plain());
    assert!(
        out.contains("Z0.900 ; corbel brick raised"),
        "the raise is half a layer over the plane:\n{out}"
    );
    assert!(
        !out.contains("Z1.300"),
        "a loop was raised from the hop instead of the plane:\n{out}"
    );
    // And the lift itself survives. The nozzle is no longer standing at the
    // raise once the slicer has lifted it, so nothing is owed a reset —
    // writing one would pull the nozzle back through what the lift cleared.
    assert!(
        out.contains("G1 Z1.20 F600"),
        "the slicer's own lift was dropped:\n{out}"
    );
    assert!(
        !out.contains("corbel brick reset"),
        "the reset cancelled a lift the slicer wrote:\n{out}"
    );
}

/// What follows a region's last bead lays nothing. A retraction pulls a
/// stated length back and the prime that answers it is written in the next
/// region, at whatever flow that one asks for; scaling one end of the pair
/// and not the other leaves filament in the melt chamber every region, and
/// a capped loop's factor halves the retraction outright.
#[test]
fn the_retraction_that_leaves_a_region_is_not_metered_at_the_wall_flow() {
    let source = middle_layer(&format!(
        ";TYPE:Perimeter\n{}G1 E-0.8 F2400\n",
        wall(2, "loop")
    ));
    let out = run(&source, &Config::default());
    let retractions: Vec<&str> = out
        .lines()
        .filter(|line| line.starts_with("G1 E-"))
        .collect();
    assert!(
        !retractions.is_empty(),
        "the retraction was dropped:\n{out}"
    );
    for line in retractions {
        assert_eq!(
            line, "G1 E-0.8 F2400",
            "the retraction was re-metered:\n{out}"
        );
    }
}

/// An adaptive slice can more than halve the height from one layer to the
/// next, which leaves the bead below standing further proud than this layer
/// is thick. The gap this bead crosses then reads as negative — and a
/// negative factor is not a thin bead but a retraction written mid-wall,
/// which unprimes the nozzle and leaves the extruder measuring from a
/// position it never reached.
#[test]
fn a_layer_thinner_than_the_seam_beneath_it_never_meters_a_bead_backwards() {
    let body = format!(";TYPE:Perimeter\n{}", wall(2, "loop"));
    let same = untagged(&body);
    let mut source = String::from("M83\n");
    for z in [0.5, 1.0, 1.5, 2.0] {
        source.push_str(&layer(z));
        source.push_str(&same);
    }
    // A twelfth of the layer below it, so the seam that layer left standing
    // is taller than the whole of this one.
    source.push_str(&layer(2.15));
    source.push_str(&body);
    source.push_str(&layer(2.3));
    source.push_str(&same);

    let out = run(&source, &Config::default());
    assert!(
        out.contains("corbel brick raised"),
        "the fixture never raised anything to meter:\n{out}"
    );
    let backwards: Vec<&str> = out
        .lines()
        .filter(|line| line.contains(" X") && line.contains(" E-"))
        .collect();
    assert!(
        backwards.is_empty(),
        "a bead was metered backwards: {backwards:?}\n{out}"
    );
}

/// The visible wall is never raised, and a file that states no width for
/// it has nothing to move it by, so it comes through exactly as sliced.
#[test]
fn external_perimeters_are_never_raised() {
    let source = middle_layer(";TYPE:External perimeter\nG1 X10 Y0 E0.5\nG1 X20 Y0 E0.5\n");
    assert!(!run(&source, &Config::default()).contains("corbel"));
}

/// The multiplier is a flow for the hidden walls, not compensation owed to
/// the raise, so the loop left on the plane is scaled beside the one that
/// was raised.
#[test]
fn every_internal_wall_is_metered_at_the_multiplier() {
    let source = middle_layer(&format!(
        ";TYPE:Perimeter\n{}",
        wall_of(2, "loop", 0.0, 10.0, 1.0)
    ));
    let config = Config {
        wall_flow: Some(1.5),
        ..Config::default()
    };
    let out = run(&source, &config);
    assert!(out.contains("E1.50000 ; loop2"), "raised loop:\n{out}");
    assert!(out.contains("E1.50000 ; loop1"), "flat loop:\n{out}");
    assert!(!out.contains("E1 ; loop"), "nothing left as sliced:\n{out}");
}

/// Everything the eye lands on that is not a wall is left alone: the solid
/// surfaces that close the part top and bottom, and the infill between
/// them, are not perimeters and are never rescaled.
#[test]
fn the_surfaces_that_show_are_left_as_sliced() {
    let source = middle_layer(&format!(
        ";TYPE:Top solid infill\nG1 X0 Y-2 F9000\nG1 X10 Y-2 E1.0 ; ceiling\n\
         ;TYPE:Internal infill\nG1 X0 Y-3 F9000\nG1 X10 Y-3 E1.0 ; fill\n\
         ;TYPE:Perimeter\n{}",
        wall_of(2, "loop", 0.0, 10.0, 1.0)
    ));
    let config = Config {
        wall_flow: Some(1.5),
        ..Config::default()
    };
    let out = run(&source, &config);
    for tag in ["ceiling", "fill"] {
        assert!(
            out.contains(&format!("E1.0 ; {tag}")),
            "{tag} moved:\n{out}"
        );
    }
    assert!(out.contains("E1.50000 ; loop1"), "wall scaled:\n{out}");
}

/// A bead on the plate is pressed by the plate rather than by a layer, so
/// surplus flow there has nowhere to go but sideways.
#[test]
fn the_layer_on_the_bed_is_left_as_sliced() {
    let mut source = String::from("; layer_height = 0.2\nM83\n");
    for index in 0..5 {
        source.push_str(&layer(0.2 + f64::from(index) * 0.2));
        source.push_str(";TYPE:Perimeter\n");
        source.push_str(&wall_of(2, &format!("L{index}loop"), 0.0, 10.0, 1.0));
    }
    let config = Config {
        wall_flow: Some(1.5),
        ..Config::default()
    };
    let out = run(&source, &config);
    assert!(out.contains("E1 ; L0loop1"), "bed layer, flat loop:\n{out}");
    assert!(
        out.contains("E1 ; L0loop2"),
        "bed layer, raised loop:\n{out}"
    );
    assert!(
        out.contains("E1.50000 ; L3loop1"),
        "the layers above:\n{out}"
    );
}

/// A file handed no settings at all still gets the shipped slope, and the
/// arithmetic of the raise is unchanged by it.
#[test]
fn the_shipped_default_meters_the_hidden_walls_over() {
    let source = middle_layer(&format!(
        ";TYPE:Perimeter\n{}",
        wall_of(2, "loop", 0.0, 10.0, 1.0)
    ));
    assert_eq!(Config::default().wall_flow, None);
    assert_eq!(Config::default().extra_flow, DEFAULT_EXTRA_FLOW);
    let out = run(&source, &Config::default());
    assert!(out.contains("E1.02500 ; loop1"), "{out}");
    assert!(out.contains("E1.02500 ; loop2"), "{out}");
}

/// The reference profile is the anchor: its layer is half its nozzle, so
/// it takes half of whatever slope is set. A file that states no width is
/// metered as if it were that profile.
#[test]
fn the_reference_profile_takes_half_the_slope() {
    let flow = |width| automatic_flow(0.2, width, DEFAULT_EXTRA_FLOW);
    assert_eq!(flow(Some(0.45)), 1.025);
    assert_eq!(flow(None), 1.025);
    assert_eq!(automatic_flow(0.2, Some(0.45), 0.10), 1.05);
    assert_eq!(automatic_flow(0.2, Some(0.45), 0.0), 1.0);
}

/// The corner two beads leave between them is as tall as the layer and as
/// wide as what is left of the spacing, so the share of a bead sitting in
/// one — and the flow that pays for it — rises with the layer height and
/// falls as the wall widens.
#[test]
fn the_flow_follows_the_layer_height_against_the_wall_width() {
    let round = |value: f64| (value * 1000.0).round() / 1000.0;
    let flow = |height, width| automatic_flow(height, width, DEFAULT_EXTRA_FLOW);
    assert_eq!(round(flow(0.1, Some(0.45))), 1.012);
    assert_eq!(round(flow(0.28, Some(0.45))), 1.037);
    assert_eq!(round(flow(0.2, Some(0.35))), 1.033);
    assert_eq!(round(flow(0.2, Some(0.65))), 1.017);
    // A 0.8 mm nozzle at a fine layer has barely any junction to pay for.
    assert_eq!(round(flow(0.15, Some(0.85))), 1.009);
}

/// Every number reaching the nozzle is held to what a printer can act on,
/// and a settings block is not a trustworthy source of any of them.
#[test]
fn a_width_a_bead_cannot_have_falls_back_to_the_shipped_flow() {
    for impossible in [
        Some(0.0),
        Some(-0.45),
        Some(f64::NAN),
        Some(f64::INFINITY),
        // Narrower than the caps the layer's own height gives the bead, so
        // the spacing works out at zero or less.
        Some(0.04),
        None,
    ] {
        assert_eq!(
            automatic_flow(0.2, impossible, DEFAULT_EXTRA_FLOW),
            1.025,
            "{impossible:?}"
        );
    }
    for impossible in [0.0, -0.2, f64::NAN] {
        assert_eq!(
            automatic_flow(impossible, Some(0.45), DEFAULT_EXTRA_FLOW),
            1.025,
            "{impossible}"
        );
    }
    // A slope that is not a number falls back to the shipped one, and
    // nothing derived may ask for more than a bead's neighbour can take.
    assert_eq!(automatic_flow(0.2, Some(0.45), f64::NAN), 1.025);
    assert_eq!(automatic_flow(0.2, Some(0.45), -1.0), 1.0);
    // A 0.3 mm layer through a 0.1 mm line is beads already laid past one
    // another's centres, so the ceiling floors out and it takes no extra.
    assert_eq!(automatic_flow(0.3, Some(0.1), 1e9), 1.0);
}

/// The ceiling is a guard against a width no slicer would state, not a
/// setting anyone prints against, so pin that it stays out of the way over
/// every geometry a slicer will produce: nozzles from 0.2 to 1.2 mm,
/// widths out to 1.2× the nozzle, and layers from a tenth of it to four
/// fifths. The bead only reaches its neighbour's centre past `h/s` of
/// 1.38, and the widest layer in that sweep is four fifths of a nozzle
/// narrower than its own line.
#[test]
fn the_flow_ceiling_never_binds_on_a_geometry_a_slicer_produces() {
    let (mut span, mut bound) = (0, 0);
    for nozzle in [0.2, 0.25, 0.3, 0.4, 0.5, 0.6, 0.8, 1.0, 1.2] {
        for wide in 0..=20 {
            let width = nozzle * (1.0 + f64::from(wide) / 100.0);
            for thick in 10..=80 {
                let height = nozzle * f64::from(thick) / 100.0;
                let ceiling = flow_ceiling(height, bead_spacing(height, width));
                span += 1;
                bound +=
                    usize::from(automatic_flow(height, Some(width), MAX_EXTRA_FLOW) >= ceiling);
            }
        }
    }
    assert_eq!(bound, 0, "{bound} of {span} reached the ceiling");
    // It is still a real limit. A 0.4 mm layer laid at a 0.37 mm line is
    // a bead nearly as tall as the gap beside it, and the top of the dial
    // is held back to what that gap can take.
    let (height, width) = (0.4, 0.37);
    assert_eq!(
        automatic_flow(height, Some(width), MAX_EXTRA_FLOW),
        flow_ceiling(height, bead_spacing(height, width))
    );
}

/// A pinned flow reaches the nozzle by the same road as a derived one, and
/// was taking none of the same guards on the way: a library caller or a
/// test could put NaN in every `E` word of a file, or ask for a bead wider
/// than the gap beside it, or drop the flow under 1 — which takes material
/// off every wall while [`Pass::skin_offset`] turns negative, so the
/// visible wall is scaled without being moved and the part grows.
#[test]
fn a_pinned_flow_is_held_to_the_same_bounds_as_a_derived_one() {
    let flow_at = |pinned: f64, height: f64, width: Option<f64>| {
        Config {
            wall_flow: Some(pinned),
            ..Config::default()
        }
        .flow_at(height, width)
    };
    for broken in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let flow = flow_at(broken, 0.2, Some(0.45));
        assert_eq!(flow, 1.0, "{broken} reached the nozzle as {flow}");
    }
    // Under 1 is floored, whatever it was asked for.
    assert_eq!(flow_at(0.5, 0.2, Some(0.45)), 1.0);
    assert_eq!(flow_at(-1e9, 0.2, Some(0.45)), 1.0);
    // Over the bead model's own ceiling is held to it, which is the same
    // number a derived flow is held to on the same geometry.
    let (height, width) = (0.4, 0.37);
    let ceiling = flow_ceiling(height, bead_spacing(height, width));
    assert_eq!(flow_at(1e9, height, Some(width)), ceiling);
    assert!(ceiling < FLOW_LIMIT);
    // And a file that states no width to solve a ceiling against is held
    // to the limit every ceiling approaches, rather than to nothing.
    assert_eq!(flow_at(1e9, 0.2, None), FLOW_LIMIT);
    // A flow the geometry allows is passed through untouched, so nothing
    // that already pins one is measuring something new.
    assert_eq!(flow_at(1.3, 0.2, Some(0.45)), 1.3);
    assert_eq!(flow_at(2.0, 0.2, None), 2.0);
}

/// `--extra-flow` names the extra a wall takes where the layer is as thick
/// as the nozzle, and the nozzle deliberately does not appear in
/// [`automatic_flow`]. What the flow is read from is the line width the
/// file states, and a stock profile sets that width at 1.06 to 1.13 times
/// its nozzle — so the spacing already carries the nozzle through, and
/// dividing by a stated one as well would count it twice. Measured over
/// the rows below that halves a 0.8 mm profile and doubles a 0.2 mm one,
/// where the dial's own meaning says every row at the same layer-to-nozzle
/// ratio belongs in the same place.
#[test]
fn the_dial_means_the_same_thing_whatever_the_nozzle() {
    // Nozzle, the line width a stock profile pairs with it, layer height,
    // and the extra the README quotes for that row.
    let profiles = [
        (0.2, 0.22, 0.06, 1.47),
        (0.2, 0.22, 0.10, 2.56),
        (0.4, 0.45, 0.08, 0.94),
        (0.4, 0.45, 0.12, 1.44),
        (0.4, 0.45, 0.16, 1.96),
        (0.4, 0.45, 0.20, 2.50),
        (0.4, 0.45, 0.24, 3.06),
        (0.4, 0.45, 0.28, 3.65),
        (0.6, 0.65, 0.15, 1.24),
        (0.6, 0.65, 0.20, 1.68),
        (0.6, 0.65, 0.30, 2.61),
        (0.8, 0.85, 0.20, 1.26),
        (0.8, 0.85, 0.28, 1.80),
        (0.8, 0.85, 0.40, 2.66),
    ];
    for (nozzle, width, height, quoted) in profiles {
        let extra = (automatic_flow(height, Some(width), DEFAULT_EXTRA_FLOW) - 1.0) * 100.0;
        assert_eq!(
            (extra * 100.0).round() / 100.0,
            quoted,
            "a {nozzle} mm nozzle at {height} mm asks for {extra:.4}%, \
             against the {quoted}% the README states"
        );
        // And the rule of thumb the dial is written around — the extra is
        // the dial times the layer over the nozzle — holds to 7%, which is
        // the tolerance the README states beside those rows.
        let promised = DEFAULT_EXTRA_FLOW * 100.0 * height / nozzle;
        assert!(
            (extra / promised - 1.0).abs() < 0.07,
            "a {nozzle} mm nozzle at {height} mm asks for {extra:.4}%, \
             against the {promised:.4}% the dial promises"
        );
    }
}

/// The width the file states is the one the flow is read from, so a
/// profile that lays wider beads pays less for the same layer.
#[test]
fn the_width_the_file_states_sets_the_flow() {
    let walls = format!(";TYPE:Perimeter\n{}", wall_of(2, "loop", 0.0, 10.0, 1.0));
    let narrow = run(
        &format!("; inner_wall_line_width = 0.35\n{}", middle_layer(&walls)),
        &Config::default(),
    );
    let wide = run(
        &format!(
            "; perimeter_extrusion_width = 0.65\n{}",
            middle_layer(&walls)
        ),
        &Config::default(),
    );
    assert!(narrow.contains("E1.03314 ; loop1"), "{narrow}");
    assert!(wide.contains("E1.01676 ; loop1"), "{wide}");
}

/// The flow is read per layer, not once per file, so a slice whose layers
/// vary meters each of them for the seam it actually has. A thick layer
/// leaves more of each bead in the corner beside it than a thin one.
#[test]
fn an_adaptive_slice_meters_each_layer_at_its_own_flow() {
    let walls = |tag: &str| format!(";TYPE:Perimeter\n{}", wall_of(2, tag, 0.0, 10.0, 1.0));
    let mut source = String::from("; inner_wall_line_width = 0.45\nM83\n");
    // Heights of 0.1 and 0.3 either side of the reference, laid down as
    // planes so the survey measures them rather than reading a nominal.
    for (index, z) in [0.2, 0.3, 0.4, 0.7, 1.0, 1.1].into_iter().enumerate() {
        source.push_str(&layer(z));
        source.push_str(&walls(&format!("L{index}loop")));
    }
    let outcome = apply(&source, &Config::default());
    let out = outcome.gcode;

    assert!(outcome.stats.flow.is_some(), "{:?}", outcome.stats);
    let (low, high) = outcome.stats.flow.expect("walls were metered");
    let asks = |height| automatic_flow(height, Some(0.45), DEFAULT_EXTRA_FLOW);
    assert!(
        (low - asks(0.1)).abs() < 1e-9 && (high - asks(0.3)).abs() < 1e-9,
        "{low} to {high}"
    );
    // Layer 3 is 0.3 mm over a 0.4 mm plane, layer 1 is 0.1 mm over 0.2.
    assert!(out.contains("E1.03959 ; L3loop1"), "thick layer:\n{out}");
    assert!(out.contains("E1.01187 ; L1loop1"), "thin layer:\n{out}");
}

/// The dial names the slope, not the answer: it is the extra a wall takes
/// where the layer is as thick as the nozzle, and the geometry decides
/// where on that line each layer sits.
#[test]
fn the_dial_is_the_extra_a_layer_as_thick_as_the_nozzle_takes() {
    let source = format!(
        "; inner_wall_line_width = 0.45\n{}",
        middle_layer(&format!(
            ";TYPE:Perimeter\n{}",
            wall_of(2, "loop", 0.0, 10.0, 1.0)
        ))
    );
    let flow = |extra: f64| {
        let config = Config {
            extra_flow: extra,
            ..Config::default()
        };
        let out = run(&source, &config);
        let bead = out
            .lines()
            .find(|line| line.ends_with("; loop1"))
            .unwrap_or_else(|| panic!("a hidden wall:\n{out}"))
            .to_owned();
        Line::parse(&bead).e.expect("an E word")
    };
    // A 0.2 mm layer is half of the 0.4 mm nozzle, so it takes half.
    assert!((flow(0.05) - 1.025).abs() < 1e-9, "{}", flow(0.05));
    assert!((flow(0.10) - 1.05).abs() < 1e-9, "{}", flow(0.10));
    assert!((flow(0.02) - 1.01).abs() < 1e-9, "{}", flow(0.02));
    // Zero is the raise and nothing else: metered exactly as sliced.
    assert_eq!(flow(0.0), 1.0);
}

/// An adaptive slice still meters every layer for its own height whatever
/// the slope is set to — the dial tilts the line, it does not replace it
/// with a constant.
#[test]
fn the_dial_keeps_a_layer_metered_for_its_own_height() {
    let walls = |tag: &str| format!(";TYPE:Perimeter\n{}", wall_of(2, tag, 0.0, 10.0, 1.0));
    let mut source = String::from("; inner_wall_line_width = 0.45\nM83\n");
    for (index, z) in [0.2, 0.3, 0.4, 0.7, 1.0, 1.1].into_iter().enumerate() {
        source.push_str(&layer(z));
        source.push_str(&walls(&format!("L{index}loop")));
    }
    let config = Config {
        extra_flow: 0.025,
        ..Config::default()
    };
    let outcome = apply(&source, &config);
    let (low, high) = outcome.stats.flow.expect("walls were metered");
    let asks = |height| automatic_flow(height, Some(0.45), 0.025);
    assert!(
        (low - asks(0.1)).abs() < 1e-9 && (high - asks(0.3)).abs() < 1e-9,
        "{low} to {high}"
    );
    assert!(low < high, "the layers must still differ: {low} to {high}");
}

/// The visible wall is moved by half the width the flow adds, so turning
/// the flow down moves it less. Scaling one without the other would grow
/// or shrink the part.
#[test]
fn the_visible_wall_moves_with_the_dial() {
    let walls = format!(
        ";TYPE:Perimeter\n{}{}",
        wall_of(1, "loop", 0.6, 8.8, 1.0),
        skin()
    );
    let source = format!(
        "; external_perimeter_extrusion_width = 0.4\n; inner_wall_line_width = 0.45\n{}",
        middle_layer(&walls)
    );
    let inset = |extra: f64| {
        let config = Config {
            extra_flow: extra,
            ..Config::default()
        };
        let out = run(&source, &config);
        let bead = out
            .lines()
            .find(|line| line.ends_with("; skin1"))
            .unwrap_or_else(|| panic!("the visible wall:\n{out}"))
            .to_owned();
        Line::parse(&bead).y.expect("a Y")
    };
    // Half of (flow - 1) times the spacing the 0.4 mm wall is laid at,
    // 0.357 at these layers, on the three-decimal grid a coordinate is
    // written to: 0.0045 at the shipped slope, and half of that, 0.0022,
    // lands on 0.002.
    assert_eq!(inset(0.05), 0.004);
    assert_eq!(inset(0.025), 0.002);
    assert_eq!(inset(0.10), 0.009);
    // No extra flow is no extra width, so there is nothing to move.
    assert_eq!(inset(0.0), 0.0);
}

/// Nothing a library caller passes may reach the nozzle as a coordinate it
/// cannot act on, and `f64::clamp` hands NaN straight back. A number past
/// the range is pulled into it, and the flow ceiling holds whatever is
/// left.
#[test]
fn a_slope_a_printer_cannot_act_on_is_ignored() {
    let source = format!(
        "; inner_wall_line_width = 0.45\n{}",
        middle_layer(&format!(
            ";TYPE:Perimeter\n{}",
            wall_of(2, "loop", 0.0, 10.0, 1.0)
        ))
    );
    for impossible in [f64::NAN, f64::INFINITY, -1.0, 1e9] {
        let config = Config {
            extra_flow: impossible,
            ..Config::default()
        };
        let out = run(&source, &config);
        assert!(!out.contains("NaN"), "{impossible}");
        let top = automatic_flow(0.2, Some(0.45), MAX_EXTRA_FLOW);
        let (low, high) = apply(&source, &config).stats.flow.expect("metered");
        assert!(
            (1.0..=top).contains(&low) && (1.0..=top).contains(&high),
            "{impossible} gave {low} to {high}"
        );
    }
}

/// A number on the command line is the answer, whatever the file states,
/// so a print can be tested at a flow the geometry would never pick.
#[test]
fn a_flow_given_on_the_command_line_overrides_the_file() {
    let source = format!(
        "; inner_wall_line_width = 0.35\n{}",
        middle_layer(&format!(
            ";TYPE:Perimeter\n{}",
            wall_of(2, "loop", 0.0, 10.0, 1.0)
        ))
    );
    let config = Config {
        wall_flow: Some(1.1),
        ..Config::default()
    };
    let out = run(&source, &config);
    assert!(out.contains("E1.10000 ; loop1"), "{out}");
}

/// The multiplier is booked apart from the flow the geometry asks for, so
/// a climbing or capped bead books only the percentage and not the step it
/// was already metered for.
#[test]
fn the_multiplier_is_booked_apart_from_the_flow_it_scales() {
    let source = middle_layer(&format!(
        ";TYPE:Perimeter\n{}",
        wall_of(2, "loop", 0.0, 10.0, 1.0)
    ));
    let config = Config {
        wall_flow: Some(1.05),
        ..Config::default()
    };
    let outcome = apply(&source, &config);
    // The fixture lays 40 mm metered for its own geometry, 8 mm of it on
    // the bed, which is never scaled; 5% of the 32 mm above it is 1.6.
    assert!(
        (outcome.stats.multiplier_filament - 1.6).abs() < 1e-9,
        "{:?}",
        outcome.stats
    );
    assert!(
        (outcome.stats.filament - 41.6).abs() < 1e-9,
        "{:?}",
        outcome.stats
    );
}

#[test]
fn a_multiplier_of_one_leaves_every_bead_metered_as_sliced() {
    let source = middle_layer(&format!(
        ";TYPE:Perimeter\n{}",
        wall_of(2, "loop", 0.0, 10.0, 1.0)
    ));
    let outcome = apply(&source, &plain());
    assert_eq!(outcome.stats.multiplier_filament, 0.0);
    assert!(outcome.gcode.contains("E1 ; loop2"), "{}", outcome.gcode);
    assert!(outcome.gcode.contains("E1 ; loop1"), "{}", outcome.gcode);
}

/// The visible wall is brought toward the loop behind it by half of what
/// the multiplier would have added as flow, closing the same volume across
/// the staggered joint without putting more material into the part.
#[test]
fn the_visible_wall_is_drawn_in_toward_the_loop_behind_it() {
    let source = with_skin_width(&middle_layer(&format!(
        ";TYPE:Perimeter\n{}{}",
        wall_of(1, "loop", 0.6, 8.8, 1.0),
        skin()
    )));
    let out = run(&source, &drawn_in());
    // A 0.4 mm wall is laid 0.357 mm from its neighbour at these layers, so
    // 1.3 of the flow widens it by 0.107 and it moves in by half of that,
    // 0.054, on every side. The bead gains 1.3 of the flow it had, over a
    // path 0.9893 of its old length.
    assert!(out.contains("G1 X9.946 Y0.054 E1.28607 ; skin1"), "{out}");
    assert!(out.contains("G1 X9.946 Y9.946 E1.28607"), "{out}");
    assert!(out.contains("G1 X0.054 Y9.946 E1.28607"), "{out}");
    assert!(out.contains("G1 X0.054 Y0.054 E1.28607"), "{out}");
    // The travel that reaches the loop has to land where it now starts.
    assert!(out.contains("G1 X0.054 Y0.054 F9000"), "{out}");
}

/// With Z-hop on, the hop back down is the *last* line of the loop's lead,
/// and it names no `X` or `Y` to be given the moved corner in. Choosing it
/// wrote nothing at all, and the write that did not happen was counted as
/// one that did, so the travel went out as sliced: the ring was drawn in
/// around a nozzle still standing on the seam the slicer chose, and the
/// first bead ran diagonally across the wall to catch up with it.
#[test]
fn a_wall_reached_over_a_hop_is_still_approached_at_the_ring_it_moved_to() {
    let source = with_a_hop_before_every_travel(&with_skin_width(&middle_layer(&format!(
        ";TYPE:Perimeter\n{}{}",
        wall_of(1, "loop", 0.6, 8.8, 1.0),
        skin()
    ))));
    let out = run(&source, &drawn_in());
    let at = out
        .lines()
        .position(|line| line.ends_with("; skin1"))
        .unwrap_or_else(|| panic!("the visible wall:\n{out}"));
    let travel = out
        .lines()
        .take(at)
        .filter(|line| {
            let parsed = Line::parse(line);
            parsed.is_xy_move() && parsed.e.is_none()
        })
        .last()
        .unwrap_or_else(|| panic!("the travel that reaches the visible wall:\n{out}"));
    let (x, y) = Line::parse(travel)
        .xy()
        .unwrap_or_else(|| panic!("a corner on {travel}"));
    assert!(
        (x - 0.054).abs() < 5e-4 && (y - 0.054).abs() < 5e-4,
        "the nozzle is left at {x}, {y} rather than taken to the ring the \
         wall was moved to:\n{out}"
    );
    assert!(out.contains("G1 X9.946 Y0.054 E1.28607 ; skin1"), "{out}");
}

/// A slicer names only the axes that change, so the travel that reaches a
/// loop can name one of them — and one word has nowhere to put half of a
/// new corner. Half-moving the loop is the worst of both: the ring is drawn
/// in while the nozzle stays where the slicer left it, so the first bead is
/// dragged across the wall. The loop is passed through as sliced instead.
#[test]
fn a_wall_whose_approach_cannot_be_moved_is_left_exactly_where_it_was() {
    // The loop behind it ends at 0.60, 0.60, so the travel that reaches the
    // visible wall changes X alone.
    let source = with_skin_width(&middle_layer(&format!(
        ";TYPE:Perimeter\n{}\
         ;TYPE:External perimeter\n\
         G1 X0.15 F9000\n\
         G1 X9.85 Y0.60 E1 ; skin1\n\
         G1 X9.85 Y9.40 E1\n\
         G1 X0.15 Y9.40 E1\n\
         G1 X0.15 Y0.64 E1\n",
        wall_of(1, "loop", 0.6, 8.8, 1.0)
    )));
    let out = run(&source, &drawn_in());
    let sliced = [(9.85, 0.60), (9.85, 9.40), (0.15, 9.40), (0.15, 0.64)];
    let beads: Vec<(f64, f64)> = out
        .lines()
        .skip_while(|line| !line.ends_with("; skin1"))
        .take(sliced.len())
        .map(|line| {
            let parsed = Line::parse(line);
            (parsed.x.expect("an X"), parsed.y.expect("a Y"))
        })
        .collect();
    assert_eq!(beads.len(), sliced.len(), "the visible wall:\n{out}");
    for (bead, corner) in beads.iter().zip(sliced) {
        assert!(
            (bead.0 - corner.0).abs() < 5e-4 && (bead.1 - corner.1).abs() < 5e-4,
            "the wall was drawn in to {bead:?} with nothing in its lead able \
             to take the nozzle there, so it is laid from the corner the \
             slicer chose:\n{out}"
        );
    }
}

/// A bead that runs along one axis names one word, and reading that as a
/// travel cut the wall it belonged to in two. Neither half closed, so the
/// visible wall was scaled without ever being moved — the one failure that
/// grows the part — and the bead itself, stranded in the second half's
/// lead, went out metered for a flow no wall was printed at.
#[test]
fn a_bead_that_names_one_axis_is_a_bead_and_not_a_travel() {
    let source = with_skin_width(&middle_layer(&format!(
        ";TYPE:Perimeter\n{}\
         ;TYPE:External perimeter\n\
         G1 X0.00 Y0.00 F9000\n\
         G1 X10.00 Y0.00 E1 ; skin1\n\
         G1 Y10.00 E1\n\
         G1 X0.00 Y10.00 E1\n\
         G1 X0.00 Y0.04 E1\n",
        wall_of(1, "loop", 0.6, 8.8, 1.0)
    )));
    let out = run(&source, &drawn_in());
    // One closed ring, so the wall is still taken in by the offset the flow
    // asked for, approach and all.
    assert!(out.contains("G1 X0.054 Y0.054 F9000"), "{out}");
    assert!(out.contains("G1 X9.946 Y0.054 E1.28607 ; skin1"), "{out}");
    // And the bead itself is one of the wall's, metered at the wall's flow
    // rather than replayed untouched as part of a travel — and moved along
    // the one axis it names. The axis it does not name it inherits, and that
    // one is already on the ring, because the bead before it was moved onto
    // it: the corner comes out at (9.946, 9.946) with only the `Y` written,
    // and the bead is metered over the shortened edge exactly as the two
    // beside it are.
    assert!(out.contains("G1 Y9.946 E1.28607"), "{out}");
}

/// The offset and the flow are two halves of one answer, so where the flow
/// is derived the offset has to move with it. A file stating a wall width
/// the geometry reads a higher flow off draws the visible wall in further
/// than one stating a width it reads a lower flow off.
#[test]
fn the_visible_wall_follows_the_flow_the_file_asked_for() {
    let walls = format!(
        ";TYPE:Perimeter\n{}{}",
        wall_of(1, "loop", 0.6, 8.8, 1.0),
        skin()
    );
    let inset = |stated: &str| {
        let source = format!(
            "; external_perimeter_extrusion_width = 0.4\n; inner_wall_line_width = {stated}\n{}",
            middle_layer(&walls)
        );
        let out = run(&source, &Config::default());
        let bead = out
            .lines()
            .find(|line| line.ends_with("; skin1"))
            .unwrap_or_else(|| panic!("the visible wall:\n{out}"))
            .to_owned();
        Line::parse(&bead).y.expect("a Y")
    };
    // Half of (flow - 1) times the 0.357 mm spacing the visible wall is
    // laid at: 0.0059 at the narrow width against 0.0030 at the wide one.
    let (narrow, wide) = (inset("0.35"), inset("0.65"));
    assert!((narrow - 0.0059).abs() < 5e-4, "narrow wall at {narrow}");
    assert!((wide - 0.0030).abs() < 5e-4, "wide wall at {wide}");
    assert!(narrow > wide, "{narrow} should sit further in than {wide}");
}

/// A ring does not return exactly to where it started: a slicer stops the
/// last bead short so the two ends do not pile up at the seam. Measured
/// over 308 loops of two real OrcaSlicer files, every one lands 0.0385 to
/// 0.0411 mm short — the `seam_gap` default, a tenth of a 0.4 mm nozzle —
/// and not one closes to the micron this used to demand. On a real file
/// that left the visible wall scaled but never moved, which grows the part
/// by half the width every bead gained.
#[test]
fn a_ring_stopped_short_of_its_seam_is_still_moved() {
    let source = with_skin_width(&middle_layer(&format!(
        ";TYPE:Perimeter\n{}{}",
        wall_of(1, "loop", 0.6, 8.8, 1.0),
        seamed_skin(0.04)
    )));
    let out = run(&source, &Config::default());
    let bead = out
        .lines()
        .find(|line| line.ends_with("; skin1"))
        .unwrap_or_else(|| panic!("the visible wall:\n{out}"));
    // A 0.4 mm wall laid at 0.357 mm spacing, metered at the 1.025 its
    // geometry asks for, moves 0.004 inward.
    let moved = Line::parse(bead).y.expect("a Y");
    assert!(
        (moved - 0.004).abs() < 1e-9,
        "a ring left 0.04 mm short must still be drawn in: {bead}"
    );
}

/// The gap is there so the seam does not get a double bead, so offsetting
/// the ring must not quietly pull it shut. Every vertex the loop was drawn
/// through is offset, the closing one included.
#[test]
fn offsetting_a_ring_leaves_its_seam_gap_where_it_was() {
    let source = with_skin_width(&middle_layer(&format!(
        ";TYPE:Perimeter\n{}{}",
        wall_of(1, "loop", 0.6, 8.8, 1.0),
        seamed_skin(0.04)
    )));
    let out = run(&source, &Config::default());
    let beads: Vec<(f64, f64)> = out
        .lines()
        .skip_while(|line| !line.ends_with("; skin1"))
        .take(4)
        .map(|line| {
            let parsed = Line::parse(line);
            (parsed.x.expect("an X"), parsed.y.expect("a Y"))
        })
        .collect();
    assert_eq!(beads.len(), 4, "the four beads of the wall:\n{out}");
    let closes = beads[3];
    // The loop now starts at (0.004, 0.004) and its last bead has to stop
    // the same 0.04 short of it as the slicer left.
    assert!(
        (closes.0 - 0.004).abs() < 1e-9 && (closes.1 - 0.044).abs() < 1e-9,
        "the seam gap must survive: {closes:?}"
    );
}

/// An open fragment has no inside to move toward. A thin wall breaks into
/// them, and offsetting one drags a visible surface sideways for no
/// reason, so the two ends being a whole bead apart is where a ring stops
/// being a ring.
#[test]
fn an_open_fragment_is_left_exactly_where_it_was() {
    let source = with_skin_width(&middle_layer(&format!(
        ";TYPE:Perimeter\n{}{}",
        wall_of(1, "loop", 0.6, 8.8, 1.0),
        // The stated width is 0.4, and 1 mm apart is well past it.
        seamed_skin(1.0)
    )));
    let out = run(&source, &Config::default());
    let bead = out
        .lines()
        .find(|line| line.ends_with("; skin1"))
        .unwrap_or_else(|| panic!("the visible wall:\n{out}"));
    assert_eq!(
        Line::parse(bead).y.expect("a Y"),
        0.0,
        "an open fragment must not be moved: {bead}"
    );
}

/// A bead widens about its own centre, so a wall moved in by half of what
/// it gains reaches further into the joint while the face the eye lands on
/// does not move at all. The part measures what it was sliced to measure.
#[test]
fn the_visible_wall_keeps_the_dimension_it_was_sliced_to() {
    let source = with_skin_width(&middle_layer(&format!(
        ";TYPE:Perimeter\n{}{}",
        wall_of(1, "loop", 0.6, 8.8, 1.0),
        skin()
    )));
    let out = run(&source, &drawn_in());
    let bead = out
        .lines()
        .find(|line| line.ends_with("; skin1"))
        .expect("the visible wall");
    let shifted = Line::parse(bead).y.expect("a Y");

    let (nominal, flow, height) = (0.4, 1.3, 0.2);
    // The area scales, not the nominal width, so the bead keeps its round
    // caps and gains `flow - 1` of its spacing.
    let widened =
        flow * bead_spacing(height, nominal) + height * (1.0 - std::f64::consts::FRAC_PI_4);
    let outer_face = |centre: f64, width: f64| centre - width / 2.0;
    assert!(
        // The coordinate itself is written on a micron grid.
        (outer_face(shifted, widened) - outer_face(0.0, nominal)).abs() < 5e-4,
        "the wall ran at 0.0 and now runs at {shifted}, and widening it by \
         {} must leave its outer face where it was",
        widened - nominal
    );
}

/// Flow per mm is what the slicer metered plus what the widening asks for,
/// so a path a shade shorter carries proportionally less of it.
#[test]
fn a_wall_drawn_in_carries_the_filament_its_new_width_asks_for() {
    let source = with_skin_width(&middle_layer(&format!(
        ";TYPE:Perimeter\n{}{}",
        wall_of(1, "loop", 0.6, 8.8, 1.0),
        skin()
    )));
    let out = run(&source, &drawn_in());
    // Each side runs 9.893 of the 10 mm it did, at 1.3 the flow.
    assert!(!out.contains("E1.00000 ; skin1"), "{out}");
    assert!(out.contains("E1.28607 ; skin1"), "{out}");
}

/// A hole is emitted clockwise, so the same rule moves its wall out of the
/// hole and into the material around it.
#[test]
fn a_hole_is_opened_up_rather_than_closed() {
    let mut hole = String::from(";TYPE:External perimeter\nG1 X0.00 Y0.00 F9000\n");
    for (x, y) in [(0.0, 10.0), (10.0, 10.0), (10.0, 0.0), (0.0, 0.0)] {
        hole.push_str(&format!("G1 X{x:.2} Y{y:.2} E1.0\n"));
    }
    let source = with_skin_width(&middle_layer(&format!(
        ";TYPE:Perimeter\n{}{hole}",
        wall_of(1, "loop", 0.6, 8.8, 1.0)
    )));
    let out = run(&source, &drawn_in());
    assert!(
        out.contains("G1 X-0.054 Y10.054"),
        "a hole must open, not close:\n{out}"
    );
}

/// A file that states no width still has its visible wall drawn in, on the
/// reference profile the flow already falls back to. Scaling a wall
/// without moving it grows the part by half of what it gained, so the two
/// halves of the change have to fall back together — Cura writes its
/// settings in a form nothing else parses, and this is that file.
#[test]
fn a_file_that_states_no_wall_width_falls_back_to_the_reference_profile() {
    let source = middle_layer(&format!(
        ";TYPE:Perimeter\n{}{}",
        wall_of(1, "loop", 0.6, 8.8, 1.0),
        skin()
    ));
    let out = run(&source, &drawn_in());
    // 0.45 mm at 0.2 mm layers and a flow of 1.3 is an offset of 0.061.
    assert!(out.contains("G1 X9.939 Y0.061"), "{out}");
}

/// Asking for no extra flow asks for no compensation either.
#[test]
fn a_multiplier_of_one_leaves_the_visible_wall_where_it_was() {
    let source = with_skin_width(&middle_layer(&format!(
        ";TYPE:Perimeter\n{}{}",
        wall_of(1, "loop", 0.6, 8.8, 1.0),
        skin()
    )));
    let out = run(&source, &plain());
    assert!(out.contains("G1 X10.00 Y0.00 E1 ; skin1"), "{out}");
}

/// The visible wall takes its place in the alternation instead of being
/// held out of it. Three walls leave both ends of the stack flat and raise
/// the one between them; four raise the far end, so a wall exposed on both
/// faces has one of them raised whenever the count is even.
#[test]
fn the_visible_wall_anchors_an_alternation_that_runs_the_whole_stack() {
    let stack = |walls: usize| {
        let mut body = String::from(";TYPE:Perimeter\n");
        for wall in (1..walls).rev() {
            body.push_str(&wall_of(
                1,
                &format!("in{wall}"),
                0.45 * wall as f64,
                10.0 - 0.9 * wall as f64,
                1.0,
            ));
        }
        body.push_str(";TYPE:External perimeter\n");
        body.push_str(&wall_of(1, "skin", 0.0, 10.0, 1.0));
        let out = run(&middle_layer(&body), &plain());
        loop_states(&out)
    };

    assert_eq!(
        stack(3),
        vec![
            ("in21".to_owned(), false),
            ("in11".to_owned(), true),
            ("skin1".to_owned(), false),
        ],
        "three walls: only the one between the ends"
    );
    assert_eq!(
        stack(4),
        vec![
            ("in31".to_owned(), true),
            ("in21".to_owned(), false),
            ("in11".to_owned(), true),
            ("skin1".to_owned(), false),
        ],
        "four walls: the far end is raised too"
    );
}

/// A wall printed `inner-outer-inner` puts the visible wall between the
/// two halves of the stack, so a loop's place in the buffer is no longer
/// its place in the wall: the innermost loop is printed immediately after
/// the visible one while sitting a whole stack away from it. Numbering by
/// buffer position leaves it and its neighbour on the same level, which is
/// the one thing bricking exists to prevent.
#[test]
fn a_wall_printed_inner_outer_inner_still_alternates_by_geometry() {
    let ring =
        |wall: usize, tag: &str| wall_of(1, tag, 0.45 * wall as f64, 10.0 - 0.9 * wall as f64, 1.0);
    // Four walls, the visible one third out of the nozzle.
    let body = format!(
        ";TYPE:Perimeter\n{}{}\
         ;TYPE:External perimeter\n{}\
         ;TYPE:Perimeter\n{}",
        ring(2, "in2"),
        ring(1, "in1"),
        ring(0, "skin"),
        ring(3, "in3"),
    );
    let states = loop_states(&run(&middle_layer(&body), &plain()));

    assert_eq!(
        states,
        vec![
            ("in21".to_owned(), false),
            ("in11".to_owned(), true),
            ("skin1".to_owned(), false),
            ("in31".to_owned(), true),
        ],
        "reading outwards that is skin flat, in1 raised, in2 flat, in3 raised"
    );
}

/// The loop printed after the visible wall in `inner-outer-inner` is the
/// innermost one, which on a thick wall runs further from it than any two
/// neighbours ever do. Grouping only against the loop printed before would
/// split it off into a contour of its own and number it from scratch.
#[test]
fn a_thick_wall_stays_one_contour_however_its_loops_were_sequenced() {
    let ring =
        |wall: usize, tag: &str| wall_of(1, tag, 0.45 * wall as f64, 12.0 - 0.9 * wall as f64, 1.0);
    let mut body = String::from(";TYPE:Perimeter\n");
    for wall in [4usize, 3, 2, 1] {
        body.push_str(&ring(wall, &format!("in{wall}")));
    }
    body.push_str(";TYPE:External perimeter\n");
    body.push_str(&ring(0, "skin"));
    body.push_str(";TYPE:Perimeter\n");
    // 2.25 mm from the visible wall, well past `MAX_LOOP_GAP`.
    body.push_str(&ring(5, "in5"));
    let states = loop_states(&run(&middle_layer(&body), &plain()));

    assert_eq!(
        states,
        vec![
            ("in41".to_owned(), false),
            ("in31".to_owned(), true),
            ("in21".to_owned(), false),
            ("in11".to_owned(), true),
            ("skin1".to_owned(), false),
            ("in51".to_owned(), true),
        ],
        "the innermost loop belongs to the wall, not to a contour of its own"
    );
}

/// One wall shows one visible loop, so a second one is a second wall
/// however close it runs. Grouping purely by "runs beside anything already
/// in this contour" chains a part's islands together as each joined loop
/// widens the contour's reach: measured on a 2-wall Benchy, 61 contours
/// held two walls and one held nine, and every wall after the first was
/// numbered from the first wall's visible loop.
#[test]
fn each_visible_wall_opens_a_contour_of_its_own() {
    // Three islands a millimetre apart, closer than two loops of one wall.
    let mut body = String::new();
    for (island, origin) in [(1, 0.0), (2, 11.0), (3, 22.0)] {
        body.push_str(";TYPE:Perimeter\n");
        body.push_str(&wall_of(1, &format!("in{island}"), origin + 0.45, 9.1, 1.0));
        body.push_str(";TYPE:External perimeter\n");
        body.push_str(&wall_of(1, &format!("out{island}"), origin, 10.0, 1.0));
    }
    let states = loop_states(&run(&middle_layer(&body), &plain()));

    assert_eq!(
        states,
        vec![
            ("in11".to_owned(), true),
            ("out11".to_owned(), false),
            ("in21".to_owned(), true),
            ("out21".to_owned(), false),
            ("in31".to_owned(), true),
            ("out31".to_owned(), false),
        ],
        "every island must be numbered from its own visible wall"
    );
}

/// An arc moves with the rest of the loop: it keeps the centre it was
/// drawn about, its radius changes by the offset, and the `I`/`J` that
/// name the centre from its start point are restated because that start
/// point moved. Without the restatement the printer sweeps the old centre
/// and the bead spirals away from the loop it belongs to.
#[test]
fn a_visible_wall_drawn_with_an_arc_moves_with_its_centre() {
    let arc = ";TYPE:External perimeter\n\
         G1 X0.00 Y0.00 F9000\n\
         G1 X10.00 Y0.00 E1.0 ; arcskin\n\
         G2 X10.00 Y10.00 I0 J5 E1.0\n\
         G1 X0.00 Y10.00 E1.0\n\
         G1 X0.00 Y0.00 E1.0\n";
    let source = with_skin_width(&middle_layer(&format!(
        ";TYPE:Perimeter\n{}{arc}",
        wall_of(1, "loop", 0.6, 8.8, 1.0)
    )));
    let out = run(&source, &drawn_in());
    // The arc turns clockwise, so the material is on the far side of its
    // centre and the offset takes the radius from 5 out to 5.054.
    assert!(
        out.contains("G2 X10.000 Y10.054 I0.000 J5.054"),
        "the arc must be redrawn about the centre it kept: {out}"
    );
    // Start (10.000, -0.054) plus J puts the centre back at (10, 5).
    assert!(
        out.contains("G1 X10.000 Y-0.054 E1.29311 ; arcskin"),
        "{out}"
    );
}

/// An open fragment has no inside, so there is no direction to move it in.
#[test]
fn an_open_run_of_visible_wall_is_not_moved() {
    let open = ";TYPE:External perimeter\n\
         G1 X0.00 Y0.00 F9000\n\
         G1 X10.00 Y0.00 E1.0 ; open\n\
         G1 X10.00 Y10.00 E1.0\n";
    let source = with_skin_width(&middle_layer(&format!(
        ";TYPE:Perimeter\n{}{open}",
        wall_of(1, "loop", 0.6, 8.8, 1.0)
    )));
    let out = run(&source, &drawn_in());
    assert!(out.contains("G1 X10.00 Y0.00 E1.30000 ; open"), "{out}");
}

/// The half layer a column is displaced by is paid over two layers rather
/// than in one bead, and given back in one when the column is capped.
#[test]
fn a_column_climbs_to_its_offset_instead_of_jumping() {
    let mut source = String::from("; layer_height = 0.2\nM83\n");
    for index in 0..5 {
        source.push_str(&layer(0.2 + f64::from(index) * 0.2));
        source.push_str(";TYPE:Perimeter\n");
        source.push_str(&wall_of(2, &format!("L{index}loop"), 0.0, 10.0, 1.0));
    }
    let out = run(&source, &plain());
    let flow = |tag: &str| {
        out.lines()
            .find(|line| line.ends_with(tag))
            .map(|line| Line::parse(line).e.expect("an extrusion"))
            .unwrap_or_else(|| panic!("{tag} missing from:\n{out}"))
    };
    // Flat on the bed, a quarter of a layer taller on each of the two
    // climbing layers, as sliced once the column is up, and half a layer
    // short where the cap gives the climb back.
    assert_eq!(flow("L0loop2"), 1.0, "bed layer");
    assert_eq!(flow("L1loop2"), 1.25, "first climb");
    assert_eq!(flow("L2loop2"), 1.25, "second climb");
    assert_eq!(flow("L3loop2"), 1.0, "column up");
    assert_eq!(flow("L4loop2"), 0.5, "cap");
    assert!(
        out.contains("Z0.450 ; corbel brick raised"),
        "half the shift on the first climb:\n{out}"
    );
    assert!(
        out.contains("Z0.700 ; corbel brick raised"),
        "full shift on the second:\n{out}"
    );
}

/// A bead on the bed is pressed against the build plate rather than
/// against a layer, so raising it presses nothing and the extra flow it
/// would need spreads sideways. There is no seam under it to stagger
/// either. On a Benchy this filled in the bottom nameplate, which is one
/// layer deep.
#[test]
fn the_layer_laid_on_the_bed_is_never_raised() {
    let mut source = String::from("; layer_height = 0.2\nM83\n");
    for index in 0..4 {
        source.push_str(&layer(0.2 + f64::from(index) * 0.2));
        source.push_str(";TYPE:Perimeter\n");
        source.push_str(&wall_of(2, &format!("L{index}loop"), 0.0, 10.0, 1.0));
    }
    let outcome = apply(&source, &Config::default());
    let bed: Vec<&str> = outcome
        .gcode
        .lines()
        .take_while(|line| !line.contains("L1loop"))
        .collect();
    assert!(
        !bed.iter().any(|line| line.contains("raised")),
        "nothing may be raised on the bed layer:\n{}",
        bed.join("\n")
    );
    assert!(
        bed.iter().all(|line| !line.contains("E1.5")),
        "nor over-extruded:\n{}",
        bed.join("\n")
    );
}

/// The cap gives back exactly what the column climbed, which is less than
/// half a layer when the wall ended before it finished climbing.
#[test]
fn a_cap_gives_back_only_the_climb_the_column_took() {
    let mut source = String::from("; layer_height = 0.2\nM83\n");
    for index in 0..3 {
        source.push_str(&layer(0.2 + f64::from(index) * 0.2));
        source.push_str(";TYPE:Perimeter\n");
        source.push_str(&wall_of(2, &format!("L{index}loop"), 0.0, 10.0, 1.0));
    }
    let out = run(&source, &plain());
    let flow = |tag: &str| {
        out.lines()
            .find(|line| line.ends_with(tag))
            .map(|line| Line::parse(line).e.expect("an extrusion"))
            .unwrap_or_else(|| panic!("{tag} missing from:\n{out}"))
    };
    assert_eq!(flow("L1loop2"), 1.25, "climbed a quarter of a layer");
    assert_eq!(flow("L2loop2"), 0.75, "so the cap gives a quarter back");
}

/// A wall that stands for two layers never climbs, so it is left exactly
/// as the slicer wrote it. Embossed text and other one- or two-layer
/// detail lands here.
#[test]
fn a_wall_too_short_to_climb_is_left_alone() {
    let wall = format!(";TYPE:Perimeter\n{}", wall_of(2, "loop", 0.0, 10.0, 1.0));
    let source = relative(&format!("{}{wall}{}{wall}", layer(0.2), layer(0.4)));
    let out = run(&source, &Config::default());
    assert!(!out.contains("raised"), "{out}");
    assert!(!out.contains("E1.25000"), "{out}");
}

/// A file whose slicer varied the layer height, with `body` on a layer
/// half as deep as the rest of them.
///
/// The layer under test runs 0.6 to 0.7 while every other one is 0.2, so a
/// raise taken from its own height cannot be confused with one taken from
/// the 0.2 the file declares. It sits three layers above the bed, clear of
/// the [`RAMP`], and carries a wall above it for the reason
/// [`middle_layer`] does.
fn varied_layers(body: &str) -> String {
    let same = untagged(body);
    let wall = ";TYPE:Perimeter\n\
         G1 X0 Y0 F9000\n\
         G1 X10 Y0 E0.5\n\
         G1 X10 Y10 E0.5\n\
         G1 X0 Y10 E0.5\n\
         G1 X0 Y0 E0.5\n";
    relative(&format!(
        "{}{wall}{}{same}{}{same}{}{body}{}{wall}",
        layer(0.2),
        layer(0.4),
        layer(0.6),
        layer(0.7),
        layer(0.9),
    ))
}

/// Half of one layer height for the whole file staggers every layer that
/// is not that height by the wrong amount, and an adaptive slice has
/// almost none that are. Measured on a real Benchy sliced adaptively: the
/// layers ran 0.081 to 0.119 mm against a declared 0.2, so 383 of 511 were
/// lifted further than their own height and stood clear of the layer above
/// with a gap underneath.
#[test]
fn a_raise_is_half_of_the_layer_it_belongs_to() {
    let source = varied_layers(&format!(";TYPE:Perimeter\n{}", wall(2, "loop")));
    let out = run(&source, &Config::default());
    assert!(
        out.contains("Z0.750 ; corbel brick raised"),
        "the layer is 0.1 deep, so it takes 0.05:\n{out}"
    );
    assert!(
        !out.contains("Z0.800 ; corbel brick raised"),
        "half the declared 0.2 is a whole layer here:\n{out}"
    );
}

/// A raised bead starts on top of whatever its own column left on the
/// layer below, so where the layer thins the column has already filled
/// part of it and the bead is metered for the gap that is left.
#[test]
fn a_bead_is_metered_for_the_gap_its_own_column_left() {
    let source = varied_layers(&format!(";TYPE:Perimeter\n{}", wall(2, "loop")));
    let out = run(&source, &plain());
    let raised = out
        .lines()
        .find(|line| line.ends_with("loop2"))
        .map(|line| Line::parse(line).e.expect("an extrusion"))
        .unwrap_or_else(|| panic!("loop2 missing from:\n{out}"));
    // The column below stands 0.1 above the 0.6 plane and the nozzle is at
    // 0.75, so the bead spans 0.05 of a layer metered for 0.1.
    assert_eq!(raised, 0.25, "half the flow of a 0.5 bead:\n{out}");
}

/// The region buffer and the loop list are reused between regions, so a
/// second wall in the same layer has to be grouped and numbered from
/// scratch rather than from whatever the first left.
#[test]
fn a_second_region_in_a_layer_is_grouped_on_its_own() {
    let source = middle_layer(&format!(
        ";TYPE:Perimeter\n{}\
         ;TYPE:External perimeter\nG1 X0 Y0 F9000\nG1 X10 Y0 E0.5\n\
         ;TYPE:Perimeter\n{}",
        wall_of(2, "near", 0.0, 10.0, 0.5),
        wall_of(3, "far", 40.0, 10.0, 0.5),
    ));
    let out = run(&source, &Config::default());

    // Numbering runs outwards from the loop against the visible wall,
    // which each slicer prints last, so the raise alternates from there.
    assert_eq!(
        loop_states(&out),
        [
            ("near1".to_owned(), false),
            ("near2".to_owned(), true),
            ("far1".to_owned(), true),
            ("far2".to_owned(), false),
            ("far3".to_owned(), true),
        ],
        "{out}"
    );
}

/// Slicers scatter their own annotations through a wall. They are not
/// region markers, so they must neither end the region nor be dropped.
#[test]
fn a_slicer_annotation_inside_a_wall_is_replayed_in_place() {
    let source = middle_layer(&format!(
        ";TYPE:Perimeter\n; LINE_WIDTH: 0.42\n{}",
        wall(2, "loop")
    ));
    let out = run(&source, &Config::default());

    assert!(
        out.contains("; LINE_WIDTH: 0.42"),
        "annotation kept:\n{out}"
    );
    assert_eq!(
        loop_states(&out)
            .into_iter()
            .filter(|(tag, _)| tag.starts_with("loop"))
            .collect::<Vec<_>>(),
        [("loop1".to_owned(), false), ("loop2".to_owned(), true)],
        "the annotation must not split the wall:\n{out}"
    );
}

#[test]
fn absolute_extrusion_stays_continuous() {
    let body = ";TYPE:Perimeter\n\
         G1 X0.45 Y0.45 F9000\n\
         G1 X9.55 Y0.45 E{a} ; loop1\n\
         G1 X9.55 Y9.55 E{b}\n\
         G1 X0.45 Y9.55 E{c}\n\
         G1 X0.45 Y0.45 E{d}\n\
         G1 X0 Y0 F9000\n\
         G1 X10 Y0 E{e} ; loop2\n\
         G1 X10 Y10 E{f}\n\
         G1 X0 Y10 E{g}\n\
         G1 X0 Y0 E{h}\n";
    // One absolute stream climbing by 1 mm a move, over a wall that runs
    // the height of the file so the layer under test is neither where its
    // column starts nor where it ends.
    let mut source = String::from("; layer_height = 0.2\nM82\n");
    let mut e = 0.0;
    let next = |e: &mut f64| {
        let mut text = body.to_string();
        for key in ["{a}", "{b}", "{c}", "{d}", "{e}", "{f}", "{g}", "{h}"] {
            *e += 1.0;
            text = text.replacen(key, &format!("{e:.1}"), 1);
        }
        text
    };
    // Only the layer under test keeps its tags, so the copies that give
    // its column something to stand on cannot be matched instead.
    for z in [0.2, 0.4, 0.6, 0.8] {
        source.push_str(&layer(z));
        source.push_str(&untagged(&next(&mut e)));
    }
    source.push_str(&layer(1.0));
    source.push_str(&next(&mut e));
    e += 1.0;
    source.push_str(&format!(";TYPE:Solid infill\nG1 X30 Y0 E{e:.1}\n"));
    source.push_str(&layer(1.2));
    source.push_str(&untagged(&next(&mut e)));

    let config = Config {
        wall_flow: Some(2.0),
        ..Config::default()
    };
    let out = run(&source, &config);

    // Read the stream back as per-move deltas, which is what the machine
    // acts on and what has to stay right however much the rescale shifted
    // the absolute values.
    let mut last = 0.0;
    let mut moves = Vec::new();
    for line in out.lines() {
        let parsed = Line::parse(line);
        if !parsed.draws() {
            continue;
        }
        if let Some(value) = parsed.e {
            moves.push((line.to_owned(), value - last));
            last = value;
        }
    }
    assert!(
        moves.iter().all(|(_, delta)| *delta > 0.0),
        "the extruder never runs backwards: {out}"
    );
    let delta = |tag: &str| {
        moves
            .iter()
            .find(|(line, _)| line.ends_with(tag))
            .map(|(_, delta)| *delta)
            .unwrap_or_else(|| panic!("{tag} missing from:\n{out}"))
    };
    assert_eq!(delta("; loop1"), 2.0, "the loop on the plane is doubled");
    assert_eq!(delta("; loop2"), 2.0, "and so is the raised one");
    assert!(
        out.contains("G1 X30 Y0 E"),
        "the infill after it is kept: {out}"
    );
}

/// Whether a line has to be written again is whether the value it should
/// carry differs from the one it already has. Asking a global drift flag
/// instead is wrong inside a buffered region: the region is read to its
/// end before any of it is emitted, so the input position sits ahead of
/// the output and the two coincide by accident every so often. The line
/// where they met came out carrying its original, now stale, absolute
/// value — on a Cura-flavoured file the extruder ran 0.6 mm backwards
/// mid-wall and then asked for a double-length move to catch up.
#[test]
fn an_absolute_stream_never_runs_backwards() {
    // The first layer's raised bead is metered thicker, which shifts every
    // value after it and sets up the coincidence.
    let mut source = String::from("; layer_height = 0.2\nM82\nG92 E0\n");
    let mut e = 0.0;
    for index in 0..6 {
        source.push_str(&layer(0.2 + f64::from(index) * 0.2));
        source.push_str(";TYPE:Perimeter\n");
        for inset in [0.9_f64, 0.45] {
            source.push_str(&format!("G1 X{inset:.2} Y{inset:.2} F9000\n"));
            let far = 20.0 - inset;
            for (x, y) in [(far, inset), (far, far), (inset, far), (inset, inset)] {
                e += 0.6;
                source.push_str(&format!("G1 X{x:.3} Y{y:.3} E{e:.5}\n"));
            }
        }
        source.push_str(";TYPE:Solid infill\nG1 X2 Y2 F9000\n");
        e += 1.2;
        source.push_str(&format!("G1 X18 Y18 E{e:.5}\n"));
    }
    let out = run(&source, &Config::default());

    let mut position = 0.0;
    let mut moves = 0;
    for line in out.lines() {
        let parsed = Line::parse(line);
        let Some(value) = parsed.e.filter(|_| parsed.draws()) else {
            continue;
        };
        moves += 1;
        assert!(
            value >= position,
            "{line} pulls the filament back from {position}:\n{out}"
        );
        assert!(
            value - position <= 1.5,
            "{line} asks for {} mm in one move:\n{out}",
            value - position
        );
        position = value;
    }
    assert!(moves > 50, "expected the whole file to be checked");
}

#[test]
fn numbering_follows_the_wall_order_the_slicer_used() {
    // Printed the other way round, the loop against the visible wall is
    // the first one out of the nozzle rather than the last.
    let source = middle_layer(&format!(";TYPE:Perimeter\n{}", wall(2, "loop")));
    let config = Config {
        external_perimeters_first: true,
        ..Config::default()
    };
    let out = run(&source, &config);
    assert_eq!(
        loop_states(&out),
        vec![("loop1".to_owned(), true), ("loop2".to_owned(), false)],
        "{out}"
    );
}

/// A layer height that is not a length would become half a shift and drive
/// the nozzle down into the layer below, or write `ZNaN` into the file.
/// Every source of one is filtered, this included.
#[test]
fn a_layer_height_that_is_not_a_length_is_ignored() {
    let source = middle_layer(&format!(";TYPE:Perimeter\n{}", wall(2, "loop")));
    let sane = run(&source, &Config::default());

    for height in [0.0, -0.4, f64::NAN, f64::INFINITY] {
        let config = Config {
            layer_height: Some(height),
            ..Config::default()
        };
        let outcome = apply(&source, &config);
        assert_eq!(
            outcome.gcode, sane,
            "--layer-height {height} should fall back to the file's own"
        );
        for line in outcome.gcode.lines() {
            if let Some(z) = Line::parse(line).z {
                assert!(z.is_finite() && z >= 0.0, "{line}");
            }
        }
    }
}

/// Cura resets the extruder origin periodically to keep `E` from growing
/// without bound, and the reset can land inside a wall. The region's own
/// moves have not been metered out when it arrives, so replaying them
/// after it measured their absolute positions from the wrong zero: the
/// first move after `G92 E0` asked for 2.5 mm of filament in one go.
#[test]
fn a_g92_inside_a_wall_keeps_the_absolute_stream_honest() {
    let source = format!(
        "; layer_height = 0.2\nM82\n{}{}{};TYPE:Perimeter\n\
         G1 X0.45 Y0.45 F9000\n\
         G1 X9.55 Y0.45 E1.0\n\
         G1 X9.55 Y9.55 E2.0\n\
         G92 E0\n\
         G1 X0 Y0 F9000\n\
         G1 X10 Y0 E1.0\n\
         G1 X10 Y10 E2.0\n",
        layer(0.2),
        layer(0.4),
        layer(0.6),
    );
    let config = Config {
        wall_flow: Some(1.3),
        ..Config::default()
    };
    let out = run(&source, &config);

    // The origin line is not an extrusion and must survive untouched.
    assert!(out.contains("\nG92 E0\n"), "{out}");

    // Every absolute value after the reset is measured from the new zero,
    // so none of them may jump.
    let after = out.split("\nG92 E0\n").nth(1).expect("the reset is kept");
    let mut position = 0.0;
    for line in after.lines() {
        let parsed = Line::parse(line);
        if !parsed.draws() {
            continue;
        }
        if let Some(e) = parsed.e {
            assert!(
                e >= position && e - position <= 2.0,
                "{line} asks for {} mm in one move:\n{out}",
                e - position
            );
            position = e;
        }
    }
}

/// A file sliced to complete individual objects builds each one from the
/// bed up, so it holds several first and last layers rather than one pair.
/// Metering only the file's own leaves every later object's bed layer
/// starved and bricks the top of every object but the last.
#[test]
fn every_object_gets_its_own_first_and_last_layer() {
    let mut source = String::from("; layer_height = 0.2\nM83\n");
    for object in 1..=2 {
        for index in 0..4 {
            source.push_str(&layer(0.2 + f64::from(index) * 0.2));
            source.push_str(";TYPE:Perimeter\n");
            source.push_str(&wall_of(
                2,
                &format!("o{object}L{index}loop"),
                0.0,
                10.0,
                0.5,
            ));
        }
    }
    let out = run(&source, &plain());

    // A column climbs over the two layers above the bed and gives the
    // climb back where it is capped, and it does that once per object
    // rather than once per file.
    let flow = |tag: &str| {
        out.lines()
            .find(|line| line.ends_with(tag))
            .map(|line| Line::parse(line).e.expect("an extrusion"))
            .unwrap_or_else(|| panic!("{tag} missing from:\n{out}"))
    };
    for object in 1..=2 {
        assert_eq!(
            flow(&format!("o{object}L0loop2")),
            0.5,
            "object {object} bed layer"
        );
        assert_eq!(
            flow(&format!("o{object}L1loop2")),
            0.625,
            "object {object} first climb"
        );
        assert_eq!(
            flow(&format!("o{object}L2loop2")),
            0.625,
            "object {object} second climb"
        );
        assert_eq!(
            flow(&format!("o{object}L3loop2")),
            0.25,
            "object {object} top layer"
        );
    }
}

/// Two concentric loops of one curved wall, each drawn as a pair of `G3`
/// arcs but cut at different angles — which is what a slicer emits with arc
/// fitting on, and that is the Bambu Studio default.
///
/// Read off the endpoints alone the two rings are 14.5 mm apart, far past
/// `MAX_LOOP_GAP`, so each became a contour of its own, each was numbered
/// as a lone wall, and both came out raised. Every curved wall in the file
/// lost the stagger this exists to create.
#[test]
fn a_wall_drawn_with_arcs_is_one_contour_however_its_arcs_were_cut() {
    // Both rings turn about (20, 20). The inner one, radius 10, is cut at
    // 0 and 180 degrees; the outer, a bead further out, at 90 and 270.
    let body = ";TYPE:Perimeter\n\
         G1 X30.00 Y20.00 F9000\n\
         G3 X10.00 Y20.00 I-10 J0 E1.0 ; inner\n\
         G3 X30.00 Y20.00 I10 J0 E1.0\n\
         G1 X20.00 Y30.45 F9000\n\
         G3 X20.00 Y9.55 I0 J-10.45 E1.0 ; outer\n\
         G3 X20.00 Y30.45 I0 J10.45 E1.0\n";
    let out = run(&middle_layer(body), &plain());
    assert_eq!(
        loop_states(&out),
        vec![("inner".to_owned(), false), ("outer".to_owned(), true)],
        "the two rings are one wall and must alternate: {out}"
    );
}

/// A slicer lays gap fill between two loops of a wall, where a third would
/// not fit, and labels it as its own region in the middle of that wall.
///
/// The label carries the word "fill", which used to classify it as infill
/// and so end the wall's region. The two halves were then numbered
/// separately, and the half without the visible wall in it fell back to
/// counting from a fixed end — which inverted it. On this four-loop wall
/// that left the two loops either side of the gap both raised, so the seam
/// the transform exists to stagger came out unstaggered.
#[test]
fn gap_fill_inside_a_wall_does_not_split_it() {
    let ring = |inset: f64, tag: &str| {
        let (near, far) = (inset, 10.0 - inset);
        format!(
            "G1 X{near:.2} Y{near:.2} F9000\n\
             G1 X{far:.2} Y{near:.2} E0.5 ; {tag}\n\
             G1 X{far:.2} Y{far:.2} E0.5\n\
             G1 X{near:.2} Y{far:.2} E0.5\n\
             G1 X{near:.2} Y{near:.2} E0.5\n"
        )
    };
    let body = format!(
        ";TYPE:Perimeter\n{}{}\
         ;TYPE:Gap fill\n\
         G1 X2.00 Y0.68 F9000\n\
         G1 X5.00 Y0.68 E0.2 ; gap1\n\
         ;TYPE:Perimeter\n{}\
         ;TYPE:External perimeter\n{}",
        ring(1.35, "loop1"),
        ring(0.90, "loop2"),
        ring(0.45, "loop3"),
        ring(0.00, "loop4"),
    );
    let out = run(&middle_layer(&body), &plain());
    assert_eq!(
        loop_states(&out),
        vec![
            ("loop1".to_owned(), true),
            ("loop2".to_owned(), false),
            ("gap1".to_owned(), false),
            ("loop3".to_owned(), true),
            ("loop4".to_owned(), false),
        ],
        "the wall is numbered from its visible loop straight through the gap \
         fill, which takes no place of its own in the alternation: {out}"
    );
    assert!(
        out.contains("G1 X5.00 Y0.68 E0.2 ; gap1"),
        "and the gap fill itself is left exactly as it was sliced: {out}"
    );
}

/// A hole's wall running close to an island's is a second wall, not more
/// loops of the first.
///
/// Asking only whether two loops come close *anywhere* pulled the hole into
/// the island's contour, where its loops were then ordered by their distance
/// to the island's visible wall. That distance drifts layer to layer on
/// anything tapered or curved, so the hole's stagger flipped between layers,
/// and the hole's own visible wall tripped the "one contour, one visible
/// wall" rule into a contour of its own where nothing is ever raised. A
/// screw hole near an edge, a lattice and embossed text all produce it.
#[test]
fn a_hole_beside_an_island_keeps_its_own_contour() {
    // Rectangles walked in millimetre steps, the way a slicer emits them. A
    // corner-to-corner move would leave a 20 mm face with one point on it
    // and nothing in between to probe.
    fn ring(near: (f64, f64), far: (f64, f64), tag: &str) -> String {
        let corners = [
            (near.0, near.1),
            (far.0, near.1),
            (far.0, far.1),
            (near.0, far.1),
        ];
        let mut text = format!("G1 X{:.3} Y{:.3} F9000\n", corners[0].0, corners[0].1);
        let mut first = true;
        for (&from, &to) in corners.iter().zip(corners.iter().cycle().skip(1)) {
            let steps = (to.0 - from.0).hypot(to.1 - from.1).ceil().max(1.0) as usize;
            for step in 1..=steps {
                let share = step as f64 / steps as f64;
                let x = from.0 + (to.0 - from.0) * share;
                let y = from.1 + (to.1 - from.1) * share;
                let note = match std::mem::take(&mut first) {
                    true => format!(" ; {tag}"),
                    false => String::new(),
                };
                text.push_str(&format!("G1 X{x:.3} Y{y:.3} E0.05{note}\n"));
            }
        }
        text
    }

    let body = format!(
        ";TYPE:Perimeter\n{}\
         ;TYPE:External perimeter\n{}\
         ;TYPE:Perimeter\n{}\
         ;TYPE:External perimeter\n{}",
        ring((0.45, 0.45), (19.55, 19.55), "island2"),
        ring((0.00, 0.00), (20.00, 20.00), "island1"),
        // Its near face runs 1.2 mm from the island's, over 5 mm of the
        // island's 20 mm one — a quarter of the hole's own path.
        ring((21.20, 7.50), (26.20, 12.50), "hole2"),
        ring((21.65, 7.95), (25.75, 12.05), "hole1"),
    );
    let out = run(&middle_layer(&body), &plain());
    assert_eq!(
        loop_states(&out),
        vec![
            ("island2".to_owned(), true),
            ("island1".to_owned(), false),
            ("hole2".to_owned(), true),
            ("hole1".to_owned(), false),
        ],
        "the hole must stagger against its own visible wall: {out}"
    );
}

/// The layer above a column's first climbing layer stands on half a raise,
/// not a whole one.
///
/// A column that opens partway up — the roof of a bridged hole, the
/// underside of a shelf — is supported from its second layer on, so from
/// its third its loops read as old as the object and the layer below was
/// taken for a settled raise. It was really the middle of the climb, half
/// as tall: the bead was metered to span one layer where it crosses a
/// layer and a quarter, four fifths of the gap it has to fill.
#[test]
fn a_bead_over_a_climbing_column_is_metered_for_the_half_raise_below_it() {
    let base = wall_of(2, "base", 0.0, 10.0, 0.5);
    let roof = wall_of(2, "roof", 20.0, 10.0, 0.5);
    let mut source = String::new();
    for z in [0.2, 0.4, 0.6] {
        source.push_str(&layer(z));
        source.push_str(&untagged(&format!(";TYPE:Perimeter\n{base}")));
    }
    // The roof's column opens here, over nothing, and climbs on the next.
    for z in [0.8, 1.0] {
        source.push_str(&layer(z));
        source.push_str(&untagged(&format!(";TYPE:Perimeter\n{base}{roof}")));
    }
    // Two layers on. The column is supported now and as old as the object,
    // so nothing but the footprint below says it was still climbing.
    source.push_str(&layer(1.2));
    source.push_str(&format!(";TYPE:Perimeter\n{base}{roof}"));
    for z in [1.4, 1.6] {
        source.push_str(&layer(z));
        source.push_str(&untagged(&format!(";TYPE:Perimeter\n{base}{roof}")));
    }
    let out = run(&relative(&source), &plain());

    let flow = |tag: &str| {
        out.lines()
            .find(|line| line.ends_with(tag))
            .map(|line| Line::parse(line).e.expect("an extrusion"))
            .unwrap_or_else(|| panic!("{tag} missing from:\n{out}"))
    };
    assert_eq!(
        flow("; roof2"),
        0.625,
        "it stands on the middle of a climb, so it spans a layer and a quarter"
    );
    assert_eq!(
        flow("; base2"),
        0.5,
        "the column that started on the bed settled two layers ago"
    );
}

#[test]
fn g92_resets_the_extrusion_offset() {
    let source = format!(
        "M82\n{};TYPE:Perimeter\nG1 X10 Y0 E1.0\n;TYPE:Solid infill\nG92 E0\nG1 X20 Y0 E1.0\n",
        layer(0.2),
    );
    let config = Config {
        wall_flow: Some(2.0),
        ..Config::default()
    };
    let out = run(&source, &config);
    assert!(out.contains("G1 X20 Y0 E1.0\n"), "{out}");
}

/// A line longer than any reader holds whole is handed over in pieces, and
/// those pieces are one line of the file — a thumbnail, or a comment carrying
/// an object name out of a project. A newline written between two of them
/// does not merely reformat the file: it turns the tail of that line into a
/// command, and the tail of a base64 thumbnail is whatever it happens to
/// spell. Here it spells a nozzle fifty millimetres up with nine millimetres
/// of filament behind it.
///
/// Both transforms run over the same stream in one pass, so the line has to
/// come out of both of them exactly as it went in — and the line after it
/// must not be glued to its tail either.
#[test]
fn a_line_over_the_cap_survives_both_transforms_whole() {
    let long = format!(
        ";thumbnail {}G1 Z50 E9{}",
        "x".repeat(MAX_LINE),
        "y".repeat(MAX_LINE)
    );
    let source = format!(
        "{}{long}\n;TYPE:Solid infill\nG1 X1.00 Y1.00 E0.1 ; after\n",
        middle_layer(&format!(";TYPE:Perimeter\n{}", wall(2, "loop"))),
    );

    // Composed exactly as the binary composes them: the surface pass is the
    // writer bricking writes into.
    let survey = Survey::read(source.as_bytes()).expect("surveying a slice cannot fail");
    let contour = crate::zaa::Config {
        bricked: true,
        ..crate::zaa::Config::default()
    };
    let mut written = Vec::new();
    let mut pass = crate::zaa::Pass::new(&mut written, source.as_bytes(), &contour, &survey);
    stream(source.as_bytes(), &mut pass, &plain(), &survey).expect("rewriting cannot fail");
    pass.finish().expect("finishing cannot fail");
    let out = String::from_utf8(written).expect("the output is UTF-8");

    assert_eq!(
        out.lines().filter(|line| *line == long).count(),
        1,
        "the over-long line must be rebuilt exactly as it arrived"
    );
    assert!(
        out.trim_end().ends_with("; after"),
        "and the line after it must still be a line of its own"
    );
    // And the wall it sits beside is bricked as it would be without it.
    assert!(
        loop_states(&out).contains(&("loop2".to_owned(), true)),
        "the fixture itself must still brick"
    );
}

#[test]
fn gcode_without_perimeters_is_unchanged() {
    let source = "M83\nG1 Z0.2\nG1 X1 Y1 E0.1\nM104 S0\n";
    assert_eq!(run(source, &Config::default()), source);
}

#[test]
fn empty_input_produces_empty_output() {
    assert_eq!(run("", &Config::default()), "");
}

#[test]
fn reports_what_it_did() {
    let source = middle_layer(&format!(";TYPE:Perimeter\n{}", wall(2, "loop")));
    let stats = apply(&source, &Config::default()).stats;
    assert_eq!(stats.loops, 10);
    assert_eq!(stats.raised, 3);
    assert_eq!(stats.layers, 5);
    assert_eq!(stats.layer_height, 0.2);
    assert!(stats.layer_height_detected);
}

/// A `G2` that closes on its own start names no coordinate at all — the
/// `I` and `J` are the whole of it — and that is what a slicer emits for a
/// ring it fitted to a single arc.
///
/// Asking whether the move names an `X` or a `Y` called it a travel, so
/// every such loop was replayed as sliced while the survey, which asked a
/// different question, drew it as a wall. The two passes have to lay a file
/// out off the same beads or the rewrite meters a loop against a layer it
/// never counted.
#[test]
fn a_full_circle_that_names_no_coordinate_is_still_a_bead() {
    // Two rings about (20, 20), a bead apart, each written as one full
    // turn from a start point the arc returns to.
    let body = ";TYPE:Perimeter\n\
         G1 X30.00 Y20.00 F9000\n\
         G2 I-10 J0 E2.0 ; inner\n\
         G1 X30.45 Y20.00 F9000\n\
         G2 I-10.45 J0 E2.0 ; outer\n";
    let out = run(&middle_layer(body), &plain());
    assert_eq!(
        loop_states(&out),
        vec![("inner".to_owned(), false), ("outer".to_owned(), true)],
        "a whole ring in one move is still a loop of the wall: {out}"
    );
}

/// The same visible wall as
/// [`a_visible_wall_drawn_with_an_arc_moves_with_its_centre`], with its arc
/// written the other way a slicer writes one.
///
/// An `R` names the radius and leaves the centre to be worked out from both
/// ends of the move, so a reader that only looked for `I`/`J` saw no arc at
/// all: the sweep was offset as though it were the chord across it, which
/// pulls the bead a sagitta off the curve it was drawn on and leaves the
/// `R` naming a radius its new ends do not span. Resolved from where the
/// move starts, the two forms are the same arc and print the same wall.
#[test]
fn a_visible_wall_drawn_with_a_radius_moves_with_the_centre_that_radius_names() {
    let arc = ";TYPE:External perimeter\n\
         G1 X0.00 Y0.00 F9000\n\
         G1 X10.00 Y0.00 E1.0 ; arcskin\n\
         G2 X10.00 Y10.00 R5 E1.0\n\
         G1 X0.00 Y10.00 E1.0\n\
         G1 X0.00 Y0.00 E1.0\n";
    let source = with_skin_width(&middle_layer(&format!(
        ";TYPE:Perimeter\n{}{arc}",
        wall_of(1, "loop", 0.6, 8.8, 1.0)
    )));
    let out = run(&source, &drawn_in());
    // The radius moves with the ends it spans: 5 out to 5.054, exactly as
    // the `I`/`J` form of the same arc does.
    assert!(
        out.contains("G2 X10.000 Y10.054 R5.054"),
        "the arc must keep the circle it was drawn on: {out}"
    );
    assert!(
        out.contains("G1 X10.000 Y-0.054 E1.29311 ; arcskin"),
        "{out}"
    );
}

/// A move no printer makes cannot be rasterised, so the cells along it are
/// never drawn — and a footprint with a hole in it is not a smaller
/// footprint. Read as one it says nothing stands where the wall plainly
/// does, which is how a bead ends up half a layer proud under something
/// metered for a whole one. Every layer that holds such a move, and every
/// layer measured against one, keeps the heights the slicer gave it.
#[test]
fn a_layer_holding_a_move_that_cannot_be_followed_is_left_unbricked() {
    let body = format!(";TYPE:Perimeter\n{}", wall(2, "loop"));
    // Twenty metres is past what the grid can walk at any cell size.
    let unreachable = format!("{body}G1 X20000.00 Y0.00 E1.0\n");
    let out = run(&middle_layer(&unreachable), &plain());
    assert_eq!(
        loop_states(&out),
        vec![("loop1".to_owned(), false), ("loop2".to_owned(), false)],
        "nothing about this wall was measured, so nothing of it is raised: {out}"
    );
    // And the same wall without the impossible move is bricked as usual, so
    // it is the move that refused it and not the fixture.
    assert_eq!(
        loop_states(&run(&middle_layer(&body), &plain())),
        vec![("loop1".to_owned(), false), ("loop2".to_owned(), true)],
        "the fixture itself must brick"
    );
}

/// A thin wall is a bead of its own, and the rewrite meters it against
/// whatever the layer below left standing under it — so a raise printed
/// over by one is already paid for and its column must not be capped as
/// well. Solid infill, a top surface and ironing come out exactly as the
/// slicer metered them, which is why a column under one of those still is.
#[test]
fn a_thin_wall_over_a_column_does_not_cap_it() {
    let on = wall_of(2, "on", 0.0, 10.0, 0.5);
    let body = untagged(&format!(";TYPE:Perimeter\n{on}"));
    let mut source = String::new();
    // The column runs up from the bed, or the layer under test would be
    // where it begins rather than the steady state.
    for z in [0.2, 0.4, 0.6, 0.8] {
        source.push_str(&layer(z));
        source.push_str(&body);
    }
    source.push_str(&layer(1.0));
    source.push_str(&format!(";TYPE:Perimeter\n{on}"));
    // The same footprint again, printed as one bead because the feature
    // narrowed past what two loops fit in.
    source.push_str(&layer(1.2));
    source.push_str(&format!(";TYPE:Thin wall\n{}", untagged(&on)));
    let out = run(&relative(&source), &plain());
    assert_eq!(
        loop_states(&out),
        vec![("on1".to_owned(), false), ("on2".to_owned(), true)],
        "the thin wall over it meters the raise itself: {out}"
    );
}

/// Gap fill is material standing on the layer below, exactly as a thin wall
/// is: a column under it did not end, so capping it as though the part
/// stopped there throws the stagger away for nothing. And the bead itself
/// then has to be metered for the gap that column left it.
///
/// Those two are one change and cannot be made apart. Leave the coverage out
/// and the column is capped, comes back to its plane, and there is nothing
/// left to re-meter — which is why gap fill was safe at a flow of 1.0 for as
/// long as it was left out. Put the coverage in without the metering and the
/// bead is fed a whole layer over half a gap, which is the blob this
/// transform exists not to make. This fixture fails on either half alone: the
/// states below need the coverage, and the flow below needs the metering.
#[test]
fn gap_fill_over_a_column_covers_it_and_is_metered_for_what_it_left() {
    let on = wall_of(2, "on", 0.0, 10.0, 0.5);
    let mut source = String::new();
    // The column runs up from the bed, so the layer under test is the steady
    // state rather than a climb.
    for z in [0.2, 0.4, 0.6, 0.8] {
        source.push_str(&layer(z));
        source.push_str(&untagged(&format!(";TYPE:Perimeter\n{on}")));
    }
    source.push_str(&layer(1.0));
    source.push_str(&format!(";TYPE:Perimeter\n{on}"));
    // The feature narrows here past what two loops fit in: only the inner
    // one is still laid as a wall, and the ring the outer one occupied is
    // filled with a bead of its own, labelled as gap fill.
    source.push_str(&layer(1.2));
    source.push_str(&format!(
        ";TYPE:Perimeter\n{};TYPE:Gap fill\n{}",
        untagged(&wall_of(1, "kept", 0.45, 9.10, 0.5)),
        wall_of(1, "gap", 0.0, 10.0, 0.5),
    ));
    let out = run(&relative(&source), &plain());

    assert_eq!(
        loop_states(&out),
        vec![
            ("on1".to_owned(), false),
            ("on2".to_owned(), true),
            ("gap1".to_owned(), false),
        ],
        "the gap fill stands on the raised column, so the column is not \
         capped — and the gap fill itself is never raised: {out}"
    );
    let flow = out
        .lines()
        .find(|line| line.ends_with("; gap1"))
        .map(|line| Line::parse(line).e.expect("an extrusion"))
        .unwrap_or_else(|| panic!("the gap fill is missing from:\n{out}"));
    assert_eq!(
        flow, 0.25,
        "and it crosses half a layer, not the whole one it was sliced for: {out}"
    );
}

/// A slicer lays a thin wall where a feature narrowed past two loops, and
/// labels it as its own region in the middle of the wall it belongs to —
/// exactly as it does gap fill.
///
/// The label ended the wall's region, so the loops either side of it were
/// numbered separately and the half without the visible wall in it counted
/// from a fixed end, which inverted it. The seam this transform exists to
/// stagger came out unstaggered.
#[test]
fn a_thin_wall_inside_a_wall_does_not_split_it() {
    let ring = |inset: f64, tag: &str| {
        let (near, far) = (inset, 10.0 - inset);
        format!(
            "G1 X{near:.2} Y{near:.2} F9000\n\
             G1 X{far:.2} Y{near:.2} E0.5 ; {tag}\n\
             G1 X{far:.2} Y{far:.2} E0.5\n\
             G1 X{near:.2} Y{far:.2} E0.5\n\
             G1 X{near:.2} Y{near:.2} E0.5\n"
        )
    };
    let body = format!(
        ";TYPE:Perimeter\n{}{}\
         ;TYPE:Thin wall\n\
         G1 X2.00 Y0.68 F9000\n\
         G1 X5.00 Y0.68 E0.2 ; thin1\n\
         ;TYPE:Perimeter\n{}\
         ;TYPE:External perimeter\n{}",
        ring(1.35, "loop1"),
        ring(0.90, "loop2"),
        ring(0.45, "loop3"),
        ring(0.00, "loop4"),
    );
    let out = run(&middle_layer(&body), &plain());
    assert_eq!(
        loop_states(&out),
        vec![
            ("loop1".to_owned(), true),
            ("loop2".to_owned(), false),
            ("thin1".to_owned(), false),
            ("loop3".to_owned(), true),
            ("loop4".to_owned(), false),
        ],
        "the wall is numbered from its visible loop straight through the \
         thin wall, which takes no place of its own in the alternation: {out}"
    );
    // It stands on the plane here and nothing below it is raised, so its own
    // bead is metered for exactly the layer it crosses.
    assert!(
        out.contains("G1 X5.00 Y0.68 E0.2 ; thin1"),
        "and the thin wall itself is left as it was sliced: {out}"
    );
}

/// The point a ring closes on is not a corner. It sits partway along the
/// same edge the ring starts from, a seam gap short of the start, and
/// offered to the offset as a vertex it becomes the far end of an edge
/// 0.04 mm long — which the miter turns end for end as soon as the offset
/// is a fraction of that, and the whole loop is then declined.
///
/// Measured on the seam gap every real file carries, that declined every
/// visible wall of every file above `--extra-flow` 40 or so, which is
/// exactly where the wall most needs bringing in.
#[test]
fn a_ring_stopped_short_of_its_seam_is_moved_however_wide_the_offset() {
    let source = with_skin_width(&middle_layer(&format!(
        ";TYPE:Perimeter\n{}{}",
        wall_of(1, "loop", 0.6, 8.8, 1.0),
        seamed_skin(0.04)
    )));
    let config = Config {
        wall_flow: Some(1.5),
        ..Config::default()
    };
    let out = run(&source, &config);
    let beads: Vec<(f64, f64)> = out
        .lines()
        .skip_while(|line| !line.ends_with("; skin1"))
        .take(4)
        .map(|line| {
            let parsed = Line::parse(line);
            (parsed.x.expect("an X"), parsed.y.expect("a Y"))
        })
        .collect();
    assert_eq!(beads.len(), 4, "the four beads of the wall:\n{out}");
    // Half of (1.5 - 1) times the 0.357 mm spacing is 0.0893, more than
    // twice the 0.04 mm the ring stops short by.
    assert!(
        (beads[0].1 - 0.089).abs() < 1e-9,
        "the wall must be drawn in however far: {beads:?}\n{out}"
    );
    assert!(
        (beads[3].0 - 0.089).abs() < 1e-9 && (beads[3].1 - 0.129).abs() < 1e-9,
        "and the seam gap survives the offset: {beads:?}"
    );
}
