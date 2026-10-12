//! Locale queries behind the daemon's choice of a `LANG` for its children:
//! the user's preferred locale as the operating system reports it, and
//! whether a locale of a given name is installed.
//!
//! Neither query changes this process's own locale.

/// The user's preferred locale, as a language code and an optional country
/// (region) code, e.g. `ja` and `JP`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreferredLocale {
    /// The ISO 639 language code, e.g. `ja`.
    pub language: String,
    /// The ISO 3166 country code, e.g. `JP`, when the preference names one.
    pub country: Option<String>,
}

impl PreferredLocale {
    /// The UTF-8 POSIX locale name this preference corresponds to, e.g.
    /// `ja_JP.UTF-8`.
    ///
    /// `None` without a country or with an empty code: POSIX locale names are
    /// `language_COUNTRY`, and a language alone names no installed locale.
    #[must_use]
    pub fn utf8_name(&self) -> Option<String> {
        let country = self.country.as_deref().filter(|code| !code.is_empty())?;
        if self.language.is_empty() {
            return None;
        }
        Some(format!("{}_{country}.UTF-8", self.language))
    }
}

/// The user's preferred locale as the operating system reports it.
///
/// macOS answers from the user's current `CFLocale` (the language and region
/// chosen in System Settings), which a process started from Finder or the
/// Dock can read even though it inherits no `LANG`. Other systems return
/// `None`: Linux has no system-wide preference to ask outside the
/// environment variables themselves, and Windows programs do not use `LANG`.
#[must_use]
pub fn preferred_locale() -> Option<PreferredLocale> {
    preferred_locale_impl()
}

#[cfg(target_os = "macos")]
fn preferred_locale_impl() -> Option<PreferredLocale> {
    use core_foundation_sys::base::CFRelease;
    use core_foundation_sys::locale::{
        CFLocaleCopyCurrent, CFLocaleGetValue, kCFLocaleCountryCode, kCFLocaleLanguageCode,
    };

    // SAFETY: CFLocaleCopyCurrent takes no arguments and returns an owned
    // (+1) reference or NULL; the non-NULL case is released below.
    let locale = unsafe { CFLocaleCopyCurrent() };
    if locale.is_null() {
        return None;
    }
    // SAFETY: `locale` is a live CFLocale for the duration of both reads, and
    // the keys are CoreFoundation's own immutable constants. CFLocaleGetValue
    // follows the Get rule, so the returned values are not released here.
    let (language, country) = unsafe {
        (
            cf_string_value(CFLocaleGetValue(locale, kCFLocaleLanguageCode)),
            cf_string_value(CFLocaleGetValue(locale, kCFLocaleCountryCode)),
        )
    };
    // SAFETY: releases the +1 reference CFLocaleCopyCurrent returned, once,
    // after the last use of `locale` and of the values borrowed from it.
    unsafe { CFRelease(locale.cast()) };
    Some(PreferredLocale {
        language: language?,
        country,
    })
}

/// Copies a borrowed CoreFoundation value into a `String` when it is a
/// `CFString` representable as UTF-8.
///
/// # Safety
///
/// `value` must be NULL or a live CoreFoundation object for the whole call.
#[cfg(target_os = "macos")]
unsafe fn cf_string_value(value: core_foundation_sys::base::CFTypeRef) -> Option<String> {
    use core_foundation_sys::base::{CFGetTypeID, CFIndex};
    use core_foundation_sys::string::{
        CFStringGetCString, CFStringGetLength, CFStringGetMaximumSizeForEncoding,
        CFStringGetTypeID, CFStringRef, kCFStringEncodingUTF8,
    };

    if value.is_null() {
        return None;
    }
    // SAFETY: the caller guarantees `value` is a live CoreFoundation object;
    // the type check makes the CFString calls below valid for it.
    unsafe {
        if CFGetTypeID(value) != CFStringGetTypeID() {
            return None;
        }
        let string: CFStringRef = value.cast();
        let max =
            CFStringGetMaximumSizeForEncoding(CFStringGetLength(string), kCFStringEncodingUTF8);
        // One more byte for the terminating NUL CFStringGetCString writes.
        let capacity = usize::try_from(max).ok()?.checked_add(1)?;
        let mut buffer: Vec<u8> = vec![0; capacity];
        let buffer_len = CFIndex::try_from(buffer.len()).ok()?;
        if CFStringGetCString(
            string,
            buffer.as_mut_ptr().cast(),
            buffer_len,
            kCFStringEncodingUTF8,
        ) == 0
        {
            return None;
        }
        let text = std::ffi::CStr::from_bytes_until_nul(&buffer).ok()?;
        text.to_str().ok().map(str::to_owned)
    }
}

#[cfg(not(target_os = "macos"))]
fn preferred_locale_impl() -> Option<PreferredLocale> {
    None
}

/// Whether a locale named `name` is installed, so a child given it as `LANG`
/// would not fall back to `C` with a warning.
///
/// Unix asks `newlocale(3)` and frees the result at once, leaving this
/// process's own locale untouched (no `setlocale`). An empty name (which
/// means "from the environment" to the C library, not a locale) and a name
/// with an interior NUL are never installed. Always `false` on Windows, whose
/// programs do not read `LANG`.
#[must_use]
pub fn locale_exists(name: &str) -> bool {
    locale_exists_impl(name)
}

#[cfg(unix)]
fn locale_exists_impl(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    let Ok(name) = std::ffi::CString::new(name) else {
        return false;
    };
    // SAFETY: `name` is a NUL-terminated string that outlives the call, and a
    // null base asks newlocale for a fresh object rather than modifying one.
    let locale = unsafe { libc::newlocale(libc::LC_ALL_MASK, name.as_ptr(), std::ptr::null_mut()) };
    if locale.is_null() {
        return false;
    }
    // SAFETY: `locale` is the non-null object newlocale just returned, freed
    // exactly once and never used afterwards.
    unsafe { libc::freelocale(locale) };
    true
}

#[cfg(not(unix))]
fn locale_exists_impl(_name: &str) -> bool {
    false
}
