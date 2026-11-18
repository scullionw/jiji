//! Jiji's design tokens: ten palettes (five dark, five light) plus the
//! structure every palette shares. Depth comes from four close surface
//! steps and hairlines; the one shadow belongs to surfaces that float.
//!
//!   bg0  the frame: title bar, rail, status bar
//!   bg1  the working panes: graph, diff, section pages
//!   bg2  surfaces above a pane: popovers, menus, file headers
//!   bg3  controls: buttons, active segments
//!
//! Text steps hold their contrast on every surface from bg0 to bg2 (text2
//! at 8:1 or better, text3 at 5.5:1 or better), so text3 is the quiet step,
//! never a faded one.
//!
//! bg0 is a clear step below bg1, so the frame reads as a frame around the
//! work. Hue is identity: six lane hues name workstreams (and, through
//! [`Theme::identity`], repositories and authors), status hues name
//! states, file hues name languages. Surfaces stay neutral.

use std::rc::Rc;

use gpui::{point, px, App, BoxShadow, Global, Hsla, Rgba, SharedString};

use crate::diff::highlight::Syn;
use crate::logic::inspect::Tone;

pub const TITLEBAR_H: f32 = 36.;
pub const STATUSBAR_H: f32 = 24.;
pub const RAIL_W: f32 = 44.;
pub const TOOLBAR_H: f32 = 36.;
/// A list row: one line of text with room to breathe.
pub const ROW_H: f32 = 28.;

pub const TEXT_XS: f32 = 11.;
pub const TEXT_S: f32 = 12.;
pub const TEXT_M: f32 = 13.;
pub const TEXT_L: f32 = 14.;
/// The title of what a pane is about.
pub const TEXT_HEAD: f32 = 16.;
pub const TEXT_XL: f32 = 20.;

pub const RADIUS_S: f32 = 4.;
pub const RADIUS_M: f32 = 6.;
pub const RADIUS_L: f32 = 10.;

/// The platform's own UI face.
pub const FONT_UI: &str = ".SystemUIFont";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Scheme {
    Dark,
    Light,
}

#[derive(Clone)]
pub struct Theme {
    pub id: &'static str,
    pub scheme: Scheme,

    pub bg0: Hsla,
    pub bg1: Hsla,
    pub bg2: Hsla,
    pub bg3: Hsla,
    pub bg_hover: Hsla,
    /// The row that is selected: neutral, a clear step past hover.
    pub selected: Hsla,
    /// Inputs and segmented tracks: recessed below the pane.
    pub bg_well: Hsla,
    /// A secondary button: a faint raise the pane shows through.
    pub btn_bg: Hsla,
    /// A secondary button under the pointer.
    pub btn_hover: Hsla,
    pub border1: Hsla,
    pub border2: Hsla,
    pub text1: Hsla,
    pub text2: Hsla,
    pub text3: Hsla,

    pub accent: Hsla,
    pub accent_strong: Hsla,
    pub accent_dim: Hsla,
    pub accent_contrast: Hsla,
    pub accent_hover: Hsla,

    pub working_copy: Hsla,
    pub ok: Hsla,
    pub warn: Hsla,
    pub danger: Hsla,
    pub danger_dim: Hsla,

    /// Workstream lanes: hue is identity. Slot 0 rides the accent.
    pub lanes: [Hsla; 6],

    pub add_fg: Hsla,
    pub add_bg: Hsla,
    pub add_gutter: Hsla,
    pub add_word: Hsla,
    pub del_fg: Hsla,
    pub del_bg: Hsla,
    pub del_gutter: Hsla,
    pub del_word: Hsla,
    pub gutter_surface: Hsla,
    pub recess: Hsla,
    pub file_head: Hsla,

    pub selection: Hsla,
    pub scrollbar: Hsla,
    pub scrollbar_hover: Hsla,
    /// The dimmer behind modal surfaces (palette, search).
    pub scrim: Hsla,

    shadow: (u32, f32, f32),
    syn: [Hsla; 8],
    pub mono: SharedString,
}

fn hex(rgb: u32) -> Hsla {
    gpui::rgb(rgb).into()
}

fn alpha(rgb: u32, a: f32) -> Hsla {
    Rgba {
        r: ((rgb >> 16) & 0xff) as f32 / 255.,
        g: ((rgb >> 8) & 0xff) as f32 / 255.,
        b: (rgb & 0xff) as f32 / 255.,
        a,
    }
    .into()
}

/// `color-mix(in srgb, a <pct>, b)` for opaque colors.
fn mix(a: u32, pct: f32, b: u32) -> Hsla {
    let ch = |shift: u32| {
        let x = ((a >> shift) & 0xff) as f32;
        let y = ((b >> shift) & 0xff) as f32;
        (x * pct + y * (1. - pct)) / 255.
    };
    Rgba {
        r: ch(16),
        g: ch(8),
        b: ch(0),
        a: 1.,
    }
    .into()
}

/// One palette as tokens.css states it; everything derived is resolved in
/// [`Pal::build`].
struct Pal {
    id: &'static str,
    label: &'static str,
    scheme: Scheme,
    bg: [u32; 4],
    hover: (u32, f32),
    /// Hairline color and its two strengths.
    border: (u32, f32, f32),
    text: [u32; 3],
    accent: u32,
    accent_strong: u32,
    accent_dim: f32,
    accent_contrast: u32,
    working_copy: u32,
    ok: u32,
    warn: u32,
    danger: u32,
    scrollbar: (u32, f32, f32),
    shadow: (u32, f32, f32),
}

const PALETTES: [Pal; 10] = [
    Pal {
        id: "midnight",
        label: "Midnight",
        scheme: Scheme::Dark,
        bg: [0x0c0d10, 0x17181d, 0x1e1f25, 0x272830],
        hover: (0xffffff, 0.045),
        border: (0xffffff, 0.125, 0.084),
        text: [0xeceef3, 0xb3b6c3, 0x9599a6],
        accent: 0x7078ee,
        accent_strong: 0xa6abff,
        accent_dim: 0.17,
        accent_contrast: 0xffffff,
        working_copy: 0x3fc6a4,
        ok: 0x4fb77a,
        warn: 0xd9a441,
        danger: 0xe5564e,
        scrollbar: (0xffffff, 0.13, 0.24),
        shadow: (0x000000, 0.55, 0.3),
    },
    Pal {
        id: "graphite",
        label: "Graphite",
        scheme: Scheme::Dark,
        bg: [0x0d0d0d, 0x171717, 0x1d1d1d, 0x272727],
        hover: (0xffffff, 0.045),
        border: (0xffffff, 0.138, 0.091),
        text: [0xececec, 0xb3b3b3, 0x949494],
        accent: 0x7d9fc8,
        accent_strong: 0xa8c1df,
        accent_dim: 0.17,
        accent_contrast: 0x0e1116,
        working_copy: 0x54c0a6,
        ok: 0x6cb685,
        warn: 0xd4a35a,
        danger: 0xdc6660,
        scrollbar: (0xffffff, 0.13, 0.24),
        shadow: (0x000000, 0.55, 0.3),
    },
    Pal {
        id: "abyss",
        label: "Abyss",
        scheme: Scheme::Dark,
        bg: [0x070b13, 0x0e1422, 0x131a2a, 0x1a2336],
        hover: (0x8cb4ff, 0.055),
        border: (0xa0beff, 0.163, 0.105),
        text: [0xdde6f5, 0xa2b1cb, 0x8492ab],
        accent: 0x3eb3dc,
        accent_strong: 0x74cdee,
        accent_dim: 0.16,
        accent_contrast: 0x04121a,
        working_copy: 0x46c78f,
        ok: 0x4fbd7f,
        warn: 0xd9a84f,
        danger: 0xe26a68,
        scrollbar: (0xaac8ff, 0.14, 0.26),
        shadow: (0x000000, 0.65, 0.35),
    },
    Pal {
        id: "moss",
        label: "Moss",
        scheme: Scheme::Dark,
        bg: [0x090d0a, 0x111713, 0x161e18, 0x1e2821],
        hover: (0xbee6c8, 0.045),
        border: (0xbee6c8, 0.138, 0.091),
        text: [0xe3ebe3, 0xa8b6a9, 0x889788],
        accent: 0x6fbf80,
        accent_strong: 0x97d6a3,
        accent_dim: 0.16,
        accent_contrast: 0x09130b,
        working_copy: 0x6aaede,
        ok: 0x66b878,
        warn: 0xd3a356,
        danger: 0xdd6b62,
        scrollbar: (0xffffff, 0.13, 0.24),
        shadow: (0x000000, 0.55, 0.3),
    },
    Pal {
        id: "ember",
        label: "Ember",
        scheme: Scheme::Dark,
        bg: [0x0d0b0a, 0x181413, 0x1f1a18, 0x29221f],
        hover: (0xffdcb4, 0.045),
        border: (0xffdcb4, 0.125, 0.084),
        text: [0xede7df, 0xbbafa3, 0x9b9085],
        accent: 0xe2844f,
        accent_strong: 0xf1a67a,
        accent_dim: 0.16,
        accent_contrast: 0x170d07,
        working_copy: 0x4ec0ae,
        ok: 0x74b87b,
        warn: 0xd4a44e,
        danger: 0xe25d55,
        scrollbar: (0xffffff, 0.13, 0.24),
        shadow: (0x000000, 0.55, 0.3),
    },
    Pal {
        id: "paper",
        label: "Paper",
        scheme: Scheme::Light,
        bg: [0xeeeef2, 0xffffff, 0xffffff, 0xffffff],
        hover: (0x141628, 0.045),
        border: (0x141628, 0.154, 0.096),
        text: [0x1b1c22, 0x42454e, 0x595c66],
        accent: 0x4e54d6,
        accent_strong: 0x3b40c2,
        accent_dim: 0.10,
        accent_contrast: 0xffffff,
        working_copy: 0x007c61,
        ok: 0x077e41,
        warn: 0x966300,
        danger: 0xcf2d2d,
        scrollbar: (0x141628, 0.18, 0.32),
        shadow: (0x141628, 0.22, 0.08),
    },
    Pal {
        id: "linen",
        label: "Linen",
        scheme: Scheme::Light,
        bg: [0xefe9e0, 0xfbf8f3, 0xfffdf9, 0xfffdf9],
        hover: (0x503c1e, 0.05),
        border: (0x503c1e, 0.165, 0.102),
        text: [0x221d16, 0x4c4337, 0x645b4e],
        accent: 0xaa4f2c,
        accent_strong: 0x9d4a2b,
        accent_dim: 0.10,
        accent_contrast: 0xffffff,
        working_copy: 0x007766,
        ok: 0x33782c,
        warn: 0x8d6100,
        danger: 0xbd3c2e,
        scrollbar: (0x503c1e, 0.20, 0.34),
        shadow: (0x463214, 0.22, 0.08),
    },
    Pal {
        id: "glacier",
        label: "Glacier",
        scheme: Scheme::Light,
        bg: [0xe9eef4, 0xf8fafc, 0xffffff, 0xffffff],
        hover: (0x143c6e, 0.045),
        border: (0x143c6e, 0.165, 0.102),
        text: [0x16202c, 0x38485c, 0x516072],
        accent: 0x1b6cc2,
        accent_strong: 0x185aa2,
        accent_dim: 0.10,
        accent_contrast: 0xffffff,
        working_copy: 0x007970,
        ok: 0x027c41,
        warn: 0x936200,
        danger: 0xc63638,
        scrollbar: (0x143c6e, 0.18, 0.32),
        shadow: (0x143264, 0.22, 0.08),
    },
    Pal {
        id: "meadow",
        label: "Meadow",
        scheme: Scheme::Light,
        bg: [0xeaeee7, 0xf9fbf8, 0xffffff, 0xffffff],
        hover: (0x1e4628, 0.045),
        border: (0x1e4628, 0.165, 0.102),
        text: [0x1a211b, 0x3d493f, 0x566157],
        accent: 0x277a4b,
        accent_strong: 0x216640,
        accent_dim: 0.10,
        accent_contrast: 0xffffff,
        working_copy: 0x007591,
        ok: 0x1c7b3e,
        warn: 0x926200,
        danger: 0xc23b35,
        scrollbar: (0x1e4628, 0.18, 0.32),
        shadow: (0x193c23, 0.22, 0.08),
    },
    Pal {
        id: "dawn",
        label: "Dawn",
        scheme: Scheme::Light,
        bg: [0xf2eaec, 0xfcf8f9, 0xffffff, 0xffffff],
        hover: (0x5a283c, 0.045),
        border: (0x5a283c, 0.165, 0.102),
        text: [0x251c1f, 0x4f4247, 0x685a5f],
        accent: 0xad3564,
        accent_strong: 0x922a53,
        accent_dim: 0.10,
        accent_contrast: 0xffffff,
        working_copy: 0x00786b,
        ok: 0x227a40,
        warn: 0x946000,
        danger: 0xc5362f,
        scrollbar: (0x5a283c, 0.18, 0.32),
        shadow: (0x502337, 0.22, 0.08),
    },
];

pub const DEFAULT_DARK: &str = "midnight";
pub const DEFAULT_LIGHT: &str = "paper";

impl Pal {
    fn build(&self, mono: SharedString) -> Theme {
        let [bg0, bg1, bg2, bg3] = self.bg;
        let dark = self.scheme == Scheme::Dark;
        let text3 = hex(self.text[2]);
        // The add/remove pair and syntax set are per scheme, not per theme.
        let (add, del) = if dark {
            (0x38aa5a, 0xec544a)
        } else {
            (0x2ea44f, 0xe6463c)
        };
        let syn = if dark {
            [
                hex(0xd2a0eb),
                hex(0x9fc17e),
                hex(0xd9a86b),
                hex(0x6fc0d0),
                hex(0x89b6f0),
                hex(0x9cb4d6),
                text3,
                hex(0xd7aa86),
            ]
        } else {
            [
                hex(0x8039a8),
                hex(0x2c6700),
                hex(0x864c00),
                hex(0x006278),
                hex(0x2656af),
                hex(0x435a83),
                text3,
                hex(0x814e11),
            ]
        };
        // Lines on a transit map: six hues of one weight, none of them the
        // working copy's green or a conflict's red.
        let lanes = if dark {
            [
                hex(self.accent),
                hex(0x4aa8f0),
                hex(0xc07cf0),
                hex(0xf06f9c),
                hex(0xe9a23b),
                hex(0x93c94a),
            ]
        } else {
            [
                hex(self.accent),
                hex(0x1a7cc4),
                hex(0x9442c4),
                hex(0xc8336c),
                hex(0x9a6a00),
                hex(0x4f8a12),
            ]
        };
        Theme {
            id: self.id,
            scheme: self.scheme,
            bg0: hex(bg0),
            bg1: hex(bg1),
            bg2: hex(bg2),
            bg3: hex(bg3),
            bg_hover: alpha(self.hover.0, self.hover.1),
            selected: alpha(self.hover.0, self.hover.1 * 2.),
            bg_well: mix(bg0, if dark { 0.70 } else { 0.55 }, bg1),
            btn_bg: if dark {
                alpha(self.text[0], 0.05)
            } else {
                hex(bg3)
            },
            btn_hover: if dark {
                alpha(self.text[0], 0.10)
            } else {
                mix(bg3, 0.93, self.text[0])
            },
            border1: alpha(self.border.0, self.border.1),
            border2: alpha(self.border.0, self.border.2),
            text1: hex(self.text[0]),
            text2: hex(self.text[1]),
            text3,
            accent: hex(self.accent),
            accent_strong: hex(self.accent_strong),
            accent_dim: alpha(self.accent, self.accent_dim),
            accent_contrast: hex(self.accent_contrast),
            accent_hover: mix(self.accent, 0.88, self.text[0]),
            working_copy: hex(self.working_copy),
            ok: hex(self.ok),
            warn: hex(self.warn),
            danger: hex(self.danger),
            danger_dim: alpha(self.danger, 0.14),
            lanes,
            add_fg: hex(if dark { 0x57c97a } else { 0x00671f }),
            add_bg: alpha(add, if dark { 0.14 } else { 0.12 }),
            add_gutter: mix(add, if dark { 0.22 } else { 0.18 }, bg1),
            add_word: alpha(add, if dark { 0.36 } else { 0.30 }),
            del_fg: hex(if dark { 0xfb7c74 } else { 0xb1101a }),
            del_bg: alpha(del, if dark { 0.13 } else { 0.10 }),
            del_gutter: mix(del, if dark { 0.20 } else { 0.15 }, bg1),
            del_word: alpha(del, if dark { 0.36 } else { 0.26 }),
            gutter_surface: mix(bg0, if dark { 0.45 } else { 0.40 }, bg1),
            recess: mix(bg0, if dark { 0.60 } else { 0.65 }, bg1),
            file_head: if dark { hex(bg2) } else { mix(bg0, 0.45, bg1) },
            // Strong enough to read as "selected", not as a mystery tint.
            selection: alpha(self.accent, 0.32),
            scrollbar: alpha(self.scrollbar.0, self.scrollbar.1),
            scrollbar_hover: alpha(self.scrollbar.0, self.scrollbar.2),
            scrim: alpha(bg0, 0.6),
            shadow: self.shadow,
            syn,
            mono,
        }
    }
}

/// Picker metadata for one palette, without building it.
pub struct ThemeDef {
    pub id: &'static str,
    pub label: &'static str,
    pub scheme: Scheme,
    pub bg: Hsla,
    pub accent: Hsla,
}

pub fn theme_defs() -> Vec<ThemeDef> {
    PALETTES
        .iter()
        .map(|p| ThemeDef {
            id: p.id,
            label: p.label,
            scheme: p.scheme,
            bg: hex(p.bg[1]),
            accent: hex(p.accent),
        })
        .collect()
}

pub fn scheme_of(id: &str) -> Option<Scheme> {
    PALETTES.iter().find(|p| p.id == id).map(|p| p.scheme)
}

impl Theme {
    /// The color a status tone reads in.
    pub fn tone(&self, tone: Tone) -> Hsla {
        match tone {
            Tone::Ok => self.ok,
            Tone::Warn => self.warn,
            Tone::Danger => self.danger,
            Tone::Muted => self.text3,
        }
    }

    /// The palette named `id`; Midnight when the id is unknown.
    pub fn named(id: &str, mono: SharedString) -> Self {
        PALETTES
            .iter()
            .find(|p| p.id == id)
            .unwrap_or(&PALETTES[0])
            .build(mono)
    }

    /// The color a syntax class paints in; `None` for the classes that only
    /// change weight or slant (markdown structure).
    pub fn syn(&self, class: Syn) -> Option<Hsla> {
        Some(match class {
            Syn::Kw => self.syn[0],
            Syn::Str => self.syn[1],
            Syn::Const => self.syn[2],
            Syn::Type => self.syn[3],
            Syn::Fn | Syn::Link => self.syn[4],
            Syn::Prop => self.syn[5],
            Syn::Comment => self.syn[6],
            Syn::Meta => self.syn[7],
            Syn::Heading | Syn::Em | Syn::Strong => return None,
        })
    }

    /// A workstream's hue: streams take a slot in snapshot order and cycle.
    pub fn lane(&self, slot: usize) -> Hsla {
        self.lanes[slot % self.lanes.len()]
    }

    /// A stable hue for a name — a repository, an author — drawn from the
    /// lane set so identity colors and lines read as one family.
    pub fn identity(&self, key: &str) -> Hsla {
        self.lane(identity_slot(key))
    }

    pub fn is_dark(&self) -> bool {
        self.scheme == Scheme::Dark
    }

    /// The one shadow: floating surfaces only.
    pub fn popover_shadow(&self) -> Vec<BoxShadow> {
        let (rgb, strong, soft) = self.shadow;
        vec![
            BoxShadow {
                color: alpha(rgb, strong),
                offset: point(px(0.), px(12.)),
                blur_radius: px(32.),
                spread_radius: px(-8.),
                inset: false,
            },
            BoxShadow {
                color: alpha(rgb, soft),
                offset: point(px(0.), px(2.)),
                blur_radius: px(6.),
                spread_radius: px(0.),
                inset: false,
            },
        ]
    }

    /// `color-mix(in srgb, a <pct>, transparent)`.
    pub fn fade(color: Hsla, pct: f32) -> Hsla {
        Hsla {
            a: color.a * pct,
            ..color
        }
    }

    /// `color-mix(in srgb, a <pct>, b)` for two opaque colors.
    pub fn blend(a: Hsla, pct: f32, b: Hsla) -> Hsla {
        let (a, b) = (Rgba::from(a), Rgba::from(b));
        Rgba {
            r: a.r * pct + b.r * (1. - pct),
            g: a.g * pct + b.g * (1. - pct),
            b: a.b * pct + b.b * (1. - pct),
            a: 1.,
        }
        .into()
    }
}

/// FNV-1a over the name's bytes: the same name lands on the same slot on
/// every run and every machine.
pub fn identity_slot(key: &str) -> usize {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in key.bytes() {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash as usize
}

/// A file's language family, for the hue its icon wears. The hues follow
/// the colors these languages are known by (GitHub's linguist set), each
/// pitched for the scheme.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FileHue {
    Rust,
    TypeScript,
    JavaScript,
    Svelte,
    Markup,
    Style,
    Data,
    Config,
    Docs,
    Python,
    Go,
    Shell,
    Native,
    Image,
    Muted,
}

impl Theme {
    pub fn file_hue(&self, hue: FileHue) -> Hsla {
        let (dark, light) = match hue {
            FileHue::Rust => (0xe59a6b, 0xa8501c),
            FileHue::TypeScript => (0x5aa2f0, 0x2766b0),
            FileHue::JavaScript => (0xe2c24e, 0x8a6d00),
            FileHue::Svelte => (0xf47a52, 0xc4421a),
            FileHue::Markup => (0xec8458, 0xb34a1c),
            FileHue::Style => (0xb08cf0, 0x7044b8),
            FileHue::Data => (0xd8b45a, 0x8a6a00),
            FileHue::Config => (0x9aa7b8, 0x56606e),
            FileHue::Docs => (0x82abdc, 0x40669a),
            FileHue::Python => (0x6aaee6, 0x2e6ca4),
            FileHue::Go => (0x5ccbe0, 0x0b7c99),
            FileHue::Shell => (0x8fcf6c, 0x3f7f22),
            FileHue::Native => (0x8aa6e6, 0x3d5fa8),
            FileHue::Image => (0xd38bd8, 0x93408f),
            FileHue::Muted => return self.text3,
        };
        hex(if self.is_dark() { dark } else { light })
    }
}

struct ActiveTheme(Rc<Theme>);

impl Global for ActiveTheme {}

/// The palette on screen.
pub fn theme(cx: &App) -> Rc<Theme> {
    cx.global::<ActiveTheme>().0.clone()
}

pub fn set_theme(theme: Theme, cx: &mut App) {
    cx.set_global(ActiveTheme(Rc::new(theme)));
    cx.refresh_windows();
}

pub fn has_theme(cx: &App) -> bool {
    cx.has_global::<ActiveTheme>()
}
