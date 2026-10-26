//! A minimal PNG writer, so the art crates can dump their sheets for
//! inspection without an image library: RGBA8, no filtering, and deflate's
//! stored (uncompressed) blocks. Files are big but exact.

/// Encode a `w × h` RGBA8 image (`rgba` holds `4 · w · h` bytes, rows top to
/// bottom) as a PNG file. PNG images are at least one pixel wide and high.
pub fn encode(w: usize, h: usize, rgba: &[u8]) -> Vec<u8> {
    assert!(
        w > 0 && h > 0 && rgba.len() == w * h * 4,
        "{w}×{h} RGBA8 image"
    );
    // Scanlines, each behind filter byte 0 (none).
    let mut raw = Vec::with_capacity(h * (w * 4 + 1));
    for row in rgba.chunks_exact(w * 4) {
        raw.push(0);
        raw.extend_from_slice(row);
    }
    // A zlib stream of stored deflate blocks.
    let mut z = vec![0x78, 0x01];
    let mut blocks = raw.chunks(0xFFFF).peekable();
    while let Some(b) = blocks.next() {
        z.push(blocks.peek().is_none() as u8);
        let n = b.len() as u16;
        z.extend_from_slice(&n.to_le_bytes());
        z.extend_from_slice(&(!n).to_le_bytes());
        z.extend_from_slice(b);
    }
    z.extend_from_slice(&adler32(&raw).to_be_bytes());

    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&(w as u32).to_be_bytes());
    ihdr.extend_from_slice(&(h as u32).to_be_bytes());
    // 8 bits per channel, RGBA, deflate, adaptive filtering, no interlace.
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}

/// Write an RGBA8 image (rows top to bottom) to `path` for a dump test,
/// bottom row first. The dumps used to go through macroquad's
/// `Image::export_png`, which flips rows (it is made for GL read-backs);
/// this keeps the files the same. Panics if the file cannot be written.
pub fn dump(path: &str, w: usize, h: usize, rgba: &[u8]) {
    let flipped: Vec<u8> = rgba.chunks_exact(w * 4).rev().flatten().copied().collect();
    std::fs::write(path, encode(w, h, &flipped)).unwrap_or_else(|e| panic!("{path}: {e}"));
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let start = out.len();
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let crc = crc32(&out[start..]);
    out.extend_from_slice(&crc.to_be_bytes());
}

const CRC_TABLE: [u32; 256] = {
    let mut t = [0u32; 256];
    let mut n = 0;
    while n < 256 {
        let mut c = n as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        t[n] = c;
        n += 1;
    }
    t
};

fn crc32(data: &[u8]) -> u32 {
    !data.iter().fold(!0u32, |c, &b| {
        CRC_TABLE[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8)
    })
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for chunk in data.chunks(5552) {
        for &x in chunk {
            a += x as u32;
            b += a;
        }
        a %= 65_521;
        b %= 65_521;
    }
    (b << 16) | a
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksums_match_known_values() {
        assert_eq!(crc32(b"IEND"), 0xAE42_6082);
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
    }

    #[test]
    fn stored_blocks_hold_the_scanlines() {
        // Wide enough for two deflate blocks.
        let (w, h) = (300, 120);
        let rgba: Vec<u8> = (0..w * h * 4).map(|i| (i * 7 % 251) as u8).collect();
        let png = encode(w, h, &rgba);
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(&png[12..16], b"IHDR");
        assert_eq!(&png[16..24], &[0, 0, 1, 44, 0, 0, 0, 120]);
        assert!(png.ends_with(&[0, 0, 0, 0, b'I', b'E', b'N', b'D', 0xAE, 0x42, 0x60, 0x82]));
        // Undo the stored blocks and compare with the filtered scanlines.
        let len = u32::from_be_bytes(png[33..37].try_into().unwrap()) as usize;
        assert_eq!(&png[37..41], b"IDAT");
        let z = &png[41..41 + len];
        let (mut i, mut raw, mut last) = (2, Vec::new(), false);
        while !last {
            last = z[i] == 1;
            let n = u16::from_le_bytes([z[i + 1], z[i + 2]]);
            assert_eq!(!n, u16::from_le_bytes([z[i + 3], z[i + 4]]));
            raw.extend_from_slice(&z[i + 5..i + 5 + n as usize]);
            i += 5 + n as usize;
        }
        assert_eq!(&z[i..], &adler32(&raw).to_be_bytes());
        assert_eq!(raw.len(), h * (w * 4 + 1));
        for (y, line) in raw.chunks(w * 4 + 1).enumerate() {
            assert_eq!(line[0], 0);
            assert_eq!(&line[1..], &rgba[y * w * 4..(y + 1) * w * 4]);
        }
    }
}
