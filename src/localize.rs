// SPDX-License-Identifier: GPL-3.0-only

use i18n_embed::fluent::{FluentLanguageLoader, fluent_language_loader};
use i18n_embed::{DefaultLocalizer, LanguageLoader, Localizer};
#[cfg(not(unix))]
use icu_collator::options::CollatorOptions;
#[cfg(not(unix))]
use icu_collator::preferences::CollationNumericOrdering;
#[cfg(not(unix))]
use icu_collator::{Collator, CollatorBorrowed, CollatorPreferences};
#[cfg(not(unix))]
use icu_locale::Locale;
use rust_embed::RustEmbed;
#[cfg(unix)]
use std::cmp::Ordering;
#[cfg(unix)]
use std::ffi::CString;
use std::sync::LazyLock;

#[derive(RustEmbed)]
#[folder = "i18n/"]
struct Localizations;

pub static LANGUAGE_LOADER: LazyLock<FluentLanguageLoader> = LazyLock::new(|| {
    let loader: FluentLanguageLoader = fluent_language_loader!();

    loader
        .load_fallback_language(&Localizations)
        .expect("Error while loading fallback language");

    loader
});

#[cfg(not(unix))]
pub static LANGUAGE_SORTER: LazyLock<CollatorBorrowed> = LazyLock::new(|| {
    let create_collator = |locale: Locale| {
        let mut prefs = CollatorPreferences::from(locale);
        prefs.numeric_ordering = Some(CollationNumericOrdering::True);
        Collator::try_new(prefs, CollatorOptions::default()).ok()
    };

    Locale::try_from_str(&LANGUAGE_LOADER.current_language().to_string())
            .ok()
            .and_then(create_collator)
            .or_else(|| {
                Locale::try_from_str(&LANGUAGE_LOADER.fallback_language().to_string())
                    .ok()
                    .and_then(create_collator)
            })
            .unwrap_or_else(|| {
                let locale = Locale::try_from_str("en-US").expect("en-US is a valid BCP-47 tag");
                create_collator(locale)
                    .expect("Creating a collator from the system's current language, the fallback language, or American English should succeed")
            })
});

#[cfg(unix)]
#[derive(Clone, Copy)]
struct LocaleHandle(usize);

#[cfg(unix)]
unsafe extern "C" {
    fn strcoll_l(
        left: *const libc::c_char,
        right: *const libc::c_char,
        locale: libc::locale_t,
    ) -> libc::c_int;
}

#[cfg(unix)]
impl LocaleHandle {
    fn as_raw(self) -> libc::locale_t {
        self.0 as libc::locale_t
    }
}

#[cfg(unix)]
fn locale_for(mask: libc::c_int, category: &str) -> LocaleHandle {
    let mut candidates = Vec::new();
    if let Some(locale) = std::env::var_os("LC_ALL").and_then(|locale| locale.into_string().ok())
        && !locale.is_empty()
    {
        candidates.push(locale);
    } else {
        if let Some(locale) =
            std::env::var_os(category).and_then(|locale| locale.into_string().ok())
            && !locale.is_empty()
        {
            candidates.push(locale);
        }
        if let Some(locale) = std::env::var_os("LANG").and_then(|locale| locale.into_string().ok())
            && !locale.is_empty()
        {
            candidates.push(locale);
        }
    }
    candidates.extend(["C.UTF-8".to_owned(), "C".to_owned()]);

    for name in candidates {
        let Ok(name) = CString::new(name) else {
            continue;
        };
        // These locale objects are immutable after construction. The two *_l APIs below accept
        // them explicitly, so they can be used without changing process-global locale state.
        let locale = unsafe { libc::newlocale(mask, name.as_ptr(), std::ptr::null_mut()) };
        if !locale.is_null() {
            return LocaleHandle(locale as usize);
        }
    }

    unreachable!("the C locale must always be available")
}

#[cfg(unix)]
pub struct LanguageSorter {
    locale: LocaleHandle,
}

#[cfg(unix)]
impl LanguageSorter {
    pub fn compare(&self, left: &str, right: &str) -> Ordering {
        let mut left_index = 0;
        let mut right_index = 0;

        while left_index < left.len() && right_index < right.len() {
            let left_end = natural_segment_end(left, left_index);
            let right_end = natural_segment_end(right, right_index);
            let left_segment = &left[left_index..left_end];
            let right_segment = &right[right_index..right_end];
            let left_is_number = left.as_bytes()[left_index].is_ascii_digit();
            let right_is_number = right.as_bytes()[right_index].is_ascii_digit();

            let ordering = if left_is_number && right_is_number {
                compare_numbers(left_segment, right_segment)
            } else {
                compare_locale_text(self.locale, left_segment, right_segment)
            };
            if ordering != Ordering::Equal {
                return ordering;
            }

            left_index = left_end;
            right_index = right_end;
        }

        match (left_index == left.len(), right_index == right.len()) {
            (true, true) => Ordering::Equal,
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            (false, false) => unreachable!(),
        }
    }
}

#[cfg(unix)]
fn natural_segment_end(value: &str, start: usize) -> usize {
    let starts_with_number = value.as_bytes()[start].is_ascii_digit();
    value[start..]
        .char_indices()
        .find_map(|(offset, character)| {
            (character.is_ascii_digit() != starts_with_number).then_some(start + offset)
        })
        .unwrap_or(value.len())
}

#[cfg(unix)]
fn compare_numbers(left: &str, right: &str) -> Ordering {
    let left_significant = left.trim_start_matches('0');
    let right_significant = right.trim_start_matches('0');
    let left_significant = if left_significant.is_empty() {
        "0"
    } else {
        left_significant
    };
    let right_significant = if right_significant.is_empty() {
        "0"
    } else {
        right_significant
    };

    left_significant
        .len()
        .cmp(&right_significant.len())
        .then_with(|| left_significant.cmp(right_significant))
        .then_with(|| left.len().cmp(&right.len()))
}

#[cfg(unix)]
fn compare_locale_text(locale: LocaleHandle, left: &str, right: &str) -> Ordering {
    let (Ok(left), Ok(right)) = (CString::new(left), CString::new(right)) else {
        return left.cmp(right);
    };
    unsafe { strcoll_l(left.as_ptr(), right.as_ptr(), locale.as_raw()) }.cmp(&0)
}

#[cfg(unix)]
pub static LANGUAGE_SORTER: LazyLock<LanguageSorter> = LazyLock::new(|| LanguageSorter {
    locale: locale_for(libc::LC_COLLATE_MASK, "LC_COLLATE"),
});

#[cfg(not(unix))]
pub static LOCALE: LazyLock<Locale> = LazyLock::new(|| {
    for var in ["LC_TIME", "LC_ALL", "LANG"] {
        if let Ok(locale_str) = std::env::var(var) {
            let cleaned_locale = locale_str
                .split('.')
                .next()
                .unwrap_or(&locale_str)
                .replace('_', "-");

            if let Ok(locale) = Locale::try_from_str(&cleaned_locale) {
                return locale;
            }

            // Try language-only fallback (e.g., "en" from "en-US")
            if let Some(lang) = cleaned_locale.split('-').next()
                && let Ok(locale) = Locale::try_from_str(lang)
            {
                return locale;
            }
        }
    }
    log::warn!("No valid locale found in environment, using fallback");
    Locale::try_from_str("en-US").expect("Failed to parse fallback locale 'en-US'")
});

#[cfg(unix)]
static TIME_LOCALE: LazyLock<LocaleHandle> =
    LazyLock::new(|| locale_for(libc::LC_TIME_MASK, "LC_TIME"));

pub struct DateTimeFormatter {
    #[cfg(unix)]
    military_time: bool,
    #[cfg(unix)]
    include_date: bool,
    #[cfg(not(unix))]
    inner: IcuDateTimeFormatter,
}

#[cfg(not(unix))]
enum IcuDateTimeFormatter {
    DateTime(icu_datetime::DateTimeFormatter<icu_datetime::fieldsets::YMDT>),
    Time(icu_datetime::DateTimeFormatter<icu_datetime::fieldsets::T>),
}

impl DateTimeFormatter {
    pub fn new(military_time: bool, include_date: bool) -> Self {
        #[cfg(unix)]
        {
            Self {
                military_time,
                include_date,
            }
        }

        #[cfg(not(unix))]
        {
            use icu_datetime::options::TimePrecision;
            use icu_datetime::{DateTimeFormatterPreferences, fieldsets};
            use icu_locale::preferences::extensions::unicode::keywords::HourCycle;
            use jiff_icu::ConvertFrom;

            let mut prefs = DateTimeFormatterPreferences::from(LOCALE.clone());
            prefs.hour_cycle = Some(if military_time {
                HourCycle::H23
            } else {
                HourCycle::H12
            });

            let inner = if include_date {
                let mut fields = fieldsets::YMDT::medium();
                fields = fields.with_time_precision(TimePrecision::Minute);
                IcuDateTimeFormatter::DateTime(
                    icu_datetime::DateTimeFormatter::try_new(prefs, fields)
                        .expect("failed to create DateTimeFormatter"),
                )
            } else {
                let mut fields = fieldsets::T::medium();
                fields = fields.with_time_precision(TimePrecision::Minute);
                IcuDateTimeFormatter::Time(
                    icu_datetime::DateTimeFormatter::try_new(prefs, fields)
                        .expect("failed to create DateTimeFormatter"),
                )
            };

            Self { inner }
        }
    }

    pub fn format(&self, time: std::time::SystemTime) -> String {
        #[cfg(unix)]
        {
            format_with_system_locale(time, self.military_time, self.include_date).unwrap_or_else(
                || match jiff::Zoned::try_from(time) {
                    Ok(zoned) => zoned.to_string(),
                    Err(_) => String::new(),
                },
            )
        }

        #[cfg(not(unix))]
        {
            let Ok(zoned) = jiff::Zoned::try_from(time) else {
                return String::new();
            };
            let datetime = jiff_icu::DateTime::convert_from(zoned.datetime());
            match &self.inner {
                IcuDateTimeFormatter::DateTime(formatter) => {
                    formatter.format(&datetime).to_string()
                }
                IcuDateTimeFormatter::Time(formatter) => formatter.format(&datetime).to_string(),
            }
        }
    }
}

#[cfg(unix)]
fn format_with_system_locale(
    time: std::time::SystemTime,
    military_time: bool,
    include_date: bool,
) -> Option<String> {
    use std::time::UNIX_EPOCH;

    let seconds = match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => i64::try_from(duration.as_secs()).ok()?,
        Err(error) => -i64::try_from(error.duration().as_secs()).ok()?,
    };
    let timestamp: libc::time_t = seconds.try_into().ok()?;
    let mut broken_down = std::mem::MaybeUninit::<libc::tm>::uninit();
    if unsafe { libc::localtime_r(&timestamp, broken_down.as_mut_ptr()) }.is_null() {
        return None;
    }
    let broken_down = unsafe { broken_down.assume_init() };

    let pattern = match (include_date, military_time) {
        (true, true) => c"%x %H:%M",
        (true, false) => c"%x %I:%M %p",
        (false, true) => c"%H:%M",
        (false, false) => c"%I:%M %p",
    };
    let mut output = [0u8; 128];
    let written = unsafe {
        libc::strftime_l(
            output.as_mut_ptr().cast(),
            output.len(),
            pattern.as_ptr(),
            &broken_down,
            TIME_LOCALE.as_raw(),
        )
    };
    (written != 0).then(|| String::from_utf8_lossy(&output[..written]).into_owned())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn language_sorter_compares_numeric_segments_by_value() {
        assert_eq!(LANGUAGE_SORTER.compare("item 2", "item 10"), Ordering::Less);
        assert_eq!(LANGUAGE_SORTER.compare("item 2", "item 02"), Ordering::Less);
    }

    #[test]
    fn system_date_formatter_returns_a_value() {
        assert!(
            !DateTimeFormatter::new(true, true)
                .format(std::time::SystemTime::now())
                .is_empty()
        );
    }
}

#[macro_export]
macro_rules! fl {
    ($message_id:literal) => {{
        i18n_embed_fl::fl!($crate::localize::LANGUAGE_LOADER, $message_id)
    }};

    ($message_id:literal, $($args:expr),*) => {{
        i18n_embed_fl::fl!($crate::localize::LANGUAGE_LOADER, $message_id, $($args), *)
    }};
}

// Get the `Localizer` to be used for localizing this library.
pub fn localizer() -> Box<dyn Localizer> {
    Box::from(DefaultLocalizer::new(&*LANGUAGE_LOADER, &Localizations))
}

pub fn localize() {
    let localizer = localizer();
    let requested_languages = i18n_embed::DesktopLanguageRequester::requested_languages();

    if let Err(error) = localizer.select(&requested_languages) {
        eprintln!("Error while loading language for COSMIC Files {error}");
    }
}
