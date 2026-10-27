use serde_json::Value;
use worker::{Env, Error, Result};
use wasm_bindgen::JsValue;

pub fn int(value: &Value) -> Option<i64> {
    value.as_i64()
}

pub fn now() -> i64 {
    (js_sys::Date::now() / 1000.0).floor() as i64
}

/// UTC calendar day of an epoch timestamp, independent of Cron's minute.
pub fn utc_day(seconds: i64) -> i64 {
    seconds.div_euclid(86_400)
}

pub fn field_int(value: &Value, name: &str) -> Option<i64> {
    int(&value[name])
}

pub fn field_str<'a>(value: &'a Value, name: &str) -> &'a str {
    value[name].as_str().unwrap_or("")
}

pub fn js_i64(v: i64) -> JsValue {
    JsValue::from_f64(v as f64)
}

pub fn js_str(v: &str) -> JsValue {
    JsValue::from_str(v)
}

pub fn config(env: &Env, name: &str) -> String {
    env.var(name).map(|v| v.to_string()).unwrap_or_default()
}

pub fn flag(env: &Env, name: &str, default: bool) -> bool {
    match config(env, name).as_str() {
        "true" => true,
        "false" => false,
        _ => default,
    }
}

pub fn secret(env: &Env, name: &str) -> Result<String> {
    env.secret(name).map(|v| v.to_string())
}

pub fn fail(text: &str) -> Error {
    Error::RustError(text.to_string())
}

pub fn safe_equal(a: &str, b: &str) -> bool {
    let a = a.as_bytes();
    let b = b.as_bytes();
    let mut difference = a.len() ^ b.len();
    let len = std::cmp::max(a.len(), b.len());
    for i in 0..len {
        difference |= (a.get(i).copied().unwrap_or(0) ^ b.get(i).copied().unwrap_or(0)) as usize;
    }
    difference == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn utc_day_has_no_one_minute_midnight_window() {
        let midnight = 20_000 * 86_400;
        assert_eq!(utc_day(midnight - 1), 19_999);
        assert_eq!(utc_day(midnight), 20_000);
        assert_eq!(utc_day(midnight + 59), 20_000);
        assert_eq!(utc_day(midnight + 61), 20_000);
        assert_eq!(utc_day(midnight + 86_399), 20_000);
        assert_eq!(utc_day(midnight + 86_400), 20_001);
        assert_eq!(utc_day(-1), -1);
    }
    #[test]
    fn safe_equal_handles_mismatches() {
        assert!(safe_equal("s3cRet", "s3cRet"));
        assert!(!safe_equal("s3cRet", "s3cReT"));
        assert!(!safe_equal("s3cRet", "s3cRet2"));
        assert!(!safe_equal("", "s3cRet"));
    }
}
