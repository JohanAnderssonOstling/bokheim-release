use super::*;

pub(super) struct Payload {
    pub bytes: Vec<u8>,
    pub filter: Vec<u8>,
    pub parameters: Dictionary,
}

/// Peel ordinary PDF filters in order, retaining the final image codec and its
/// own DecodeParms. A parameter array belongs to individual filters, not all of them.
pub(super) fn payload(document: &Document, stream: &Stream) -> Option<Payload> {
    let filters = match stream.dict.get(b"Filter") {
        Ok(value) => match resolve(document, value)? {
            Object::Array(values) => values.iter().map(|value| resolve(document, value)?.as_name().ok()).collect::<Option<Vec<_>>>()?,
            value => vec![value.as_name().ok()?],
        },
        Err(_) => Vec::new(),
    };
    if filters.len() > 8 {
        return None;
    }
    let parameters = match stream.dict.get(b"DecodeParms") {
        Ok(value) => Some(resolve(document, value)?),
        Err(_) => None,
    };
    let mut bytes = stream.content.clone();
    let limit = usize::try_from(MAX_COVER_IMAGE_PIXELS).ok()?.checked_mul(4)?.checked_add(MAX_COVER_IMAGE_DIMENSION as usize)?;
    if bytes.len() > limit {
        return None;
    }
    for (index, filter) in filters.iter().enumerate() {
        let parameter = match parameters {
            Some(Object::Array(values)) if values.len() == filters.len() => resolve(document, &values[index])?,
            Some(Object::Array(_)) => return None,
            Some(value) if filters.len() == 1 => value,
            None => &Object::Null,
            _ => return None,
        };
        let parameters = match parameter {
            Object::Null => Dictionary::new(),
            value => value.as_dict().ok()?.clone(),
        };
        if matches!(*filter, b"DCTDecode" | b"JPXDecode") {
            if index + 1 != filters.len() || bytes.len() > MAX_COVER_IMAGE_BYTES {
                return None;
            }
            return Some(Payload { bytes, filter: filter.to_vec(), parameters });
        }
        if !matches!(*filter, b"FlateDecode" | b"LZWDecode" | b"ASCII85Decode") {
            return None;
        }
        let mut dictionary = Dictionary::new();
        dictionary.set("Filter", Object::Name(filter.to_vec()));
        dictionary.set("DecodeParms", parameters);
        bytes = Stream::new(dictionary, bytes).get_plain_content_with_limit(limit).ok()?;
    }
    Some(Payload { bytes, filter: Vec::new(), parameters: Dictionary::new() })
}

pub(super) fn jpeg(payload: &Payload, width: u32, height: u32, channels: usize, reason: &mut &'static str) -> Option<Vec<u8>> {
    let limit = usize::try_from(u64::from(width) * u64::from(height)).ok()?.checked_mul(channels)?;
    let mut decoder = jpeg_decoder::Decoder::new(Cursor::new(&payload.bytes));
    decoder.set_max_decoding_buffer_size(limit);
    *reason = "JPEG header parsing";
    decoder.read_info().ok()?;
    *reason = "JPEG header dimensions";
    let info = decoder.info()?;
    if (u32::from(info.width), u32::from(info.height), info.pixel_format.pixel_bytes()) != (width, height, channels) {
        return None;
    }
    *reason = "JPEG ColorTransform parameter";
    if let Ok(transform) = payload.parameters.get(b"ColorTransform") {
        let transform = match (transform.as_i64().ok()?, channels) {
            (0, 3) => jpeg_decoder::ColorTransform::RGB,
            (0, 4) => jpeg_decoder::ColorTransform::CMYK,
            (1, 3) => jpeg_decoder::ColorTransform::YCbCr,
            (1, 4) => jpeg_decoder::ColorTransform::YCCK,
            _ => return None,
        };
        decoder.set_color_transform(transform);
    }
    *reason = "JPEG sample decoding";
    let mut samples = decoder.decode().ok()?;
    // Undo jpeg-decoder's CMYK polarity normalization before PDF /Decode.
    if channels == 4 {
        for sample in &mut samples {
            *sample = 255 - *sample;
        }
    }
    Some(samples)
}

pub(super) struct Jpx {
    pub samples: Vec<u8>,
    pub space: color::Space,
}
pub(super) fn jpeg2000(payload: &Payload, width: u32, height: u32) -> Option<Jpx> {
    let image = hayro_jpeg2000::Image::new(&payload.bytes, &Default::default()).ok()?;
    if image.has_alpha() || (image.width(), image.height()) != (width, height) {
        return None;
    }
    let space = match image.color_space() {
        hayro_jpeg2000::ColorSpace::Gray => color::Space::Device(1),
        hayro_jpeg2000::ColorSpace::RGB => color::Space::Device(3),
        hayro_jpeg2000::ColorSpace::CMYK => color::Space::Device(4),
        hayro_jpeg2000::ColorSpace::Icc { profile, num_channels } => color::Space::icc(profile, usize::from(*num_channels))?,
        hayro_jpeg2000::ColorSpace::Unknown { .. } => return None,
    };
    let channels = space.channels();
    let mut context = hayro_jpeg2000::DecoderContext::default();
    let decoded = image.decode(&mut context).ok()?;
    let data = decoded.data_u8();
    let pixels = usize::try_from(u64::from(width) * u64::from(height)).ok()?;
    if data.len() != pixels.checked_mul(channels + usize::from(image.has_alpha()))? {
        return None;
    }
    Some(Jpx { samples: data, space })
}
