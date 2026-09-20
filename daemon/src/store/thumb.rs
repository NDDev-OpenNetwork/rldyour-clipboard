//! Thumbnails for image entries.
//!
//! This is the only place the daemon decodes a picture, and it exists so that
//! nothing else has to. A shell extension that decoded a twenty-megabyte
//! screenshot would do it on the compositor's own thread, which is the single
//! thing this architecture is arranged to prevent. The daemon does it once, at
//! capture, on its own thread, and every later draw is a small PNG read.

/// The longest edge of a generated thumbnail, in pixels.
///
/// A list row draws at roughly ninety logical pixels; this covers that at a
/// 2x scale factor with room to spare, and still encodes to a few kilobytes.
pub const EDGE: u32 = 256;

/// Refuse to decode an image with more pixels than this.
///
/// A decoded frame costs four bytes a pixel whatever the encoded size, so a
/// small file can still describe an enormous allocation. Forty megapixels is
/// past any screenshot and well short of a problem.
#[cfg(feature = "thumbnails")]
const MAX_PIXELS: u64 = 40_000_000;

/// What could be learned about an image representation.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Thumbnail {
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// An encoded PNG, or `None` when one could not be produced.
    pub png: Option<Vec<u8>>,
}

/// Reads an image's dimensions and renders a thumbnail of it.
///
/// Never fails: an image this build cannot decode is simply an entry without a
/// thumbnail, and the UI draws a kind icon instead. A corrupt or hostile
/// payload must not cost the archive the entry.
#[cfg(feature = "thumbnails")]
pub fn make(content: &[u8]) -> Thumbnail {
    use image::ImageReader;
    use std::io::Cursor;

    let Ok(reader) = ImageReader::new(Cursor::new(content)).with_guessed_format() else {
        return Thumbnail::default();
    };
    let format = reader.format();
    let Ok((width, height)) = reader.into_dimensions() else {
        return Thumbnail::default();
    };

    // Dimensions are worth recording even when the thumbnail is not worth
    // making: the UI shows them, and they cost only a header read.
    let measured = Thumbnail {
        width: Some(width),
        height: Some(height),
        png: None,
    };

    if u64::from(width) * u64::from(height) > MAX_PIXELS {
        return measured;
    }

    let mut reader = ImageReader::new(Cursor::new(content));
    if let Some(format) = format {
        reader.set_format(format);
    }
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(MAX_PIXELS * 4);
    reader.limits(limits);

    let Ok(decoded) = reader.decode() else {
        return measured;
    };

    // `thumbnail` keeps the aspect ratio but scales in both directions, so an
    // image already inside the box would come back enlarged and blurry. A
    // thumbnail only ever shrinks.
    let scaled = if width <= EDGE && height <= EDGE {
        decoded
    } else {
        decoded.thumbnail(EDGE, EDGE)
    };
    let mut png = Vec::new();
    if scaled
        .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
        .is_err()
    {
        return measured;
    }

    Thumbnail {
        png: Some(png),
        ..measured
    }
}

/// Without the `thumbnails` feature the daemon never decodes an image, so it
/// reports neither dimensions nor a thumbnail and the UI draws a kind icon.
#[cfg(not(feature = "thumbnails"))]
pub fn make(_content: &[u8]) -> Thumbnail {
    Thumbnail::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny PNG built here rather than checked in, so the test explains what
    /// it is feeding the decoder.
    #[cfg(feature = "thumbnails")]
    fn png(width: u32, height: u32) -> Vec<u8> {
        use std::io::Cursor;
        let image = image::RgbaImage::from_fn(width, height, |x, y| {
            image::Rgba([(x % 256) as u8, (y % 256) as u8, 128, 255])
        });
        let mut out = Vec::new();
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    }

    #[test]
    #[cfg(feature = "thumbnails")]
    fn measures_an_image_and_shrinks_it_into_the_box() {
        let outcome = make(&png(800, 400));
        assert_eq!((outcome.width, outcome.height), (Some(800), Some(400)));

        let thumbnail = outcome.png.expect("a decodable png gets a thumbnail");
        let (width, height) = image::ImageReader::new(std::io::Cursor::new(&thumbnail))
            .with_guessed_format()
            .unwrap()
            .into_dimensions()
            .unwrap();
        // The long edge meets the box and the aspect ratio is kept.
        assert_eq!(width, EDGE);
        assert_eq!(height, EDGE / 2);
    }

    #[test]
    #[cfg(feature = "thumbnails")]
    fn leaves_an_image_smaller_than_the_box_at_its_own_size() {
        let outcome = make(&png(32, 16));
        let thumbnail = outcome.png.unwrap();
        let (width, height) = image::ImageReader::new(std::io::Cursor::new(&thumbnail))
            .with_guessed_format()
            .unwrap()
            .into_dimensions()
            .unwrap();
        assert_eq!((width, height), (32, 16), "never blown up");
    }

    #[test]
    fn content_that_is_not_an_image_yields_nothing_and_does_not_fail() {
        assert_eq!(make(b"not an image at all"), Thumbnail::default());
        assert_eq!(make(&[]), Thumbnail::default());
        // A truncated header is the shape a hostile payload takes.
        assert_eq!(make(&[0x89, b'P', b'N', b'G']), Thumbnail::default());
    }
}
