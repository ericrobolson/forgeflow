//! Blend a reference-guided face edit into exactly the user-selected region.
use crate::picker::{FaceEditSettings, Pick};

pub fn validate(pick: &Pick, settings: &FaceEditSettings) -> Result<[u32; 4], String> {
    let [x, y, width, height] = pick.region.ok_or("draw a box around the target face")?;
    if width < 4 || height < 4 || x.checked_add(width).is_none_or(|v| v > pick.width)
        || y.checked_add(height).is_none_or(|v| v > pick.height) {
        return Err("face region must be at least 4×4 pixels and inside the target image".into());
    }
    if settings.instruction.trim().is_empty() { return Err("the face edit instruction is empty".into()); }
    if !(1..=32).contains(&settings.steps) || !settings.blend.is_finite() || !(0.0..=1.0).contains(&settings.blend) || settings.feather > 64 || !settings.context.is_finite() || !(0.0..=1.0).contains(&settings.context) || !settings.strength.is_finite() || !(0.25..=1.0).contains(&settings.strength) {
        return Err("invalid face edit settings: steps 1–32, blend/context 0–1, denoising strength 0.25–1, feather 0–64 pixels".into());
    }
    Ok([x, y, width, height])
}

/// White marks editable pixels; black protects the surrounding target during sampling.
/// The same region and oval are used for final compositing at source resolution.
pub fn context_mask(pick: &Pick, settings: &FaceEditSettings) -> Result<image::GrayImage, String> {
    let [x, y, width, height] = validate(pick, settings)?;
    let [left, top, w, h] = context_pick(pick, settings)?.region.unwrap();
    let mut mask = image::GrayImage::new(w, h);
    let (rx, ry) = ((width - 1) as f32 / 2.0, (height - 1) as f32 / 2.0);
    for dy in 0..height {
        for dx in 0..width {
            if !settings.oval_mask || ((dx as f32 - rx) / rx).powi(2) + ((dy as f32 - ry) / ry).powi(2) < 1.0 {
                mask.put_pixel(x - left + dx, y - top + dy, image::Luma([255]));
            }
        }
    }
    Ok(mask)
}

pub fn composite(original: &image::DynamicImage, patch: &image::DynamicImage, pick: &Pick, settings: &FaceEditSettings) -> Result<image::DynamicImage, String> {
    let [x, y, width, height] = validate(pick, settings)?;
    if (original.width(), original.height()) != (pick.width, pick.height) {
        return Err("the target image dimensions changed after the region was selected".into());
    }
    let mut output = original.to_rgba8();
    let patch = patch.resize_exact(width, height, image::imageops::FilterType::Lanczos3).to_rgba8();
    for dy in 0..height {
        for dx in 0..width {
            let edge = if settings.oval_mask {
                let (rx, ry) = ((width - 1) as f32 / 2.0, (height - 1) as f32 / 2.0);
                let radius = (((dx as f32 - rx) / rx).powi(2) + ((dy as f32 - ry) / ry).powi(2)).sqrt();
                if radius >= 1.0 { continue; }
                (1.0 - radius) * rx.min(ry)
            } else { dx.min(dy).min(width - 1 - dx).min(height - 1 - dy) as f32 };
            let t = if settings.feather == 0 { 1.0 } else { (edge / settings.feather as f32).min(1.0) };
            let amount = t * t * (3.0 - 2.0 * t) * settings.blend * (patch[(dx, dy)][3] as f32 / 255.0);
            let pixel = output.get_pixel_mut(x + dx, y + dy);
            for channel in 0..3 {
                pixel[channel] = (pixel[channel] as f32 * (1.0 - amount) + patch[(dx, dy)][channel] as f32 * amount).round() as u8;
            }
        }
    }
    Ok(image::DynamicImage::ImageRgba8(output))
}

/// Add target context for the model without expanding the region we will change.
pub fn context_pick(pick: &Pick, settings: &FaceEditSettings) -> Result<Pick, String> {
    let [x, y, width, height] = validate(pick, settings)?;
    let margin = (width.max(height) as f32 * settings.context).round() as u32;
    let (left, top) = (x.saturating_sub(margin), y.saturating_sub(margin));
    let (right, bottom) = ((x + width).saturating_add(margin).min(pick.width), (y + height).saturating_add(margin).min(pick.height));
    Ok(Pick { region: Some([left, top, right - left, bottom - top]), ..pick.clone() })
}

pub fn composite_context(original: &image::DynamicImage, generated: &image::DynamicImage, pick: &Pick, settings: &FaceEditSettings) -> Result<image::DynamicImage, String> {
    let expanded = context_pick(pick, settings)?;
    let [left, top, width, height] = expanded.region.unwrap();
    let [x, y, w, h] = pick.region.unwrap();
    let resized = generated.resize_exact(width, height, image::imageops::FilterType::Lanczos3);
    let face = resized.crop_imm(x - left, y - top, w, h);
    composite(original, &face, pick, settings)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pick() -> Pick { Pick { width: 20, height: 20, region: Some([5, 5, 10, 10]), ..Pick::default() } }
    #[test]
    fn edits_only_the_region_and_preserves_alpha_and_feathered_edges() {
        let original = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(20, 20, image::Rgba([0, 0, 0, 90])));
        let patch = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(3, 3, image::Rgba([200, 100, 50, 255])));
        let settings = FaceEditSettings { feather: 2, oval_mask: false, ..FaceEditSettings::default() };
        let result = composite(&original, &patch, &pick(), &settings).unwrap().to_rgba8();
        for (x, y, pixel) in result.enumerate_pixels() {
            assert_eq!(pixel[3], 90);
            if !(5..15).contains(&x) || !(5..15).contains(&y) { assert_eq!(*pixel, image::Rgba([0, 0, 0, 90])); }
        }
        assert_eq!(result[(5, 5)], image::Rgba([0, 0, 0, 90]));
        assert_eq!(result[(7, 7)], image::Rgba([200, 100, 50, 90]));
        assert_eq!(result[(6, 6)], image::Rgba([100, 50, 25, 90]));
        let settings = FaceEditSettings { feather: 0, blend: 0.5, ..settings };
        let result = composite(&original, &patch, &pick(), &settings).unwrap().to_rgba8();
        assert_eq!(result[(5, 5)], image::Rgba([100, 50, 25, 90]));
    }
    #[test]
    fn oval_mask_preserves_corners_and_context_does_not_expand_the_edit() {
        let original = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(20, 20, image::Rgba([0, 0, 0, 255])));
        let patch = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(20, 20, image::Rgba([200, 100, 50, 255])));
        let settings = FaceEditSettings { feather: 0, ..Default::default() };
        let expanded = context_pick(&pick(), &settings).unwrap();
        assert_eq!(expanded.region, Some([0, 0, 20, 20]));
        let result = composite_context(&original, &patch, &pick(), &settings).unwrap().to_rgba8();
        assert_eq!(result[(5, 5)], image::Rgba([0, 0, 0, 255]));
        assert_eq!(result[(10, 10)], image::Rgba([200, 100, 50, 255]));
        assert_eq!(result[(1, 1)], image::Rgba([0, 0, 0, 255]));
    }

    #[test]
    fn rejects_out_of_bounds_boxes_and_invalid_parameters() {
        let settings = FaceEditSettings::default();
        let invalid = Pick { region: Some([u32::MAX, 0, 10, 10]), ..pick() };
        assert!(validate(&invalid, &settings).is_err());
        assert!(validate(&pick(), &FaceEditSettings { blend: f32::NAN, ..settings }).is_err());
    }

    #[test]
    fn sampling_mask_and_composite_agree_at_every_pixel() {
        for region in [[5, 5, 10, 10], [0, 0, 8, 12], [12, 12, 8, 8]] {
            for oval_mask in [true, false] {
                let pick = Pick { region: Some(region), ..pick() };
                let settings = FaceEditSettings { oval_mask, feather: 0, context: 0.8, ..Default::default() };
                let mask = context_mask(&pick, &settings).unwrap();
                let [left, top, w, h] = context_pick(&pick, &settings).unwrap().region.unwrap();
                assert_eq!(mask.dimensions(), (w, h));
                let original = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(20, 20, image::Rgba([0, 0, 0, 255])));
                let patch = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(w, h, image::Rgba([255, 255, 255, 255])));
                let result = composite_context(&original, &patch, &pick, &settings).unwrap().to_rgba8();
                for (x, y, pixel) in result.enumerate_pixels() {
                    let expected = x.checked_sub(left).zip(y.checked_sub(top)).filter(|&(x,y)| x < w && y < h)
                        .map_or(0, |(x,y)| mask[(x,y)][0]);
                    assert_eq!(pixel[0], expected, "region={region:?}, oval={oval_mask}, pixel={x},{y}");
                }
            }
        }
    }
}
