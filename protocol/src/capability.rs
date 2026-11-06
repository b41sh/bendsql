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

/// What a server supports, decided from its version.
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capability {
    pub streaming_load: bool,
    pub arrow_data: bool,
    pub server_side_params: bool,
}

impl Capability {
    /// From the version a login returns (`v1.2.835-nightly`, `1.2.900`). A version that doesn't
    /// parse supports nothing optional.
    pub fn from_server_version(version: &str) -> Capability {
        let Some(v) = parse_version(version) else {
            return Capability::default();
        };
        Capability {
            streaming_load: v > (1, 2, 781),
            arrow_data: v > (1, 2, 835),
            // TODO: update the threshold when the server change is in a release
            server_side_params: v > (1, 2, 900),
        }
    }
}

/// `major.minor.patch` at the start of a version, after an optional `v`. A pre-release or build
/// suffix (`-nightly`, `+abc`) is ignored: against the release thresholds above, semver's order
/// gives the same answers (`1.2.836-nightly` is above `1.2.835`, and like `1.2.836` itself it is
/// not above `1.2.836`).
fn parse_version(version: &str) -> Option<(u64, u64, u64)> {
    let version = version.trim();
    let version = version.strip_prefix('v').unwrap_or(version);
    let core = match version.find(['-', '+']) {
        Some(i) => version.get(..i)?,
        None => version,
    };
    let mut parts = core.split('.').map(|p| p.parse::<u64>().ok());
    let (major, minor, patch) = (parts.next()??, parts.next()??, parts.next()??);
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(parse_version("v1.2.835"), Some((1, 2, 835)));
        assert_eq!(parse_version("1.2.836-nightly"), Some((1, 2, 836)));
        assert_eq!(parse_version("1.2.836+abc"), Some((1, 2, 836)));
        assert_eq!(parse_version("1.2"), None);
        assert_eq!(parse_version("x.y.z"), None);
        assert_eq!(parse_version(""), None);
    }

    #[test]
    fn capabilities() {
        let c = Capability::from_server_version("v1.2.901");
        assert!(c.streaming_load && c.arrow_data && c.server_side_params);
        let c = Capability::from_server_version("v1.2.836");
        assert!(c.arrow_data && !c.server_side_params);
        // as semver orders them: above 1.2.835, not above 1.2.900
        let c = Capability::from_server_version("1.2.836-nightly-f5457e85f4");
        assert!(c.arrow_data && !c.server_side_params);
        let c = Capability::from_server_version("1.2.900-nightly");
        assert!(!c.server_side_params);
        assert_eq!(
            Capability::from_server_version("junk"),
            Capability::default()
        );
    }
}
