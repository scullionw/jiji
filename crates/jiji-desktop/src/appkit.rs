//! What the app asks AppKit for itself, where GPUI has no call: the
//! standard About panel, the system's Reduce Motion setting, and its spell
//! checker. Messages go through the Objective-C runtime the process
//! already links; strings and dictionaries cross as their toll-free bridged
//! Core Foundation twins. Everything here runs on the main thread.

// Tests never reach the system: their spell checker is a stand-in.
#![cfg_attr(test, allow(dead_code))]

use std::ops::Range;

/// The standard About panel: the app's icon, name and version, as every
/// Mac app's About item shows it.
pub fn about_panel(version: &str) {
    // A test has no application for AppKit to hang a panel on.
    #[cfg(all(target_os = "macos", not(test)))]
    mac::about_panel(version);
    #[cfg(any(not(target_os = "macos"), test))]
    let _ = version;
}

/// Whether the user has asked the system to reduce motion.
pub fn reduce_motion() -> bool {
    #[cfg(target_os = "macos")]
    {
        mac::reduce_motion()
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// One document's worth of the system spell checker. Words ignored in it
/// stay ignored only there, as in any Mac app; learned words go to the
/// user's dictionary, for every app.
pub struct SpellDocument {
    #[cfg(target_os = "macos")]
    tag: isize,
}

impl SpellDocument {
    /// `None` when the user has turned spell checking while typing off for
    /// every app (`NSAllowContinuousSpellChecking`), as text views honour.
    pub fn open() -> Option<Self> {
        #[cfg(target_os = "macos")]
        {
            mac::continuous_checking_allowed().then(|| Self {
                tag: mac::document_tag(),
            })
        }
        #[cfg(not(target_os = "macos"))]
        {
            None
        }
    }

    /// The misspelled words of `text`, as byte ranges, in the languages the
    /// user's spelling settings name (automatically by language, by
    /// default).
    pub fn misspelled(&self, text: &str) -> Vec<Range<usize>> {
        #[cfg(target_os = "macos")]
        {
            mac::misspelled(text, self.tag)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = text;
            Vec::new()
        }
    }

    /// What the word at `word` in `text` might have been meant to be, best
    /// first.
    pub fn guesses(&self, text: &str, word: Range<usize>) -> Vec<String> {
        #[cfg(target_os = "macos")]
        {
            mac::guesses(text, word, self.tag)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (text, word);
            Vec::new()
        }
    }

    pub fn ignore(&self, word: &str) {
        #[cfg(target_os = "macos")]
        mac::ignore(word, self.tag);
        #[cfg(not(target_os = "macos"))]
        let _ = word;
    }

    pub fn learn(&self, word: &str) {
        #[cfg(target_os = "macos")]
        mac::learn(word);
        #[cfg(not(target_os = "macos"))]
        let _ = word;
    }
}

#[cfg(target_os = "macos")]
impl Drop for SpellDocument {
    fn drop(&mut self) {
        mac::close_document(self.tag);
    }
}

/// The byte offset of a UTF-16 offset into `text`, which is how AppKit
/// counts.
fn byte_offset(text: &str, utf16: usize) -> usize {
    let mut units = 0;
    for (index, ch) in text.char_indices() {
        if units >= utf16 {
            return index;
        }
        units += ch.len_utf16();
    }
    text.len()
}

/// The UTF-16 offset of a byte offset into `text`.
fn utf16_offset(text: &str, byte: usize) -> usize {
    text[..byte.min(text.len())]
        .chars()
        .map(char::len_utf16)
        .sum()
}

/// Sending Objective-C messages: each send is `objc_msgSend` at the
/// signature of the method it calls, the receiver and selector first.
#[cfg(target_os = "macos")]
pub(crate) mod objc {
    use std::ffi::{c_char, c_void, CStr};

    pub type Id = *mut c_void;
    pub type Sel = *mut c_void;

    #[link(name = "objc")]
    extern "C" {
        fn objc_getClass(name: *const c_char) -> Id;
        fn sel_registerName(name: *const c_char) -> Sel;
        fn objc_msgSend();
        fn objc_autoreleasePoolPush() -> *mut c_void;
        fn objc_autoreleasePoolPop(pool: *mut c_void);
    }

    pub fn class(name: &CStr) -> Id {
        // SAFETY: a lookup by a NUL-terminated name.
        unsafe { objc_getClass(name.as_ptr()) }
    }

    pub fn sel(name: &CStr) -> Sel {
        // SAFETY: registration by a NUL-terminated name.
        unsafe { sel_registerName(name.as_ptr()) }
    }

    /// `objc_msgSend` as a function of type `F`.
    ///
    /// # Safety
    ///
    /// `F` must be an `unsafe extern "C" fn` taking the receiver, the
    /// selector and then exactly the arguments of the method it will send,
    /// and returning what that method returns.
    pub unsafe fn send<F: Copy>() -> F {
        const {
            assert!(size_of::<F>() == size_of::<unsafe extern "C" fn()>());
        }
        let send: unsafe extern "C" fn() = objc_msgSend;
        // SAFETY: both are function pointers; the caller vouches for the
        // signature.
        unsafe { std::mem::transmute_copy(&send) }
    }

    /// Run `f` inside an autorelease pool, so what AppKit hands back
    /// autoreleased is let go when it returns.
    pub fn pooled<R>(f: impl FnOnce() -> R) -> R {
        // SAFETY: a push matched by its pop on the same thread.
        let pool = unsafe { objc_autoreleasePoolPush() };
        let result = f();
        unsafe { objc_autoreleasePoolPop(pool) };
        result
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use std::ops::Range;
    use std::ptr;

    use core_foundation::base::{CFTypeRef, TCFType};
    use core_foundation::boolean::CFBoolean;
    use core_foundation::dictionary::CFDictionary;
    use core_foundation::string::{CFString, CFStringRef};

    use super::objc::{class, pooled, sel, send, Id, Sel};
    use super::{byte_offset, utf16_offset};

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct NSRange {
        location: usize,
        length: usize,
    }

    /// `NSTextCheckingTypeSpelling`.
    const SPELLING: u64 = 1 << 1;

    #[link(name = "AppKit", kind = "framework")]
    extern "C" {
        static NSAboutPanelOptionApplicationName: CFStringRef;
        static NSAboutPanelOptionApplicationVersion: CFStringRef;
        static NSAboutPanelOptionVersion: CFStringRef;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFPreferencesCopyAppValue(key: CFStringRef, application: CFStringRef) -> CFTypeRef;
        static kCFPreferencesCurrentApplication: CFStringRef;
    }

    fn id(string: &CFString) -> Id {
        string.as_concrete_TypeRef() as Id
    }

    /// `[Class shared…]`, the singleton a class method hands out.
    fn shared(name: &std::ffi::CStr, selector: &std::ffi::CStr) -> Id {
        // SAFETY: a class method taking nothing and returning an object.
        unsafe {
            let get: unsafe extern "C" fn(Id, Sel) -> Id = send();
            get(class(name), sel(selector))
        }
    }

    fn spell_checker() -> Id {
        shared(c"NSSpellChecker", c"sharedSpellChecker")
    }

    pub fn about_panel(version: &str) {
        // SAFETY: AppKit's option keys, read as the CFStrings they are.
        let options = unsafe {
            CFDictionary::from_CFType_pairs(&[
                (
                    CFString::wrap_under_get_rule(NSAboutPanelOptionApplicationName),
                    CFString::from_static_string("Jiji"),
                ),
                (
                    CFString::wrap_under_get_rule(NSAboutPanelOptionApplicationVersion),
                    CFString::new(version),
                ),
                // The bundle's build number is the same string; an empty
                // one keeps the panel from saying it twice.
                (
                    CFString::wrap_under_get_rule(NSAboutPanelOptionVersion),
                    CFString::from_static_string(""),
                ),
            ])
        };
        let app = shared(c"NSApplication", c"sharedApplication");
        // SAFETY: `orderFrontStandardAboutPanelWithOptions:` takes a
        // dictionary and returns nothing.
        unsafe {
            let show: unsafe extern "C" fn(Id, Sel, Id) = send();
            show(
                app,
                sel(c"orderFrontStandardAboutPanelWithOptions:"),
                options.as_concrete_TypeRef() as Id,
            );
        }
    }

    pub fn reduce_motion() -> bool {
        let workspace = shared(c"NSWorkspace", c"sharedWorkspace");
        // SAFETY: a property returning a BOOL, one byte on both ABIs.
        unsafe {
            let flag: unsafe extern "C" fn(Id, Sel) -> i8 = send();
            flag(workspace, sel(c"accessibilityDisplayShouldReduceMotion")) != 0
        }
    }

    pub fn continuous_checking_allowed() -> bool {
        let key = CFString::from_static_string("NSAllowContinuousSpellChecking");
        // SAFETY: a Copy-rule lookup; the value, when there is one, is
        // released by the wrapper that takes it.
        unsafe {
            let value = CFPreferencesCopyAppValue(
                key.as_concrete_TypeRef(),
                kCFPreferencesCurrentApplication,
            );
            if value.is_null() {
                return true;
            }
            core_foundation::base::CFType::wrap_under_create_rule(value)
                .downcast::<CFBoolean>()
                .is_none_or(bool::from)
        }
    }

    pub fn document_tag() -> isize {
        // SAFETY: `+uniqueSpellDocumentTag` returns an NSInteger.
        unsafe {
            let tag: unsafe extern "C" fn(Id, Sel) -> isize = send();
            tag(class(c"NSSpellChecker"), sel(c"uniqueSpellDocumentTag"))
        }
    }

    pub fn close_document(tag: isize) {
        // SAFETY: `closeSpellDocumentWithTag:` takes an NSInteger.
        unsafe {
            let close: unsafe extern "C" fn(Id, Sel, isize) = send();
            close(spell_checker(), sel(c"closeSpellDocumentWithTag:"), tag);
        }
    }

    pub fn misspelled(text: &str, tag: isize) -> Vec<Range<usize>> {
        if text.is_empty() {
            return Vec::new();
        }
        let string = CFString::new(text);
        let whole = NSRange {
            location: 0,
            length: string.char_len() as usize,
        };
        pooled(|| {
            // SAFETY: the signature of
            // `checkString:range:types:options:inSpellDocumentWithTag:orthography:wordCount:`;
            // the array it returns is read before the pool lets it go.
            unsafe {
                let check: unsafe extern "C" fn(
                    Id,
                    Sel,
                    Id,
                    NSRange,
                    u64,
                    Id,
                    isize,
                    *mut Id,
                    *mut isize,
                ) -> Id = send();
                let results = check(
                    spell_checker(),
                    sel(c"checkString:range:types:options:inSpellDocumentWithTag:orthography:wordCount:"),
                    id(&string),
                    whole,
                    SPELLING,
                    ptr::null_mut(),
                    tag,
                    ptr::null_mut(),
                    ptr::null_mut(),
                );
                let count: unsafe extern "C" fn(Id, Sel) -> usize = send();
                let at: unsafe extern "C" fn(Id, Sel, usize) -> Id = send();
                let kind: unsafe extern "C" fn(Id, Sel) -> u64 = send();
                let range: unsafe extern "C" fn(Id, Sel) -> NSRange = send();
                (0..count(results, sel(c"count")))
                    .map(|index| at(results, sel(c"objectAtIndex:"), index))
                    .filter(|result| kind(*result, sel(c"resultType")) == SPELLING)
                    .map(|result| {
                        let found = range(result, sel(c"range"));
                        byte_offset(text, found.location)
                            ..byte_offset(text, found.location + found.length)
                    })
                    .collect()
            }
        })
    }

    pub fn guesses(text: &str, word: Range<usize>, tag: isize) -> Vec<String> {
        let string = CFString::new(text);
        let start = utf16_offset(text, word.start);
        let range = NSRange {
            location: start,
            length: utf16_offset(text, word.end) - start,
        };
        pooled(|| {
            // SAFETY: the signature of
            // `guessesForWordRange:inString:language:inSpellDocumentWithTag:`
            // (a nil language is the one the spelling settings choose); its
            // strings are copied out before the pool lets them go.
            unsafe {
                let guess: unsafe extern "C" fn(Id, Sel, NSRange, Id, Id, isize) -> Id = send();
                let guesses = guess(
                    spell_checker(),
                    sel(c"guessesForWordRange:inString:language:inSpellDocumentWithTag:"),
                    range,
                    id(&string),
                    ptr::null_mut(),
                    tag,
                );
                if guesses.is_null() {
                    return Vec::new();
                }
                let count: unsafe extern "C" fn(Id, Sel) -> usize = send();
                let at: unsafe extern "C" fn(Id, Sel, usize) -> Id = send();
                (0..count(guesses, sel(c"count")))
                    .map(|index| {
                        let guess = at(guesses, sel(c"objectAtIndex:"), index);
                        CFString::wrap_under_get_rule(guess as CFStringRef).to_string()
                    })
                    .collect()
            }
        })
    }

    pub fn ignore(word: &str, tag: isize) {
        let word = CFString::new(word);
        // SAFETY: `ignoreWord:inSpellDocumentWithTag:` takes a string and
        // an NSInteger.
        unsafe {
            let ignore: unsafe extern "C" fn(Id, Sel, Id, isize) = send();
            ignore(
                spell_checker(),
                sel(c"ignoreWord:inSpellDocumentWithTag:"),
                id(&word),
                tag,
            );
        }
    }

    pub fn learn(word: &str) {
        let word = CFString::new(word);
        // SAFETY: `learnWord:` takes a string.
        unsafe {
            let learn: unsafe extern "C" fn(Id, Sel, Id) = send();
            learn(spell_checker(), sel(c"learnWord:"), id(&word));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{byte_offset, utf16_offset};

    #[test]
    fn appkit_counts_in_utf16_and_the_text_in_bytes() {
        let text = "fix 🐛 in café";
        // The bug is two UTF-16 units and four bytes; é is one and two.
        assert_eq!(byte_offset(text, 4), 4);
        assert_eq!(byte_offset(text, 6), 8);
        assert_eq!(utf16_offset(text, 8), 6);
        assert_eq!(
            byte_offset(text, utf16_offset(text, text.len())),
            text.len()
        );
        assert_eq!(byte_offset(text, 999), text.len());
    }
}
