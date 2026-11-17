//! Media generated in code for the capability probe (Present PROTOCOL 6.1): a 64x64 PNG
//! showing a two-digit number, and a one-page PDF showing a short code. New in this crate.
//! No image or PDF library: the PNG uses stored (uncompressed) deflate blocks.

/// Side of the probe image in pixels.
pub const PROBE_IMAGE_SIZE: usize = 64;

/// 5x7 glyphs for the digits 0-9, one row per byte, most significant of 5 bits on the left.
const DIGITS: [[u8; 7]; 10] = [
    [
        0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110,
    ],
    [
        0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110,
    ],
    [
        0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111,
    ],
    [
        0b11111, 0b00010, 0b00100, 0b00010, 0b00001, 0b10001, 0b01110,
    ],
    [
        0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010,
    ],
    [
        0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110,
    ],
    [
        0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110,
    ],
    [
        0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000,
    ],
    [
        0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110,
    ],
    [
        0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100,
    ],
];

const SCALE: usize = 5;
const GLYPH_W: usize = 5;
const GLYPH_H: usize = 7;
const GAP: usize = 5;

/// Grayscale pixels (row-major, 0 = black, 255 = white) of the probe image for `number`
/// (taken modulo 100 and drawn as two digits, black on white).
pub fn vision_probe_pixels(number: u8) -> Vec<u8> {
    let n = number % 100;
    let digits = [(n / 10) as usize, (n % 10) as usize];
    let size = PROBE_IMAGE_SIZE;
    let mut px = vec![255u8; size * size];
    let total_w = 2 * GLYPH_W * SCALE + GAP;
    let x0 = (size - total_w) / 2;
    let y0 = (size - GLYPH_H * SCALE) / 2;
    for (i, d) in digits.iter().enumerate() {
        let gx = x0 + i * (GLYPH_W * SCALE + GAP);
        for (row, bits) in DIGITS[*d].iter().enumerate() {
            for col in 0..GLYPH_W {
                if bits & (1 << (GLYPH_W - 1 - col)) == 0 {
                    continue;
                }
                for dy in 0..SCALE {
                    for dx in 0..SCALE {
                        px[(y0 + row * SCALE + dy) * size + gx + col * SCALE + dx] = 0;
                    }
                }
            }
        }
    }
    px
}

/// The probe image for `number` as a PNG file (deterministic for a given number).
pub fn vision_probe_png(number: u8) -> Vec<u8> {
    encode_gray_png(
        PROBE_IMAGE_SIZE as u32,
        PROBE_IMAGE_SIZE as u32,
        &vision_probe_pixels(number),
    )
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for b in bytes {
        crc ^= *b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn adler32(bytes: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for x in bytes {
        a = (a + *x as u32) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let start = out.len();
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let crc = crc32(&out[start..]);
    out.extend_from_slice(&crc.to_be_bytes());
}

/// Encode 8-bit grayscale pixels as a PNG with an uncompressed zlib stream.
/// `pixels.len()` must be `width * height`.
pub fn encode_gray_png(width: u32, height: u32, pixels: &[u8]) -> Vec<u8> {
    assert_eq!(pixels.len(), (width * height) as usize, "pixel count");
    // Scanlines: filter type 0 followed by the row.
    let mut raw = Vec::with_capacity(pixels.len() + height as usize);
    for row in pixels.chunks(width as usize) {
        raw.push(0);
        raw.extend_from_slice(row);
    }
    // zlib: header, stored deflate blocks of at most 65535 bytes, Adler-32.
    let mut z = vec![0x78, 0x01];
    let mut blocks = raw.chunks(65535).peekable();
    while let Some(block) = blocks.next() {
        z.push(u8::from(blocks.peek().is_none()));
        let len = block.len() as u16;
        z.extend_from_slice(&len.to_le_bytes());
        z.extend_from_slice(&(!len).to_le_bytes());
        z.extend_from_slice(block);
    }
    z.extend_from_slice(&adler32(&raw).to_be_bytes());

    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 0, 0, 0, 0]); // 8-bit, grayscale, deflate, adaptive, no interlace
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}

/// A one-page PDF whose only content is the line `CODE <code>` in Helvetica.
/// `code` must be ASCII letters and digits.
pub fn document_probe_pdf(code: &str) -> Vec<u8> {
    assert!(
        code.chars().all(|c| c.is_ascii_alphanumeric()),
        "probe code must be alphanumeric"
    );
    let content = format!("BT /F1 36 Tf 40 60 Td (CODE {code}) Tj ET");
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 400 144] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".to_string(),
        format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
    ];
    let mut out = String::from("%PDF-1.4\n");
    let mut offsets = Vec::with_capacity(objects.len());
    for (i, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.push_str(&format!("{} 0 obj\n{body}\nendobj\n", i + 1));
    }
    let xref = out.len();
    out.push_str(&format!(
        "xref\n0 {}\n0000000000 65535 f \n",
        objects.len() + 1
    ));
    for off in offsets {
        out.push_str(&format!("{off:010} 00000 n \n"));
    }
    out.push_str(&format!(
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
        objects.len() + 1
    ));
    out.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksums_match_known_vectors() {
        assert_eq!(crc32(b"IEND"), 0xAE42_6082);
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
    }

    /// Decode a PNG produced by `encode_gray_png` (stored blocks only), checking every CRC.
    fn decode(png: &[u8]) -> (u32, u32, Vec<u8>) {
        assert_eq!(&png[..8], &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
        let mut pos = 8;
        let (mut w, mut h, mut idat) = (0, 0, Vec::new());
        let mut kinds = Vec::new();
        while pos < png.len() {
            let len = u32::from_be_bytes(png[pos..pos + 4].try_into().unwrap()) as usize;
            let kind = &png[pos + 4..pos + 8];
            let data = &png[pos + 8..pos + 8 + len];
            let crc = u32::from_be_bytes(png[pos + 8 + len..pos + 12 + len].try_into().unwrap());
            assert_eq!(crc, crc32(&png[pos + 4..pos + 8 + len]));
            kinds.push(String::from_utf8_lossy(kind).into_owned());
            match kind {
                b"IHDR" => {
                    w = u32::from_be_bytes(data[..4].try_into().unwrap());
                    h = u32::from_be_bytes(data[4..8].try_into().unwrap());
                    assert_eq!(&data[8..], &[8, 0, 0, 0, 0]);
                }
                b"IDAT" => idat.extend_from_slice(data),
                _ => {}
            }
            pos += 12 + len;
        }
        assert_eq!(kinds, ["IHDR", "IDAT", "IEND"]);
        assert_eq!(&idat[..2], &[0x78, 0x01]);
        let (mut p, mut raw) = (2, Vec::new());
        loop {
            let last = idat[p] == 1;
            let len = u16::from_le_bytes([idat[p + 1], idat[p + 2]]);
            let nlen = u16::from_le_bytes([idat[p + 3], idat[p + 4]]);
            assert_eq!(len, !nlen);
            raw.extend_from_slice(&idat[p + 5..p + 5 + len as usize]);
            p += 5 + len as usize;
            if last {
                break;
            }
        }
        assert_eq!(
            u32::from_be_bytes(idat[p..p + 4].try_into().unwrap()),
            adler32(&raw)
        );
        let mut px = Vec::new();
        for row in raw.chunks(w as usize + 1) {
            assert_eq!(row[0], 0);
            px.extend_from_slice(&row[1..]);
        }
        (w, h, px)
    }

    #[test]
    fn probe_png_is_64_square_and_round_trips() {
        let png = vision_probe_png(47);
        let (w, h, px) = decode(&png);
        assert_eq!((w, h), (64, 64));
        assert_eq!(px, vision_probe_pixels(47));
        // Both digits are drawn: ink in the left and in the right half, white margins.
        let ink = |x0: usize, x1: usize| (0..64).any(|y| (x0..x1).any(|x| px[y * 64 + x] == 0));
        assert!(ink(4, 29) && ink(34, 59));
        assert!(!ink(0, 4) && !ink(59, 64) && !ink(29, 34));
        assert!(px[..14 * 64].iter().all(|p| *p == 255));
    }

    #[test]
    fn every_two_digit_number_gives_a_distinct_image() {
        let mut seen = std::collections::BTreeSet::new();
        for n in 10..=99u8 {
            assert!(seen.insert(vision_probe_png(n)), "duplicate image for {n}");
        }
    }

    #[test]
    fn multi_block_images_encode() {
        let (w, h) = (300u32, 300u32);
        let px: Vec<u8> = (0..w * h).map(|i| (i % 251) as u8).collect();
        let (dw, dh, back) = decode(&encode_gray_png(w, h, &px));
        assert_eq!((dw, dh), (w, h));
        assert_eq!(back, px);
    }

    #[test]
    fn probe_pdf_has_consistent_offsets() {
        let pdf = String::from_utf8(document_probe_pdf("4821")).unwrap();
        assert!(pdf.starts_with("%PDF-1.4\n") && pdf.ends_with("%%EOF\n"));
        assert!(pdf.contains("(CODE 4821) Tj"));
        let xref_at: usize = pdf
            .rsplit("startxref\n")
            .next()
            .unwrap()
            .lines()
            .next()
            .unwrap()
            .parse()
            .unwrap();
        assert!(pdf[xref_at..].starts_with("xref\n0 6\n"));
        let entries: Vec<usize> = pdf[xref_at..]
            .lines()
            .skip(3)
            .take(5)
            .map(|l| l[..10].parse().unwrap())
            .collect();
        for (i, off) in entries.iter().enumerate() {
            assert!(pdf[*off..].starts_with(&format!("{} 0 obj\n", i + 1)));
        }
    }
}
