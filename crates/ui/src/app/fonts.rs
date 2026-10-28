//! The UI font set.
//!
//! egui's bundled fonts (Ubuntu-Light, Hack, a monochrome emoji set) cover
//! Latin, Greek, Cyrillic and a few symbol ranges; every other character
//! drew as a box. Windows ships a UI font for each script it can display, so
//! those go after egui's in every family; on Linux the desktop's own fallback
//! chain from fontconfig does. egui draws each character with the first font
//! in the family that has it, so text that rendered before keeps its look and
//! only the former boxes change.
//!
//! The system fonts are memory-mapped, not read. The CJK faces alone are
//! ~70 MB; a mapped system font costs only the pages glyphs are actually
//! drawn from, and those pages are shared with every other process using
//! the font. A heap copy would cost all of it, twice: epaint clones owned
//! font bytes every time it rebuilds its font atlas.
//!
//! What this does not give: epaint draws one glyph per character with no
//! shaping, so scripts that need it (Arabic joining, right-to-left order,
//! Indic conjuncts) show their letters in isolated forms, and emoji built
//! from several code points (flags, skin tones, ZWJ families) show as their
//! parts. Emoji stay monochrome, tinted with the text colour: epaint
//! rasterizes glyph outlines only.

pub(crate) fn setup_fonts(ctx: &egui::Context) {
    ctx.set_fonts(font_definitions());
}

/// egui's fonts, then [`invisible_font`], then the system fallbacks, in
/// every family.
pub(crate) fn font_definitions() -> egui::FontDefinitions {
    let mut fonts = egui::FontDefinitions::default();
    let mut push = |key: &str, data: egui::FontData| {
        fonts.font_data.insert(key.to_owned(), std::sync::Arc::new(data));
        for family in fonts.families.values_mut() {
            family.push(key.to_owned());
        }
    };
    push("invisible", egui::FontData::from_static(invisible_font()));
    #[cfg(any(windows, target_os = "linux"))]
    for face in system::faces() {
        let mut data = egui::FontData::from_static(face.data);
        data.index = face.index;
        push(&face.key, data);
    }
    fonts
}

/// Code points that are never drawn themselves: variation selectors (they
/// pick the text or emoji form of the character before them, as in ❤️),
/// Mongolian free variation selectors, tag characters (subdivision flags)
/// and the Arabic letter mark. A shaper consumes them; epaint has none and
/// draws each with the font's own glyph, which is a box in most fonts and,
/// in Segoe UI Emoji, a blank 1.4 em wide.
const INVISIBLE: &[(u32, u32)] = &[
    (0x061C, 0x061C),
    (0x180B, 0x180F),
    (0xFE00, 0xFE0F),
    (0xE0000, 0xE0FFF), // tags and the variation selectors supplement
];

/// A generated font that maps every [`INVISIBLE`] code point to one empty
/// glyph of zero width. It goes ahead of the fallbacks, so those code
/// points take no space whatever font would otherwise have them. It has
/// only the tables a parser requires plus the character map; with no
/// outlines, nothing is drawn.
fn invisible_font() -> &'static [u8] {
    static FONT: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
    FONT.get_or_init(|| {
        fn u16s(v: &[u16]) -> Vec<u8> {
            v.iter().flat_map(|x| x.to_be_bytes()).collect()
        }
        let mut head = u16s(&[1, 0, 1, 0, 0, 0, 0x5F0F, 0x3CF5, 0, 1000]);
        head.resize(head.len() + 28, 0); // dates, bounding box, style, ppem
        head.extend(u16s(&[2, 0, 0])); // direction hint, short loca, glyf format
        let mut hhea = u16s(&[1, 0, 800, (-200i16) as u16]);
        hhea.resize(36, 0); // no horizontal metrics: every advance is 0
        let maxp = u16s(&[0, 0x5000, 2]); // two glyphs: .notdef and the empty one
        // One format 13 (many-to-one) subtable, Windows full-Unicode encoding.
        let mut cmap = u16s(&[0, 1, 3, 10, 0, 12, 13, 0]);
        let length = 16 + 12 * INVISIBLE.len() as u32;
        for x in [length, 0, INVISIBLE.len() as u32] {
            cmap.extend(x.to_be_bytes());
        }
        for &(first, last) in INVISIBLE {
            for x in [first, last, 1] {
                cmap.extend(x.to_be_bytes());
            }
        }

        let tables = [(*b"cmap", cmap), (*b"head", head), (*b"hhea", hhea), (*b"maxp", maxp)];
        let mut font = u16s(&[1, 0, tables.len() as u16, 0, 0, 0]);
        let mut offset = 12 + 16 * tables.len() as u32;
        for (tag, data) in &tables {
            font.extend(tag);
            for x in [0, offset, data.len() as u32] {
                font.extend(x.to_be_bytes());
            }
            offset += data.len().next_multiple_of(4) as u32;
        }
        for (_, data) in &tables {
            font.extend(data);
            font.resize(font.len().next_multiple_of(4), 0);
        }
        font
    })
}

#[cfg(windows)]
mod system {
    use std::sync::OnceLock;

    /// A Windows font face: its file under `%WINDIR%\Fonts`, and the full
    /// name of the face to take from it. A TrueType collection (`.ttc`)
    /// holds several faces; the "UI" ones share Segoe UI's vertical metrics,
    /// so mixed-script lines sit level.
    struct Face {
        file: &'static str,
        name: &'static str,
    }

    const fn face(file: &'static str, name: &'static str) -> Face {
        Face { file, name }
    }

    /// The fallbacks the app always had: Arabic, Hebrew, Armenian, Georgian,
    /// then the arrows and symbols (↶ ↷) Segoe UI lacks.
    const SEGOE: &[Face] = &[
        face("segoeui.ttf", "Segoe UI"),
        face("seguisym.ttf", "Segoe UI Symbol"),
    ];

    /// Han ideographs share code points across Japanese, Simplified and
    /// Traditional Chinese and Korean but each draws them differently, and
    /// whichever of these fonts comes first draws all the shared ones. Kana
    /// and Hangul only appear in their own language's font among these, so
    /// the order only decides how the ideographs look.
    #[derive(Clone, Copy, PartialEq, Debug)]
    pub(super) enum Cjk { Ja, Hans, Hant, Ko }

    impl Cjk {
        /// Japanese first when the user reads none of them: kanji then keep
        /// Japanese forms, and Chinese-only characters still fall through to
        /// the Chinese fonts.
        const DEFAULT_ORDER: [Cjk; 4] = [Cjk::Ja, Cjk::Hans, Cjk::Hant, Cjk::Ko];

        fn face(self) -> Face {
            match self {
                Cjk::Ja => face("YuGothM.ttc", "Yu Gothic UI"),
                Cjk::Hans => face("msyh.ttc", "Microsoft YaHei UI"),
                Cjk::Hant => face("msjh.ttc", "Microsoft JhengHei UI"),
                Cjk::Ko => face("malgun.ttf", "Malgun Gothic"),
            }
        }

        /// The CJK language a BCP-47 tag (`ja-JP`, `zh-Hant-TW`) names.
        pub(super) fn from_tag(tag: &str) -> Option<Cjk> {
            let tag = tag.to_ascii_lowercase();
            let mut parts = tag.split(['-', '_']);
            match parts.next()? {
                "ja" => Some(Cjk::Ja),
                "ko" => Some(Cjk::Ko),
                "zh" => {
                    let rest: Vec<&str> = parts.collect();
                    if rest.contains(&"hans") {
                        Some(Cjk::Hans)
                    } else if rest.iter().any(|p| matches!(*p, "hant" | "tw" | "hk" | "mo")) {
                        Some(Cjk::Hant)
                    } else {
                        Some(Cjk::Hans)
                    }
                }
                _ => None,
            }
        }
    }

    /// Every other script Windows has a UI font for, then historic scripts,
    /// math, the newer emoji egui's set lacks, and the rare CJK extension
    /// ideographs.
    const REST: &[Face] = &[
        face("Nirmala.ttc", "Nirmala UI"),          // Indic scripts
        face("LeelawUI.ttf", "Leelawadee UI"),      // Thai, Lao, Khmer
        face("ebrima.ttf", "Ebrima"),               // Ethiopic, N'Ko, Tifinagh, Vai
        face("gadugi.ttf", "Gadugi"),               // Cherokee, Canadian syllabics
        face("mmrtext.ttf", "Myanmar Text"),
        face("himalaya.ttf", "Microsoft Himalaya"), // Tibetan
        face("monbaiti.ttf", "Mongolian Baiti"),
        face("msyi.ttf", "Microsoft Yi Baiti"),
        face("ntailu.ttf", "Microsoft New Tai Lue"),
        face("taile.ttf", "Microsoft Tai Le"),
        face("phagspa.ttf", "Microsoft PhagsPa"),
        face("javatext.ttf", "Javanese Text"),
        face("seguihis.ttf", "Segoe UI Historic"),
        face("cambria.ttc", "Cambria Math"),
        // Its colour layers are lost (see the module docs), but every emoji
        // also has a plain outline, which is what gets drawn.
        face("seguiemj.ttf", "Segoe UI Emoji"),
        face("mingliub.ttc", "PMingLiU-ExtB"),
        face("simsunb.ttf", "SimSun-ExtB"),
        face("SimsunExtG.ttf", "SimSun-ExtG"),
    ];

    pub(super) struct Loaded {
        pub(super) key: String,
        pub(super) data: &'static [u8],
        pub(super) index: u32,
    }

    /// The fallback faces in order, each mapped once for the life of the
    /// process. Faces whose file is missing (fonts differ between Windows
    /// versions and editions) are left out.
    pub(super) fn faces() -> &'static [Loaded] {
        static FACES: OnceLock<Vec<Loaded>> = OnceLock::new();
        FACES.get_or_init(|| {
            let dir = std::env::var_os("WINDIR")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| r"C:\Windows".into())
                .join("Fonts");
            let cjk: Vec<Face> = cjk_order(&user_languages()).into_iter().map(Cjk::face).collect();
            SEGOE.iter().chain(&cjk).chain(REST).filter_map(|f| load(&dir, f)).collect()
        })
    }

    fn load(dir: &std::path::Path, face: &Face) -> Option<Loaded> {
        let data = map(&dir.join(face.file))?;
        let index = face_index(data, face.name)?;
        Some(Loaded { key: format!("win:{}", face.name), data, index })
    }

    /// Map a font file read-only and keep the mapping for the rest of the
    /// process. Writers are shut out while it is open, so the bytes cannot
    /// change under the mapping.
    fn map(path: &std::path::Path) -> Option<&'static [u8]> {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 1;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .open(path)
            .ok()?;
        // SAFETY: the file is open without write sharing, so nothing can
        // modify or truncate it while it is mapped, and the mapping is
        // leaked, so the slice stays valid for the life of the process.
        let mmap = unsafe { memmap2::Mmap::map(&file) }.ok()?;
        Some(&**Box::leak(Box::new(mmap)))
    }

    /// Index of the face named `name` in a font file (0 for a single-face
    /// file), or `None` when the file has no such face or does not parse.
    /// epaint panics on a font it cannot parse, so only faces that parse
    /// here are handed to it.
    fn face_index(data: &[u8], name: &str) -> Option<u32> {
        let count = ttf_parser::fonts_in_collection(data).unwrap_or(1);
        (0..count).find(|&i| {
            let Ok(face) = ttf_parser::Face::parse(data, i) else { return false };
            face.names().into_iter().any(|n| {
                n.name_id == ttf_parser::name_id::FULL_NAME
                    && n.to_string().is_some_and(|full| {
                        full == name || full.strip_suffix(" Regular") == Some(name)
                    })
            })
        })
    }

    /// The user's preferred CJK language first, then the rest in
    /// [`Cjk::DEFAULT_ORDER`].
    pub(super) fn cjk_order(languages: &[String]) -> Vec<Cjk> {
        let first = languages.iter().find_map(|l| Cjk::from_tag(l));
        first
            .into_iter()
            .chain(Cjk::DEFAULT_ORDER.into_iter().filter(|&c| Some(c) != first))
            .collect()
    }

    /// The user's Windows display languages in preference order, then their
    /// regional format, as BCP-47 tags.
    fn user_languages() -> Vec<String> {
        use windows_sys::Win32::Globalization::{
            GetUserDefaultLocaleName, GetUserPreferredUILanguages, MUI_LANGUAGE_NAME,
        };
        let mut out = Vec::new();
        // SAFETY: the first call only reports the buffer length; the second
        // writes at most `len` u16s into a buffer of exactly that length.
        unsafe {
            let (mut num, mut len) = (0u32, 0u32);
            if GetUserPreferredUILanguages(MUI_LANGUAGE_NAME, &mut num, std::ptr::null_mut(), &mut len) != 0
                && len > 0
            {
                let mut buf = vec![0u16; len as usize];
                if GetUserPreferredUILanguages(MUI_LANGUAGE_NAME, &mut num, buf.as_mut_ptr(), &mut len) != 0 {
                    out.extend(
                        buf.split(|&c| c == 0)
                            .filter(|s| !s.is_empty())
                            .map(String::from_utf16_lossy),
                    );
                }
            }
        }
        const LOCALE_NAME_MAX_LENGTH: usize = 85;
        let mut buf = [0u16; LOCALE_NAME_MAX_LENGTH];
        // SAFETY: writes at most `buf.len()` u16s, NUL included.
        let n = unsafe { GetUserDefaultLocaleName(buf.as_mut_ptr(), buf.len() as i32) };
        if n > 1 {
            out.push(String::from_utf16_lossy(&buf[..n as usize - 1]));
        }
        out
    }
}

#[cfg(target_os = "linux")]
mod system {
    use std::sync::OnceLock;

    pub(super) struct Loaded {
        pub(super) key: String,
        pub(super) data: &'static [u8],
        pub(super) index: u32,
    }

    /// The desktop's fallback chain for sans-serif, as fontconfig sorts it
    /// for the user's languages and font settings. `fc-match --sort` trims
    /// the list to fonts that add characters the ones before them lack, so
    /// this is the same chain GTK and Qt apps fall back through, CJK order
    /// included. Bold and italic faces are left out (they'd draw a fallback
    /// character heavier or slanted), and so are faces with no outlines,
    /// such as bitmap colour emoji, which epaint can't draw. Empty when
    /// fontconfig isn't installed.
    pub(super) fn faces() -> &'static [Loaded] {
        static FACES: OnceLock<Vec<Loaded>> = OnceLock::new();
        FACES.get_or_init(|| {
            let Ok(out) = std::process::Command::new("fc-match")
                .args(["--sort", "--format", "%{index}|%{file}\\n", "sans-serif"])
                .output()
            else {
                return Vec::new();
            };
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .filter_map(|line| {
                    let (index, file) = line.split_once('|')?;
                    load(std::path::Path::new(file), index.parse().ok()?)
                })
                .collect()
        })
    }

    fn load(path: &std::path::Path, index: u32) -> Option<Loaded> {
        let data = map(path)?;
        // epaint panics on a font it cannot parse, so only faces that parse
        // here are handed to it.
        let face = ttf_parser::Face::parse(data, index).ok()?;
        let tables = face.tables();
        let outlines = tables.glyf.is_some() || tables.cff.is_some() || tables.cff2.is_some();
        if !outlines || face.is_bold() || face.is_italic() || face.is_oblique() {
            return None;
        }
        Some(Loaded { key: format!("fc:{}#{index}", path.display()), data, index })
    }

    /// Map a root-owned font file read-only and keep the mapping for the
    /// rest of the process; read any other font into memory instead. Linux
    /// can't shut writers out the way Windows does, and touching a mapped
    /// file that was truncated kills the process (SIGBUS). A font the user
    /// installed can be overwritten in place; a root-owned one can't be by
    /// the user, and package managers replace files rather than rewrite
    /// them, which leaves the mapped original intact.
    fn map(path: &std::path::Path) -> Option<&'static [u8]> {
        use std::os::unix::fs::MetadataExt;
        let file = std::fs::File::open(path).ok()?;
        if file.metadata().ok()?.uid() != 0 {
            return Some(Vec::leak(std::fs::read(path).ok()?));
        }
        // SAFETY: the file is root-owned, so (see above) nothing running as
        // the user can truncate it under the mapping; the mapping is leaked,
        // so the slice stays valid for the life of the process.
        let mmap = unsafe { memmap2::Mmap::map(&file) }.ok()?;
        Some(&**Box::leak(Box::new(mmap)))
    }
}

#[cfg(test)]
mod tests {
    use super::font_definitions;

    fn with_fonts(f: impl FnOnce(&mut egui::epaint::text::FontsView<'_>, &egui::FontId)) {
        let mut fonts = egui::epaint::text::Fonts::new(2048, Default::default(), font_definitions());
        f(&mut fonts.with_pixels_per_point(1.0), &egui::FontId::proportional(13.0));
    }

    #[test]
    fn invisible_code_points_take_no_space() {
        with_fonts(|fonts, id| {
            for c in ['\u{061C}', '\u{180B}', '\u{FE0E}', '\u{FE0F}', '\u{E0067}', '\u{E0100}'] {
                assert!(fonts.has_glyph(id, c), "U+{:04X} unmapped", c as u32);
                assert_eq!(fonts.glyph_width(id, c), 0.0, "U+{:04X}", c as u32);
            }
            assert!(fonts.glyph_width(id, 'A') > 0.0);
        });
    }

    #[cfg(windows)]
    #[test]
    fn windows_fallbacks_cover_other_scripts() {
        with_fonts(|fonts, id| {
            // Japanese, Chinese, Korean, Devanagari, Thai, Ethiopic, an emoji
            // newer than egui's set, and the full-width plus Touch Zones uses.
            for c in ['あ', '这', '這', '한', 'क', 'ก', 'ሀ', '🫨', '＋'] {
                assert!(fonts.has_glyph(id, c), "{c} (U+{:04X}) has no font", c as u32);
            }
        });
    }

    /// The title bar's undo/redo arrows and the menu chevron, which egui's own
    /// fonts lack. Needs a desktop font set (DejaVu or Noto Symbols).
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_fallbacks_cover_ui_symbols() {
        with_fonts(|fonts, id| {
            for c in ['↶', '↷', '▾'] {
                assert!(fonts.has_glyph(id, c), "{c} (U+{:04X}) has no font", c as u32);
            }
        });
    }
}

#[cfg(all(test, windows))]
mod cjk_tests {
    use super::system::{cjk_order, Cjk};

    #[test]
    fn cjk_tags() {
        assert_eq!(Cjk::from_tag("ja-JP"), Some(Cjk::Ja));
        assert_eq!(Cjk::from_tag("ko-KR"), Some(Cjk::Ko));
        assert_eq!(Cjk::from_tag("zh-CN"), Some(Cjk::Hans));
        assert_eq!(Cjk::from_tag("zh-Hans-HK"), Some(Cjk::Hans));
        assert_eq!(Cjk::from_tag("zh-TW"), Some(Cjk::Hant));
        assert_eq!(Cjk::from_tag("zh-Hant"), Some(Cjk::Hant));
        assert_eq!(Cjk::from_tag("zh-HK"), Some(Cjk::Hant));
        assert_eq!(Cjk::from_tag("en-US"), None);
        assert_eq!(Cjk::from_tag("jv-Latn"), None);
    }

    #[test]
    fn first_cjk_language_leads() {
        let langs = |l: &[&str]| l.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(cjk_order(&langs(&["en-US", "de-DE"])), [Cjk::Ja, Cjk::Hans, Cjk::Hant, Cjk::Ko]);
        assert_eq!(cjk_order(&langs(&["en-US", "zh-TW", "ja-JP"])), [Cjk::Hant, Cjk::Ja, Cjk::Hans, Cjk::Ko]);
        assert_eq!(cjk_order(&langs(&["ko-KR"])), [Cjk::Ko, Cjk::Ja, Cjk::Hans, Cjk::Hant]);
    }
}
