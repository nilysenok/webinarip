//! Minimal EBML (the binary format under Matroska/WebM): element IDs, sizes, values.

pub fn vint_size(n: u64) -> Vec<u8> {
    // Smallest length that fits; all-ones is reserved for "unknown".
    for len in 1..=8u32 {
        let max = (1u64 << (7 * len)) - 1;
        if n < max {
            return (n | (1u64 << (7 * len))).to_be_bytes()[8 - len as usize..].to_vec();
        }
    }
    panic!("EBML size too large");
}

/// Size field of a fixed 8-byte width, so it can be patched in place later.
pub fn vint8(n: u64) -> [u8; 8] {
    let mut b = n.to_be_bytes();
    b[0] = 0x01;
    b
}

pub fn id(id: u32) -> Vec<u8> {
    let b = id.to_be_bytes();
    let skip = b.iter().position(|&x| x != 0).unwrap_or(3);
    b[skip..].to_vec()
}

pub fn element(el: u32, body: &[u8]) -> Vec<u8> {
    let mut v = id(el);
    v.extend(vint_size(body.len() as u64));
    v.extend_from_slice(body);
    v
}

pub fn uint(el: u32, n: u64) -> Vec<u8> {
    let b = n.to_be_bytes();
    let skip = b.iter().position(|&x| x != 0).unwrap_or(7);
    element(el, &b[skip..])
}

pub fn float(el: u32, x: f64) -> Vec<u8> {
    element(el, &x.to_be_bytes())
}

pub fn string(el: u32, s: &str) -> Vec<u8> {
    element(el, s.as_bytes())
}

pub fn master(el: u32, children: &[Vec<u8>]) -> Vec<u8> {
    element(el, &children.concat())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_sizes_and_ids() {
        assert_eq!(vint_size(0), vec![0x80]);
        assert_eq!(vint_size(126), vec![0xfe]);
        assert_eq!(vint_size(127), vec![0x40, 0x7f]);
        assert_eq!(id(0x1A45DFA3), vec![0x1a, 0x45, 0xdf, 0xa3]);
        assert_eq!(uint(0x86, 1), vec![0x86, 0x81, 0x01]);
        assert_eq!(vint8(5), [1, 0, 0, 0, 0, 0, 0, 5]);
    }
}
