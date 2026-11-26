//! Licences of what the window embeds and the Rust crates HushMic ships,
//! for About › Licenses and packaging. The crates and their texts are
//! generated from Cargo.lock (`scripts/gen-licenses.py`).

pub struct License {
    pub name: &'static str,
    pub text: &'static str,
}

pub const LICENSES: &[License] = &[
    License {
        name: "Geist and Geist Mono (SIL Open Font License 1.1)",
        text: include_str!("../../licenses/OFL-Geist.txt"),
    },
    License {
        name: "Lucide icons (ISC License)",
        text: include_str!("../../licenses/ISC-Lucide.txt"),
    },
    // the models and the C runtime in hushmic-denoiser/native: the model
    // card and the repository both say Apache-2.0 (no NOTICE file)
    License {
        name: "DPDFNet noise models and native runtime by Ceva (Apache License 2.0)",
        text: concat!(
            "The noise models (dpdfnet8_48khz_hr, dpdfnet2_48khz_hr) come from \
             https://huggingface.co/Ceva-IP/DPDFNet, the native runtime from \
             https://github.com/ceva-ip/DPDFNet (commit afa8292).\n\n",
            include_str!("../../../hushmic-denoiser/native/LICENSE")
        ),
    },
    // the release package's LICENSE and ThirdPartyNotices.txt, as shipped
    // next to libonnxruntime.so (ORT_VERSION in scripts/setup-assets.sh)
    License {
        name: "ONNX Runtime 1.27.0 (MIT License)",
        text: include_str!("../../licenses/onnxruntime/LICENSE.txt"),
    },
    License {
        name: "ONNX Runtime 1.27.0: third-party notices",
        text: include_str!("../../licenses/onnxruntime/ThirdPartyNotices.txt"),
    },
    // libopus.a as the release builds link it in from their build host,
    // ubuntu:22.04 (libopus-dev 1.3.1-0.1build2): upstream's COPYING at
    // v1.3.1
    License {
        name: "Opus 1.3.1 (libopus, BSD 3-Clause License)",
        text: concat!(
            "libopus from https://opus-codec.org, linked into the release \
             tarball, the .deb and the AppImage.\n\n",
            include_str!("../../licenses/opus/COPYING.txt")
        ),
    },
];

/// A crate HushMic ships.
pub struct Crate {
    pub name: &'static str,
    pub version: &'static str,
    /// Where its source is (the crate's repository; may be empty).
    pub source: &'static str,
}

/// Crates under the same licence texts.
pub struct CrateGroup {
    /// The licence as the crates state it (an SPDX expression).
    pub license: &'static str,
    pub crates: &'static [Crate],
    pub texts: &'static [&'static str],
}

pub use super::licenses_crates::GROUPS;

impl CrateGroup {
    /// The crates by name and version ("symphonia 0.5.5, …").
    pub fn names(&self) -> String {
        let names: Vec<String> = self
            .crates
            .iter()
            .map(|c| format!("{} {}", c.name, c.version))
            .collect();
        names.join(", ")
    }

    /// Its source must be offered with the binary (MPL-2.0: Symphonia):
    /// where it is, once per repository.
    pub fn sources(&self) -> Vec<&'static str> {
        if !self.license.contains("MPL") {
            return Vec::new();
        }
        let mut out: Vec<&'static str> = Vec::new();
        for c in self.crates {
            if !c.source.is_empty() && !out.contains(&c.source) {
                out.push(c.source);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The generated list matches Cargo.lock: every crate in it is locked
    /// at that version (a dependency update without
    /// `scripts/gen-licenses.py` fails here).
    #[test]
    fn the_crate_list_matches_the_lockfile() {
        let lock = include_str!("../../../../Cargo.lock");
        let mut locked = std::collections::HashSet::new();
        let mut name = None;
        for line in lock.lines() {
            if let Some(n) = line.strip_prefix("name = ") {
                name = Some(n.trim_matches('"'));
            } else if let (Some(v), Some(n)) = (line.strip_prefix("version = "), name.take()) {
                locked.insert((n, v.trim_matches('"')));
            }
        }
        let mut count = 0;
        for g in GROUPS {
            assert!(!g.texts.is_empty(), "{}", g.names());
            for c in g.crates {
                assert!(
                    locked.contains(&(c.name, c.version)),
                    "{} {} is not in Cargo.lock: run scripts/gen-licenses.py",
                    c.name,
                    c.version
                );
                count += 1;
            }
        }
        assert!(count > 100, "{count} crates");
    }

    /// The ONNX Runtime notices are those of the version every package
    /// ships.
    #[test]
    fn the_onnx_runtime_notices_match_the_shipped_version() {
        let assets = include_str!("../../../../scripts/setup-assets.sh");
        let version = assets
            .lines()
            .find_map(|l| l.strip_prefix("ORT_VERSION=\""))
            .and_then(|v| v.strip_suffix('"'))
            .expect("ORT_VERSION");
        let names: Vec<&str> = LICENSES.iter().map(|l| l.name).collect();
        let ort = names
            .iter()
            .filter(|n| n.starts_with("ONNX Runtime"))
            .count();
        assert_eq!(ort, 2, "{names:?}");
        for n in names.iter().filter(|n| n.starts_with("ONNX Runtime")) {
            assert!(n.contains(version), "{n} vs ORT_VERSION {version}");
        }
    }

    #[test]
    fn symphonia_says_where_its_source_is() {
        let g = GROUPS
            .iter()
            .find(|g| g.crates.iter().any(|c| c.name == "symphonia"))
            .expect("symphonia");
        assert_eq!(g.license, "MPL-2.0");
        assert_eq!(g.sources(), ["https://github.com/pdeljanov/Symphonia"]);
        assert!(g.texts[0].starts_with("Mozilla Public License Version 2.0"));
    }
}
