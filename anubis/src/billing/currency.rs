//! How the wallet's micro-units meet the amounts Stripe charges.
//!
//! The wallet keeps every amount in micro-units, millionths of the currency's
//! major unit, so a charge for one minute of usage never rounds to zero. Stripe
//! takes amounts in the currency's smallest unit instead: cents for `usd`, yen
//! for `jpy`. This module is the one place the two meet, and it follows
//! Stripe's own rules (<https://docs.stripe.com/currencies>):
//!
//! - **Two-decimal** currencies, the default, are charged in hundredths.
//!   `huf` and `twd` are among them: Stripe treats both as zero-decimal for
//!   payouts only, which a wallet never makes.
//! - **Zero-decimal** currencies such as `jpy` are charged in whole units,
//!   and the amount is the charge.
//! - **`isk` and `ugx`** became zero-decimal, but Stripe still takes them in
//!   two-decimal form with the decimals always `00`: 5 ISK is an `amount` of
//!   `500`, and a fraction of one cannot be charged.
//! - **Three-decimal** currencies (`bhd`, `jod`, `kwd`, `omr`, `tnd`) carry
//!   charging rules of their own that this module does not apply, so a wallet
//!   refuses to hold one rather than guess; see [`is_supported`].

/// Micro-units in one major unit of any currency: one dollar, one yen.
pub const MICROS_PER_UNIT: i64 = 1_000_000;

/// The most Stripe charges in one payment, in the currency's smallest unit.
///
/// Stripe accepts amounts of up to eight digits for most payment methods
/// (99,999,999 is $999,999.99), so a top-up past it is refused here with a
/// reason rather than by Stripe as a server error.
pub const MAX_CHARGE_MINOR: i64 = 99_999_999;

/// Currencies Stripe charges in whole units, with no smaller unit.
const ZERO_DECIMAL: [&str; 15] = [
    "bif", "clp", "djf", "gnf", "jpy", "kmf", "krw", "mga", "pyg", "rwf", "vnd", "vuv", "xaf",
    "xof", "xpf",
];

/// Zero-decimal currencies Stripe still takes in two-decimal form, decimals always `00`.
const WHOLE_UNITS_IN_HUNDREDTHS: [&str; 2] = ["isk", "ugx"];

/// Three-decimal currencies, which a wallet does not hold.
const THREE_DECIMAL: [&str; 5] = ["bhd", "jod", "kwd", "omr", "tnd"];

/// How Stripe writes amounts of one currency, as far as the wallet needs to know.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Representation {
    /// Micro-units in one unit of the `amount` Stripe takes and reports.
    micros_per_minor: i64,
    /// The smallest step a charge moves in, in micro-units.
    charge_step_micros: i64,
    /// The decimals a person reads the currency in.
    decimals: u32,
}

/// Stripe's representation of `currency`, or `None` for one a wallet does not hold.
fn representation(currency: &str) -> Option<Representation> {
    let hundredths = MICROS_PER_UNIT / 100;
    if THREE_DECIMAL.contains(&currency) {
        return None;
    }
    if ZERO_DECIMAL.contains(&currency) {
        return Some(Representation {
            micros_per_minor: MICROS_PER_UNIT,
            charge_step_micros: MICROS_PER_UNIT,
            decimals: 0,
        });
    }
    if WHOLE_UNITS_IN_HUNDREDTHS.contains(&currency) {
        return Some(Representation {
            micros_per_minor: hundredths,
            charge_step_micros: MICROS_PER_UNIT,
            decimals: 0,
        });
    }
    Some(Representation {
        micros_per_minor: hundredths,
        charge_step_micros: hundredths,
        decimals: 2,
    })
}

/// Whether a wallet can hold `currency`.
///
/// Every currency but the three-decimal ones, whose charging rules this
/// module does not apply. `billing.yml` is refused at boot for one.
#[must_use]
pub fn is_supported(currency: &str) -> bool {
    representation(currency).is_some()
}

/// The smallest step a charge moves in, in micro-units: a cent, or a whole yen.
///
/// `None` for a currency a wallet does not hold.
#[must_use]
pub fn charge_step_micros(currency: &str) -> Option<i64> {
    representation(currency).map(|units| units.charge_step_micros)
}

/// The amount Stripe charges for `micros`, or `None` when Stripe cannot charge it.
///
/// `None` for an amount that is not a whole [`charge_step_micros`], for one
/// past [`MAX_CHARGE_MINOR`], and for a currency a wallet does not hold.
#[must_use]
pub fn to_minor(micros: i64, currency: &str) -> Option<i64> {
    let units = representation(currency)?;
    if micros % units.charge_step_micros != 0 {
        return None;
    }
    Some(micros / units.micros_per_minor).filter(|minor| minor.abs() <= MAX_CHARGE_MINOR)
}

/// The micro-units an amount Stripe reports is worth.
///
/// `None` past `i64`, and for a currency a wallet does not hold.
#[must_use]
pub fn from_minor(minor: i64, currency: &str) -> Option<i64> {
    minor.checked_mul(representation(currency)?.micros_per_minor)
}

/// Renders an amount for a stored English notice, e.g. `1.97 USD`.
///
/// Rounded toward zero at the currency's own decimals, because a notice that
/// says a little less is left than is left never promises credit that is not
/// there.
#[must_use]
pub fn display(micros: i64, currency: &str) -> String {
    let decimals = representation(currency).map_or(2, |units| units.decimals);
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
    use super::{
        MAX_CHARGE_MINOR, charge_step_micros, display, from_minor, is_supported, to_minor,
    };

    #[test]
    fn a_two_decimal_currency_is_charged_in_cents() {
        assert_eq!(
            to_minor(10_000_000, "usd"),
            Some(1_000),
            "$10.00 is 1000 cents"
        );
        assert_eq!(from_minor(1_000, "usd"), Some(10_000_000));
        assert_eq!(to_minor(10_001, "usd"), None, "a fraction of a cent");
        assert_eq!(charge_step_micros("usd"), Some(10_000));
    }

    #[test]
    fn a_zero_decimal_currency_is_charged_in_whole_units() {
        assert_eq!(
            to_minor(5_000_000, "jpy"),
            Some(5),
            "five yen is an amount of 5"
        );
        assert_eq!(from_minor(5, "jpy"), Some(5_000_000));
        assert_eq!(to_minor(1_500_000, "jpy"), None, "a fraction of a yen");
    }

    #[test]
    fn isk_and_ugx_are_whole_units_written_in_hundredths() {
        for currency in ["isk", "ugx"] {
            // Stripe's own example: to charge 5, provide an amount of 500.
            assert_eq!(to_minor(5_000_000, currency), Some(500), "{currency}");
            assert_eq!(from_minor(500, currency), Some(5_000_000), "{currency}");
            assert_eq!(
                to_minor(1_050_500_000, currency),
                None,
                "no fractions of {currency}"
            );
            assert_eq!(charge_step_micros(currency), Some(1_000_000), "{currency}");
            assert_eq!(
                display(12_999_999, currency),
                format!("12 {}", currency.to_uppercase())
            );
        }
    }

    #[test]
    fn huf_and_twd_are_charged_in_hundredths() {
        // Zero-decimal for payouts only, which a wallet never makes.
        for currency in ["huf", "twd"] {
            assert_eq!(to_minor(10_450_000, currency), Some(1_045), "{currency}");
            assert_eq!(from_minor(1_045, currency), Some(10_450_000), "{currency}");
            assert_eq!(charge_step_micros(currency), Some(10_000), "{currency}");
        }
    }

    #[test]
    fn a_three_decimal_currency_is_one_a_wallet_does_not_hold() {
        for currency in ["bhd", "jod", "kwd", "omr", "tnd"] {
            assert!(!is_supported(currency), "{currency}");
            assert_eq!(to_minor(1_230_000, currency), None, "{currency}");
            assert_eq!(from_minor(1_230, currency), None, "{currency}");
        }
        assert!(is_supported("usd") && is_supported("jpy") && is_supported("isk"));
    }

    #[test]
    fn an_amount_past_eight_digits_is_not_charged() {
        assert_eq!(to_minor((MAX_CHARGE_MINOR + 1) * 10_000, "usd"), None);
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
    }
}
