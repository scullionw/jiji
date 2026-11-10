//! Midnight, the app's base palette, transcribed from
//! `src/lib/styles/tokens.css` so the spike can be judged side by side with
//! the webview. The `color-mix()` surfaces are resolved here once.

use gpui::{Hsla, Rgba};

use crate::highlight::Syn;

#[derive(Clone)]
pub struct Theme {
    pub bg0: Hsla,
    pub bg1: Hsla,
    pub bg3: Hsla,
    pub border1: Hsla,
    pub border2: Hsla,
    pub text1: Hsla,
    pub text2: Hsla,
    pub text3: Hsla,
    pub accent_strong: Hsla,
    pub warn: Hsla,
    pub danger: Hsla,

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

    syn: [Hsla; 8],
}

fn hex(rgb: u32) -> Hsla {
    gpui::rgb(rgb).into()
}

fn rgba(r: u8, g: u8, b: u8, a: f32) -> Hsla {
    Rgba {
        r: r as f32 / 255.,
        g: g as f32 / 255.,
        b: b as f32 / 255.,
        a,
    }
    .into()
}

/// `color-mix(in srgb, a <pct>%, b)` for opaque colors.
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

impl Theme {
    pub fn midnight() -> Self {
        const BG0: u32 = 0x101115;
        const BG1: u32 = 0x15161b;
        const ADD: u32 = 0x38aa5a; // rgb(56 170 90)
        const DEL: u32 = 0xec544a; // rgb(236 84 74)
        let text3 = hex(0x9093a0);
        Self {
            bg0: hex(BG0),
            bg1: hex(BG1),
            bg3: hex(0x24252d),
            border1: rgba(255, 255, 255, 0.10),
            border2: rgba(255, 255, 255, 0.06),
            text1: hex(0xe9eaf0),
            text2: hex(0xafb2bf),
            text3,
            accent_strong: hex(0xa6abff),
            warn: hex(0xd9a441),
            danger: hex(0xe5564e),

            add_fg: hex(0x57c97a),
            add_bg: rgba(56, 170, 90, 0.14),
            add_gutter: mix(ADD, 0.22, BG1),
            add_word: rgba(56, 170, 90, 0.36),
            del_fg: hex(0xfb7c74),
            del_bg: rgba(236, 84, 74, 0.13),
            del_gutter: mix(DEL, 0.20, BG1),
            del_word: rgba(236, 84, 74, 0.36),
            gutter_surface: mix(BG0, 0.45, BG1),
            recess: mix(BG0, 0.60, BG1),
            file_head: hex(0x1b1c22),

            // The webview uses the system highlight; an accent wash reads the
            // same on this palette without depending on the OS setting.
            selection: rgba(112, 120, 238, 0.34),
            scrollbar: rgba(255, 255, 255, 0.13),
            scrollbar_hover: rgba(255, 255, 255, 0.24),

            syn: [
                hex(0xd2a0eb), // kw
                hex(0x9fc17e), // str
                hex(0xd9a86b), // const
                hex(0x6fc0d0), // type
                hex(0x89b6f0), // fn
                hex(0x9cb4d6), // prop
                text3,         // comment
                hex(0xd7aa86), // meta
            ],
        }
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
}
