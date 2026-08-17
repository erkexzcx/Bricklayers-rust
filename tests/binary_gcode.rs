//! Checks the binary G-code container against real PrusaSlicer output.
//!
//! The fixtures are Prusa's own test files, so decoding them is the closest
//! thing to testing against the reference implementation.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::{env, fs};

use corbel::{Source, bgcode};

const BIN: &str = env!("CARGO_BIN_EXE_corbel");

/// PrusaSlicer 2.8.1, one G-code block, heatshrink 12/4 + MeatPack.
const SINGLE: &[u8] = include_bytes!("fixtures/mini_cube_ps2.8.1.bgcode");
/// PrusaSlicer 2.6.0, ten G-code blocks.
const MULTI: &[u8] = include_bytes!("fixtures/mini_cube_b.bgcode");

struct Sandbox(PathBuf);

impl Sandbox {
    fn new(label: &str) -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = env::temp_dir().join(format!("corbel-{label}-{}-{id}", std::process::id()));
        fs::create_dir_all(&path).expect("create sandbox");
        Self(path)
    }

    fn with(&self, name: &str, contents: &[u8]) -> PathBuf {
        let path = self.0.join(name);
        fs::write(&path, contents).expect("write fixture");
        path
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Runs the binary with both transforms, which is what these pinned before
/// naming one became mandatory.
fn run(args: &[&str]) -> std::process::Output {
    Command::new(BIN)
        .args(["--bricks", "--zaa"])
        .args(args)
        .output()
        .expect("run binary")
}

/// Runs the binary with the layer height a slicer would have exported.
///
/// A container states its own layer height in a metadata block and the G-code
/// it carries does not, so comparing the two paths means handing the text one
/// the same figure. Otherwise the comparison is of what each could work the
/// height out from rather than of what each did with it.
fn run_at_layer_height(args: &[&str]) -> std::process::Output {
    Command::new(BIN)
        .args(["--bricks", "--zaa"])
        .args(args)
        .env("SLIC3R_LAYER_HEIGHT", "0.2")
        .output()
        .expect("run binary")
}

/// The whole point of reading the container's metadata: a binary file gets its
/// flow from its own geometry, not from the fallback, even though nothing in
/// its G-code stream states a width. Nothing is passed on the command line,
/// because nothing can be.
#[test]
fn a_binary_file_gets_its_flow_from_its_own_metadata() {
    let sandbox = Sandbox::new("binary-flow");
    let path = sandbox.with("cube.bgcode", SINGLE);
    let output = run(&["--verbose", path.to_str().unwrap()]);
    assert!(output.status.success(), "{output:?}");

    let report = String::from_utf8_lossy(&output.stderr);
    assert!(report.contains("binary G-code container"), "{report}");
    assert!(
        !report.contains("states no internal wall width"),
        "the container states one, so nothing may fall back: {report}"
    );
    // 0.2 mm layers at a 0.45 mm wall, both out of the metadata blocks.
    assert!(report.contains("a flow of 1.025"), "{report}");
}

/// The G-code a [`Source`] hands a transform, however it is packed.
fn streamed(path: &Path) -> String {
    let source = Source::open(path).expect("open source");
    let mut text = String::new();
    source
        .reader()
        .expect("reader")
        .read_to_string(&mut text)
        .expect("read source");
    text
}

/// Whatever a run left in the sandbox besides the file it was pointed at.
fn leftovers(sandbox: &Sandbox) -> Vec<std::ffi::OsString> {
    fs::read_dir(sandbox.path())
        .expect("list sandbox")
        .filter_map(|entry| entry.ok().map(|entry| entry.file_name()))
        .filter(|name| name != "part.bgcode")
        .collect()
}

/// A wall with two internal perimeter loops, over `layers` layers.
///
/// Both fixtures are two-perimeter cubes, so their single internal loop has
/// nothing to stagger against and the transform correctly leaves them alone.
/// Repacking this into a real container gives the binary path something to do.
fn brickable_gcode(layers: usize) -> String {
    let mut text = String::from("M83\n; layer_height = 0.2\n");
    for layer in 1..=layers {
        let z = layer as f64 * 0.2;
        text.push_str(";LAYER_CHANGE\n");
        text.push_str(&format!("G1 Z{z:.3} F720\n"));
        text.push_str(";TYPE:External perimeter\n");
        text.push_str("G1 X0 Y0 F9000\nG1 X20 Y0 E0.66000\n");
        text.push_str(";TYPE:Perimeter\n");
        for inset in [0.45_f64, 0.90] {
            let far = 20.0 - inset;
            text.push_str(&format!("G1 X{inset:.2} Y{inset:.2} F9000\n"));
            for (x, y) in [(far, inset), (far, far), (inset, far), (inset, inset)] {
                text.push_str(&format!("G1 X{x:.2} Y{y:.2} E0.64000\n"));
            }
        }
    }
    text
}

/// Values taken from output verified line by line against the ASCII G-code
/// libbgcode's own converter produces for these files.
#[test]
fn decodes_real_prusaslicer_files() {
    for (label, bytes, size, checksum, height) in [
        ("single block", SINGLE, 53_301, 0x6A01_FC76, 0.2),
        ("ten blocks", MULTI, 621_913, 0xA1DD_ECAF, 0.15),
    ] {
        assert!(bgcode::is_binary(bytes), "{label}");
        let (container, gcode) = bgcode::parse(bytes).unwrap_or_else(|e| panic!("{label}: {e}"));

        assert_eq!(gcode.len(), size, "{label}");
        assert_eq!(crc32fast::hash(gcode.as_bytes()), checksum, "{label}");
        assert_eq!(container.layer_height, Some(height), "{label}");
        assert!(gcode.contains(";TYPE:Perimeter\n"), "{label}");
        assert!(gcode.contains(";TYPE:External perimeter\n"), "{label}");
    }
}

/// The geometry the flow is derived from is in the metadata of a binary file
/// and nowhere in its G-code, so a search of the decoded stream finds none of
/// it. The two are read together because a profile may state the width as a
/// share of the nozzle, and the nozzle can arrive a block later; these two
/// state it as a length.
#[test]
fn reads_the_geometry_the_flow_is_derived_from() {
    for (label, bytes) in [("single block", SINGLE), ("ten blocks", MULTI)] {
        let (container, gcode) = bgcode::parse(bytes).unwrap_or_else(|e| panic!("{label}: {e}"));
        assert_eq!(container.nozzle, Some(0.4), "{label}");
        assert_eq!(container.wall_width, Some(0.45), "{label}");
        assert!(!gcode.contains("perimeter_extrusion_width"), "{label}");
    }
}

#[test]
fn plain_text_is_left_to_the_text_path() {
    assert!(!bgcode::is_binary(b"G1 X1 Y1 E1\n"));
    assert!(bgcode::parse(b"G1 X1 Y1 E1\n").is_err());
}

#[test]
fn a_truncated_file_fails_with_a_readable_message() {
    let sandbox = Sandbox::new("truncated");
    let path = sandbox.with("part.bgcode", &SINGLE[..SINGLE.len() / 2]);

    let output = run(&[path.to_str().unwrap()]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("not valid binary G-code"),
        "unhelpful message: {stderr}"
    );
}

/// A G-code block's checksum is only reached on the pass that decodes it,
/// which is a pass earlier than the first byte of output — so the file the
/// block came from has to survive being refused.
#[test]
fn a_corrupted_block_is_rejected() {
    let sandbox = Sandbox::new("corrupt");
    let mut damaged = SINGLE.to_vec();
    *damaged.last_mut().unwrap() ^= 0xFF;
    let path = sandbox.with("part.bgcode", &damaged);

    let output = run(&[path.to_str().unwrap()]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("checksum"));

    assert_eq!(
        fs::read(&path).expect("read result"),
        damaged,
        "input changed"
    );
    assert!(leftovers(&sandbox).is_empty(), "left a temporary behind");
}

#[test]
fn brick_rewrites_binary_gcode_in_place() {
    let sandbox = Sandbox::new("brick-binary");
    let (container, _) = bgcode::parse(SINGLE).expect("fixture should parse");
    let path = sandbox.with("part.bgcode", &container.serialize(&brickable_gcode(3)));

    let output = run(&["--verbose", path.to_str().unwrap()]);
    assert!(output.status.success(), "{output:?}");

    let written = fs::read(&path).expect("read result");
    assert!(bgcode::is_binary(&written), "output is no longer binary");

    let (_, gcode) = bgcode::parse(&written).expect("output should parse");
    assert!(gcode.contains("; corbel brick raised"), "{gcode}");
    assert!(gcode.contains(";TYPE:External perimeter\n"), "{gcode}");

    assert!(leftovers(&sandbox).is_empty(), "left a temporary behind");
}

/// The container must be a pure transport detail: the same input G-code has to
/// come out the same whether it arrived as text or packed into blocks.
#[test]
fn the_binary_path_matches_the_text_path() {
    let sandbox = Sandbox::new("paths-agree");
    let (_, decoded) = bgcode::parse(SINGLE).expect("fixture should parse");

    let text = sandbox.with("part.gcode", decoded.as_bytes());
    let binary = sandbox.with("part.bgcode", SINGLE);

    for path in [&text, &binary] {
        let output = run_at_layer_height(&[path.to_str().unwrap()]);
        assert!(output.status.success(), "{output:?}");
    }

    let from_text = fs::read_to_string(&text).expect("read text result");
    assert_eq!(streamed(&binary), from_text);
}

/// A print's G-code fills many blocks, so a rewrite reads and writes across
/// their boundaries in the middle of a wall. Blocks are a transport detail, so
/// none of that may show in the result.
#[test]
fn a_rewrite_across_many_blocks_matches_the_text_path() {
    let sandbox = Sandbox::new("many-blocks");
    let (container, _) = bgcode::parse(SINGLE).expect("fixture should parse");
    let gcode = brickable_gcode(2_000);
    // The writer packs about 64 kB to a block, so this is roughly ten of them.
    assert!(gcode.len() > 512 * 1024, "expected many blocks of G-code");

    let text = sandbox.with("part.gcode", gcode.as_bytes());
    let binary = sandbox.with("part.bgcode", &container.serialize(&gcode));

    for path in [&text, &binary] {
        let output = run_at_layer_height(&[path.to_str().unwrap()]);
        assert!(output.status.success(), "{output:?}");
    }

    let rewritten = streamed(&binary);
    assert!(rewritten.contains("; corbel brick raised"), "no change");
    assert_eq!(
        rewritten,
        fs::read_to_string(&text).expect("read text result")
    );
}

#[test]
fn rewriting_preserves_metadata_blocks_untouched() {
    let sandbox = Sandbox::new("metadata");
    let path = sandbox.with("part.bgcode", SINGLE);

    assert!(run(&[path.to_str().unwrap()]).status.success());
    let written = fs::read(&path).expect("read result");

    let shared = written
        .iter()
        .zip(SINGLE)
        .take_while(|(new, old)| new == old)
        .count();

    // Settings and two thumbnails fill roughly 16 kB before the G-code block,
    // and they are copied rather than re-encoded, so they must survive intact.
    assert!(
        shared > 16_000,
        "metadata blocks diverge after {shared} bytes"
    );
}
