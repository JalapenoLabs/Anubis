//! How the wallet's micro-units meet the amounts Stripe charges.
//!
//! The wallet keeps every amount in micro-units, millionths of the currency's
//! major unit, so a charge for one minute of usage never rounds to zero. Stripe
//! takes amounts in the currency's smallest unit instead: cents for `usd`, yen
//! for `jpy`, fils for `bhd`. This module is the one place the two meet, and
//! it follows Stripe's own list of which currencies have how many decimals
//! (<https://docs.stripe.com/currencies#zero-decimal>).

/// Micro-units in one major unit of any currency: one dollar, one yen.
pub const MICROS_PER_UNIT: i64 = 1_000_000;

/// The most Stripe charges in one payment, in the currency's smallest unit.
///
/// Stripe accepts amounts of up to eight digits (99,999,999 is $999,999.99),
/// so a top-up past it is refused here with a reason rather than by Stripe as
/// a server error.
pub const MAX_CHARGE_MINOR: i64 = 99_999_999;

/// Currencies Stripe charges in whole units, with no smaller unit.
const ZERO_DECIMAL: [&str; 16] = [
    "bif", "clp", "djf", "gnf", "jpy", "kmf", "krw", "mga", "pyg", "rwf", "ugx", "vnd", "vuv",
    "xaf", "xof", "xpf",
];

/// Currencies Stripe charges in thousandths, with the last digit always zero.
const THREE_DECIMAL: [&str; 5] = ["bhd", "jod", "kwd", "omr", "tnd"];

/// Micro-units in one of the currency's smallest units, as Stripe counts them.
#[must_use]
pub fn micros_per_minor(currency: &str) -> i64 {
    if ZERO_DECIMAL.contains(&currency) {
        MICROS_PER_UNIT
    } else if THREE_DECIMAL.contains(&currency) {
        MICROS_PER_UNIT / 1_000
    } else {
        MICROS_PER_UNIT / 100
    }
}

/// The smallest step a charge moves in, in micro-units.
///
/// A whole smallest unit, except for the three-decimal currencies, where Stripe
/// requires the last digit to be zero and so charges in hundredths.
#[must_use]
pub fn charge_step_micros(currency: &str) -> i64 {
    if ZERO_DECIMAL.contains(&currency) {
        MICROS_PER_UNIT
    } else {
        MICROS_PER_UNIT / 100
    }
}

/// The amount Stripe charges for `micros`, or `None` when Stripe cannot charge it.
///
/// `None` for an amount that is not a whole [`charge_step_micros`], and for one
/// past [`MAX_CHARGE_MINOR`].
#[must_use]
pub fn to_minor(micros: i64, currency: &str) -> Option<i64> {
    if micros % charge_step_micros(currency) != 0 {
        return None;
    }
    Some(micros / micros_per_minor(currency)).filter(|minor| minor.abs() <= MAX_CHARGE_MINOR)
}

/// The micro-units an amount Stripe reports is worth, or `None` past `i64`.
#[must_use]
pub fn from_minor(minor: i64, currency: &str) -> Option<i64> {
    minor.checked_mul(micros_per_minor(currency))
}

/// Renders an amount for a stored English notice, e.g. `1.97 USD`.
///
/// Rounded toward zero at the currency's own decimals, because a notice that
/// says a little less is left than is left never promises credit that is not
/// there.
#[must_use]
pub fn display(micros: i64, currency: &str) -> String {
    let decimals: u32 = if ZERO_DECIMAL.contains(&currency) {
        0
    } else if THREE_DECIMAL.contains(&currency) {
        3
    } else {
        2
    };
    let code = currency.to_ascii_uppercase();
    let sign = if micros < 0 { "-" } else { "" };
    let magnitude = micros.unsigned_abs();
    let unit = MICROS_PER_UNIT.unsigned_abs();
    let whole = magnitude / unit;
    if decimals == 0 {
        return format!("{sign}{whole} {code}");
    }
    let fraction = (magnitude % unit) / 10_u64.pow(6 - decimals);
    let width = usize::try_from(decimals).unwrap_or(2);
    format!("{sign}{whole}.{fraction:0width$} {code}")
}

#[cfg(test)]
mod tests {
    use super::{MAX_CHARGE_MINOR, charge_step_micros, display, from_minor, to_minor};

    #[test]
    fn cents_dollars_and_yen_convert_both_ways() {
        assert_eq!(
            to_minor(10_000_000, "usd"),
            Some(1_000),
            "$10.00 is 1000 cents"
        );
        assert_eq!(from_minor(1_000, "usd"), Some(10_000_000));
        assert_eq!(to_minor(5_000_000, "jpy"), Some(5), "five yen");
        assert_eq!(from_minor(5, "jpy"), Some(5_000_000));
        assert_eq!(
            to_minor(1_230_000, "bhd"),
            Some(1_230),
            "fils, last digit zero"
        );
        assert_eq!(from_minor(1_230, "bhd"), Some(1_230_000));
    }

    #[test]
    fn an_amount_stripe_cannot_charge_has_no_minor_value() {
        assert_eq!(to_minor(10_001, "usd"), None, "a fraction of a cent");
        assert_eq!(to_minor(1_500_000, "jpy"), None, "a fraction of a yen");
        assert_eq!(
            to_minor(1_231_000, "bhd"),
            None,
            "Stripe needs the last fils digit zero"
        );
        assert_eq!(charge_step_micros("bhd"), 10_000);
        assert_eq!(
            to_minor((MAX_CHARGE_MINOR + 1) * 10_000, "usd"),
            None,
            "past eight digits",
        );
        assert_eq!(
            to_minor(MAX_CHARGE_MINOR * 10_000, "usd"),
            Some(MAX_CHARGE_MINOR)
        );
        assert_eq!(from_minor(i64::MAX, "usd"), None);
    }

    #[test]
    fn a_notice_never_shows_more_than_is_left() {
        assert_eq!(display(1_979_999, "usd"), "1.97 USD");
        assert_eq!(display(417, "usd"), "0.00 USD");
        assert_eq!(display(-2_500_000, "usd"), "-2.50 USD");
        assert_eq!(display(12_999_999, "jpy"), "12 JPY");
        assert_eq!(display(1_234_567, "kwd"), "1.234 KWD");
    }
}
