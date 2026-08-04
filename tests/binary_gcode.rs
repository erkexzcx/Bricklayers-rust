//! Checks the binary G-code container against real PrusaSlicer output.
//!
//! The fixtures are Prusa's own test files, so decoding them is the closest
//! thing to testing against the reference implementation.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::{env, fs};

use bricklayers::{Source, bgcode};

const BIN: &str = env!("CARGO_BIN_EXE_bricklayers");

/// PrusaSlicer 2.8.1, one G-code block, heatshrink 12/4 + MeatPack.
const SINGLE: &[u8] = include_bytes!("fixtures/mini_cube_ps2.8.1.bgcode");
/// PrusaSlicer 2.6.0, ten G-code blocks.
const MULTI: &[u8] = include_bytes!("fixtures/mini_cube_b.bgcode");

struct Sandbox(PathBuf);

impl Sandbox {
    fn new(label: &str) -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = env::temp_dir().join(format!("bricklayers-{label}-{}-{id}", std::process::id()));
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

fn run(args: &[&str]) -> std::process::Output {
    Command::new(BIN).args(args).output().expect("run binary")
}

/// Three layers of a wall with two internal perimeter loops.
///
/// Both fixtures are two-perimeter cubes, so their single internal loop has
/// nothing to stagger against and the transform correctly leaves them alone.
/// Repacking this into a real container gives the binary path something to do.
fn brickable_gcode() -> String {
    let mut text = String::from("M83\n; layer_height = 0.2\n");
    for z in [0.2_f64, 0.4, 0.6] {
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
/// libbgcode's own converter produces for these files. The ten-block file is
/// sliced with a first layer thicker than the rest, which is the default.
#[test]
fn decodes_real_prusaslicer_files() {
    for (label, bytes, size, checksum, height, first) in [
        ("single block", SINGLE, 53_301, 0x6A01_FC76, 0.2, 0.2),
        ("ten blocks", MULTI, 621_913, 0xA1DD_ECAF, 0.15, 0.2),
    ] {
        assert!(bgcode::is_binary(bytes), "{label}");
        let (container, gcode) = bgcode::parse(bytes).unwrap_or_else(|e| panic!("{label}: {e}"));

        assert_eq!(gcode.len(), size, "{label}");
        assert_eq!(crc32fast::hash(gcode.as_bytes()), checksum, "{label}");
        assert_eq!(container.layer_height, Some(height), "{label}");
        assert_eq!(container.first_layer_height, Some(first), "{label}");
        assert!(gcode.contains(";TYPE:Perimeter\n"), "{label}");
        assert!(gcode.contains(";TYPE:External perimeter\n"), "{label}");
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

    let output = run(&["brick", path.to_str().unwrap()]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("not valid binary G-code"),
        "unhelpful message: {stderr}"
    );
}

#[test]
fn a_corrupted_block_is_rejected() {
    let sandbox = Sandbox::new("corrupt");
    let mut damaged = SINGLE.to_vec();
    *damaged.last_mut().unwrap() ^= 0xFF;
    let path = sandbox.with("part.bgcode", &damaged);

    let output = run(&["brick", path.to_str().unwrap()]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("checksum"));
}

#[test]
fn brick_rewrites_binary_gcode_in_place() {
    let sandbox = Sandbox::new("brick-binary");
    let (container, _) = bgcode::parse(SINGLE).expect("fixture should parse");
    let path = sandbox.with("part.bgcode", &container.serialize(&brickable_gcode()));

    let output = run(&["brick", "--verbose", path.to_str().unwrap()]);
    assert!(output.status.success(), "{output:?}");

    let written = fs::read(&path).expect("read result");
    assert!(bgcode::is_binary(&written), "output is no longer binary");

    let (_, gcode) = bgcode::parse(&written).expect("output should parse");
    assert!(gcode.contains("; bricklayers brick raised"), "{gcode}");
    assert!(gcode.contains(";TYPE:External perimeter\n"), "{gcode}");

    let leftovers: Vec<_> = fs::read_dir(sandbox.path())
        .expect("list sandbox")
        .filter_map(|entry| entry.ok().map(|entry| entry.file_name()))
        .filter(|name| name != "part.bgcode")
        .collect();
    assert!(leftovers.is_empty(), "left behind {leftovers:?}");
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
        let output = run(&["brick", "--layer-height", "0.2", path.to_str().unwrap()]);
        assert!(output.status.success(), "{output:?}");
    }

    let from_text = fs::read_to_string(&text).expect("read text result");
    let from_binary = Source::open(&binary).expect("read binary result");
    assert_eq!(from_binary.decoded(), Some(from_text.as_str()));
}

#[test]
fn rewriting_preserves_metadata_blocks_untouched() {
    let sandbox = Sandbox::new("metadata");
    let path = sandbox.with("part.bgcode", SINGLE);

    assert!(run(&["brick", path.to_str().unwrap()]).status.success());
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
