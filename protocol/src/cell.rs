// Copyright 2021 Datafuse Labs
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Reading the cells of a JSON result whose text depends on the result's settings.
//!
//! Every cell of a JSON page is text (or `null`). These read the kinds whose text a caller
//! would otherwise have to know: booleans (`1` and `0`), numbers, and binary and geometry cells,
//! encoded as the result's settings say. Dates and strings are left as text.

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;

use crate::error::Error;
use crate::settings::{BinaryFormat, GeometryDataType};

/// A geometry or geography cell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Geometry {
    /// WKB or EWKB bytes.
    Binary(Vec<u8>),
    /// WKT, EWKT or GeoJSON.
    Text(String),
}

/// A `Boolean` cell: the server writes `1` and `0`; `true` and `false` (any case) are taken too.
pub fn boolean(cell: &str) -> Result<bool, Error> {
    match cell.trim() {
        "1" => Ok(true),
        "0" => Ok(false),
        t if t.eq_ignore_ascii_case("true") => Ok(true),
        t if t.eq_ignore_ascii_case("false") => Ok(false),
        _ => Err(not_a("Boolean", cell)),
    }
}

/// A signed integer cell (`Int8` to `Int64`, and unsigned ones that fit).
pub fn int(cell: &str) -> Result<i64, Error> {
    cell.trim()
        .parse()
        .map_err(|_| not_a("signed integer", cell))
}

/// An unsigned integer cell (`UInt8` to `UInt64`, counts).
pub fn uint(cell: &str) -> Result<u64, Error> {
    cell.trim()
        .parse()
        .map_err(|_| not_a("unsigned integer", cell))
}

/// A `Float32`, `Float64` or `Decimal` cell as a double (a decimal of more than about 15 digits
/// loses its last ones). `inf`, `-inf` and `nan` read as themselves.
pub fn float(cell: &str) -> Result<f64, Error> {
    cell.trim().parse().map_err(|_| not_a("number", cell))
}

fn not_a(what: &str, cell: &str) -> Error {
    Error::Protocol(format!(
        "a {what} cell holds {:?}",
        cell.chars().take(64).collect::<String>()
    ))
}

/// The bytes of a `Binary` cell.
pub fn binary(cell: &str, format: BinaryFormat) -> Result<Vec<u8>, Error> {
    match format {
        BinaryFormat::Hex => hex(cell),
        BinaryFormat::Base64 => BASE64_STANDARD
            .decode(cell)
            .map_err(|e| Error::Protocol(format!("a binary cell isn't base64: {e}"))),
        BinaryFormat::Utf8 | BinaryFormat::Utf8Lossy => Ok(cell.as_bytes().to_vec()),
    }
}

/// A `Geometry` or `Geography` cell: WKB and EWKB come as hex, the other formats as text.
pub fn geometry(cell: &str, format: GeometryDataType) -> Result<Geometry, Error> {
    match format {
        GeometryDataType::WKB | GeometryDataType::EWKB => hex(cell).map(Geometry::Binary),
        GeometryDataType::WKT | GeometryDataType::EWKT | GeometryDataType::GEOJSON => {
            Ok(Geometry::Text(cell.to_string()))
        }
    }
}

/// Decodes hex text, in either case.
pub fn hex(text: &str) -> Result<Vec<u8>, Error> {
    fn nibble(b: u8) -> Option<u8> {
        match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => None,
        }
    }
    let (pairs, rest) = text.as_bytes().as_chunks::<2>();
    if !rest.is_empty() {
        return Err(Error::Protocol(format!(
            "hex text of odd length {}",
            text.len()
        )));
    }
    pairs
        .iter()
        .enumerate()
        .map(|(i, [hi, lo])| match (nibble(*hi), nibble(*lo)) {
            (Some(hi), Some(lo)) => Ok((hi << 4) | lo),
            _ => Err(Error::Protocol(format!(
                "a hex cell holds a character other than 0-9, a-f at byte {}",
                i * 2
            ))),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn booleans_and_numbers() {
        assert!(boolean("1").unwrap());
        assert!(!boolean("0").unwrap());
        assert!(boolean("TRUE").unwrap());
        assert!(boolean("2").is_err());
        assert_eq!(int("-42").unwrap(), -42);
        assert!(int("18446744073709551615").is_err());
        assert_eq!(uint("18446744073709551615").unwrap(), u64::MAX);
        assert!(uint("-1").is_err());
        assert_eq!(float("12.5").unwrap(), 12.5);
        assert!((float("123456789.123456789").unwrap() - 123_456_789.123_456_79).abs() < 1e-6);
        assert!(float("inf").unwrap().is_infinite());
        assert!(float("1,5").is_err());
        let long = "x".repeat(10_000);
        let Err(Error::Protocol(m)) = int(&long) else {
            panic!("expected an error")
        };
        assert!(m.len() < 200);
    }

    #[test]
    fn binary_formats() {
        for (format, encoded) in [
            (BinaryFormat::Hex, "78797A"),
            (BinaryFormat::Hex, "78797a"),
            (BinaryFormat::Base64, "eHl6"),
            (BinaryFormat::Utf8, "xyz"),
            (BinaryFormat::Utf8Lossy, "xyz"),
        ] {
            assert_eq!(binary(encoded, format).unwrap(), b"xyz", "{format:?}");
        }
        assert!(binary("7", BinaryFormat::Hex).is_err());
        assert!(binary("zz", BinaryFormat::Hex).is_err());
        assert!(binary("é1", BinaryFormat::Hex).is_err());
        assert!(binary("***", BinaryFormat::Base64).is_err());
    }

    #[test]
    fn geometry_formats() {
        // POINT(1 2) as little-endian WKB
        let wkb = "0101000000000000000000F03F0000000000000040";
        let Geometry::Binary(bytes) = geometry(wkb, GeometryDataType::WKB).unwrap() else {
            panic!("WKB should be bytes")
        };
        assert_eq!(bytes.len(), 21);
        assert_eq!(bytes.first(), Some(&1));
        assert_eq!(
            geometry("POINT(1 2)", GeometryDataType::WKT).unwrap(),
            Geometry::Text("POINT(1 2)".into())
        );
        assert!(geometry("POINT(1 2)", GeometryDataType::EWKB).is_err());
    }
}
