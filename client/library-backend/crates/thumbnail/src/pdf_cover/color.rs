use super::*;

#[derive(Clone)]
pub(super) enum Space {
    Device(usize),
    Indexed(Vec<[u8; 3]>),
    Icc(Box<moxcms::ColorProfile>, usize),
}
impl Space {
    pub(super) fn channels(&self) -> usize {
        match self {
            Self::Device(n) | Self::Icc(_, n) => *n,
            Self::Indexed(_) => 1,
        }
    }
    pub(super) fn map_samples(&self, samples: &mut [u8], bits: i64, mapping: Option<&[f64]>) -> Option<()> {
        let indexed = matches!(self, Self::Indexed(_));
        let channels = self.channels();
        if (indexed && bits > 8) || mapping.is_some_and(|values| values.len() != channels * 2) {
            return None;
        }
        if !indexed && mapping.is_none() {
            return Some(());
        }
        let default_max = if indexed { ((1_u32 << bits) - 1) as f64 } else { 1. };
        let scale = if indexed { 1. } else { 255. };
        for pixel in samples.chunks_exact_mut(channels) {
            for (i, sample) in pixel.iter_mut().enumerate() {
                let (min, max) = mapping.map_or((0., default_max), |values| (values[2 * i], values[2 * i + 1]));
                let value = min + f64::from(*sample) / 255. * (max - min);
                *sample = (value * scale).round().clamp(0., 255.) as u8;
            }
        }
        Some(())
    }
    pub(super) fn icc(bytes: &[u8], channels: usize) -> Option<Self> {
        if bytes.len() > 4 * 1024 * 1024 {
            return None;
        }
        let profile = moxcms::ColorProfile::new_from_slice(bytes).ok()?;
        let count = match profile.color_space {
            moxcms::DataColorSpace::Gray => 1,
            moxcms::DataColorSpace::Rgb => 3,
            moxcms::DataColorSpace::Cmyk => 4,
            _ => return None,
        };
        (channels == count).then_some(Self::Icc(Box::new(profile), channels))
    }
    pub(super) fn rgb(&self, samples: Vec<u8>, width: u32, height: u32) -> Option<RgbImage> {
        let pixels = usize::try_from(u64::from(width) * u64::from(height)).ok()?;
        if samples.len() != pixels.checked_mul(self.channels())? {
            return None;
        }
        match self {
            Self::Indexed(palette) => {
                let mut rgb = Vec::with_capacity(pixels * 3);
                for index in samples {
                    rgb.extend_from_slice(palette.get(usize::from(index).min(palette.len() - 1))?);
                }
                RgbImage::from_raw(width, height, rgb)
            }
            Self::Icc(profile, n) => {
                let layout = match n {
                    1 => moxcms::Layout::Gray,
                    3 => moxcms::Layout::Rgb,
                    4 => moxcms::Layout::Rgba,
                    _ => return None,
                };
                let transform = profile.create_transform_8bit(layout, &moxcms::ColorProfile::new_srgb(), moxcms::Layout::Rgb, Default::default()).ok()?;
                let mut rgb = vec![0; pixels.checked_mul(3)?];
                transform.transform(&samples, &mut rgb).ok()?;
                RgbImage::from_raw(width, height, rgb)
            }
            Self::Device(3) => RgbImage::from_raw(width, height, samples),
            Self::Device(1) => Some(DynamicImage::ImageLuma8(image::GrayImage::from_raw(width, height, samples)?).to_rgb8()),
            Self::Device(4) => {
                let mut rgb = Vec::with_capacity(pixels.checked_mul(3)?);
                for pixel in samples.chunks_exact(4) {
                    for channel in &pixel[..3] {
                        rgb.push(((255 - u16::from(*channel)) * (255 - u16::from(pixel[3])) / 255) as u8);
                    }
                }
                RgbImage::from_raw(width, height, rgb)
            }
            _ => None,
        }
    }
}

pub(super) fn resolve_space(document: &Document, resources: &Dictionary, object: &Object) -> Option<Space> {
    resolve_inner(document, resources, object, 0)
}
fn resolve_inner(document: &Document, resources: &Dictionary, object: &Object, depth: usize) -> Option<Space> {
    if depth > 8 {
        return None;
    }
    let mut object = object;
    let mut use_default = true;
    for _ in 0..32 {
        object = resolve(document, object)?;
        if let Ok(array) = object.as_array() {
            match array.first()?.as_name().ok()? {
                b"Indexed" if array.len() == 4 => {
                    let base = resolve_inner(document, resources, &array[1], depth + 1)?;
                    if matches!(base, Space::Indexed(_)) {
                        return None;
                    }
                    let high = resolve(document, &array[2])?.as_i64().ok()?;
                    if !(0..=255).contains(&high) {
                        return None;
                    }
                    let lookup = resolve(document, &array[3])?;
                    let bytes = match lookup {
                        Object::String(bytes, _) => bytes.clone(),
                        Object::Stream(stream) => stream.get_plain_content_with_limit(1024).ok()?,
                        _ => return None,
                    };
                    let length = (high as usize + 1) * base.channels();
                    let rgb = base.rgb(bytes.get(..length)?.to_vec(), high as u32 + 1, 1)?;
                    return Some(Space::Indexed(rgb.pixels().map(|p| p.0).collect()));
                }
                _ => (),
            }
            // CalGray, CalRGB, Lab and other unsupported spaces require PDFium.
            if array.len() != 2 || array[0].as_name().ok()? != b"ICCBased" {
                return None;
            }
            let profile = resolve(document, &array[1])?.as_stream().ok()?;
            let n = usize::try_from(profile.dict.get(b"N").ok()?.as_i64().ok()?).ok()?;
            return Space::icc(&profile.get_plain_content_with_limit(4 * 1024 * 1024).ok()?, n);
        }
        let name = object.as_name().ok()?;
        let device = match name {
            b"DeviceGray" => Some((1, b"DefaultGray".as_slice())),
            b"DeviceRGB" => Some((3, b"DefaultRGB".as_slice())),
            b"DeviceCMYK" => Some((4, b"DefaultCMYK".as_slice())),
            _ => None,
        };
        let spaces = match resources.get(b"ColorSpace") {
            Ok(value) => Some(resolve(document, value)?.as_dict().ok()?),
            Err(_) => None,
        };
        if let Some((n, default)) = device {
            if use_default {
                use_default = false;
                if let Some(value) = spaces.and_then(|s| s.get(default).ok()) {
                    object = value;
                    continue;
                }
            }
            return Some(Space::Device(n));
        }
        object = spaces?.get(name).ok()?;
    }
    None
}
