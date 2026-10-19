use std::fmt;
pub struct AsciiDebug<'a>(pub &'a [u8]);

impl fmt::Debug for AsciiDebug<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "\"")?;
        for &byte in self.0 {
            // Check if the byte is a standard printable character.
            if (32..126).contains(&byte) {
                write!(f, "{}", byte as char)?;
            } else {
                write!(f, "\\{{{byte:02X}}}")?;
            }
        }
        write!(f, "\"")?;
        Ok(())
    }
}

#[macro_export]
macro_rules! report_error { ($message:expr, extra: { $($key:literal => $mode:tt $value:expr),* $(,)? } $(,)?) => {{ log::error!("{}", $message); $(log::debug!("{}: {:?}", $key, $value);)* }}; ($($arg:tt)*) => { log::error!($($arg)*) }; }

#[macro_export]
macro_rules! safe_warn { (safe: ($($safe:tt)*), full: ($($full:tt)*) $(,)?) => {{ log::warn!($($safe)*); if false { let _ = format_args!($($full)*); } }}; ($($arg:tt)*) => { log::warn!($($arg)*) }; }

#[macro_export]
macro_rules! safe_error { (safe: ($($safe:tt)*), full: ($($full:tt)*) $(,)?) => {{ log::error!($($safe)*); if false { let _ = format_args!($($full)*); } }}; ($($arg:tt)*) => { log::error!($($arg)*) }; }

#[macro_export]
macro_rules! safe_debug { (safe: ($($safe:tt)*), full: ($($full:tt)*) $(,)?) => {{ log::debug!($($safe)*); if false { let _ = format_args!($($full)*); } }}; ($($arg:tt)*) => { log::debug!($($arg)*) }; }

/// Asserts that a condition is true, logging an error if it is not.
///
/// This macro is similar to the standard `debug_assert!` macro, but it logs
/// an error if the condition is not met.  This should generally be preferred
/// over `debug_assert!`, as it will log in production, though should not be
/// used in codepaths where the error log could be produced with high volume.
#[macro_export]
macro_rules! safe_assert {
    ($cond:expr $(,)?) => {{
        debug_assert!($cond);
        match &$cond {
            cond => {
                if !(*cond) {
                    $crate::report_error!("{}", format_args!("Assertion `{}` failed", stringify!($cond)));
                }
            }
        }
    }};
    ($cond:expr, $($arg:tt)+) => {{
        debug_assert!($cond, $($arg)+);
        match &$cond {
            cond => {
                if !(*cond) {
                    $crate::report_error!("{}", format_args!("Assertion `{}` failed: {}", stringify!($cond), format_args!($($arg)+)));
                }
            }
        }
    }};
}
pub use safe_assert;

/// Asserts that two expressions are equal, logging an error if they are not.
///
/// This macro is similar to the standard `debug_assert_eq!` macro, but it logs
/// an error if the values are not equal. This should generally be preferred
/// over `debug_assert_eq!`, as it will log in production, though should not be
/// used in codepaths where the error log could be produced with high volume.
#[macro_export]
macro_rules! safe_assert_eq {
    ($left:expr, $right:expr $(,)?) => {{
        debug_assert_eq!($left, $right);
        match (&$left, &$right) {
            (left_val, right_val) => {
                if !(*left_val == *right_val) {
                    $crate::report_error!("{}", format_args!("Assertion `{} == {}` failed: expected {left_val}, found {right_val}.", stringify!($left), stringify!($right)));
                }
            }
        }
    }};
    ($left:expr, $right:expr, $($arg:tt)+) => {{
        debug_assert_eq!($left, $right, $($arg)+);
        match (&$left, &$right) {
            (left_val, right_val) => {
                if !(*left_val == *right_val) {
                    $crate::report_error!("{}", format_args!("Assertion `{} == {}` failed: expected {left_val}, found {right_val}. {}", stringify!($left), stringify!($right), format_args!($($arg)+)));
                }
            }
        }
    }};
}
pub use safe_assert_eq;
