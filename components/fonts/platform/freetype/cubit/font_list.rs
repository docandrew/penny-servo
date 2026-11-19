/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! The font list on CuBit: the faces Penny links (cubit-fonts' bundled
//! faces, the ones every CuBit program draws its UI with), described by
//! their own tables (read-fonts). There is no fontconfig and no font
//! directory; the font store (UI-034) joins this list when sysmgr grants
//! Penny that place (docs/servo-port.md).

use std::sync::LazyLock;

use log::{debug, error};
use read_fonts::{FontRef, TableProvider};
use style::values::computed::font::GenericFontFamily;
use style::values::computed::{
    FontStyle as StyleFontStyle, FontWeight as StyleFontWeight, FontWidth as StyleFontWidth,
};

use crate::{
    CubitFace, FallbackFontSelectionOptions, FontIdentifier, FontTemplate,
    FontTemplateDescriptor, LocalFontIdentifier, LowercaseFontFamilyName,
};

static FONT_LIST: LazyLock<FontList> = LazyLock::new(FontList::new);

/// The name id of a font's typographic family (OpenType `name` table),
/// preferred over the legacy family, which splits weights into families.
const TYPOGRAPHIC_FAMILY_NAME: u16 = 16;
const LEGACY_FAMILY_NAME: u16 = 1;
/// OS/2 fsSelection bit 0: the face is italic.
const FS_SELECTION_ITALIC: u16 = 1;

struct Font {
    face: CubitFace,
    weight: StyleFontWeight,
    style: StyleFontStyle,
    width: StyleFontWidth,
}

struct FontFamily {
    name: String,
    fonts: Vec<Font>,
}

struct FontList {
    families: Vec<FontFamily>,
}

/// Why Penny has no font to draw text with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FontListError {
    /// None of the linked faces could be read.
    NoUsableFace,
}

/// CSS generic families and the families that serve them, in order of
/// preference. Penny links no serif face: serif text is set in the sans
/// face until the font store provides one.
const GENERICS: &[(&str, &[&str])] = &[
    ("sans-serif", &["IBM Plex Sans"]),
    ("serif", &["IBM Plex Sans"]),
    ("monospace", &["IBM Plex Mono"]),
];

/// The family every character falls back to.
const FALLBACK_FAMILY: &str = "IBM Plex Sans";

/// OS/2 usWidthClass values (1 ultra-condensed .. 9 ultra-expanded).
fn width_of_class(class: u16) -> StyleFontWidth {
    match class {
        1 => StyleFontWidth::ULTRA_CONDENSED,
        2 => StyleFontWidth::EXTRA_CONDENSED,
        3 => StyleFontWidth::CONDENSED,
        4 => StyleFontWidth::SEMI_CONDENSED,
        6 => StyleFontWidth::SEMI_EXPANDED,
        7 => StyleFontWidth::EXPANDED,
        8 => StyleFontWidth::EXTRA_EXPANDED,
        9 => StyleFontWidth::ULTRA_EXPANDED,
        _ => StyleFontWidth::NORMAL,
    }
}

fn describe(face: CubitFace) -> Option<(String, Font)> {
    let font = FontRef::new(face.bytes()).ok()?;
    let names = font.name().ok()?;
    let family = [TYPOGRAPHIC_FAMILY_NAME, LEGACY_FAMILY_NAME]
        .iter()
        .find_map(|&id| {
            names
                .name_record()
                .iter()
                .filter(|record| record.name_id().to_u16() == id)
                .find_map(|record| record.string(names.string_data()).ok())
                .map(|s| s.to_string())
        })?;
    let (weight, width, italic) = match font.os2() {
        Ok(os2) => (
            StyleFontWeight::from_float(os2.us_weight_class() as f32),
            width_of_class(os2.us_width_class()),
            os2.fs_selection().bits() & FS_SELECTION_ITALIC != 0,
        ),
        Err(_) => (StyleFontWeight::NORMAL, StyleFontWidth::NORMAL, false),
    };
    let italic = italic ||
        font
            .post()
            .is_ok_and(|post| post.italic_angle() != (0_i32).into());
    Some((
        family,
        Font {
            face,
            weight,
            style: if italic {
                StyleFontStyle::ITALIC
            } else {
                StyleFontStyle::NORMAL
            },
            width,
        },
    ))
}

impl FontList {
    fn new() -> FontList {
        let mut families: Vec<FontFamily> = vec![];
        for face in CubitFace::ALL {
            let Some((name, font)) = describe(face) else {
                error!("CuBit font list: the linked face {face:?} could not be read");
                continue;
            };
            debug!("Font {name:?}: {face:?}");
            match families
                .iter_mut()
                .find(|f| f.name.eq_ignore_ascii_case(&name))
            {
                Some(family) => family.fonts.push(font),
                None => families.push(FontFamily {
                    name,
                    fonts: vec![font],
                }),
            }
        }
        let list = FontList { families };
        if let Err(error) = list.status() {
            error!("CuBit font list: {error:?}: pages cannot show text");
        }
        list
    }

    fn status(&self) -> Result<usize, FontListError> {
        let faces = self.families.iter().map(|f| f.fonts.len()).sum();
        if faces == 0 {
            Err(FontListError::NoUsableFace)
        } else {
            Ok(faces)
        }
    }

    fn find_family(&self, name: &str) -> Option<&FontFamily> {
        self.families
            .iter()
            .find(|family| family.name.eq_ignore_ascii_case(name))
    }

    /// The family serving a CSS generic family name, if any.
    fn resolve_generic(&self, name: &str) -> Option<&FontFamily> {
        let (_, candidates) = GENERICS
            .iter()
            .find(|(generic, _)| generic.eq_ignore_ascii_case(name))?;
        candidates.iter().find_map(|c| self.find_family(c))
    }
}

/// The number of faces pages can be drawn with, or why there is none
/// (the embedder shows it to the user: no text is never silent).
pub fn status() -> Result<usize, FontListError> {
    FONT_LIST.status()
}

// Functions used by SystemFontService

pub(crate) fn for_each_available_family<F>(mut callback: F)
where
    F: FnMut(String),
{
    for family in &FONT_LIST.families {
        callback(family.name.clone());
    }
    for (generic, _) in GENERICS {
        if FONT_LIST.resolve_generic(generic).is_some() {
            callback(generic.to_string());
        }
    }
}

pub(crate) fn for_each_variation<F>(family_name: &str, mut callback: F)
where
    F: FnMut(FontTemplate),
{
    let Some(family) = FONT_LIST
        .find_family(family_name)
        .or_else(|| FONT_LIST.resolve_generic(family_name))
    else {
        return;
    };
    for font in &family.fonts {
        callback(FontTemplate::new(
            FontIdentifier::Local(LocalFontIdentifier { face: font.face }),
            FontTemplateDescriptor::new(font.weight, font.width, font.style),
            None,
        ));
    }
}

pub fn fallback_font_families(_options: FallbackFontSelectionOptions) -> Vec<&'static str> {
    vec![FALLBACK_FAMILY]
}

pub(crate) fn default_system_generic_font_family(
    generic: GenericFontFamily,
) -> LowercaseFontFamilyName {
    let name = match generic {
        GenericFontFamily::Monospace => "monospace",
        GenericFontFamily::Serif => "serif",
        _ => "sans-serif",
    };
    match FONT_LIST.resolve_generic(name) {
        Some(family) => family.name.as_str().into(),
        None => FALLBACK_FAMILY.into(),
    }
}
