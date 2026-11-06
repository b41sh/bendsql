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

use std::str::FromStr;

use serde::Deserialize;

use crate::error::Error;

/// How the server formatted a result, as it says in `QueryResponse::settings`. The time zone
/// stays a name: parsing it needs a time zone database, which this crate leaves to its callers.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
pub struct QueryResultFormatSettings {
    pub timezone: Option<String>,
    pub geometry_output_format: Option<String>,
    pub arrow_result_version: Option<i64>,
    pub binary_output_format: Option<String>,
}

impl QueryResultFormatSettings {
    /// The geometry format, GeoJSON when the server doesn't say.
    pub fn geometry_format(&self) -> Result<GeometryDataType, Error> {
        match &self.geometry_output_format {
            None => Ok(GeometryDataType::default()),
            Some(t) => t.parse(),
        }
    }

    /// The binary format, hex when the server doesn't say.
    pub fn binary_format(&self) -> Result<BinaryFormat, Error> {
        match &self.binary_output_format {
            None => Ok(BinaryFormat::default()),
            Some(t) => t.parse(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
pub enum GeometryDataType {
    WKB,
    WKT,
    EWKB,
    EWKT,
    #[default]
    GEOJSON,
}

impl FromStr for GeometryDataType {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_uppercase().as_str() {
            "WKB" => Ok(GeometryDataType::WKB),
            "WKT" => Ok(GeometryDataType::WKT),
            "EWKB" => Ok(GeometryDataType::EWKB),
            "EWKT" => Ok(GeometryDataType::EWKT),
            "GEOJSON" => Ok(GeometryDataType::GEOJSON),
            _ => Err(Error::Protocol("Invalid geometry type format".to_string())),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BinaryFormat {
    #[default]
    Hex,
    Base64,
    Utf8,
    Utf8Lossy,
}

impl FromStr for BinaryFormat {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "hex" => Ok(BinaryFormat::Hex),
            "base64" => Ok(BinaryFormat::Base64),
            "utf-8" | "utf8" => Ok(BinaryFormat::Utf8),
            "utf-8-lossy" | "utf8-lossy" => Ok(BinaryFormat::Utf8Lossy),
            other => Err(Error::Protocol(format!(
                "Invalid binary format '{}', valid values: HEX | BASE64 | UTF-8 | UTF-8-LOSSY",
                other.chars().take(64).collect::<String>()
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_settings() {
        let settings: QueryResultFormatSettings = serde_json::from_str(
            r#"{
                "timezone": "Asia/Shanghai",
                "geometry_output_format": "wkt",
                "arrow_result_version": 2,
                "binary_output_format": "utf-8"
            }"#,
        )
        .unwrap();
        assert_eq!(settings.geometry_format().unwrap(), GeometryDataType::WKT);
        assert_eq!(settings.binary_format().unwrap(), BinaryFormat::Utf8);
        assert_eq!(settings.timezone.as_deref(), Some("Asia/Shanghai"));
        assert_eq!(settings.arrow_result_version, Some(2));
    }

    #[test]
    fn defaults() {
        let settings: QueryResultFormatSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(
            settings.geometry_format().unwrap(),
            GeometryDataType::GEOJSON
        );
        assert_eq!(settings.binary_format().unwrap(), BinaryFormat::Hex);
    }

    #[test]
    fn unknown_formats_are_errors() {
        assert!("WKX".parse::<GeometryDataType>().is_err());
        assert!("base32".parse::<BinaryFormat>().is_err());
    }
}
