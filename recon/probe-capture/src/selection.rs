//! User-facing encoder choice: which backend (GPU hardware encoder or CPU
//! software encoder) and which GPU. Pure logic so the policy is unit-tested;
//! the Windows side only supplies the discovered adapters and encoders.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Backend {
    /// Hardware on the preferred GPU, falling back to the CPU encoder.
    #[default]
    Auto,
    /// GPU hardware encoder only; never falls back to the CPU.
    Hardware,
    /// CPU software encoder only.
    Software,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AdapterChoice {
    /// The capture GPU first (zero-copy), then the rest by performance.
    #[default]
    Auto,
    /// Windows' "high performance" ordering: usually the discrete GPU.
    HighPerformance,
    /// Windows' "power saving" ordering: usually the integrated GPU.
    MinimumPower,
    /// An explicit adapter index as listed by `--devices`.
    Index(u32),
}

impl Backend {
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "auto" => Some(Self::Auto),
            "hardware" | "gpu" => Some(Self::Hardware),
            "software" | "cpu" => Some(Self::Software),
            _ => None,
        }
    }
}

impl AdapterChoice {
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "auto" => Some(Self::Auto),
            "high-performance" | "dgpu" | "discrete" => Some(Self::HighPerformance),
            "minimum-power" | "igpu" | "integrated" => Some(Self::MinimumPower),
            _ => text.parse().ok().map(Self::Index),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdapterKind {
    Discrete,
    Integrated,
    /// Microsoft Basic Render Driver / WARP: never a hardware encoder.
    Software,
}

impl AdapterKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Discrete => "discrete",
            Self::Integrated => "integrated",
            Self::Software => "software",
        }
    }

    /// Classification from DXGI data. Integrated GPUs report little or no
    /// dedicated memory; 512 MiB is the conventional threshold.
    pub fn classify(software_flag: bool, dedicated_video_memory: u64) -> Self {
        if software_flag {
            Self::Software
        } else if dedicated_video_memory < 512 * 1024 * 1024 {
            Self::Integrated
        } else {
            Self::Discrete
        }
    }
}

pub fn vendor_name(vendor_id: u32) -> &'static str {
    match vendor_id {
        0x10de => "nvidia",
        0x1002 | 0x1022 => "amd",
        0x8086 => "intel",
        0x1414 => "microsoft",
        0x5143 => "qualcomm",
        _ => "other",
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Adapter {
    pub index: u32,
    pub kind: AdapterKind,
    /// Position in Windows' high-performance ordering (0 = preferred).
    pub high_performance_rank: u32,
    /// Position in Windows' minimum-power ordering (0 = preferred).
    pub minimum_power_rank: u32,
    pub is_capture: bool,
    /// Hardware encoders for the requested codec bound to this adapter.
    pub hardware_encoders: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Gpu(u32),
    Cpu,
}

/// Ordered encoder targets to try for a preference. Empty means the request
/// cannot be satisfied on this machine (reported, never silently changed).
pub fn plan(
    backend: Backend,
    choice: AdapterChoice,
    adapters: &[Adapter],
    software_available: bool,
) -> Vec<Target> {
    let hardware: Vec<&Adapter> = adapters
        .iter()
        .filter(|a| a.kind != AdapterKind::Software)
        .collect();
    // An explicit choice names exactly one GPU, resolved *before* looking at
    // encoders: if that GPU cannot encode the codec, the request is refused
    // (or falls to the CPU under Backend::Auto), never moved to another GPU.
    let named = match choice {
        AdapterChoice::Auto => None,
        AdapterChoice::Index(index) => Some(hardware.iter().find(|a| a.index == index).copied()),
        AdapterChoice::HighPerformance => Some(
            hardware
                .iter()
                .min_by_key(|a| a.high_performance_rank)
                .copied(),
        ),
        AdapterChoice::MinimumPower => Some(
            hardware
                .iter()
                .min_by_key(|a| a.minimum_power_rank)
                .copied(),
        ),
    };
    let mut gpus: Vec<&Adapter> = match named {
        Some(one) => one.into_iter().collect(),
        None => {
            let mut all = hardware.clone();
            all.sort_by_key(|a| (!a.is_capture, a.high_performance_rank));
            all
        }
    };
    gpus.retain(|a| a.hardware_encoders > 0);
    let mut targets: Vec<Target> = match backend {
        Backend::Software => Vec::new(),
        _ => gpus.iter().map(|a| Target::Gpu(a.index)).collect(),
    };
    if software_available && matches!(backend, Backend::Auto | Backend::Software) {
        targets.push(Target::Cpu);
    }
    targets
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adapters() -> Vec<Adapter> {
        vec![
            // index 0: integrated, drives the display (capture), power saver
            Adapter {
                index: 0,
                kind: AdapterKind::Integrated,
                high_performance_rank: 1,
                minimum_power_rank: 0,
                is_capture: true,
                hardware_encoders: 1,
            },
            // index 1: discrete, high performance
            Adapter {
                index: 1,
                kind: AdapterKind::Discrete,
                high_performance_rank: 0,
                minimum_power_rank: 1,
                is_capture: false,
                hardware_encoders: 1,
            },
            // index 2: Basic Render Driver
            Adapter {
                index: 2,
                kind: AdapterKind::Software,
                high_performance_rank: 2,
                minimum_power_rank: 2,
                is_capture: false,
                hardware_encoders: 0,
            },
        ]
    }

    #[test]
    fn auto_prefers_zero_copy_capture_gpu_then_cpu() {
        assert_eq!(
            plan(Backend::Auto, AdapterChoice::Auto, &adapters(), true),
            vec![Target::Gpu(0), Target::Gpu(1), Target::Cpu]
        );
    }

    #[test]
    fn explicit_gpu_preferences_pick_exactly_one_gpu() {
        let a = adapters();
        assert_eq!(
            plan(Backend::Hardware, AdapterChoice::HighPerformance, &a, true),
            vec![Target::Gpu(1)]
        );
        assert_eq!(
            plan(Backend::Hardware, AdapterChoice::MinimumPower, &a, true),
            vec![Target::Gpu(0)]
        );
        assert_eq!(
            plan(Backend::Hardware, AdapterChoice::Index(1), &a, true),
            vec![Target::Gpu(1)]
        );
        assert_eq!(
            plan(Backend::Auto, AdapterChoice::Index(1), &a, true),
            vec![Target::Gpu(1), Target::Cpu]
        );
    }

    #[test]
    fn unsatisfiable_requests_are_empty_not_substituted() {
        let a = adapters();
        assert!(
            plan(Backend::Hardware, AdapterChoice::Index(2), &a, true).is_empty(),
            "software adapter"
        );
        assert!(
            plan(Backend::Hardware, AdapterChoice::Index(9), &a, true).is_empty(),
            "missing adapter"
        );
        assert!(
            plan(Backend::Software, AdapterChoice::Auto, &a, false).is_empty(),
            "no CPU encoder (HEVC/AV1)"
        );
        let mut none = a.clone();
        none.iter_mut().for_each(|x| x.hardware_encoders = 0);
        assert!(plan(Backend::Hardware, AdapterChoice::Auto, &none, true).is_empty());
        assert_eq!(
            plan(Backend::Software, AdapterChoice::HighPerformance, &a, true),
            vec![Target::Cpu]
        );
    }

    #[test]
    fn explicit_gpu_without_codec_encoder_is_never_swapped_for_another_gpu() {
        // Observed on a dGPU + iGPU machine: the iGPU has no AV1 encoder.
        let mut a = adapters();
        a[0].hardware_encoders = 0; // integrated, minimum-power GPU
        assert!(plan(Backend::Hardware, AdapterChoice::MinimumPower, &a, false).is_empty());
        assert_eq!(
            plan(Backend::Auto, AdapterChoice::MinimumPower, &a, true),
            vec![Target::Cpu]
        );
        assert_eq!(
            plan(Backend::Hardware, AdapterChoice::Auto, &a, false),
            vec![Target::Gpu(1)]
        );
    }

    #[test]
    fn parses_user_choices_and_classifies_adapters() {
        assert_eq!(Backend::parse("cpu"), Some(Backend::Software));
        assert_eq!(Backend::parse("gpu"), Some(Backend::Hardware));
        assert_eq!(
            AdapterChoice::parse("igpu"),
            Some(AdapterChoice::MinimumPower)
        );
        assert_eq!(
            AdapterChoice::parse("dgpu"),
            Some(AdapterChoice::HighPerformance)
        );
        assert_eq!(AdapterChoice::parse("2"), Some(AdapterChoice::Index(2)));
        assert_eq!(AdapterChoice::parse("fast"), None);
        assert_eq!(AdapterKind::classify(true, 1 << 33), AdapterKind::Software);
        assert_eq!(
            AdapterKind::classify(false, 128 << 20),
            AdapterKind::Integrated
        );
        assert_eq!(AdapterKind::classify(false, 8 << 30), AdapterKind::Discrete);
        assert_eq!(vendor_name(0x10de), "nvidia");
    }
}
