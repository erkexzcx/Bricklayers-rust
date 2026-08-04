//! MeatPack, the G-code text encoding used inside binary G-code blocks.
//!
//! Fifteen characters cover most of a G-code stream, so each is packed into a
//! nibble and two share a byte; the sixteenth nibble value escapes to a
//! following full-width byte. Two `0xFF` bytes introduce a command that toggles
//! packing, which is how comment lines survive in the "keep comments" variant.
//!
//! Encoding is deliberately absent. It drops whitespace and inline comments, so
//! rewritten G-code is stored unencoded rather than passed through a second
//! lossy round.
//!
//! Ported from Prusa's reference decoder in `libbgcode`, itself derived from
//! Scott Mudge's MeatPack firmware.

const SIGNAL: u8 = 0xFF;
const ENABLE_PACKING: u8 = 251;
const DISABLE_PACKING: u8 = 250;
const RESET_ALL: u8 = 249;
const ENABLE_NO_SPACES: u8 = 247;
const DISABLE_NO_SPACES: u8 = 246;

/// The nibble values below 15; 11 is a space unless "no spaces" is active.
const PACKED: [u8; 15] = *b"0123456789. \nGX";

pub fn decode(source: &[u8]) -> Vec<u8> {
    let mut state = Unpacker::default();
    let mut out = Emitter::with_capacity(source.len() * 2);
    let mut signals = 0u8;
    let mut command = false;

    for &byte in source {
        if byte == SIGNAL {
            if signals > 0 {
                command = true;
                signals = 0;
            } else {
                signals += 1;
            }
            continue;
        }
        if command {
            state.command(byte);
            command = false;
            continue;
        }
        // A lone signal byte was literal data after all.
        if signals > 0 {
            state.receive(SIGNAL, &mut out);
            signals = 0;
        }
        state.receive(byte, &mut out);
    }
    out.out
}

#[derive(Default)]
struct Unpacker {
    packing: bool,
    no_spaces: bool,
    /// Second character of a pair, held back until its escaped partner arrives.
    held: u8,
    /// Full-width bytes still owed by escapes already seen.
    owed: usize,
}

impl Unpacker {
    fn command(&mut self, code: u8) {
        match code {
            ENABLE_PACKING => self.packing = true,
            DISABLE_PACKING | RESET_ALL => self.packing = false,
            ENABLE_NO_SPACES => self.no_spaces = true,
            DISABLE_NO_SPACES => self.no_spaces = false,
            _ => {}
        }
    }

    fn character(&self, nibble: u8) -> u8 {
        match nibble {
            11 if self.no_spaces => b'E',
            nibble if nibble < 15 => PACKED[nibble as usize],
            _ => 0,
        }
    }

    fn receive(&mut self, byte: u8, out: &mut Emitter) {
        if !self.packing {
            out.push(byte);
            return;
        }
        if self.owed > 0 {
            out.push(byte);
            if self.held > 0 {
                out.push(self.held);
                self.held = 0;
            }
            self.owed -= 1;
            return;
        }

        let low = byte & 0x0F;
        let high = byte >> 4;
        match (low == 0x0F, high == 0x0F) {
            (true, true) => self.owed += 2,
            (true, false) => {
                self.owed += 1;
                self.held = self.character(high);
            }
            (false, _) => {
                let first = self.character(low);
                out.push(first);
                // A newline ends the line, so its partner nibble is padding.
                if first != b'\n' {
                    if high == 0x0F {
                        self.owed += 1;
                    } else {
                        out.push(self.character(high));
                    }
                }
            }
        }
    }
}

/// Re-inserts the spaces MeatPack drops, matching the reference decoder so the
/// text is identical to what Prusa's own tooling produces.
struct Emitter {
    out: Vec<u8>,
    spacing: bool,
}

impl Emitter {
    fn with_capacity(capacity: usize) -> Self {
        Self {
            out: Vec::with_capacity(capacity),
            spacing: false,
        }
    }

    fn push(&mut self, character: u8) {
        let mut opened = false;
        if character == b'G' && self.out.last().is_none_or(|&last| last == b'\n') {
            self.spacing = true;
            opened = true;
        } else if character == b'\n' {
            self.spacing = false;
        }

        if !opened
            && self.spacing
            && self.out.last().is_none_or(|&last| last != b' ')
            && is_word(character)
        {
            self.out.push(b' ');
        }

        if character != b'\n' || self.out.last().is_none_or(|&last| last != b'\n') {
            self.out.push(character);
        }
    }
}

fn is_word(character: u8) -> bool {
    matches!(
        character,
        b'X' | b'Y'
            | b'Z'
            | b'E'
            | b'F'
            | b'I'
            | b'J'
            | b'R'
            | b'S'
            | b'G'
            | b'P'
            | b'W'
            | b'H'
            | b'C'
            | b'A'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packed(nibbles: &[u8]) -> u8 {
        nibbles[0] | (nibbles[1] << 4)
    }

    #[test]
    fn passes_through_unpacked_bytes() {
        assert_eq!(decode(b"; a comment\n"), b"; a comment\n");
    }

    #[test]
    fn unpacks_a_command_delimited_run() {
        // Enable packing, then "1.5\n" as two packed bytes.
        let stream = [
            SIGNAL,
            SIGNAL,
            ENABLE_PACKING,
            packed(&[1, 10]),
            packed(&[5, 12]),
        ];
        assert_eq!(decode(&stream), b"1.5\n");
    }

    #[test]
    fn escapes_to_full_width_characters() {
        // 'Y' is unpackable, so the low nibble escapes and the byte follows.
        let stream = [SIGNAL, SIGNAL, ENABLE_PACKING, packed(&[0x0F, 1]), b'Y'];
        assert_eq!(decode(&stream), b"Y1");
    }

    #[test]
    fn both_nibbles_may_escape() {
        let stream = [
            SIGNAL,
            SIGNAL,
            ENABLE_PACKING,
            packed(&[0x0F, 0x0F]),
            b'M',
            b'K',
        ];
        assert_eq!(decode(&stream), b"MK");
    }

    #[test]
    fn disabling_packing_restores_raw_bytes() {
        let mut stream = vec![SIGNAL, SIGNAL, ENABLE_PACKING, packed(&[1, 12])];
        stream.extend_from_slice(&[SIGNAL, SIGNAL, DISABLE_PACKING]);
        stream.extend_from_slice(b"; kept\n");
        assert_eq!(decode(&stream), b"1\n; kept\n");
    }

    #[test]
    fn no_spaces_mode_maps_the_space_slot_to_e() {
        let stream = [
            SIGNAL,
            SIGNAL,
            ENABLE_PACKING,
            SIGNAL,
            SIGNAL,
            ENABLE_NO_SPACES,
            packed(&[11, 1]),
        ];
        assert_eq!(decode(&stream), b"E1");
    }

    #[test]
    fn reinserts_spaces_between_g_words() {
        // "G1X1Y2\n" packed: G,1 then X,1 then Y escapes, then 2,\n
        let stream = [
            SIGNAL,
            SIGNAL,
            ENABLE_PACKING,
            packed(&[13, 1]),
            packed(&[14, 1]),
            packed(&[0x0F, 2]),
            b'Y',
            packed(&[12, 0]),
        ];
        assert_eq!(decode(&stream), b"G1 X1 Y2\n");
    }

    #[test]
    fn a_lone_signal_byte_is_data() {
        assert_eq!(decode(&[SIGNAL, b'a']), [SIGNAL, b'a']);
    }
}
