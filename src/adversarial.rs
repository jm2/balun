//! Test-only deterministic mutation driver. No production parser/authority API is exposed.

use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};

pub(crate) struct Generator(u64);

impl Generator {
    // SplitMix64 with explicit wrapping arithmetic; stable across platforms and toolchains.
    pub(crate) fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut value = self.0;
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        value ^ (value >> 31)
    }

    pub(crate) fn index(&mut self, limit: usize) -> usize {
        assert!(limit > 0);
        usize::try_from(self.next() % u64::try_from(limit).unwrap()).unwrap()
    }

    pub(crate) fn bytes(&mut self, limit: usize) -> Vec<u8> {
        (0..self.index(limit + 1))
            .map(|_| self.next().to_le_bytes()[0])
            .collect()
    }

    pub(crate) fn mutate(&mut self, seed: &[u8], limit: usize) -> Vec<u8> {
        let mut bytes = seed[..seed.len().min(limit)].to_vec();
        for _ in 0..=self.index(8) {
            match self.index(5) {
                0 if !bytes.is_empty() => {
                    let index = self.index(bytes.len());
                    bytes[index] ^= 1 << self.index(8);
                }
                1 if !bytes.is_empty() => {
                    bytes.remove(self.index(bytes.len()));
                }
                2 if bytes.len() < limit => {
                    bytes.insert(self.index(bytes.len() + 1), self.next().to_le_bytes()[0]);
                }
                3 => {
                    bytes.truncate(self.index(bytes.len() + 1));
                }
                _ if bytes.len() < limit => {
                    let extra = self.bytes((limit - bytes.len()).min(32));
                    bytes.extend(extra);
                }
                _ => {}
            }
        }
        bytes
    }
}

fn number(name: &str, default: u64, maximum: u64) -> u64 {
    let value = match std::env::var(name) {
        Ok(value) => value.parse().unwrap_or_else(|_| panic!("invalid {name}")),
        Err(std::env::VarError::NotPresent) => default,
        Err(_) => panic!("invalid {name}"),
    };
    assert!(value <= maximum, "{name} exceeds the test budget");
    value
}

pub(crate) fn run(name: &str, mut property: impl FnMut(&mut Generator)) {
    let seed = number("BALUN_ADVERSARIAL_SEED", 20260917, i64::MAX as u64);
    let cases = number("BALUN_ADVERSARIAL_CASES", 128, 100_000);
    assert!(cases > 0, "an empty adversarial run is not evidence");
    let first = number("BALUN_ADVERSARIAL_START", 0, 100_000);
    assert!(
        first + cases <= 100_000,
        "case range exceeds the test budget"
    );
    let domain = name.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x100_0000_01b3)
    });
    for case in first..first + cases {
        let mut generator = Generator(seed ^ domain ^ case.wrapping_mul(0x9e37_79b9_7f4a_7c15));
        if let Err(failure) = catch_unwind(AssertUnwindSafe(|| property(&mut generator))) {
            let replay = format!(
                "suite={name}\nBALUN_ADVERSARIAL_SEED={seed}\nBALUN_ADVERSARIAL_START={case}\nBALUN_ADVERSARIAL_CASES=1\n"
            );
            eprintln!("Adversarial counterexample (synthetic inputs only):\n{replay}");
            if let Some(directory) = std::env::var_os("BALUN_ADVERSARIAL_FAILURE_DIR") {
                let path = std::path::PathBuf::from(directory);
                let result = std::fs::create_dir_all(&path).and_then(|()| {
                    std::fs::write(path.join(format!("{name}-{seed}-{case}.txt")), &replay)
                });
                if let Err(error) = result {
                    eprintln!("Could not save replay receipt: {error}");
                }
            }
            resume_unwind(failure);
        }
    }
    eprintln!("adversarial {name}: seed={seed}, start={first}, cases={cases}");
}
