//! Kobo hardware descriptions shared by installation, runtime and host simulation.
//!
//! Model properties are cross-checked against reference/plato/crates/core/src/device.rs.
//! Display refresh and framebuffer identification remain FBInk's responsibility.
use std::fmt;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TouchProtocol {
    Single,
    MultitouchA,
    MultitouchB,
    MultitouchC,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Frontlight {
    None,
    Standard,
    NaturalTwoChannel,
    NaturalThreeChannel,
    Premixed { inverted_warmth: bool },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Model {
    TouchB,
    TouchC,
    Glo,
    Aura,
    AuraH2o,
    GloHd,
    AuraOne,
    AuraOneLimited,
    AuraEdition2,
    AuraEdition2V2,
    AuraH2oEdition2,
    AuraH2oEdition2V2,
    ClaraHd,
    Forma,
    Forma32,
    LibraH2o,
    Nia,
    Elipsa,
    Sage,
    Libra2,
    Clara2e,
    Elipsa2e,
    ClaraBw,
    ClaraBwTpv,
    ClaraColour,
    LibraColour,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Profile {
    pub model: Model,
    /// Numeric identifier in the sixth field of .kobo/version (not the Nxxx label).
    pub id: u16,
    pub product: &'static str,
    pub name: &'static str,
    pub width: u16,
    pub height: u16,
    pub dpi: u16,
    pub touch: TouchProtocol,
}

macro_rules! profiles {
    ($(($model:ident, $id:literal, $product:literal, $name:literal, $w:literal, $h:literal, $dpi:literal, $touch:ident)),* $(,)?) => {
        pub const PROFILES: &[Profile] = &[$(Profile {
            model: Model::$model, id: $id, product: $product, name: $name,
            width: $w, height: $h, dpi: $dpi, touch: TouchProtocol::$touch,
        }),*];
    };
}

profiles! {
    (TouchB, 310, "trilogy", "Kobo Touch B", 600, 800, 167, Single),
    (TouchC, 320, "trilogy", "Kobo Touch C", 600, 800, 167, Single),
    (Glo, 330, "kraken", "Kobo Glo", 758, 1024, 212, Single),
    (Aura, 360, "phoenix", "Kobo Aura", 758, 1024, 212, MultitouchA),
    (AuraH2o, 370, "dahlia", "Kobo Aura H2O", 1080, 1440, 265, MultitouchA),
    (GloHd, 371, "alyssum", "Kobo Glo HD", 1072, 1448, 300, MultitouchA),
    (AuraOne, 373, "daylight", "Kobo Aura ONE", 1404, 1872, 300, MultitouchA),
    (AuraEdition2, 375, "star", "Kobo Aura Edition 2", 758, 1024, 212, MultitouchA),
    (AuraH2oEdition2, 374, "snow", "Kobo Aura H2O Edition 2", 1080, 1440, 265, MultitouchB),
    (ClaraHd, 376, "nova", "Kobo Clara HD", 1072, 1448, 300, MultitouchB),
    (Forma, 377, "frost", "Kobo Forma", 1440, 1920, 300, MultitouchB),
    (AuraH2oEdition2V2, 378, "snow", "Kobo Aura H2O Edition 2 v2", 1080, 1440, 265, MultitouchB),
    (AuraEdition2V2, 379, "star", "Kobo Aura Edition 2 v2", 758, 1024, 212, MultitouchA),
    (Forma32, 380, "frost", "Kobo Forma 32GB", 1440, 1920, 300, MultitouchB),
    (AuraOneLimited, 381, "daylight", "Kobo Aura ONE Limited Edition", 1404, 1872, 300, MultitouchA),
    (Nia, 382, "luna", "Kobo Nia", 758, 1024, 212, MultitouchA),
    (Sage, 383, "cadmus", "Kobo Sage", 1440, 1920, 300, MultitouchC),
    (LibraH2o, 384, "storm", "Kobo Libra H2O", 1264, 1680, 300, MultitouchB),
    (Clara2e, 386, "goldfinch", "Kobo Clara 2E", 1072, 1448, 300, MultitouchB),
    (Elipsa, 387, "europa", "Kobo Elipsa", 1404, 1872, 227, MultitouchC),
    (Libra2, 388, "io", "Kobo Libra 2", 1264, 1680, 300, MultitouchC),
    (Elipsa2e, 389, "condor", "Kobo Elipsa 2E", 1404, 1872, 227, MultitouchC),
    (LibraColour, 390, "monza", "Kobo Libra Colour", 1264, 1680, 300, MultitouchB),
    (ClaraBw, 391, "spaBW", "Kobo Clara BW", 1072, 1448, 300, MultitouchB),
    (ClaraColour, 393, "spaColour", "Kobo Clara Colour", 1072, 1448, 300, MultitouchB),
    (ClaraBwTpv, 395, "spaBWTPV", "Kobo Clara BW TPV", 1072, 1448, 300, MultitouchB),
}

impl Profile {
    pub fn from_id(id: u16) -> Option<&'static Self> {
        PROFILES.iter().find(|profile| profile.id == id)
    }

    /// PRODUCT cannot distinguish hardware revisions. Use the version marker
    /// when present, and reject contradictory evidence rather than guessing.
    pub fn detect(product: &str, version: &str) -> Result<&'static Self, DetectionError> {
        if let Some(field) = version.trim().split(',').nth(5).filter(|field| !field.is_empty()) {
            let id = field.parse::<u16>().map_err(|_| DetectionError::InvalidMarker)?;
            let profile = Self::from_id(id).ok_or(DetectionError::UnknownModel(id))?;
            if !product.is_empty() && product != profile.product {
                return Err(DetectionError::ConflictingIdentity);
            }
            return Ok(profile);
        }
        let mut candidates = PROFILES.iter().filter(|profile| profile.product == product);
        let profile = candidates.next().ok_or(DetectionError::MissingIdentity)?;
        if candidates.next().is_some() {
            return Err(DetectionError::AmbiguousProduct);
        }
        Ok(profile)
    }

    pub fn frontlight(self) -> Frontlight {
        use Model::*;
        match self.model {
            TouchB | TouchC => Frontlight::None,
            AuraOne | AuraOneLimited => Frontlight::NaturalThreeChannel,
            AuraH2oEdition2 | AuraH2oEdition2V2 => Frontlight::NaturalTwoChannel,
            ClaraHd | Forma | Forma32 | LibraH2o | Libra2 | Clara2e | Elipsa2e | ClaraBw | ClaraBwTpv | ClaraColour | LibraColour => Frontlight::Premixed { inverted_warmth: true },
            Sage => Frontlight::Premixed { inverted_warmth: false },
            _ => Frontlight::Standard,
        }
    }

    pub fn has_page_buttons(self) -> bool {
        matches!(self.model, Model::Forma | Model::Forma32 | Model::LibraH2o | Model::Libra2 | Model::Sage | Model::LibraColour)
    }

    pub fn has_gyroscope(self) -> bool {
        self.has_page_buttons() || matches!(self.model, Model::Elipsa | Model::Elipsa2e)
    }

    pub fn has_colour_panel(self) -> bool {
        matches!(self.model, Model::ClaraColour | Model::LibraColour)
    }

    pub fn has_bluetooth(self) -> bool {
        matches!(self.model, Model::Elipsa | Model::Sage | Model::Libra2 | Model::Clara2e | Model::Elipsa2e | Model::ClaraBw | Model::ClaraBwTpv | Model::ClaraColour | Model::LibraColour)
    }

    pub fn needs_touch_reactivation(self) -> bool {
        matches!(self.model, Model::GloHd | Model::AuraH2o)
    }

    /// Raw panel translation in FBInk's canonical rotation space. These flags
    /// match FBInk's finger_trace implementation at the pinned source revision.
    pub fn touch_transform(self, canonical: u8) -> TouchTransform {
        let mut transform =
            TouchTransform { swap_axes: true, invert_x: !matches!(self.model, Model::AuraH2oEdition2 | Model::Libra2 | Model::Elipsa2e | Model::LibraColour), invert_y: matches!(self.model, Model::Elipsa2e | Model::LibraColour) };
        match canonical % 4 {
            1 => {
                transform.swap_axes = !transform.swap_axes;
                transform.invert_y = !transform.invert_y;
            }
            2 => {
                transform.invert_x = !transform.invert_x;
                transform.invert_y = !transform.invert_y;
            }
            3 => {
                transform.swap_axes = !transform.swap_axes;
                transform.invert_x = !transform.invert_x;
            }
            _ => {}
        }
        transform
    }

    /// The original Libra reports its landscape sensor directions in reverse.
    pub fn gyroscope_rotation(self, sensor: u8) -> u8 {
        if self.model == Model::LibraH2o { (sensor % 4) ^ 1 } else { (sensor % 4 + 3) % 4 }
    }

    pub fn invert_page_buttons(self, canonical: u8) -> bool {
        if !self.has_page_buttons() {
            return false;
        }
        if self.model == Model::LibraH2o { matches!(canonical % 4, 2 | 3) } else { matches!(canonical % 4, 1 | 2) }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TouchTransform {
    pub swap_axes: bool,
    pub invert_x: bool,
    pub invert_y: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DetectionError {
    MissingIdentity,
    InvalidMarker,
    UnknownModel(u16),
    AmbiguousProduct,
    ConflictingIdentity,
}

impl fmt::Display for DetectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownModel(id) => write!(f, "Kobo model {id} has no Bokheim device profile"),
            Self::MissingIdentity => f.write_str("Cannot identify the Kobo model"),
            Self::InvalidMarker => f.write_str("The Kobo version marker contains an invalid model identifier"),
            Self::AmbiguousProduct => f.write_str("The Kobo version marker is needed to identify this hardware revision"),
            Self::ConflictingIdentity => f.write_str("Kobo PRODUCT and version marker identify different devices"),
        }
    }
}

impl std::error::Error for DetectionError {}

pub fn firmware_supported(version: &str) -> bool {
    let mut parts = version.split('.');
    parts.next() == Some("4") && parts.next().and_then(|part| part.parse::<u16>().ok()).is_some_and(|minor| minor >= 6)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn usb_and_runtime_detection_agree_for_every_profile() {
        for profile in PROFILES {
            let marker = format!("serial,4.1.15,4.38.23828,unused,unused,{:013}", profile.id);
            assert_eq!(Profile::detect("", &marker).unwrap(), profile);
            assert_eq!(Profile::detect(profile.product, &marker).unwrap(), profile);
        }
    }

    #[test]
    fn unknown_and_ambiguous_devices_are_never_treated_as_a_touch_or_libra() {
        assert_eq!(Profile::detect("", "serial,os,4.38.1,x,x,999"), Err(DetectionError::UnknownModel(999)));
        assert_eq!(Profile::detect("io", "serial,os,4.38.1,x,x,376"), Err(DetectionError::ConflictingIdentity));
        assert_eq!(Profile::detect("snow", ""), Err(DetectionError::AmbiguousProduct));
        assert_eq!(Profile::detect("io", "").unwrap().model, Model::Libra2);
        assert!(Profile::detect("", "").is_err());
        assert!(Profile::detect("io", "serial,os,4.38.1,x,x,broken").is_err());
        // N418 is the Libra 2's retail label; its USB model identifier is 388.
        assert!(Profile::from_id(418).is_none());
    }

    #[test]
    fn buttonless_devices_and_lighting_families_are_explicit() {
        assert!(!Profile::from_id(376).unwrap().has_page_buttons());
        assert!(!Profile::from_id(386).unwrap().has_gyroscope());
        assert!(Profile::from_id(388).unwrap().has_page_buttons());
        assert_eq!(Profile::from_id(310).unwrap().frontlight(), Frontlight::None);
        assert_eq!(Profile::from_id(373).unwrap().frontlight(), Frontlight::NaturalThreeChannel);
        assert_eq!(Profile::from_id(374).unwrap().frontlight(), Frontlight::NaturalTwoChannel);
        assert_eq!(Profile::from_id(383).unwrap().frontlight(), Frontlight::Premixed { inverted_warmth: false });
    }

    #[test]
    fn rotation_and_touch_have_distinct_device_rules() {
        let libra = Profile::from_id(388).unwrap();
        assert_eq!(libra.touch_transform(0), TouchTransform { swap_axes: true, invert_x: false, invert_y: false });
        let clara = Profile::from_id(376).unwrap();
        assert_eq!(clara.touch_transform(0), TouchTransform { swap_axes: true, invert_x: true, invert_y: false });
        assert_ne!(libra.gyroscope_rotation(0), Profile::from_id(384).unwrap().gyroscope_rotation(0));
        for profile in PROFILES {
            let rotations: BTreeSet<_> = (0..4).map(|sensor| profile.gyroscope_rotation(sensor)).collect();
            assert_eq!(rotations.len(), 4);
            let transforms: BTreeSet<_> = (0..4)
                .map(|rotation| {
                    let transform = profile.touch_transform(rotation);
                    (transform.swap_axes, transform.invert_x, transform.invert_y)
                })
                .collect();
            assert_eq!(transforms.len(), 4);
        }
    }
}
