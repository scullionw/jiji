//! The pictures of an image diff: decoded off the UI thread through GPUI's
//! asset cache, only once a side is drawn, and the frame an animated one is
//! on. GPUI decodes its own formats; AVIF and HEIC go through the system's
//! ImageIO, as they did in the webview, so what previews follows what the
//! Mac can read.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::Duration;

use gpui::{App, Asset, Image, ImageFormat, RenderImage, SvgRenderer};

use crate::diff::model::format_bytes;

/// How a side's bytes decode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Format {
    /// One GPUI decodes.
    Gpui(ImageFormat),
    /// One the system decodes, by its uniform type identifier.
    System(&'static str),
}

/// How to decode what the backend sends as `mime`; `None` for a format
/// nothing here reads.
pub fn format_for(mime: &str) -> Option<Format> {
    Some(match mime {
        "image/png" => Format::Gpui(ImageFormat::Png),
        "image/jpeg" => Format::Gpui(ImageFormat::Jpeg),
        "image/gif" => Format::Gpui(ImageFormat::Gif),
        "image/webp" => Format::Gpui(ImageFormat::Webp),
        "image/bmp" => Format::Gpui(ImageFormat::Bmp),
        "image/tiff" => Format::Gpui(ImageFormat::Tiff),
        "image/x-icon" => Format::Gpui(ImageFormat::Ico),
        "image/avif" => Format::System("public.avif"),
        "image/heic" => Format::System("public.heic"),
        _ => return None,
    })
}

/// Why a side shows no picture. Each says so in its own words: a reader
/// should know whether to look at the file, the Mac or the change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Undrawable {
    /// The side is over the backend's per-image cap, and came without its
    /// bytes.
    TooLarge,
    /// The side fits the cap, but the change's earlier images used up what
    /// one diff carries.
    OverBudget,
    /// Nothing on this Mac decodes the format.
    Unsupported,
    /// The bytes did not decode.
    Corrupt,
}

impl Undrawable {
    /// What the side's box says, for a picture of type `mime`.
    pub fn note(self, mime: &str) -> String {
        match self {
            Undrawable::TooLarge => format!(
                "Too large to preview: over {}",
                format_bytes(jiji_core::jj::MAX_IMAGE_INLINE_BYTES as u64)
            ),
            Undrawable::OverBudget => format!(
                "Not previewed: this change's images pass {}",
                format_bytes(jiji_core::jj::MAX_IMAGE_TOTAL_BYTES as u64)
            ),
            Undrawable::Unsupported => {
                let kind = match mime {
                    "image/x-icon" => "ICO".to_owned(),
                    _ => mime.trim_start_matches("image/").to_uppercase(),
                };
                format!("No preview for {kind} on this Mac")
            }
            Undrawable::Corrupt => "This image could not be read".to_owned(),
        }
    }
}

/// One side's picture: its bytes, and how they decode.
#[derive(Clone)]
pub struct Picture {
    format: Format,
    bytes: Arc<[u8]>,
    /// The bytes' hash: what the asset cache knows the picture by.
    id: u64,
}

impl Hash for Picture {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.hash(state);
        self.format.hash(state);
    }
}

impl Picture {
    pub fn new(format: Format, bytes: Vec<u8>) -> Self {
        let mut hasher = DefaultHasher::new();
        bytes.hash(&mut hasher);
        Self {
            format,
            id: hasher.finish(),
            bytes: bytes.into(),
        }
    }

    fn decode(&self, svg: SvgRenderer) -> Result<Decoded, Undrawable> {
        let (format, bytes) = match self.format {
            Format::Gpui(format) => (format, self.bytes.to_vec()),
            Format::System(uti) => (ImageFormat::Png, image_io::png(&self.bytes, uti)?),
        };
        let image = Image::from_bytes(format, bytes)
            .to_image_data(svg)
            .map_err(|_| Undrawable::Corrupt)?;
        // Frames are BGRA: every fourth byte is a pixel's alpha.
        let opaque = (0..image.frame_count()).all(|frame| {
            image.as_bytes(frame).is_some_and(|pixels| {
                let (pixels, _) = pixels.as_chunks::<4>();
                pixels.iter().all(|pixel| pixel[3] == u8::MAX)
            })
        });
        Ok(Decoded { image, opaque })
    }
}

/// A decoded picture, and whether all of it covers what lies behind it.
#[derive(Clone)]
pub struct Decoded {
    pub image: Arc<RenderImage>,
    pub opaque: bool,
}

/// Decodes a side the first time it is drawn, on GPUI's background
/// threads, keeping the outcome for every later frame.
pub enum PictureLoader {}

impl Asset for PictureLoader {
    type Source = Picture;
    type Output = Result<Decoded, Undrawable>;

    fn load(
        source: Self::Source,
        cx: &mut App,
    ) -> impl std::future::Future<Output = Self::Output> + Send + 'static {
        let svg = cx.svg_renderer();
        async move { source.decode(svg) }
    }
}

/// Browsers hold a frame that asks for less than this for a tenth of a
/// second instead: a GIF with no delay set means "as fast as you like",
/// and played literally it spins.
const SHORTEST_FRAME: Duration = Duration::from_millis(11);
const STAND_IN_FRAME: Duration = Duration::from_millis(100);

/// The frame an animation shows `elapsed` into its loop, and how long it
/// holds from here; a still picture holds for good.
pub fn frame_at(image: &RenderImage, elapsed: Duration) -> (usize, Option<Duration>) {
    let count = image.frame_count();
    if count < 2 {
        return (0, None);
    }
    let delays: Vec<Duration> = (0..count)
        .map(|frame| match Duration::from(image.delay(frame)) {
            delay if delay < SHORTEST_FRAME => STAND_IN_FRAME,
            delay => delay,
        })
        .collect();
    let (frame, holds) = frame_in(&delays, elapsed);
    (frame, Some(holds))
}

fn frame_in(delays: &[Duration], elapsed: Duration) -> (usize, Duration) {
    let total: Duration = delays.iter().sum();
    let mut at = Duration::from_nanos((elapsed.as_nanos() % total.as_nanos().max(1)) as u64);
    for (frame, delay) in delays.iter().enumerate() {
        if at < *delay {
            return (frame, *delay - at);
        }
        at -= *delay;
    }
    (0, delays[0])
}

/// AVIF and HEIC through ImageIO, re-encoded as PNG for GPUI to decode.
#[cfg(target_os = "macos")]
mod image_io {
    use std::ffi::c_void;
    use std::ptr;
    use std::sync::OnceLock;

    use core_foundation::array::{CFArray, CFArrayRef};
    use core_foundation::base::{CFRelease, CFTypeRef, TCFType};
    use core_foundation::boolean::CFBoolean;
    use core_foundation::data::{CFData, CFDataRef};
    use core_foundation::dictionary::{CFDictionary, CFDictionaryRef};
    use core_foundation::string::{CFString, CFStringRef};

    use super::Undrawable;

    #[link(name = "ImageIO", kind = "framework")]
    extern "C" {
        fn CGImageSourceCopyTypeIdentifiers() -> CFArrayRef;
        fn CGImageSourceCreateWithData(data: CFDataRef, options: CFDictionaryRef) -> CFTypeRef;
        fn CGImageSourceCreateThumbnailAtIndex(
            source: CFTypeRef,
            index: usize,
            options: CFDictionaryRef,
        ) -> CFTypeRef;
        fn CGImageDestinationCreateWithData(
            data: CFTypeRef,
            kind: CFStringRef,
            count: usize,
            options: CFDictionaryRef,
        ) -> CFTypeRef;
        fn CGImageDestinationAddImage(
            destination: CFTypeRef,
            image: CFTypeRef,
            properties: CFDictionaryRef,
        );
        fn CGImageDestinationFinalize(destination: CFTypeRef) -> bool;
        static kCGImageSourceCreateThumbnailFromImageAlways: CFStringRef;
        static kCGImageSourceCreateThumbnailWithTransform: CFStringRef;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFDataCreateMutable(allocator: *const c_void, capacity: isize) -> CFDataRef;
    }

    /// A Core Foundation object this code made, released when dropped.
    struct Owned(CFTypeRef);

    impl Owned {
        fn new(object: CFTypeRef) -> Result<Self, Undrawable> {
            if object.is_null() {
                Err(Undrawable::Corrupt)
            } else {
                Ok(Self(object))
            }
        }
    }

    impl Drop for Owned {
        fn drop(&mut self) {
            unsafe { CFRelease(self.0) }
        }
    }

    /// Whether this Mac's ImageIO reads the format at all.
    fn readable(uti: &str) -> bool {
        static TYPES: OnceLock<Vec<String>> = OnceLock::new();
        TYPES
            .get_or_init(|| unsafe {
                let types: CFArray<CFString> =
                    CFArray::wrap_under_create_rule(CGImageSourceCopyTypeIdentifiers());
                types.iter().map(|uti| uti.to_string()).collect()
            })
            .iter()
            .any(|known| known == uti)
    }

    pub fn png(bytes: &[u8], uti: &str) -> Result<Vec<u8>, Undrawable> {
        if !readable(uti) {
            return Err(Undrawable::Unsupported);
        }
        unsafe {
            let data = CFData::from_buffer(bytes);
            let source = Owned::new(CGImageSourceCreateWithData(
                data.as_concrete_TypeRef(),
                ptr::null(),
            ))?;
            // A thumbnail is ImageIO's way to the picture turned as its
            // metadata says; asked for with no size cap, it is full size.
            let options = CFDictionary::from_CFType_pairs(&[
                (
                    CFString::wrap_under_get_rule(kCGImageSourceCreateThumbnailFromImageAlways),
                    CFBoolean::true_value(),
                ),
                (
                    CFString::wrap_under_get_rule(kCGImageSourceCreateThumbnailWithTransform),
                    CFBoolean::true_value(),
                ),
            ]);
            let image = Owned::new(CGImageSourceCreateThumbnailAtIndex(
                source.0,
                0,
                options.as_concrete_TypeRef(),
            ))?;
            let out = CFData::wrap_under_create_rule(CFDataCreateMutable(ptr::null(), 0));
            let png = CFString::from_static_string("public.png");
            let destination = Owned::new(CGImageDestinationCreateWithData(
                out.as_CFTypeRef(),
                png.as_concrete_TypeRef(),
                1,
                ptr::null(),
            ))?;
            CGImageDestinationAddImage(destination.0, image.0, ptr::null());
            if !CGImageDestinationFinalize(destination.0) {
                return Err(Undrawable::Corrupt);
            }
            drop(destination);
            Ok(out.bytes().to_vec())
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod image_io {
    use super::Undrawable;

    pub fn png(_bytes: &[u8], _uti: &str) -> Result<Vec<u8>, Undrawable> {
        Err(Undrawable::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny picture in one of the formats the backend sends, and what it
    /// decodes to: `clear.png` fades to transparent, `anim.*` has two
    /// frames.
    struct Fixture {
        bytes: &'static [u8],
        mime: &'static str,
        size: (i32, i32),
        frames: usize,
        opaque: bool,
    }

    const FIXTURES: [Fixture; 6] = [
        Fixture {
            bytes: include_bytes!("fixtures/clear.png"),
            mime: "image/png",
            size: (16, 16),
            frames: 1,
            opaque: false,
        },
        Fixture {
            bytes: include_bytes!("fixtures/anim.gif"),
            mime: "image/gif",
            size: (16, 16),
            frames: 2,
            opaque: true,
        },
        Fixture {
            bytes: include_bytes!("fixtures/anim.webp"),
            mime: "image/webp",
            size: (16, 16),
            frames: 2,
            opaque: true,
        },
        Fixture {
            bytes: include_bytes!("fixtures/icon.ico"),
            mime: "image/x-icon",
            size: (16, 16),
            frames: 1,
            opaque: true,
        },
        Fixture {
            bytes: include_bytes!("fixtures/photo.avif"),
            mime: "image/avif",
            size: (32, 32),
            frames: 1,
            opaque: true,
        },
        Fixture {
            bytes: include_bytes!("fixtures/photo.heic"),
            mime: "image/heic",
            size: (32, 32),
            frames: 1,
            opaque: true,
        },
    ];

    #[gpui::test]
    fn every_format_the_backend_sends_previews(cx: &mut gpui::TestAppContext) {
        let svg = cx.update(|cx| cx.svg_renderer());
        for Fixture {
            bytes,
            mime,
            size: (width, height),
            frames,
            opaque,
        } in FIXTURES
        {
            let picture = Picture::new(format_for(mime).expect(mime), bytes.to_vec());
            let decoded = picture
                .decode(svg.clone())
                .unwrap_or_else(|why| panic!("{mime}: {why:?}"));
            let size = decoded.image.size(0);
            assert_eq!((size.width.0, size.height.0), (width, height), "{mime}");
            assert_eq!(decoded.image.frame_count(), frames, "{mime}");
            assert_eq!(decoded.opaque, opaque, "{mime}");
            if frames > 1 {
                // Both animations hold each frame half a second.
                let half = Duration::from_millis(500);
                assert_eq!(
                    frame_at(&decoded.image, Duration::ZERO),
                    (0, Some(half)),
                    "{mime}"
                );
                assert_eq!(frame_at(&decoded.image, half).0, 1, "{mime}");
            }
        }
    }

    #[gpui::test]
    fn what_does_not_decode_and_what_nothing_reads_say_so(cx: &mut gpui::TestAppContext) {
        let svg = cx.update(|cx| cx.svg_renderer());
        // GPUI's decoder and the system's both refuse what is not a picture.
        for mime in ["image/png", "image/avif"] {
            let picture = Picture::new(format_for(mime).unwrap(), b"not a picture".to_vec());
            assert_eq!(
                picture.decode(svg.clone()).err(),
                Some(Undrawable::Corrupt),
                "{mime}"
            );
        }
        // A format this Mac has no decoder for, which is not the same.
        let unread = Picture::new(Format::System("public.no-such-picture"), b"x".to_vec());
        assert_eq!(unread.decode(svg).err(), Some(Undrawable::Unsupported));
        assert!(format_for("image/jxl").is_none());
    }

    #[test]
    fn an_animation_loops_through_its_frames_by_their_delays() {
        let delays = [Duration::from_millis(100), Duration::from_millis(200)];
        let ms = Duration::from_millis;
        assert_eq!(frame_in(&delays, ms(0)), (0, ms(100)));
        assert_eq!(frame_in(&delays, ms(99)), (0, ms(1)));
        assert_eq!(frame_in(&delays, ms(150)), (1, ms(150)));
        // Round again.
        assert_eq!(frame_in(&delays, ms(310)), (0, ms(90)));
    }

    #[test]
    fn each_reason_for_no_picture_reads_differently() {
        let notes: Vec<String> = [
            Undrawable::TooLarge,
            Undrawable::OverBudget,
            Undrawable::Unsupported,
            Undrawable::Corrupt,
        ]
        .into_iter()
        .map(|why| why.note("image/avif"))
        .collect();
        assert_eq!(notes[0], "Too large to preview: over 8.4 MB");
        assert_eq!(notes[1], "Not previewed: this change's images pass 16.8 MB");
        assert_eq!(notes[2], "No preview for AVIF on this Mac");
        assert_eq!(
            Undrawable::Unsupported.note("image/x-icon"),
            "No preview for ICO on this Mac"
        );
        assert_eq!(notes[3], "This image could not be read");
    }
}
