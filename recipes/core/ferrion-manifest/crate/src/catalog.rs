use crate::parse::validate_id_segment;
use crate::{MAX_USB_CLASS_LEN, MIN_USB_CLASS_LEN};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CapabilityClass {
    Automatic,
    Asked,
    SettingsOnly,
    Never,
}

impl CapabilityClass {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Automatic => "automatic",
            Self::Asked => "asked",
            Self::SettingsOnly => "settings-only",
            Self::Never => "never",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CapabilityMatch {
    Exact,
    Prefix,
}

impl CapabilityMatch {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Prefix => "prefix",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CapabilitySpec {
    pub name: &'static str,
    pub match_kind: CapabilityMatch,
    pub class: CapabilityClass,
}

pub const USB_DEVICE_PREFIX: &str = "devices-usb-";

pub const CATALOG: &[CapabilitySpec] = &[
    CapabilitySpec {
        name: "display",
        match_kind: CapabilityMatch::Exact,
        class: CapabilityClass::Automatic,
    },
    CapabilitySpec {
        name: "gpu",
        match_kind: CapabilityMatch::Exact,
        class: CapabilityClass::Automatic,
    },
    CapabilitySpec {
        name: "audio-playback",
        match_kind: CapabilityMatch::Exact,
        class: CapabilityClass::Automatic,
    },
    CapabilitySpec {
        name: "network-internet",
        match_kind: CapabilityMatch::Exact,
        class: CapabilityClass::Automatic,
    },
    CapabilitySpec {
        name: "sensors",
        match_kind: CapabilityMatch::Exact,
        class: CapabilityClass::Automatic,
    },
    CapabilitySpec {
        name: "code-jit",
        match_kind: CapabilityMatch::Exact,
        class: CapabilityClass::Automatic,
    },
    CapabilitySpec {
        name: "network-local",
        match_kind: CapabilityMatch::Exact,
        class: CapabilityClass::Asked,
    },
    CapabilitySpec {
        name: "network-loopback-shared",
        match_kind: CapabilityMatch::Exact,
        class: CapabilityClass::Asked,
    },
    CapabilitySpec {
        name: "gamepad",
        match_kind: CapabilityMatch::Exact,
        class: CapabilityClass::Asked,
    },
    CapabilitySpec {
        name: "files-documents",
        match_kind: CapabilityMatch::Exact,
        class: CapabilityClass::Asked,
    },
    CapabilitySpec {
        name: "files-downloads",
        match_kind: CapabilityMatch::Exact,
        class: CapabilityClass::Asked,
    },
    CapabilitySpec {
        name: "files-desktop",
        match_kind: CapabilityMatch::Exact,
        class: CapabilityClass::Asked,
    },
    CapabilitySpec {
        name: "files-pictures",
        match_kind: CapabilityMatch::Exact,
        class: CapabilityClass::Asked,
    },
    CapabilitySpec {
        name: "files-music",
        match_kind: CapabilityMatch::Exact,
        class: CapabilityClass::Asked,
    },
    CapabilitySpec {
        name: "files-videos",
        match_kind: CapabilityMatch::Exact,
        class: CapabilityClass::Asked,
    },
    CapabilitySpec {
        name: "files-removable",
        match_kind: CapabilityMatch::Exact,
        class: CapabilityClass::Asked,
    },
    CapabilitySpec {
        name: "camera",
        match_kind: CapabilityMatch::Exact,
        class: CapabilityClass::Asked,
    },
    CapabilitySpec {
        name: "microphone",
        match_kind: CapabilityMatch::Exact,
        class: CapabilityClass::Asked,
    },
    CapabilitySpec {
        name: "audio-capture-system",
        match_kind: CapabilityMatch::Exact,
        class: CapabilityClass::Asked,
    },
    CapabilitySpec {
        name: "screen-capture",
        match_kind: CapabilityMatch::Exact,
        class: CapabilityClass::Asked,
    },
    CapabilitySpec {
        name: "notifications",
        match_kind: CapabilityMatch::Exact,
        class: CapabilityClass::Asked,
    },
    CapabilitySpec {
        name: "background",
        match_kind: CapabilityMatch::Exact,
        class: CapabilityClass::Asked,
    },
    CapabilitySpec {
        name: USB_DEVICE_PREFIX,
        match_kind: CapabilityMatch::Prefix,
        class: CapabilityClass::Asked,
    },
    CapabilitySpec {
        name: "code-exec-written",
        match_kind: CapabilityMatch::Exact,
        class: CapabilityClass::Asked,
    },
    CapabilitySpec {
        name: "linux-session",
        match_kind: CapabilityMatch::Exact,
        class: CapabilityClass::Asked,
    },
    CapabilitySpec {
        name: "files-home",
        match_kind: CapabilityMatch::Exact,
        class: CapabilityClass::SettingsOnly,
    },
    CapabilitySpec {
        name: "input-emulate",
        match_kind: CapabilityMatch::Exact,
        class: CapabilityClass::SettingsOnly,
    },
    CapabilitySpec {
        name: "accessibility-control",
        match_kind: CapabilityMatch::Exact,
        class: CapabilityClass::SettingsOnly,
    },
    CapabilitySpec {
        name: "debug-other",
        match_kind: CapabilityMatch::Exact,
        class: CapabilityClass::SettingsOnly,
    },
    CapabilitySpec {
        name: "layer",
        match_kind: CapabilityMatch::Exact,
        class: CapabilityClass::Never,
    },
    CapabilitySpec {
        name: "lock",
        match_kind: CapabilityMatch::Exact,
        class: CapabilityClass::Never,
    },
    CapabilitySpec {
        name: "windows",
        match_kind: CapabilityMatch::Exact,
        class: CapabilityClass::Never,
    },
    CapabilitySpec {
        name: "outputs",
        match_kind: CapabilityMatch::Exact,
        class: CapabilityClass::Never,
    },
    CapabilitySpec {
        name: "input-method",
        match_kind: CapabilityMatch::Exact,
        class: CapabilityClass::Never,
    },
    CapabilitySpec {
        name: "sandbox",
        match_kind: CapabilityMatch::Exact,
        class: CapabilityClass::Never,
    },
    CapabilitySpec {
        name: "clipboard",
        match_kind: CapabilityMatch::Exact,
        class: CapabilityClass::Never,
    },
    CapabilitySpec {
        name: "sudo",
        match_kind: CapabilityMatch::Exact,
        class: CapabilityClass::Never,
    },
];

pub fn validate_catalog_invariants() {
    assert!(!CATALOG.is_empty());
    let mut i = 0;
    while i < CATALOG.len() {
        let spec = &CATALOG[i];
        assert!(!spec.name.is_empty());
        assert!(
            spec.name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        );
        match spec.match_kind {
            CapabilityMatch::Exact => {
                assert!(!spec.name.starts_with('-') && !spec.name.ends_with('-'));
            }
            CapabilityMatch::Prefix => {
                assert!(spec.name.ends_with('-') && !spec.name.starts_with('-'));
            }
        }
        let mut j = i + 1;
        while j < CATALOG.len() {
            assert_ne!(spec.name, CATALOG[j].name);
            j += 1;
        }
        i += 1;
    }
}

pub fn lookup_capability(name: &str) -> Option<&'static CapabilitySpec> {
    validate_catalog_invariants();
    for spec in CATALOG {
        match spec.match_kind {
            CapabilityMatch::Exact => {
                if spec.name == name {
                    return Some(spec);
                }
            }
            CapabilityMatch::Prefix => {
                if let Some(suffix) = name.strip_prefix(spec.name)
                    && validate_id_segment(suffix, MIN_USB_CLASS_LEN, MAX_USB_CLASS_LEN).is_ok()
                {
                    return Some(spec);
                }
            }
        }
    }
    None
}
