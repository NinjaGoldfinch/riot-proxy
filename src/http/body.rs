//! JSON request bodies read the way v1's Fastify did: ajv with `coerceTypes:
//! 'array'` and `removeAdditional`, failures in the envelope with ajv's wording
//! (`body/name must NOT have fewer than 1 characters`, `body must have required
//! property 'name'`). Unknown fields are ignored.
//!
//! Coercion follows ajv's table for the types the admin bodies use: strings
//! from numbers and booleans, integers from numeric strings and booleans,
//! booleans from `"true"`/`"false"` and `1`/`0`, and a scalar where an array is
//! expected becomes a one-element array.
//!
//! One deliberate difference: a body that is not JSON is a `VALIDATION` 400.
//! v1's error handler did not recognise Fastify's parse error and answered 500.

use serde_json::{Map, Value};

use crate::http::validate::invalid;
use crate::http::{ApiError, ErrorCode};

/// A parsed body object.
#[derive(Debug, Clone, Default)]
pub struct Body(Map<String, Value>);

impl Body {
    /// Parse `bytes` as a JSON object. An empty body reads as `{}` when the
    /// schema has no required fields; callers with required fields get
    /// "must have required property" from [`Body::required`] instead.
    pub fn parse(bytes: &[u8]) -> Result<Self, ApiError> {
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return Ok(Self::default());
        }
        match serde_json::from_slice::<Value>(bytes) {
            Ok(Value::Object(m)) => Ok(Self(m)),
            Ok(_) => Err(ApiError::new(ErrorCode::Validation, "body must be object")),
            Err(_) => Err(ApiError::new(ErrorCode::Validation, "body is not valid JSON")),
        }
    }

    fn get(&self, name: &str) -> Option<&Value> {
        self.0.get(name)
    }

    /// Fail unless every name is present (ajv reports the first missing one).
    pub fn required(&self, names: &[&str]) -> Result<(), ApiError> {
        match names.iter().find(|n| !self.0.contains_key(**n)) {
            Some(n) => Err(ApiError::new(
                ErrorCode::Validation,
                format!("body must have required property '{n}'"),
            )),
            None => Ok(()),
        }
    }

    /// A string field, with ajv's code-point length bounds.
    pub fn string(&self, name: &str, min: usize, max: usize) -> Result<Option<String>, ApiError> {
        let Some(v) = self.get(name) else { return Ok(None) };
        let s = to_string(v).ok_or_else(|| invalid("body", name, "must be string"))?;
        let n = s.chars().count();
        if n < min {
            return Err(invalid(
                "body",
                name,
                &format!("must NOT have fewer than {min} characters"),
            ));
        }
        if n > max {
            return Err(invalid(
                "body",
                name,
                &format!("must NOT have more than {max} characters"),
            ));
        }
        Ok(Some(s))
    }

    pub fn integer(&self, name: &str, min: i64, max: i64) -> Result<Option<i64>, ApiError> {
        let Some(v) = self.get(name) else { return Ok(None) };
        let n = to_integer(v).ok_or_else(|| invalid("body", name, "must be integer"))?;
        if n < min {
            return Err(invalid("body", name, &format!("must be >= {min}")));
        }
        if n > max {
            return Err(invalid("body", name, &format!("must be <= {max}")));
        }
        Ok(Some(n))
    }

    pub fn boolean(&self, name: &str) -> Result<Option<bool>, ApiError> {
        let Some(v) = self.get(name) else { return Ok(None) };
        to_bool(v)
            .map(Some)
            .ok_or_else(|| invalid("body", name, "must be boolean"))
    }

    /// An array of strings from `allowed`, at least `min_items` long.
    pub fn enum_array(
        &self,
        name: &str,
        allowed: &[&'static str],
        min_items: usize,
    ) -> Result<Option<Vec<&'static str>>, ApiError> {
        let Some(v) = self.get(name) else { return Ok(None) };
        let items: Vec<&Value> = match v {
            Value::Array(a) => a.iter().collect(),
            // coerceTypes 'array': a scalar is a one-element array.
            other => vec![other],
        };
        if items.len() < min_items {
            return Err(invalid(
                "body",
                name,
                &format!("must NOT have fewer than {min_items} items"),
            ));
        }
        items
            .iter()
            .enumerate()
            .map(|(i, item)| {
                let at = format!("{name}/{i}");
                let s = to_string(item).ok_or_else(|| invalid("body", &at, "must be string"))?;
                allowed
                    .iter()
                    .copied()
                    .find(|a| *a == s)
                    .ok_or_else(|| invalid("body", &at, "must be equal to one of the allowed values"))
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Some)
    }

    /// A string field checked by one of the path validators, so the rules and
    /// messages match the path parameter of the same name.
    pub fn with<T>(
        &self,
        name: &str,
        check: impl FnOnce(&str, &str) -> Result<T, ApiError>,
    ) -> Result<Option<T>, ApiError> {
        let Some(v) = self.get(name) else { return Ok(None) };
        let s = to_string(v).ok_or_else(|| invalid("body", name, "must be string"))?;
        check("body", &s).map(Some)
    }
}

fn to_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Null => Some(String::new()),
        _ => None,
    }
}

fn to_integer(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64().or_else(|| {
            let f = n.as_f64()?;
            #[allow(clippy::cast_possible_truncation, clippy::float_cmp)]
            (f.fract() == 0.0 && f.abs() < 9.0e15).then_some(f as i64)
        }),
        Value::String(s) => s.trim().parse().ok().filter(|_| !s.trim().is_empty()),
        Value::Bool(b) => Some(i64::from(*b)),
        Value::Null => Some(0),
        _ => None,
    }
}

fn to_bool(v: &Value) -> Option<bool> {
    match v {
        Value::Bool(b) => Some(*b),
        Value::String(s) if s == "true" => Some(true),
        Value::String(s) if s == "false" => Some(false),
        Value::Number(n) if n.as_i64() == Some(1) => Some(true),
        Value::Number(n) if n.as_i64() == Some(0) => Some(false),
        Value::Null => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(s: &str) -> Body {
        Body::parse(s.as_bytes()).unwrap()
    }

    fn msg<T: std::fmt::Debug>(r: Result<T, ApiError>) -> (ErrorCode, String) {
        let e = r.unwrap_err();
        (e.code, e.message)
    }

    #[test]
    fn parse_wants_an_object() {
        assert!(Body::parse(b"").is_ok());
        assert_eq!(msg(Body::parse(b"[]")).1, "body must be object");
        assert_eq!(msg(Body::parse(b"{nope")).1, "body is not valid JSON");
    }

    #[test]
    fn required_names_the_first_missing_property() {
        let b = body(r#"{"a":1}"#);
        assert!(b.required(&["a"]).is_ok());
        assert_eq!(
            msg(b.required(&["a", "name", "x"])).1,
            "body must have required property 'name'"
        );
    }

    #[test]
    fn strings_integers_and_booleans_coerce_as_ajv_did() {
        let b = body(r#"{"name":5,"q":"12","t":"true","f":0,"bad":{}}"#);
        assert_eq!(b.string("name", 1, 100).unwrap().as_deref(), Some("5"));
        assert_eq!(b.integer("q", 1, 100).unwrap(), Some(12));
        assert_eq!(b.boolean("t").unwrap(), Some(true));
        assert_eq!(b.boolean("f").unwrap(), Some(false));
        assert_eq!(b.string("absent", 1, 2).unwrap(), None);
        assert_eq!(msg(b.string("bad", 1, 100)).1, "body/bad must be string");
        assert_eq!(msg(b.integer("name", 6, 9)).1, "body/name must be >= 6");
        assert_eq!(msg(b.boolean("q")).1, "body/q must be boolean");
        assert_eq!(
            msg(body(r#"{"q":1.5}"#).integer("q", 0, 9)).1,
            "body/q must be integer"
        );
        assert_eq!(
            msg(body(r#"{"name":""}"#).string("name", 1, 100)).1,
            "body/name must NOT have fewer than 1 characters"
        );
    }

    #[test]
    fn enum_arrays() {
        let all = ["read", "admin"];
        assert_eq!(
            body(r#"{"s":["read","admin"]}"#)
                .enum_array("s", &all, 1)
                .unwrap(),
            Some(vec!["read", "admin"])
        );
        assert_eq!(
            body(r#"{"s":"admin"}"#).enum_array("s", &all, 1).unwrap(),
            Some(vec!["admin"])
        );
        assert_eq!(
            msg(body(r#"{"s":[]}"#).enum_array("s", &all, 1)).1,
            "body/s must NOT have fewer than 1 items"
        );
        assert_eq!(
            msg(body(r#"{"s":["read","root"]}"#).enum_array("s", &all, 1)).1,
            "body/s/1 must be equal to one of the allowed values"
        );
    }
}
