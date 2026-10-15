pub const MAX_OUTPUT: usize = 1 << 20;

pub fn decompress_into(source: &[u8], expected_size: usize, output: &mut Vec<u8>) -> bool {
    output.clear();
    if expected_size == 0 || expected_size > MAX_OUTPUT {
        return false;
    }

    output.reserve(expected_size);
    let mut i = 0;
    while i < source.len() {
        let token = source[i] as usize;
        i += 1;
        let mut literal = token >> 4;
        if literal == 15 {
            loop {
                let Some(&extra) = source.get(i) else { return false };
                i += 1;
                literal += extra as usize;
                if extra != 255 {
                    break;
                }
            }
        }

        if literal > source.len() - i || literal > expected_size - output.len() {
            return false;
        }

        output.extend_from_slice(&source[i..i + literal]);
        i += literal;
        if i >= source.len() {
            break;
        }

        if i + 2 > source.len() {
            return false;
        }

        let offset = source[i] as usize | (source[i + 1] as usize) << 8;
        i += 2;
        let mut length = token & 15;
        if length == 15 {
            loop {
                let Some(&extra) = source.get(i) else { return false };
                i += 1;
                length += extra as usize;
                if extra != 255 {
                    break;
                }
            }
        }

        length += 4;
        let written = output.len();
        if offset == 0 || offset > written || length > expected_size - written {
            return false;
        }

        for k in 0..length {
            let b = output[written - offset + k];
            output.push(b);
        }
    }

    output.len() == expected_size
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_block_round_trips() {
        let mut out = Vec::new();
        assert!(decompress_into(&[0x30, 1, 2, 3], 3, &mut out));
        assert_eq!(out, [1, 2, 3]);
    }

    #[test]
    fn bad_back_reference_is_rejected() {
        let mut out = Vec::new();
        assert!(!decompress_into(&[0x11, 9, 0x05, 0x00], 10, &mut out));
    }

    #[test]
    fn overlapping_match_repeats() {
        let mut out = Vec::new();
        assert!(decompress_into(&[0x15, b'a', 0x01, 0x00], 10, &mut out));
        assert_eq!(out, b"aaaaaaaaaa");
    }
}
