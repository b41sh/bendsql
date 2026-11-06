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

//! Column types, parsed from the type names of a JSON result's schema
//! (`Nullable(Decimal(38, 4))`, `Array(String NULL)`, `Geometry`).

use std::fmt;
use std::str::FromStr;

use crate::error::Error;

/// The deepest nesting of a type name parsed (`Array(Array(…))`): a deeper one is an error, so
/// a hostile schema can't exhaust the stack.
pub const MAX_TYPE_DEPTH: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NumberDataType {
    UInt8,
    UInt16,
    UInt32,
    UInt64,
    Int8,
    Int16,
    Int32,
    Int64,
    Float32,
    Float64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecimalSize {
    pub precision: u8,
    pub scale: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecimalDataType {
    Decimal64(DecimalSize),
    Decimal128(DecimalSize),
    Decimal256(DecimalSize),
}
impl DecimalDataType {
    pub fn decimal_size(&self) -> &DecimalSize {
        match self {
            DecimalDataType::Decimal64(size) => size,
            DecimalDataType::Decimal128(size) => size,
            DecimalDataType::Decimal256(size) => size,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DataType {
    Null,
    EmptyArray,
    EmptyMap,
    Boolean,
    Binary,
    String,
    Number(NumberDataType),
    Decimal(DecimalDataType),
    Timestamp,
    TimestampTz,
    Date,
    Nullable(Box<DataType>),
    Array(Box<DataType>),
    /// A tuple of the key and value types.
    Map(Box<DataType>),
    Tuple(Vec<DataType>),
    Variant,
    Bitmap,
    Geometry,
    Geography,
    Interval,
    Vector(u64),
}

impl DataType {
    pub fn is_numeric(&self) -> bool {
        match self {
            DataType::Number(_) | DataType::Decimal(_) => true,
            DataType::Nullable(inner) => inner.is_numeric(),
            _ => false,
        }
    }

    /// The type without `Nullable`.
    pub fn remove_nullable(&self) -> &DataType {
        match self {
            DataType::Nullable(inner) => inner.remove_nullable(),
            other => other,
        }
    }
}

impl fmt::Display for DataType {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            DataType::Null => write!(f, "Null"),
            DataType::EmptyArray => write!(f, "EmptyArray"),
            DataType::EmptyMap => write!(f, "EmptyMap"),
            DataType::Boolean => write!(f, "Boolean"),
            DataType::Binary => write!(f, "Binary"),
            DataType::String => write!(f, "String"),
            DataType::Number(n) => match n {
                NumberDataType::UInt8 => write!(f, "UInt8"),
                NumberDataType::UInt16 => write!(f, "UInt16"),
                NumberDataType::UInt32 => write!(f, "UInt32"),
                NumberDataType::UInt64 => write!(f, "UInt64"),
                NumberDataType::Int8 => write!(f, "Int8"),
                NumberDataType::Int16 => write!(f, "Int16"),
                NumberDataType::Int32 => write!(f, "Int32"),
                NumberDataType::Int64 => write!(f, "Int64"),
                NumberDataType::Float32 => write!(f, "Float32"),
                NumberDataType::Float64 => write!(f, "Float64"),
            },
            DataType::Decimal(d) => {
                let size = d.decimal_size();
                write!(f, "Decimal({}, {})", size.precision, size.scale)
            }
            DataType::Timestamp => write!(f, "Timestamp"),
            DataType::TimestampTz => write!(f, "Timestamp_Tz"),
            DataType::Date => write!(f, "Date"),
            DataType::Nullable(inner) => write!(f, "Nullable({inner})"),
            DataType::Array(inner) => write!(f, "Array({inner})"),
            DataType::Map(inner) => match inner.as_ref() {
                DataType::Tuple(tys) => match tys.as_slice() {
                    [key, value] => write!(f, "Map({key}, {value})"),
                    _ => write!(f, "Map({inner})"),
                },
                other => write!(f, "Map({other})"),
            },
            DataType::Tuple(inner) => {
                let inner = inner
                    .iter()
                    .map(|x| x.to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                write!(f, "Tuple({inner})")
            }
            DataType::Variant => write!(f, "Variant"),
            DataType::Bitmap => write!(f, "Bitmap"),
            DataType::Geometry => write!(f, "Geometry"),
            DataType::Geography => write!(f, "Geography"),
            DataType::Interval => write!(f, "Interval"),
            DataType::Vector(d) => write!(f, "Vector({d})"),
        }
    }
}

impl FromStr for DataType {
    type Err = Error;

    /// Parses a type name of a JSON result's schema.
    fn from_str(s: &str) -> Result<Self, Error> {
        let desc = parse_type_desc(s, 0)?;
        from_desc(&desc)
    }
}

fn bad_type(msg: impl fmt::Display) -> Error {
    Error::Protocol(format!("invalid column type: {msg}"))
}

fn one_arg<'d, 't>(desc: &'d TypeDesc<'t>) -> Result<&'d TypeDesc<'t>, Error> {
    match desc.args.as_slice() {
        [arg] => Ok(arg),
        _ => Err(bad_type(format_args!(
            "{} type must have one argument",
            desc.name
        ))),
    }
}

fn from_desc(desc: &TypeDesc) -> Result<DataType, Error> {
    if desc.nullable {
        let mut desc = desc.clone();
        desc.nullable = false;
        let inner = from_desc(&desc)?;
        return Ok(DataType::Nullable(Box::new(inner)));
    }
    let dt = match desc.name {
        "NULL" | "Null" => DataType::Null,
        "Boolean" => DataType::Boolean,
        "Binary" => DataType::Binary,
        "String" => DataType::String,
        "Int8" => DataType::Number(NumberDataType::Int8),
        "Int16" => DataType::Number(NumberDataType::Int16),
        "Int32" => DataType::Number(NumberDataType::Int32),
        "Int64" => DataType::Number(NumberDataType::Int64),
        "UInt8" => DataType::Number(NumberDataType::UInt8),
        "UInt16" => DataType::Number(NumberDataType::UInt16),
        "UInt32" => DataType::Number(NumberDataType::UInt32),
        "UInt64" => DataType::Number(NumberDataType::UInt64),
        "Float32" => DataType::Number(NumberDataType::Float32),
        "Float64" => DataType::Number(NumberDataType::Float64),
        "Decimal" => {
            let [precision, scale] = desc.args.as_slice() else {
                return Err(bad_type("Decimal type must have two arguments"));
            };
            let parse = |arg: &TypeDesc| {
                arg.name
                    .parse::<u8>()
                    .map_err(|_| bad_type(format_args!("Decimal argument {:?}", arg.name)))
            };
            let size = DecimalSize {
                precision: parse(precision)?,
                scale: parse(scale)?,
            };
            if size.precision <= 38 {
                DataType::Decimal(DecimalDataType::Decimal128(size))
            } else {
                DataType::Decimal(DecimalDataType::Decimal256(size))
            }
        }
        "Timestamp" => DataType::Timestamp,
        "Timestamp_Tz" => DataType::TimestampTz,
        "Date" => DataType::Date,
        "Nullable" => {
            // the inner type's own NULL marker adds nothing
            let mut inner = one_arg(desc)?.clone();
            inner.nullable = false;
            DataType::Nullable(Box::new(from_desc(&inner)?))
        }
        "Array" => {
            let inner = one_arg(desc)?;
            if inner.name == "Nothing" {
                DataType::EmptyArray
            } else {
                DataType::Array(Box::new(from_desc(inner)?))
            }
        }
        "Map" => match desc.args.as_slice() {
            [only] if only.name == "Nothing" => DataType::EmptyMap,
            [key, value] => DataType::Map(Box::new(DataType::Tuple(vec![
                from_desc(key)?,
                from_desc(value)?,
            ]))),
            _ => return Err(bad_type("Map type must have two arguments")),
        },
        "Tuple" => DataType::Tuple(
            desc.args
                .iter()
                .map(from_desc)
                .collect::<Result<Vec<_>, _>>()?,
        ),
        "Variant" => DataType::Variant,
        "Bitmap" => DataType::Bitmap,
        "Geometry" => DataType::Geometry,
        "Geography" => DataType::Geography,
        "Interval" => DataType::Interval,
        "Vector" => {
            let arg = one_arg(desc)?;
            let dimension = arg
                .name
                .parse::<u64>()
                .map_err(|_| bad_type(format_args!("Vector argument {:?}", arg.name)))?;
            DataType::Vector(dimension)
        }
        _ => return Err(bad_type(format_args!("unknown type {:?}", desc.name))),
    };
    Ok(dt)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TypeDesc<'t> {
    name: &'t str,
    nullable: bool,
    args: Vec<TypeDesc<'t>>,
}

/// Splits a type name into its name, arguments and `NULL` marker. `level` is how deep inside
/// other types this one is.
fn parse_type_desc(s: &str, level: usize) -> Result<TypeDesc<'_>, Error> {
    if level > MAX_TYPE_DEPTH {
        return Err(bad_type(format_args!(
            "nested more than {MAX_TYPE_DEPTH} levels deep"
        )));
    }
    let invalid = || {
        bad_type(format_args!(
            "{:?}",
            s.chars().take(200).collect::<String>()
        ))
    };
    // every split is at an ASCII delimiter, so `get` never sees a broken character
    let part = |from: usize, to: usize| s.get(from..to).ok_or_else(invalid);
    let mut name = "";
    let mut args = vec![];
    let mut depth = 0usize;
    let mut start = 0;
    let mut nullable = false;
    for (i, c) in s.char_indices() {
        match c {
            '(' => {
                if depth == 0 {
                    name = part(start, i)?;
                    start = i + 1;
                }
                depth += 1;
            }
            ')' => {
                depth = depth.checked_sub(1).ok_or_else(invalid)?;
                if depth == 0 {
                    let arg = part(start, i)?;
                    if !arg.is_empty() {
                        args.push(parse_type_desc(arg, level + 1)?);
                    }
                    start = i + 1;
                }
            }
            ',' if depth == 1 => {
                args.push(parse_type_desc(part(start, i)?, level + 1)?);
                start = i + 1;
            }
            ' ' if depth == 0 => {
                let word = part(start, i)?;
                if !word.is_empty() {
                    name = word;
                }
                start = i + 1;
            }
            _ => {}
        }
    }
    if depth != 0 {
        return Err(invalid());
    }
    let rest = s.get(start..).unwrap_or("");
    if !rest.is_empty() {
        if name.is_empty() {
            name = rest;
        } else if rest == "NULL" {
            nullable = true;
        } else {
            return Err(invalid());
        }
    }
    Ok(TypeDesc {
        name: name.trim(),
        nullable,
        args,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> DataType {
        s.parse().unwrap()
    }

    #[test]
    fn plain_and_parameterised_types() {
        assert_eq!(parse("String"), DataType::String);
        assert_eq!(parse("Geometry"), DataType::Geometry);
        assert_eq!(
            parse("Decimal(38, 4)"),
            DataType::Decimal(DecimalDataType::Decimal128(DecimalSize {
                precision: 38,
                scale: 4
            }))
        );
        assert!(matches!(
            parse("Decimal(76, 2)"),
            DataType::Decimal(DecimalDataType::Decimal256(_))
        ));
        assert_eq!(parse("Vector(3)"), DataType::Vector(3));
        assert_eq!(parse("Array(Nothing)"), DataType::EmptyArray);
        assert_eq!(parse("Map(Nothing)"), DataType::EmptyMap);
    }

    #[test]
    fn nullable_forms() {
        let nullable_string = DataType::Nullable(Box::new(DataType::String));
        assert_eq!(parse("Nullable(String)"), nullable_string);
        assert_eq!(parse("String NULL"), nullable_string);
        assert_eq!(
            parse("Nullable(Map(String, String NULL))"),
            DataType::Nullable(Box::new(DataType::Map(Box::new(DataType::Tuple(vec![
                DataType::String,
                nullable_string.clone(),
            ])))))
        );
    }

    #[test]
    fn display_round_trips() {
        for ty in [
            "Nullable(Tuple(String, Array(Int32)))",
            "Map(String, Array(Int64))",
            "Decimal(10, 2)",
            "Timestamp_Tz",
        ] {
            assert_eq!(parse(ty).to_string(), ty);
        }
    }

    #[test]
    fn hostile_type_names_are_errors() {
        for bad in [
            "",
            "Decimal",
            "Decimal(1)",
            "Decimal(999, 1)",
            "Vector",
            "Vector(x)",
            "Map(String)",
            "Array(String",
            "Array(String))",
            ")(",
            "Nope",
            "String NOTNULL",
            "Nullable(String, Int8)",
        ] {
            assert!(bad.parse::<DataType>().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn deep_nesting_is_refused_without_overflowing_the_stack() {
        let deep = "Array(".repeat(100_000) + "String" + &")".repeat(100_000);
        assert!(deep.parse::<DataType>().is_err());
        let ok = "Array(".repeat(MAX_TYPE_DEPTH) + "String" + &")".repeat(MAX_TYPE_DEPTH);
        assert!(ok.parse::<DataType>().is_ok());
    }

    #[test]
    fn multibyte_text_does_not_panic() {
        for s in ["Stríng", "Array(数)", "Map(é, ü)", "\u{0}"] {
            let _ = s.parse::<DataType>();
        }
    }
}
