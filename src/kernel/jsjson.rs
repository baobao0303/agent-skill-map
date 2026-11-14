//! JSON output that matches JavaScript's `JSON.stringify` byte for byte:
//! numbers print like `Number.prototype.toString` (`1`, not `1.0`).

use serde::Serialize;
use serde_json::ser::{CompactFormatter, Formatter, PrettyFormatter};
use std::io;

/// `Number.prototype.toString` for a finite f64.
pub fn js_number(f: f64) -> String {
    let mut buf = ryu_js::Buffer::new();
    buf.format(f).to_string()
}

struct Js<F>(F);

macro_rules! delegate {
    ($($name:ident ( $($arg:ident : $ty:ty),* );)*) => {
        $(fn $name<W: ?Sized + io::Write>(&mut self, w: &mut W $(, $arg: $ty)*) -> io::Result<()> {
            self.0.$name(w $(, $arg)*)
        })*
    };
}

impl<F: Formatter> Formatter for Js<F> {
    fn write_f64<W: ?Sized + io::Write>(&mut self, w: &mut W, value: f64) -> io::Result<()> {
        w.write_all(js_number(value).as_bytes())
    }
    fn write_f32<W: ?Sized + io::Write>(&mut self, w: &mut W, value: f32) -> io::Result<()> {
        self.write_f64(w, value as f64)
    }
    delegate! {
        begin_array();
        end_array();
        begin_array_value(first: bool);
        end_array_value();
        begin_object();
        end_object();
        begin_object_key(first: bool);
        end_object_key();
        begin_object_value();
        end_object_value();
    }
}

pub fn to_string<T: Serialize + ?Sized>(value: &T) -> String {
    let mut out = Vec::new();
    let mut ser = serde_json::Serializer::with_formatter(&mut out, Js(CompactFormatter));
    value.serialize(&mut ser).expect("serializable");
    String::from_utf8(out).expect("utf8")
}

pub fn to_string_pretty<T: Serialize + ?Sized>(value: &T) -> String {
    let mut out = Vec::new();
    let mut ser = serde_json::Serializer::with_formatter(&mut out, Js(PrettyFormatter::with_indent(b"  ")));
    value.serialize(&mut ser).expect("serializable");
    String::from_utf8(out).expect("utf8")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_print_like_javascript() {
        assert_eq!(js_number(1.0), "1");
        assert_eq!(js_number(0.25), "0.25");
        assert_eq!(js_number(0.1), "0.1");
        assert_eq!(js_number(1e21), "1e+21");
        assert_eq!(to_string(&serde_json::json!({"c": 1.0, "d": [0.85, 2]})), r#"{"c":1,"d":[0.85,2]}"#);
    }
}
