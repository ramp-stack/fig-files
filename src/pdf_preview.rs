//! Bounded PDF rasterization. CHK owns the pages that display these images.
use prism::hardware::Image;

/// Render one zero-based page without opening files or following document links.
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub fn render(bytes: &[u8], page: usize) -> Result<(Image, usize), String> {
    use objc2_core_foundation::{CFData, CGPoint, CGRect, CGSize};
    use objc2_core_graphics::{
        CGBitmapContextCreate, CGColorSpace, CGContext, CGDataProvider, CGImageAlphaInfo,
        CGImageByteOrderInfo, CGPDFBox, CGPDFDocument, CGPDFPage, CGRectIntersection,
    };
    use prism::hardware::Pixel;

    if bytes.len() > crate::storage::MAX_FILE_SIZE {
        return Err("PDF previews support files up to 8 MiB.".into());
    }
    let data = CFData::from_bytes(bytes);
    let provider =
        CGDataProvider::with_cf_data(Some(&data)).ok_or("This PDF could not be read.")?;
    let document =
        CGPDFDocument::with_provider(Some(&provider)).ok_or("This file is not a readable PDF.")?;
    if !CGPDFDocument::is_unlocked(Some(&document)) {
        return Err("This PDF needs a password. Export it to open it in a PDF app.".into());
    }
    let count = CGPDFDocument::number_of_pages(Some(&document));
    if count == 0 {
        return Err("This PDF has no readable pages.".into());
    }
    if page >= count {
        return Err(format!("Choose a page from 1 to {count}."));
    }
    let page =
        CGPDFDocument::page(Some(&document), page + 1).ok_or("This PDF page could not be read.")?;
    let crop = CGRectIntersection(
        CGPDFPage::box_rect(Some(&page), CGPDFBox::CropBox),
        CGPDFPage::box_rect(Some(&page), CGPDFBox::MediaBox),
    );
    if !crop.origin.x.is_finite() || !crop.origin.y.is_finite() {
        return Err("This PDF page has invalid dimensions.".into());
    }
    let rotation = CGPDFPage::rotation_angle(Some(&page)).rem_euclid(360);
    let (width, height) = raster_size(crop.size.width, crop.size.height, rotation)?;
    let bounds = CGRect {
        origin: CGPoint { x: 0.0, y: 0.0 },
        size: CGSize {
            width: width as f64,
            height: height as f64,
        },
    };
    let space = CGColorSpace::new_device_rgb()
        .ok_or("The PDF preview's color space could not be created.")?;
    // Premultiplied RGBA over opaque white gives ordinary RGBA pixels. Keep
    // this allocation stable and inaccessible until its CGContext is dropped.
    let mut rgba = vec![255u8; width as usize * height as usize * 4];
    let bitmap_info = CGImageByteOrderInfo::Order32Big.0 | CGImageAlphaInfo::PremultipliedLast.0;
    let context = unsafe {
        // SAFETY: The buffer has width * height * 4 bytes, remains allocated
        // throughout drawing, and is neither moved nor read until drop below.
        CGBitmapContextCreate(
            rgba.as_mut_ptr().cast(),
            width as usize,
            height as usize,
            8,
            width as usize * 4,
            Some(&space),
            bitmap_info,
        )
    }
    .ok_or("The PDF preview could not be created.")?;
    CGContext::set_rgb_fill_color(Some(&context), 1.0, 1.0, 1.0, 1.0);
    CGContext::fill_rect(Some(&context), bounds);
    let (page_width, page_height) = if matches!(rotation, 90 | 270) {
        (crop.size.height, crop.size.width)
    } else {
        (crop.size.width, crop.size.height)
    };
    let scale = (width as f64 / page_width).min(height as f64 / page_height);
    let page_bounds = CGRect {
        origin: CGPoint { x: 0.0, y: 0.0 },
        size: CGSize {
            width: width as f64 / scale,
            height: height as f64 / scale,
        },
    };
    // Core Graphics' PDF fitting transform only scales down. Apply the pixel
    // scale separately so small pages also fill their preview. Its bitmap
    // storage already gives top-to-bottom Image rows; do not flip them again.
    CGContext::scale_ctm(Some(&context), scale, scale);
    let transform =
        CGPDFPage::drawing_transform(Some(&page), CGPDFBox::CropBox, page_bounds, 0, true);
    if ![
        transform.a,
        transform.b,
        transform.c,
        transform.d,
        transform.tx,
        transform.ty,
    ]
    .into_iter()
    .all(f64::is_finite)
    {
        return Err("This PDF page has invalid dimensions.".into());
    }
    CGContext::concat_ctm(Some(&context), transform);
    CGContext::draw_pdf_page(Some(&context), Some(&page));
    CGContext::flush(Some(&context));
    drop(context);
    let pixels = rgba
        .chunks_exact(4)
        .map(|pixel| Pixel {
            red: pixel[0],
            green: pixel[1],
            blue: pixel[2],
            alpha: pixel[3],
        })
        .collect();
    Ok((Image::new((width, height), pixels), count))
}

#[cfg(not(any(target_os = "macos", target_os = "ios")))]
pub fn render(_: &[u8], _: usize) -> Result<(Image, usize), String> {
    Err(
        "PDF preview is available on macOS and iOS. Export this PDF to open it in another app."
            .into(),
    )
}

#[cfg(any(target_os = "macos", target_os = "ios", test))]
fn raster_size(width: f64, height: f64, rotation: i32) -> Result<(u32, u32), String> {
    if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
        return Err("This PDF page has invalid dimensions.".into());
    }
    let (width, height) = if matches!(rotation.rem_euclid(360), 90 | 270) {
        (height, width)
    } else {
        (width, height)
    };
    let scale = (1600.0 / width.max(height)).min(2.0);
    let width = (width * scale).round().clamp(1.0, 1600.0) as u32;
    let height = (height * scale).round().clamp(1.0, 1600.0) as u32;
    Ok((width, height))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn dimensions_are_bounded_and_follow_page_rotation() {
        assert_eq!(raster_size(100.0, 200.0, 0).unwrap(), (200, 400));
        assert_eq!(raster_size(200.0, 100.0, 90).unwrap(), (200, 400));
        for (width, height) in [(1e12, 1e12), (1e12, 1.0), (1.0, 1e12)] {
            let (w, h) = raster_size(width, height, 0).unwrap();
            assert!(w > 0 && h > 0 && w <= 1600 && h <= 1600);
            assert!(u64::from(w) * u64::from(h) <= 4_000_000);
        }
        for (width, height) in [
            (0.0, 1.0),
            (-1.0, 1.0),
            (f64::NAN, 1.0),
            (1.0, f64::INFINITY),
        ] {
            assert!(raster_size(width, height, 0).is_err());
        }
    }

    #[cfg(any(target_os = "macos", target_os = "ios"))]
    pub(crate) fn fixture() -> Vec<u8> {
        // Four colored quadrants make vertical orientation and /Rotate visible
        // in pixel assertions; page one also has a nonzero crop-box origin.
        let portrait = "1 0 0 rg 10 120 50 100 re f\n0 1 0 rg 60 120 50 100 re f\n0 0 1 rg 10 20 50 100 re f\n1 1 0 rg 60 20 50 100 re f\n";
        let landscape = "1 0 0 rg 0 50 100 50 re f\n0 1 0 rg 100 50 100 50 re f\n0 0 1 rg 0 0 100 50 re f\n1 1 0 rg 100 0 100 50 re f\n";
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R 5 0 R 7 0 R] /Count 3 >>".into(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 120 240] /CropBox [10 20 110 220] /Resources << >> /Contents 4 0 R >>".into(),
            format!("<< /Length {} >>\nstream\n{portrait}endstream", portrait.len()),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Rotate 90 /Resources << >> /Contents 6 0 R >>".into(),
            format!("<< /Length {} >>\nstream\n{landscape}endstream", landscape.len()),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources << >> /Contents 8 0 R >>".into(),
            "<< /Length 0 >>\nstream\nendstream".into(),
        ];
        let mut bytes = b"%PDF-1.4\n".to_vec();
        let mut offsets = vec![0];
        for (index, object) in objects.iter().enumerate() {
            offsets.push(bytes.len());
            bytes.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", index + 1).as_bytes());
        }
        let xref = bytes.len();
        bytes.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len()).as_bytes(),
        );
        for offset in offsets.iter().skip(1) {
            bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        bytes.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                offsets.len()
            )
            .as_bytes(),
        );
        bytes
    }

    #[cfg(any(target_os = "macos", target_os = "ios"))]
    #[test]
    fn native_pdf_pages_preserve_crop_rotation_colors_and_white_background() {
        fn colors(image: &Image) -> Vec<[u8; 4]> {
            let (w, h) = image.dimensions;
            [
                (w / 4, h / 4),
                (w * 3 / 4, h / 4),
                (w / 4, h * 3 / 4),
                (w * 3 / 4, h * 3 / 4),
            ]
            .into_iter()
            .map(|(x, y)| {
                let pixel = image[(y * w + x) as usize];
                [pixel.red, pixel.green, pixel.blue, pixel.alpha]
            })
            .collect()
        }
        let bytes = fixture();
        let (first, count) = render(&bytes, 0).unwrap();
        assert_eq!(count, 3);
        assert_eq!(first.dimensions, (200, 400));
        assert_eq!(
            colors(&first),
            vec![
                [255, 0, 0, 255],
                [0, 255, 0, 255],
                [0, 0, 255, 255],
                [255, 255, 0, 255]
            ]
        );
        let (rotated, count) = render(&bytes, 1).unwrap();
        assert_eq!(count, 3);
        assert_eq!(rotated.dimensions, (200, 400));
        assert_eq!(
            colors(&rotated),
            vec![
                [0, 0, 255, 255],
                [255, 0, 0, 255],
                [255, 255, 0, 255],
                [0, 255, 0, 255]
            ]
        );
        let (blank, _) = render(&bytes, 2).unwrap();
        assert!(blank.iter().all(|pixel| *pixel
            == prism::hardware::Pixel {
                red: 255,
                green: 255,
                blue: 255,
                alpha: 255
            }));
        assert!(render(&bytes, 3).unwrap_err().contains("1 to 3"));
        assert!(
            render(&vec![0; crate::storage::MAX_FILE_SIZE + 1], 0)
                .unwrap_err()
                .contains("8 MiB")
        );
        assert!(
            render(b"not a PDF", 0)
                .unwrap_err()
                .contains("readable PDF")
        );
    }
}
