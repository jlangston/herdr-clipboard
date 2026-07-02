use std::io;

/// Hard ceiling on decoded RGBA size (64 MP ≈ 256 MiB) — far above any real
/// screenshot, low enough that corrupt DB blobs can't force a huge allocation.
const MAX_DECODED_BYTES: usize = 256 * 1024 * 1024;

/// Encode raw RGBA8 pixels as PNG bytes.
pub fn encode_rgba_png(w: u32, h: u32, rgba: &[u8]) -> io::Result<Vec<u8>> {
    let expected = (w as usize)
        .checked_mul(h as usize)
        .and_then(|px| px.checked_mul(4))
        .ok_or_else(|| io::Error::other("image dimensions overflow"))?;
    if rgba.len() != expected {
        return Err(io::Error::other("rgba buffer does not match dimensions"));
    }
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().map_err(io::Error::other)?;
        writer.write_image_data(rgba).map_err(io::Error::other)?;
    }
    Ok(out)
}

/// Decode PNG bytes to (width, height, RGBA8). Only accepts RGBA8 — which is
/// all we ever encode; foreign PNGs are not a supported input.
pub fn decode_png(bytes: &[u8]) -> io::Result<(u32, u32, Vec<u8>)> {
    let mut decoder = png::Decoder::new(bytes);
    decoder.set_limits(png::Limits { bytes: MAX_DECODED_BYTES });
    let mut reader = decoder.read_info().map_err(io::Error::other)?;
    if reader.output_buffer_size() > MAX_DECODED_BYTES {
        return Err(io::Error::other("image dimensions exceed decode limit"));
    }
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).map_err(io::Error::other)?;
    if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
        return Err(io::Error::other("unsupported png format"));
    }
    buf.truncate(info.buffer_size());
    Ok((info.width, info.height, buf))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_roundtrip_preserves_pixels() {
        let (w, h) = (3u32, 2u32);
        let rgba: Vec<u8> = (0..(w * h * 4) as u8).collect();
        let png = encode_rgba_png(w, h, &rgba).unwrap();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        let (dw, dh, drgba) = decode_png(&png).unwrap();
        assert_eq!((dw, dh), (w, h));
        assert_eq!(drgba, rgba);
    }

    #[test]
    fn mismatched_buffer_and_garbage_are_errors() {
        assert!(encode_rgba_png(2, 2, &[0u8; 3]).is_err());
        assert!(decode_png(b"not a png").is_err());
    }

    #[test]
    fn huge_claimed_dimensions_error_without_allocating() {
        let mut bytes = Vec::new();
        {
            let enc = png::Encoder::new(&mut bytes, 100_000, 100_000);
            let _ = enc.write_header(); // header only — no pixel data needed
        }
        // The dropped Writer appends an IEND; without an IDAT the decoder
        // errors before decode_png's output allocation and the test would
        // pass vacuously. Swap the trailing IEND chunk for a bare IDAT
        // chunk header so decoding reaches the allocation path.
        assert_eq!(&bytes[bytes.len() - 8..bytes.len() - 4], b"IEND");
        bytes.truncate(bytes.len() - 12);
        bytes.extend_from_slice(&[0, 0, 0, 0]); // IDAT data length: 0
        bytes.extend_from_slice(b"IDAT");
        assert!(decode_png(&bytes).is_err());
    }
}
